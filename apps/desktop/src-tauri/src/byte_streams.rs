//! Bounded chunks over file and HTTP bodies; no aggregate content quota.
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::host_services::HostInvocation;
use crate::plugin_runtime::plugin_error;
use crate::remote_interaction::RemoteContext;
use base64::Engine as _;
use serde_json::{Value, json};
use wonderland_plugin_protocol::{
    ByteStream, FileResult, FileWriteMode, LocalFileState, PluginError, STREAM_CHUNK_BYTES,
    StreamChunk,
};

// Bound simultaneously open handles, independently of content size or lifetime.
const MAX_OPEN_STREAMS: usize = 256;

pub(crate) struct TemporaryFile {
    pub(crate) path: PathBuf,
}
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
impl TemporaryFile {
    pub(crate) fn create(parent: &Path) -> Result<(Arc<Self>, File), PluginError> {
        let token = crate::web_access::random_key().map_err(|e| plugin_error("INTERNAL", &e))?;
        let path = parent.join(format!(".wonderland-{token}.tmp"));
        let file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&path)
            .map_err(io_error)?;
        Ok((Arc::new(Self { path }), file))
    }
}

pub(crate) enum WriteTarget {
    Path {
        destination: PathBuf,
        mode: FileWriteMode,
    },
    Download {
        name: String,
        remote: Arc<RemoteContext>,
    },
    Spool,
}

enum Content {
    Closed,
    File(File),
    Http(Arc<tokio::sync::Mutex<HttpReader>>),
    Writer {
        file: File,
        temporary: Arc<TemporaryFile>,
        target: WriteTarget,
        size: u64,
    },
    Spool(Arc<TemporaryFile>),
}
struct Entry {
    invocation: HostInvocation,
    closed: Arc<AtomicBool>,
    content: Content,
}

struct HttpReader {
    body: wonderland_net::HttpResponseBody,
    pending: Vec<u8>,
    position: usize,
}

enum ReadSource {
    File(Vec<u8>, bool),
    Http(Arc<tokio::sync::Mutex<HttpReader>>),
}

fn check_stream(origin: &HostInvocation, closed: &AtomicBool) -> Result<(), PluginError> {
    origin.check()?;
    if closed.load(Ordering::Acquire) {
        return Err(plugin_error("INVALID_STREAM", "Stream is closed."));
    }
    Ok(())
}

async fn stream_ended(origin: &HostInvocation, closed: &AtomicBool) -> PluginError {
    loop {
        if let Err(error) = check_stream(origin, closed) {
            return error;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[derive(Default)]
pub(crate) struct ByteStreams {
    entries: Mutex<HashMap<String, Arc<Mutex<Entry>>>>,
}

fn io_error(error: std::io::Error) -> PluginError {
    let code = match error.kind() {
        std::io::ErrorKind::AlreadyExists => "FILE_EXISTS",
        std::io::ErrorKind::NotFound => "FILE_NOT_FOUND",
        std::io::ErrorKind::PermissionDenied => "FILE_ACCESS_DENIED",
        _ => "FILE_ERROR",
    };
    plugin_error(code, &error.to_string())
}

impl ByteStreams {
    fn insert(
        &self,
        content: Content,
        size: Option<u64>,
        invocation: &HostInvocation,
    ) -> Result<ByteStream, PluginError> {
        invocation.check()?;
        let id = crate::web_access::random_key().map_err(|e| plugin_error("INTERNAL", &e))?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Stream registry is unavailable."))?;
        if entries.len() >= MAX_OPEN_STREAMS {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Too many byte streams are open; finish or close unused handles.",
            ));
        }
        entries.insert(
            id.clone(),
            Arc::new(Mutex::new(Entry {
                invocation: invocation.clone(),
                closed: Arc::new(AtomicBool::new(false)),
                content,
            })),
        );
        Ok(ByteStream {
            stream_id: id,
            size,
        })
    }
    fn entry(
        &self,
        id: &str,
        invocation: &HostInvocation,
    ) -> Result<Arc<Mutex<Entry>>, PluginError> {
        let entry = self
            .entries
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Stream registry is unavailable."))?
            .get(id)
            .cloned()
            .ok_or_else(|| plugin_error("INVALID_STREAM", "Stream is closed or unavailable."))?;
        {
            let guard = entry
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Stream is unavailable."))?;
            if !guard.invocation.same_device(invocation) {
                return Err(plugin_error(
                    "UNAUTHORIZED",
                    "Stream belongs to a different device.",
                ));
            }
            if let Err(error) = check_stream(&guard.invocation, &guard.closed) {
                drop(guard);
                self.entries.lock().unwrap().remove(id);
                return Err(error);
            }
        }
        Ok(entry)
    }
    pub(crate) fn open_read(
        &self,
        path: &Path,
        invocation: &HostInvocation,
    ) -> Result<ByteStream, PluginError> {
        let file = File::open(path).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() {
            return Err(plugin_error("INVALID_INPUT", "Path is not a file."));
        }
        self.insert(Content::File(file), Some(metadata.len()), invocation)
    }
    pub(crate) fn http(
        &self,
        body: wonderland_net::HttpResponseBody,
        invocation: &HostInvocation,
    ) -> Result<ByteStream, PluginError> {
        self.insert(
            Content::Http(Arc::new(tokio::sync::Mutex::new(HttpReader {
                body,
                pending: Vec::new(),
                position: 0,
            }))),
            None,
            invocation,
        )
    }
    pub(crate) fn open_write(
        &self,
        parent: &Path,
        target: WriteTarget,
        invocation: &HostInvocation,
    ) -> Result<ByteStream, PluginError> {
        let (temporary, file) = TemporaryFile::create(parent)?;
        self.insert(
            Content::Writer {
                file,
                temporary,
                target,
                size: 0,
            },
            None,
            invocation,
        )
    }
    pub(crate) fn write(
        &self,
        id: &str,
        encoded: &str,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        if encoded.len() > STREAM_CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Write data exceeds the one-chunk frame budget.",
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| plugin_error("INVALID_INPUT", "Invalid chunk base64."))?;
        if bytes.len() > STREAM_CHUNK_BYTES {
            return Err(plugin_error("INVALID_INPUT", "Chunk is too large."));
        }
        let entry = self.entry(id, invocation)?;
        let mut guard = entry
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Stream is unavailable."))?;
        check_stream(&guard.invocation, &guard.closed)?;
        invocation.check()?;
        let Content::Writer { file, size, .. } = &mut guard.content else {
            return Err(plugin_error("INVALID_STREAM", "Stream is not writable."));
        };
        if let Err(error) = file.write_all(&bytes) {
            // A failed write may have appended a prefix. Poison the transaction:
            // catching the error must never let a later finish commit partial data.
            guard.closed.store(true, Ordering::Release);
            guard.content = Content::Closed;
            drop(guard);
            self.entries.lock().unwrap().remove(id);
            return Err(io_error(error));
        }
        *size = size
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "File size cannot be represented."))?;
        invocation.check()?;
        Ok(json!({"size":*size}))
    }
    pub(crate) async fn read(
        &self,
        id: &str,
        count: usize,
        invocation: &HostInvocation,
    ) -> Result<StreamChunk, PluginError> {
        if count == 0 || count > STREAM_CHUNK_BYTES {
            return Err(plugin_error("INVALID_INPUT", "Read chunk size is invalid."));
        }
        let entry = self.entry(id, invocation)?;
        // The registry and synchronous file lock never span an async wait. HTTP
        // reads use a separate async mutex, so close can interrupt a pending read.
        let (origin, closed, source) = {
            let mut guard = entry
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Stream is unavailable."))?;
            check_stream(&guard.invocation, &guard.closed)?;
            invocation.check()?;
            let source = match &mut guard.content {
                Content::File(file) => {
                    let mut bytes = vec![0; count];
                    let read = file.read(&mut bytes).map_err(io_error)?;
                    bytes.truncate(read);
                    ReadSource::File(bytes, read == 0)
                }
                Content::Http(reader) => ReadSource::Http(reader.clone()),
                _ => return Err(plugin_error("INVALID_STREAM", "Stream is not readable.")),
            };
            (guard.invocation.clone(), guard.closed.clone(), source)
        };
        let (bytes, eof) = match source {
            ReadSource::File(bytes, eof) => (bytes, eof),
            ReadSource::Http(reader) => {
                let mut reader = tokio::select! {
                    biased;
                    error = stream_ended(&origin, &closed) => return Err(error),
                    reader = reader.lock() => reader,
                };
                let HttpReader {
                    body,
                    pending,
                    position,
                } = &mut *reader;
                while *position >= pending.len() {
                    check_stream(&origin, &closed)?;
                    let result = tokio::select! {
                        biased;
                        error = stream_ended(&origin, &closed) => return Err(error),
                        result = body.chunk() => result,
                    };
                    match result.map_err(crate::host_services::transport_error)? {
                        Some(chunk) if chunk.is_empty() => continue,
                        Some(chunk) => {
                            *pending = chunk;
                            *position = 0;
                            break;
                        }
                        None => {
                            pending.clear();
                            *position = 0;
                            break;
                        }
                    }
                }
                check_stream(&origin, &closed)?;
                let end = pending.len().min(*position + count);
                let bytes = pending[*position..end].to_vec();
                *position = end;
                (bytes, pending.is_empty())
            }
        };
        check_stream(&origin, &closed)?;
        invocation.check()?;
        if eof {
            closed.store(true, Ordering::Release);
            self.entries.lock().unwrap().remove(id);
        }
        Ok(StreamChunk {
            content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            eof,
        })
    }
    pub(crate) fn finish(
        &self,
        id: &str,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        let entry = self.entry(id, invocation)?;
        let mut guard = entry
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Stream is unavailable."))?;
        check_stream(&guard.invocation, &guard.closed)?;
        invocation.check()?;
        if !matches!(guard.content, Content::Writer { .. }) {
            return Err(plugin_error("INVALID_STREAM", "Stream is not writable."));
        }
        let origin = guard.invocation.clone();
        let content = std::mem::replace(&mut guard.content, Content::Closed);
        let Content::Writer {
            file,
            temporary,
            target,
            size,
        } = content
        else {
            unreachable!();
        };
        let result = (|| {
            file.sync_all().map_err(io_error)?;
            drop(file);
            origin.check()?;
            invocation.check()?;
            let value = match target {
                WriteTarget::Path { destination, mode } => {
                    commit_file(&temporary.path, &destination, mode).map_err(io_error)?;
                    serde_json::to_value(FileResult::Local {
                        state: LocalFileState::Saved,
                        name: destination
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into(),
                        size,
                        path: destination.to_string_lossy().into(),
                    })
                    .map_err(|e| plugin_error("INTERNAL", &e.to_string()))?
                }
                WriteTarget::Download { name, remote } => {
                    remote.download_file(name, temporary, size)?
                }
                WriteTarget::Spool => {
                    guard.content = Content::Spool(temporary);
                    return Ok(json!({"streamId":id,"size":size}));
                }
            };
            Ok(value)
        })();
        let retain = result.is_ok() && matches!(guard.content, Content::Spool(_));
        drop(guard);
        if !retain {
            self.entries.lock().unwrap().remove(id);
        }
        result
    }
    pub(crate) fn spool(
        &self,
        id: &str,
        invocation: &HostInvocation,
    ) -> Result<Arc<TemporaryFile>, PluginError> {
        let entry = self.entry(id, invocation)?;
        let guard = entry
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Stream is unavailable."))?;
        check_stream(&guard.invocation, &guard.closed)?;
        invocation.check()?;
        match &guard.content {
            Content::Spool(file) => Ok(file.clone()),
            _ => Err(plugin_error(
                "INVALID_STREAM",
                "Upload stream must be finished first.",
            )),
        }
    }
    pub(crate) fn close(
        &self,
        id: &str,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        let entry = self.entry(id, invocation)?;
        entry.lock().unwrap().closed.store(true, Ordering::Release);
        self.entries.lock().unwrap().remove(id);
        Ok(json!({"ok":true}))
    }
    pub(crate) fn prune(&self) {
        self.entries
            .lock()
            .unwrap()
            .retain(|_, entry| match entry.try_lock() {
                Ok(entry) => {
                    let keep = check_stream(&entry.invocation, &entry.closed).is_ok();
                    if !keep {
                        entry.closed.store(true, Ordering::Release);
                    }
                    keep
                }
                Err(_) => true,
            });
    }
}

#[cfg(test)]
#[path = "byte_streams_tests.rs"]
mod tests;

#[cfg(windows)]
fn commit_file(source: &Path, destination: &Path, mode: FileWriteMode) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    // std::fs supports long Windows paths, but raw Win32 calls need verbatim
    // absolute paths. Resolve only the destination's parent: resolving an
    // existing destination itself would follow a symlink before replacement.
    let source = fs::canonicalize(source)?;
    let destination = fs::canonicalize(destination.parent().unwrap_or(Path::new(".")))?.join(
        destination.file_name().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "File name is missing.")
        })?,
    );
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if mode == FileWriteMode::Replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn commit_file(source: &Path, destination: &Path, mode: FileWriteMode) -> std::io::Result<()> {
    if mode == FileWriteMode::Create {
        fs::hard_link(source, destination)
    } else {
        fs::rename(source, destination)
    }
}
