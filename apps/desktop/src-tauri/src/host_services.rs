//! Core-owned file, browser, secret and network services, independent of process IPC.
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::byte_streams::{ByteStreams, WriteTarget};
use base64::Engine as _;
use serde_json::{Value, json};
use wonderland_plugin_protocol::{
    FileResult, FileWriteMode, HttpBody, HttpRequestParams, HttpResponseMeta, LocalFileState,
    PluginError,
};

use crate::host_environment::HostEnvironment;
use crate::plugin_manager::WeakPluginManager;
use crate::plugin_runtime::plugin_error;
use crate::remote_interaction::RemoteContext;

const MAX_PICKED_FILE_CHUNK_BYTES: u64 = 1024 * 1024;
// Only the legacy single-frame convenience result has a frame budget, not a file quota.
const MAX_PICKED_FILE_INLINE_BYTES: u64 = (32 * 1024 * 1024 - 64 * 1024) / 4 * 3;

#[derive(Clone)]
pub(crate) struct RuntimeServices {
    pub(crate) environment: Arc<dyn HostEnvironment>,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) manager: WeakPluginManager,
    pub(crate) instance_id: u64,
    pub(crate) startup_revision: u64,
}

impl RuntimeServices {
    pub(crate) fn new(
        environment: Arc<dyn HostEnvironment>,
        runtime: tokio::runtime::Handle,
        manager: WeakPluginManager,
    ) -> Self {
        Self {
            environment,
            runtime,
            manager,
            instance_id: 0,
            startup_revision: 0,
        }
    }

    pub(crate) fn with_instance(mut self, instance_id: u64, startup_revision: u64) -> Self {
        self.instance_id = instance_id;
        self.startup_revision = startup_revision;
        self
    }
}

pub(crate) struct HostServices {
    pub(crate) services: RuntimeServices,
    plugin_id: String,
    transport: wonderland_net::HttpTransport,
    streams: ByteStreams,
    data_dir: PathBuf,
    export_dir: PathBuf,
    picked_files: Mutex<HashMap<String, PickedFile>>,
    browsers: crate::browser_sessions::BrowserSessions,
}

struct PickedFile {
    path: PathBuf,
    name: String,
    mime_type: String,
    max_bytes: u64,
    size: u64,
}

/// Cancellation belongs to an invocation rather than to the process lifetime.
#[derive(Clone, Default)]
pub(crate) struct HostInvocation {
    pub(crate) remote: Option<Arc<RemoteContext>>,
    pub(crate) cancelled: Option<Arc<AtomicBool>>,
    ancestors: Vec<Arc<AtomicBool>>,
    depth: usize,
    deadline: Option<Instant>,
}

impl HostInvocation {
    /// A cancellable manager wait shares ancestor budgets without adding a call depth.
    pub(crate) fn for_wait(&self, timeout_ms: Option<u64>) -> Result<Self, PluginError> {
        self.check()?;
        let mut context = self.clone();
        if let Some(cancelled) = context.cancelled.take() {
            context.ancestors.push(cancelled);
        }
        context.cancelled = Some(Arc::new(AtomicBool::new(false)));
        context.with_timeout_ms(timeout_ms)
    }
    pub(crate) fn for_call(
        remote: Option<Arc<RemoteContext>>,
        timeout_ms: Option<u64>,
        parent: Option<&Self>,
    ) -> Result<Self, PluginError> {
        if let Some(parent) = parent {
            parent.check()?;
            if parent.depth >= 16 {
                return Err(plugin_error(
                    "CALL_DEPTH_EXCEEDED",
                    "Plugin call depth exceeded.",
                ));
            }
        }
        let mut ancestors = parent
            .map(|parent| parent.ancestors.clone())
            .unwrap_or_default();
        if let Some(token) = parent.and_then(|parent| parent.cancelled.clone()) {
            ancestors.push(token);
        }
        Self {
            remote,
            cancelled: Some(Arc::new(AtomicBool::new(false))),
            ancestors,
            depth: parent.map_or(0, |parent| parent.depth + 1),
            deadline: parent.and_then(|parent| parent.deadline),
        }
        .with_timeout_ms(timeout_ms)
    }

    pub(crate) fn with_timeout_ms(mut self, timeout_ms: Option<u64>) -> Result<Self, PluginError> {
        if let Some(timeout) = timeout_ms {
            if timeout == 0 {
                return Err(plugin_error(
                    "INVALID_INPUT",
                    "Deadline must be a positive duration.",
                ));
            }
            let deadline = Instant::now()
                .checked_add(Duration::from_millis(timeout))
                .ok_or_else(|| plugin_error("INVALID_INPUT", "Deadline cannot be represented."))?;
            self.deadline = Some(
                self.deadline
                    .map_or(deadline, |current| current.min(deadline)),
            );
        }
        Ok(self)
    }

    pub(crate) fn same_device(&self, other: &Self) -> bool {
        match (&self.remote, &other.remote) {
            (None, None) => true,
            (Some(left), Some(right)) => left.same_owner(right),
            _ => false,
        }
    }

    pub(crate) fn cancel(&self) {
        if let Some(token) = &self.cancelled {
            token.store(true, Ordering::Release);
        }
        if let Some(remote) = &self.remote {
            remote.cancel();
        }
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub(crate) fn with_cancellation_parent(mut self, parent: Arc<AtomicBool>) -> Self {
        self.ancestors.push(parent);
        self
    }

    pub(crate) fn check(&self) -> Result<(), PluginError> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
            || self
                .ancestors
                .iter()
                .any(|flag| flag.load(Ordering::Acquire))
            || self
                .remote
                .as_ref()
                .is_some_and(|remote| remote.is_cancelled())
        {
            Err(plugin_error("CANCELLED", "Host invocation has ended."))
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Err(plugin_error(
                "TIMEOUT",
                "Plugin invocation exceeded its time budget.",
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) async fn ended(&self) -> PluginError {
        loop {
            if let Err(error) = self.check() {
                return error;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl HostServices {
    pub(crate) fn new(
        plugin_id: String,
        data_dir: PathBuf,
        documents_dir: PathBuf,
        services: RuntimeServices,
    ) -> Self {
        let export_dir = documents_dir
            .join("Wonderland Assistant")
            .join(&plugin_id)
            .join("Exports");
        Self {
            browsers: crate::browser_sessions::BrowserSessions::new(
                services.environment.clone(),
                &data_dir,
                services.runtime.clone(),
            ),
            services,
            plugin_id,
            data_dir,
            export_dir,
            transport: wonderland_net::HttpTransport::new(),
            streams: ByteStreams::default(),
            picked_files: Mutex::new(HashMap::new()),
        }
    }

    fn wait<F, T>(&self, invocation: &HostInvocation, future: F) -> Result<T, PluginError>
    where
        F: Future<Output = Result<T, PluginError>>,
    {
        self.services.runtime.block_on(async {
            tokio::select! {
                biased;
                error = invocation.ended() => Err(error),
                result = future => result,
            }
        })
    }

    pub(crate) fn call(
        &self,
        method: &str,
        params: &Value,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        self.streams.prune();
        invocation
            .check()
            .map_err(|error| transport_interruption(method, error))?;
        let result = if let Some(remote) = &invocation.remote
            && matches!(
                method,
                "core.files.pick"
                    | "core.files.read"
                    | "core.files.export"
                    | "core.files.export_dir"
                    | "core.files.reveal_own"
                    | "core.browser.open"
            ) {
            remote.service_with_invocation(method, params, invocation)
        } else {
            match method {
                method if method.starts_with("core.lifecycle.") => self
                    .services
                    .manager
                    .upgrade()
                    .ok_or_else(|| {
                        plugin_error("SERVICE_UNAVAILABLE", "Plugin manager is unavailable.")
                    })?
                    .lifecycle_call(
                        &self.plugin_id,
                        self.services.instance_id,
                        method,
                        params,
                        invocation,
                    ),
                method if method.starts_with("core.browser.session.") => {
                    self.browsers.call(method, params, invocation)
                }
                "core.network.request" => self.network_request(params, invocation),
                "core.network.public" | "core.network.model" => Err(plugin_error(
                    "METHOD_NOT_FOUND",
                    "This Host method was removed; update the plugin to core.network.request.",
                )),
                "core.files.open_read" => self.open_read(params, invocation),
                "core.files.open_write" => self.open_write(params, invocation),
                "core.files.export_open" => self.export_open(params, invocation),
                "core.streams.create_spool" => self.create_spool(invocation),
                "core.streams.read" => self.read_stream(params, invocation),
                "core.streams.write" => self.streams.write(
                    stream_id(params)?,
                    params
                        .get("contentBase64")
                        .and_then(Value::as_str)
                        .ok_or_else(|| plugin_error("INVALID_INPUT", "Stream data is missing."))?,
                    invocation,
                ),
                "core.streams.finish" => self.streams.finish(stream_id(params)?, invocation),
                "core.streams.close" => self.streams.close(stream_id(params)?, invocation),
                "core.secrets.plugin.get" => self.secret_get(params),
                "core.secrets.plugin.set" => self.secret_set(params),
                "core.secrets.plugin.delete" => self.secret_delete(params),
                "core.secrets.plugin.has" => self.secret_has(params),
                "core.files.pick" => self.pick_file(params, invocation),
                "core.files.read" => self.read_picked_file(params),
                "core.files.export" => self.export_file(params),
                "core.files.export_dir" => self.export_dir(),
                "core.files.reveal_own" => self.reveal_own(params),
                "core.browser.open" => {
                    self.services.environment.open_url(&browser_url(params)?)?;
                    Ok(json!({ "ok": true }))
                }
                "core.plugins.call" => {
                    let call: wonderland_plugin_protocol::PluginCallParams =
                        serde_json::from_value(params.clone())
                            .map_err(|error| plugin_error("INVALID_INPUT", &error.to_string()))?;
                    let manager = self.services.manager.upgrade().ok_or_else(|| {
                        plugin_error("SERVICE_UNAVAILABLE", "Plugin manager is unavailable.")
                    })?;
                    manager.invoke_plugin_with_invocation(
                        &self.plugin_id,
                        &call.plugin_id,
                        &call.method,
                        call.params,
                        call.timeout_ms,
                        invocation,
                    )
                }
                _ => Err(plugin_error(
                    "METHOD_NOT_FOUND",
                    "Core service is not available.",
                )),
            }
        };
        let result = result.map_err(|error| transport_interruption(method, error));
        self.streams.prune();
        if let Err(error) = invocation.check() {
            if method == "core.files.pick"
                && let Ok(result) = &result
                && let Some(handle) = result.get("handle").and_then(Value::as_str)
            {
                self.picked_files.lock().unwrap().remove(handle);
            }
            if let Err(existing) = &result
                && matches!(method, "core.network.request" | "core.streams.read")
                && existing.details.is_some()
            {
                return result;
            }
            return Err(transport_interruption(method, error));
        }
        result
    }
    pub(crate) fn cleanup(&self) {
        self.streams.prune();
    }
    fn pick_file(&self, params: &Value, invocation: &HostInvocation) -> Result<Value, PluginError> {
        let extensions = match params.get("extensions") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .filter(|items| items.len() <= 128)
                .ok_or_else(|| {
                    plugin_error("INVALID_INPUT", "File picker extensions are invalid.")
                })?
                .iter()
                .map(|item| item.as_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    plugin_error("INVALID_INPUT", "File picker extensions are invalid.")
                })?,
        };
        if extensions.iter().any(|extension| {
            extension.is_empty()
                || extension.len() > 32
                || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }) {
            return Err(plugin_error(
                "INVALID_INPUT",
                "File picker extensions are invalid.",
            ));
        }
        let max_bytes = match params.get("maxBytes") {
            None => u64::MAX,
            Some(value) => value
                .as_u64()
                .filter(|size| *size > 0)
                .ok_or_else(|| plugin_error("INVALID_INPUT", "File size limit is invalid."))?,
        };
        let extensions_owned = extensions
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        let Some(path) = self.services.environment.pick_file(&extensions_owned)? else {
            return Ok(json!({ "cancelled": true }));
        };
        invocation.check()?;
        let metadata = fs::metadata(&path)
            .map_err(|error| plugin_error("INTERNAL", &format!("Cannot inspect file: {error}")))?;
        if !metadata.is_file() || metadata.len() > max_bytes {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Selected file exceeds its size limit.",
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Selected file name is invalid."))?
            .to_owned();
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !extensions.is_empty()
            && !extensions
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&extension))
        {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Selected file type does not match the requested file types.",
            ));
        }
        let mime_type = file_mime_type(&extension).to_owned();
        let token = file_handle_token()?;
        let mut picked_files = self
            .picked_files
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "File handle table is unavailable."))?;
        if picked_files.len() >= 32 {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Too many selected files are waiting to be read.",
            ));
        }
        picked_files.insert(
            token.clone(),
            PickedFile {
                path,
                name: name.clone(),
                mime_type: mime_type.clone(),
                max_bytes,
                size: metadata.len(),
            },
        );
        Ok(json!({
            "cancelled": false,
            "handle": token,
            "name": name,
            "mimeType": mime_type,
            "size": metadata.len(),
        }))
    }

    fn read_picked_file(&self, params: &Value) -> Result<Value, PluginError> {
        let handle = params
            .get("handle")
            .and_then(Value::as_str)
            .filter(|handle| !handle.is_empty() && handle.len() <= 128)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "File handle is invalid."))?;
        if params.get("offset").is_some() || params.get("chunkBytes").is_some() {
            let offset = params
                .get("offset")
                .and_then(Value::as_u64)
                .ok_or_else(|| plugin_error("INVALID_INPUT", "File chunk offset is invalid."))?;
            let chunk_bytes = params
                .get("chunkBytes")
                .and_then(Value::as_u64)
                .filter(|size| (1..=MAX_PICKED_FILE_CHUNK_BYTES).contains(size))
                .ok_or_else(|| plugin_error("INVALID_INPUT", "File chunk size is invalid."))?;
            return self.read_picked_file_chunk(handle, offset, chunk_bytes);
        }
        let picked = self
            .picked_files
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "File handle table is unavailable."))?
            .remove(handle)
            .ok_or_else(|| plugin_error("UNAUTHORIZED", "File handle is invalid or expired."))?;
        let metadata = fs::metadata(&picked.path).map_err(|error| {
            plugin_error(
                "INTERNAL",
                &format!("Cannot inspect selected file: {error}"),
            )
        })?;
        if !metadata.is_file()
            || metadata.len() > picked.max_bytes
            || metadata.len() > MAX_PICKED_FILE_INLINE_BYTES
        {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Selected file exceeds its size limit.",
            ));
        }
        let file = fs::File::open(&picked.path).map_err(|error| {
            plugin_error("INTERNAL", &format!("Cannot read selected file: {error}"))
        })?;
        let mut bytes = Vec::with_capacity(metadata.len().min(picked.max_bytes) as usize);
        file.take(
            picked
                .max_bytes
                .min(MAX_PICKED_FILE_INLINE_BYTES)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|error| {
            plugin_error("INTERNAL", &format!("Cannot read selected file: {error}"))
        })?;
        if bytes.len() as u64 > picked.max_bytes
            || bytes.len() as u64 > MAX_PICKED_FILE_INLINE_BYTES
        {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Selected file exceeds its size limit.",
            ));
        }
        Ok(json!({
            "name": picked.name,
            "mimeType": picked.mime_type,
            "contentBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
        }))
    }

    fn read_picked_file_chunk(
        &self,
        handle: &str,
        offset: u64,
        chunk_bytes: u64,
    ) -> Result<Value, PluginError> {
        let picked_files = self
            .picked_files
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "File handle table is unavailable."))?;
        let picked = picked_files
            .get(handle)
            .ok_or_else(|| plugin_error("UNAUTHORIZED", "File handle is invalid or expired."))?;
        if offset > picked.size || offset > picked.max_bytes {
            return Err(plugin_error(
                "INVALID_INPUT",
                "File chunk offset is outside the selected file.",
            ));
        }
        let path = picked.path.clone();
        let name = picked.name.clone();
        let mime_type = picked.mime_type.clone();
        let expected_size = picked.size;
        let max_bytes = picked.max_bytes;
        let remaining = expected_size.saturating_sub(offset);
        let read_size = remaining.min(chunk_bytes);
        let is_final = offset + read_size >= expected_size;
        drop(picked_files);
        let metadata = fs::metadata(&path)
            .map_err(|_| plugin_error("INTERNAL", "Selected file is unavailable."))?;
        if !metadata.is_file() || metadata.len() != expected_size || metadata.len() > max_bytes {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Selected file changed or exceeds its size limit.",
            ));
        }
        let mut file = fs::File::open(&path)
            .map_err(|_| plugin_error("INTERNAL", "Selected file cannot be read."))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| plugin_error("INTERNAL", "Selected file cannot be read."))?;
        let mut bytes = vec![0; read_size as usize];
        file.read_exact(&mut bytes)
            .map_err(|_| plugin_error("INTERNAL", "Selected file cannot be read."))?;
        if is_final && let Ok(mut table) = self.picked_files.lock() {
            table.remove(handle);
        }
        Ok(json!({
            "name": name,
            "mimeType": mime_type,
            "contentBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
            "offset": offset,
            "eof": is_final,
        }))
    }

    fn export_file(&self, params: &Value) -> Result<Value, PluginError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| safe_export_name(name))
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Export file name is invalid."))?;
        let content = params
            .get("contentBase64")
            .and_then(Value::as_str)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Export content is invalid."))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|_| plugin_error("INVALID_INPUT", "Export content is not valid base64."))?;
        fs::create_dir_all(&self.export_dir).map_err(|error| {
            plugin_error(
                "INTERNAL",
                &format!("Cannot create export directory: {error}"),
            )
        })?;
        let path = self.export_dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                plugin_error("INTERNAL", &format!("Cannot create export file: {error}"))
            })?;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(plugin_error(
                "INTERNAL",
                &format!("Cannot write export file: {error}"),
            ));
        }
        serde_json::to_value(FileResult::Local {
            state: LocalFileState::Saved,
            name: name.into(),
            size: bytes.len() as u64,
            path: path.to_string_lossy().into_owned(),
        })
        .map_err(|_| internal_error("Cannot serialize exported file result."))
    }

    fn export_dir(&self) -> Result<Value, PluginError> {
        fs::create_dir_all(&self.export_dir).map_err(|error| {
            plugin_error(
                "INTERNAL",
                &format!("Cannot create export directory: {error}"),
            )
        })?;
        Ok(json!({ "path": self.export_dir.to_string_lossy() }))
    }

    fn secret_path(&self, key: &str) -> Result<PathBuf, PluginError> {
        if key.is_empty()
            || key.len() > 120
            || key == "."
            || key == ".."
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Plugin secret key is invalid.",
            ));
        }
        let app_data = self
            .data_dir
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| plugin_error("INTERNAL", "Plugin secret directory is unavailable."))?;
        Ok(app_data
            .join("plugin-secrets")
            .join(&self.plugin_id)
            .join(format!("{key}.dpapi")))
    }

    fn secret_get(&self, params: &Value) -> Result<Value, PluginError> {
        let key = params
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Plugin secret key is invalid."))?;
        let path = self.secret_path(key)?;
        let sealed = match fs::read(path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({ "value": null }));
            }
            Err(_) => return Err(plugin_error("INTERNAL", "Plugin secret is unavailable.")),
        };
        let plain = wonderland_secret::unseal(&sealed)
            .map_err(|_| plugin_error("INTERNAL", "Plugin secret cannot be decrypted."))?;
        let value = String::from_utf8(plain)
            .map_err(|_| plugin_error("INTERNAL", "Plugin secret is not valid text."))?;
        Ok(json!({ "value": value }))
    }

    fn secret_set(&self, params: &Value) -> Result<Value, PluginError> {
        let key = params
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Plugin secret key is invalid."))?;
        let value = params
            .get("value")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty() && value.len() <= 16_384)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Plugin secret value is invalid."))?;
        let path = self.secret_path(key)?;
        let parent = path
            .parent()
            .ok_or_else(|| plugin_error("INTERNAL", "Plugin secret directory is unavailable."))?;
        fs::create_dir_all(parent)
            .map_err(|_| plugin_error("INTERNAL", "Plugin secret directory cannot be created."))?;
        let sealed = wonderland_secret::seal(value.as_bytes())
            .map_err(|_| plugin_error("INTERNAL", "Plugin secret cannot be protected."))?;
        let temp = path.with_extension(format!("{}.tmp", file_handle_token()?));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| plugin_error("INTERNAL", "Plugin secret cannot be written."))?;
        let write = file.write_all(&sealed).and_then(|()| file.sync_all());
        drop(file);
        if write
            .and_then(|()| replace_secret_file(&temp, &path))
            .is_err()
        {
            let _ = fs::remove_file(&temp);
            return Err(plugin_error(
                "INTERNAL",
                "Plugin secret cannot be committed.",
            ));
        }
        Ok(json!({ "ok": true }))
    }

    fn secret_delete(&self, params: &Value) -> Result<Value, PluginError> {
        let key = params
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Plugin secret key is invalid."))?;
        match fs::remove_file(self.secret_path(key)?) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({ "ok": true })),
            Err(_) => Err(plugin_error("INTERNAL", "Plugin secret cannot be deleted.")),
        }
    }

    fn secret_has(&self, params: &Value) -> Result<Value, PluginError> {
        let key = params
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Plugin secret key is invalid."))?;
        Ok(json!({ "exists": self.secret_path(key)?.is_file() }))
    }

    fn host_path(&self, params: &Value) -> Result<PathBuf, PluginError> {
        let raw = params
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty() && !path.contains('\0'))
            .ok_or_else(|| plugin_error("INVALID_INPUT", "File path is missing or invalid."))?;
        let path = PathBuf::from(raw);
        #[cfg(windows)]
        if !path.is_absolute()
            && (path.has_root()
                || matches!(
                    path.components().next(),
                    Some(std::path::Component::Prefix(_))
                ))
        {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Use an absolute path or a path relative to the plugin data directory.",
            ));
        }
        Ok(if path.is_absolute() {
            path
        } else {
            self.data_dir.join(path)
        })
    }
    fn open_read(&self, params: &Value, invocation: &HostInvocation) -> Result<Value, PluginError> {
        serde_json::to_value(
            self.streams
                .open_read(&self.host_path(params)?, invocation)?,
        )
        .map_err(|e| plugin_error("INTERNAL", &e.to_string()))
    }
    fn open_write(
        &self,
        params: &Value,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        let destination = self.host_path(params)?;
        let mode = serde_json::from_value::<FileWriteMode>(
            params.get("mode").cloned().unwrap_or(json!("create")),
        )
        .map_err(|e| plugin_error("INVALID_INPUT", &e.to_string()))?;
        let parent = destination
            .parent()
            .ok_or_else(|| plugin_error("INVALID_INPUT", "File has no parent directory."))?;
        serde_json::to_value(self.streams.open_write(
            parent,
            WriteTarget::Path {
                destination: destination.clone(),
                mode,
            },
            invocation,
        )?)
        .map_err(|e| plugin_error("INTERNAL", &e.to_string()))
    }
    fn spool_dir(&self) -> Result<PathBuf, PluginError> {
        let parent = self.data_dir.join(".core-transfer");
        fs::create_dir_all(&parent).map_err(|e| plugin_error("FILE_ERROR", &e.to_string()))?;
        Ok(parent)
    }
    fn create_spool(&self, invocation: &HostInvocation) -> Result<Value, PluginError> {
        serde_json::to_value(self.streams.open_write(
            &self.spool_dir()?,
            WriteTarget::Spool,
            invocation,
        )?)
        .map_err(|e| plugin_error("INTERNAL", &e.to_string()))
    }
    fn export_open(
        &self,
        params: &Value,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| safe_export_name(name))
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Export file name is invalid."))?;
        let (parent, target) = if let Some(remote) = &invocation.remote {
            (
                self.spool_dir()?,
                WriteTarget::Download {
                    name: name.into(),
                    remote: remote.clone(),
                },
            )
        } else {
            fs::create_dir_all(&self.export_dir)
                .map_err(|e| plugin_error("FILE_ERROR", &e.to_string()))?;
            (
                self.export_dir.clone(),
                WriteTarget::Path {
                    destination: self.export_dir.join(name),
                    mode: FileWriteMode::Create,
                },
            )
        };
        serde_json::to_value(self.streams.open_write(&parent, target, invocation)?)
            .map_err(|e| plugin_error("INTERNAL", &e.to_string()))
    }
    fn read_stream(
        &self,
        params: &Value,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        let count = params
            .get("chunkBytes")
            .map(Value::as_u64)
            .unwrap_or(Some(wonderland_plugin_protocol::STREAM_CHUNK_BYTES as u64))
            .and_then(|size| usize::try_from(size).ok())
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Read chunk size is invalid."))?;
        let result = self.wait(
            invocation,
            self.streams.read(stream_id(params)?, count, invocation),
        )?;
        serde_json::to_value(result).map_err(|e| plugin_error("INTERNAL", &e.to_string()))
    }
    fn network_request(
        &self,
        params: &Value,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        let request: HttpRequestParams = serde_json::from_value(params.clone())
            .map_err(|e| plugin_error("INVALID_INPUT", &e.to_string()))?;
        let invocation = invocation.clone().with_timeout_ms(request.timeout_ms)?;
        let credential_header_names = request
            .authentication
            .as_ref()
            .map(|authentication| vec![authentication.name.clone()])
            .unwrap_or_default();
        let mut headers = request
            .headers
            .into_iter()
            .map(|header| wonderland_net::HttpHeader {
                name: header.name,
                value: header.value,
            })
            .collect::<Vec<_>>();
        if let Some(auth) = request.authentication {
            let secret = self.read_request_secret(&auth.secret_id)?;
            headers.push(wonderland_net::HttpHeader {
                name: auth.name,
                value: format!("{}{}", auth.prefix, secret).into_bytes(),
            });
        }
        let mut upload = None;
        let body = match request.body {
            None => wonderland_net::HttpRequestBody::Empty,
            Some(HttpBody::Text { content }) => {
                wonderland_net::HttpRequestBody::Bytes(content.into_bytes())
            }
            Some(HttpBody::Bytes { content_base64 }) => wonderland_net::HttpRequestBody::Bytes(
                base64::engine::general_purpose::STANDARD
                    .decode(content_base64)
                    .map_err(|_| plugin_error("INVALID_INPUT", "HTTP body is not valid base64."))?,
            ),
            Some(HttpBody::File { path }) => {
                let path = self.host_path(&json!({"path":path}))?;
                let file =
                    fs::File::open(path).map_err(|e| plugin_error("FILE_ERROR", &e.to_string()))?;
                wonderland_net::HttpRequestBody::File(tokio::fs::File::from_std(file))
            }
            Some(HttpBody::Stream { stream_id }) => {
                let temporary = self.streams.spool(&stream_id, &invocation)?;
                let file = fs::File::open(&temporary.path)
                    .map_err(|e| plugin_error("FILE_ERROR", &e.to_string()))?;
                upload = Some(temporary);
                wonderland_net::HttpRequestBody::File(tokio::fs::File::from_std(file))
            }
        };
        let redirects = if request.redirects.follow {
            wonderland_net::HttpRedirectPolicy::Follow {
                max_redirects: request
                    .redirects
                    .max_redirects
                    .map(usize::try_from)
                    .transpose()
                    .map_err(|_| {
                        plugin_error("INVALID_INPUT", "Redirect count cannot be represented.")
                    })?,
            }
        } else {
            wonderland_net::HttpRedirectPolicy::None
        };
        let deadline = invocation
            .remaining()
            .and_then(|remaining| tokio::time::Instant::now().checked_add(remaining));
        let response = self.wait(&invocation, async {
            self.transport
                .send(wonderland_net::HttpRequest {
                    url: request.url,
                    method: request.method,
                    headers,
                    credential_header_names,
                    query: request.query,
                    body,
                    deadline,
                    redirects,
                })
                .await
                .map_err(transport_error)
        })?;
        drop(upload);
        let body = self.streams.http(response.body, &invocation)?;
        serde_json::to_value(HttpResponseMeta {
            status: response.status,
            headers: response
                .headers
                .into_iter()
                .map(|header| wonderland_plugin_protocol::HttpHeader {
                    name: header.name,
                    value: header.value,
                })
                .collect(),
            final_url: response.final_url,
            body,
        })
        .map_err(|e| plugin_error("INTERNAL", &e.to_string()))
    }

    fn reveal_own(&self, params: &Value) -> Result<Value, PluginError> {
        let directory = match params.get("scope").and_then(Value::as_str) {
            Some("data") => &self.data_dir,
            Some("exports") => &self.export_dir,
            _ => return Err(plugin_error("INVALID_INPUT", "Reveal scope is invalid.")),
        };
        fs::create_dir_all(directory).map_err(|error| {
            plugin_error(
                "INTERNAL",
                &format!("Cannot create plugin directory: {error}"),
            )
        })?;
        self.services.environment.reveal_dir(directory)?;
        Ok(json!({ "ok": true }))
    }

    fn read_request_secret(&self, secret_id: &str) -> Result<String, PluginError> {
        let sealed = fs::read(self.secret_path(secret_id)?).map_err(|_| {
            plugin_error(
                "SECRET_NOT_FOUND",
                "The referenced plugin secret is unavailable.",
            )
        })?;
        let plain = wonderland_secret::unseal(&sealed)
            .map_err(|_| internal_error("The referenced plugin secret cannot be decrypted."))?;
        String::from_utf8(plain)
            .map_err(|_| internal_error("The referenced plugin secret is invalid."))
    }
}

pub(crate) fn browser_url(params: &Value) -> Result<String, PluginError> {
    if let Some(raw) = params.get("url") {
        let url = raw
            .as_str()
            .and_then(|raw| wonderland_net::Url::parse(raw).ok())
            .filter(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
            })
            .ok_or_else(|| {
                plugin_error("INVALID_INPUT", "Browser URL must be an HTTP(S) address.")
            })?;
        return Ok(url.into());
    }
    Err(plugin_error("INVALID_INPUT", "Browser URL is missing."))
}

fn file_handle_token() -> Result<String, PluginError> {
    let mut bytes = [0_u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| plugin_error("INTERNAL", "Cannot create a file access handle."))?;
    Ok(hex::encode(bytes))
}

pub(crate) fn file_mime_type(extension: &str) -> &'static str {
    match extension {
        "json" => "application/json",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

pub(crate) fn safe_export_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && Path::new(name).file_name().and_then(|file| file.to_str()) == Some(name)
        && !name
            .chars()
            .any(|character| character.is_control() || matches!(character, ':' | '/' | '\\'))
}

fn internal_error(message: &str) -> PluginError {
    plugin_error("INTERNAL", message)
}

#[cfg(windows)]
fn replace_secret_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_secret_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

fn stream_id(params: &Value) -> Result<&str, PluginError> {
    params
        .get("streamId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Stream reference is invalid."))
}

fn transport_interruption(method: &str, mut error: PluginError) -> PluginError {
    let stage = match method {
        "core.network.request" => "send",
        "core.streams.read" => "body",
        _ => return error,
    };
    if error.details.is_none()
        && matches!(
            error.code.as_str(),
            "TIMEOUT" | "CANCELLED" | "INVALID_STREAM"
        )
    {
        error.details = Some(
            json!({"stage":stage,"kind":if error.code == "TIMEOUT" { "deadline_exceeded" } else { "cancelled" },"outcomeUnknown":true}),
        );
    }
    error
}

pub(crate) fn transport_error(error: wonderland_net::HttpTransportError) -> PluginError {
    use wonderland_net::HttpFailureKind;
    let code = match error.kind {
        HttpFailureKind::InvalidUrl
        | HttpFailureKind::InvalidMethod
        | HttpFailureKind::InvalidHeader => "INVALID_INPUT",
        HttpFailureKind::Timeout | HttpFailureKind::DeadlineExceeded => "TIMEOUT",
        HttpFailureKind::ClientUnavailable => "SERVICE_UNAVAILABLE",
        _ => "NETWORK_ERROR",
    };
    let mut result = plugin_error(code, &error.to_string());
    result.details = serde_json::to_value(error).ok();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_environment::HeadlessEnvironment;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "wla-host-services-{}",
                file_handle_token().unwrap()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn host(&self, runtime: &tokio::runtime::Runtime, plugin: &str) -> HostServices {
            HostServices::new(
                plugin.into(),
                self.0.join("plugin-data").join(plugin),
                self.0.clone(),
                RuntimeServices::new(
                    Arc::new(HeadlessEnvironment),
                    runtime.handle().clone(),
                    WeakPluginManager::default(),
                ),
            )
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn headless_export_receipt_confirms_saved_bytes_and_never_replaces_an_existing_export() {
        let runtime = runtime();
        let root = TestRoot::new();
        let host = root.host(&runtime, "demo");
        let params = json!({"name":"report.json", "contentBase64":"e30="});
        let result = host
            .call("core.files.export", &params, &HostInvocation::default())
            .unwrap();
        let receipt: FileResult = serde_json::from_value(result).unwrap();
        let FileResult::Local {
            state,
            name,
            size,
            path,
        } = receipt
        else {
            panic!("Expected a local receipt");
        };
        assert_eq!(state, LocalFileState::Saved);
        assert_eq!(name, "report.json");
        assert_eq!(size, 2);
        assert_eq!(
            PathBuf::from(&path),
            root.0.join("Wonderland Assistant/demo/Exports/report.json")
        );
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert!(
            host.call("core.files.export", &params, &HostInvocation::default())
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert!(
            host.call(
                "core.files.export",
                &json!({"name":"../report.json","contentBase64":"e30="}),
                &HostInvocation::default()
            )
            .is_err()
        );
    }

    #[test]
    fn headless_native_interactions_return_an_explicit_error() {
        let runtime = runtime();
        let root = TestRoot::new();
        let host = root.host(&runtime, "demo");
        for (method, params) in [
            ("core.files.pick", json!({})),
            ("core.browser.open", json!({"url":"https://example.com/"})),
            ("core.files.reveal_own", json!({"scope":"data"})),
        ] {
            assert_eq!(
                host.call(method, &params, &HostInvocation::default())
                    .unwrap_err()
                    .code,
                "INTERACTION_UNAVAILABLE"
            );
        }
        assert_eq!(
            host.call(
                "core.browser.open_official",
                &json!({"target":"onnx"}),
                &HostInvocation::default()
            )
            .unwrap_err()
            .code,
            "METHOD_NOT_FOUND"
        );
    }

    #[test]
    fn a_cancelled_native_picker_does_not_register_an_unreturned_handle() {
        struct CancelledPicker {
            path: PathBuf,
            cancelled: Arc<AtomicBool>,
        }
        impl HostEnvironment for CancelledPicker {
            fn emit(&self, _: &str, _: Value) {}
            fn pick_file(&self, _: &[String]) -> Result<Option<PathBuf>, PluginError> {
                self.cancelled.store(true, Ordering::Release);
                Ok(Some(self.path.clone()))
            }
            fn open_url(&self, _: &str) -> Result<(), PluginError> {
                unreachable!()
            }
            fn reveal_dir(&self, _: &Path) -> Result<(), PluginError> {
                unreachable!()
            }
        }
        let runtime = runtime();
        let root = TestRoot::new();
        let path = root.0.join("picked.any-extension");
        fs::write(&path, b"original selected file").unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut host = root.host(&runtime, "demo");
        host.services.environment = Arc::new(CancelledPicker {
            path: path.clone(),
            cancelled: cancelled.clone(),
        });
        let invocation = HostInvocation {
            cancelled: Some(cancelled),
            ..HostInvocation::default()
        };
        assert_eq!(
            host.call("core.files.pick", &json!({}), &invocation)
                .unwrap_err()
                .code,
            "CANCELLED"
        );
        assert!(host.picked_files.lock().unwrap().is_empty());
        assert_eq!(fs::read(path).unwrap(), b"original selected file");
    }

    #[test]
    fn explicit_host_paths_accept_outside_files_and_resolve_relative_paths_unambiguously() {
        let runtime = runtime();
        let root = TestRoot::new();
        let host = root.host(&runtime, "demo");
        let outside = root.0.join("文件无扩展名");
        fs::write(&outside, b"outside plugin directory").unwrap();
        let result = host
            .call(
                "core.files.open_read",
                &json!({"path":outside}),
                &HostInvocation::default(),
            )
            .unwrap();
        assert_eq!(result["size"], 24);
        host.call(
            "core.streams.close",
            &json!({"streamId":result["streamId"]}),
            &HostInvocation::default(),
        )
        .unwrap();
        assert_eq!(
            host.host_path(&json!({"path":"relative.bin"})).unwrap(),
            host.data_dir.join("relative.bin")
        );
        #[cfg(windows)]
        for path in ["C:relative.bin", "\\root-relative.bin"] {
            assert_eq!(
                host.host_path(&json!({"path":path})).unwrap_err().code,
                "INVALID_INPUT"
            );
        }
    }

    #[test]
    fn processed_http_posts_keep_unknown_outcome_when_parent_or_body_deadline_wins() {
        let _proxy = crate::test_proxy::direct();
        use std::io::{BufRead, BufReader};
        use std::net::TcpListener;
        for send_headers in [false, true] {
            let runtime = runtime();
            let root = TestRoot::new();
            let host = root.host(&runtime, "demo");
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/billable-post", listener.local_addr().unwrap());
            let processed = Arc::new(AtomicBool::new(false));
            let server_processed = processed.clone();
            let (release, released) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                assert_eq!(body, b"already processed");
                drop(reader);
                server_processed.store(true, Ordering::Release);
                if send_headers {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                }
                // The upstream has performed the POST but withholds headers/body.
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            });
            let invocation = HostInvocation::for_call(None, Some(500), None).unwrap();
            let request = json!({"url":url,"method":"POST","body":{"kind":"text","content":"already processed"}});
            let error = if send_headers {
                let response = host
                    .call("core.network.request", &request, &invocation)
                    .unwrap();
                host.call(
                    "core.streams.read",
                    &json!({"streamId":response["body"]["streamId"]}),
                    &invocation,
                )
                .unwrap_err()
            } else {
                host.call("core.network.request", &request, &invocation)
                    .unwrap_err()
            };
            assert!(processed.load(Ordering::Acquire));
            assert_eq!(error.code, "TIMEOUT");
            let details = error.details.unwrap();
            assert_eq!(details["outcomeUnknown"], true);
            assert_eq!(details["stage"], if send_headers { "body" } else { "send" });
            release.send(()).unwrap();
            server.join().unwrap();
        }
    }

    #[test]
    fn http_secret_reference_is_optional_and_checked_in_the_plugin_partition() {
        let runtime = runtime();
        let root = TestRoot::new();
        let host = root.host(&runtime, "demo");
        let mut params = json!({"url":"ftp://example.com"});
        assert_eq!(
            host.call("core.network.request", &params, &HostInvocation::default())
                .unwrap_err()
                .code,
            "INVALID_INPUT"
        );
        params["authentication"] =
            json!({"secretId":"missing","name":"Authorization","prefix":"Bearer "});
        assert_eq!(
            host.call("core.network.request", &params, &HostInvocation::default())
                .unwrap_err()
                .code,
            "SECRET_NOT_FOUND"
        );
        params["authentication"]["secretId"] = json!("../other/key");
        assert_eq!(
            host.call("core.network.request", &params, &HostInvocation::default())
                .unwrap_err()
                .code,
            "INVALID_INPUT"
        );
    }

    #[test]
    fn secret_replacement_keeps_the_same_format_and_plugin_scope() {
        let runtime = runtime();
        let root = TestRoot::new();
        let owner = root.host(&runtime, "owner");
        let other = root.host(&runtime, "other");
        for value in ["first-key", "replacement-key"] {
            owner
                .call(
                    "core.secrets.plugin.set",
                    &json!({"key":"profile","value":value}),
                    &HostInvocation::default(),
                )
                .unwrap();
        }
        assert_eq!(
            owner
                .call(
                    "core.secrets.plugin.get",
                    &json!({"key":"profile"}),
                    &HostInvocation::default()
                )
                .unwrap()["value"],
            "replacement-key"
        );
        assert!(
            other
                .call(
                    "core.secrets.plugin.get",
                    &json!({"key":"profile"}),
                    &HostInvocation::default()
                )
                .unwrap()["value"]
                .is_null()
        );
        assert_eq!(
            owner.read_request_secret("profile").unwrap(),
            "replacement-key"
        );
        assert_eq!(
            owner.secret_path("profile").unwrap(),
            root.0.join("plugin-secrets/owner/profile.dpapi")
        );
    }

    #[test]
    fn cancellation_interrupts_a_waiting_async_service_and_drops_its_future() {
        let runtime = runtime();
        let root = TestRoot::new();
        let host = root.host(&runtime, "demo");
        let token = Arc::new(AtomicBool::new(false));
        let invocation = HostInvocation {
            remote: None,
            cancelled: Some(token.clone()),
            ..HostInvocation::default()
        };
        let dropped = Arc::new(AtomicBool::new(false));
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let cancel = std::thread::spawn(move || {
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            token.store(true, Ordering::Release);
        });
        let result: Result<(), PluginError> = host.wait(&invocation, async {
            let _drop = DropFlag(dropped.clone());
            started_tx.send(()).unwrap();
            std::future::pending().await
        });
        assert_eq!(result.unwrap_err().code, "CANCELLED");
        assert!(dropped.load(Ordering::Acquire));
        cancel.join().unwrap();
    }

    #[test]
    fn remote_cancellation_also_revokes_ordinary_host_services() {
        let runtime = runtime();
        let root = TestRoot::new();
        let host = root.host(&runtime, "demo");
        let remote = Arc::new(RemoteContext::new(
            Arc::new(crate::remote_interaction::RemoteBroker::default()),
            "browser".into(),
            "root".into(),
        ));
        let child = remote.for_plugin("demo");
        remote.cancel();
        let invocation = HostInvocation {
            remote: Some(child),
            cancelled: None,
            ..HostInvocation::default()
        };
        assert_eq!(
            host.call(
                "core.secrets.plugin.has",
                &json!({"key":"profile"}),
                &invocation
            )
            .unwrap_err()
            .code,
            "CANCELLED"
        );
    }

    #[test]
    fn nested_local_invocations_inherit_budget_and_cancellation_with_a_bounded_depth() {
        let root = HostInvocation::for_call(None, Some(1000), None).unwrap();
        let child = HostInvocation::for_call(None, Some(60000), Some(&root)).unwrap();
        assert!(child.remaining().unwrap() <= root.remaining().unwrap() + Duration::from_millis(5));
        let grandchild = HostInvocation::for_call(None, Some(60000), Some(&child)).unwrap();
        child.cancel();
        assert!(root.check().is_ok());
        assert_eq!(grandchild.check().unwrap_err().code, "CANCELLED");

        let mut invocation = HostInvocation::for_call(None, Some(60000), None).unwrap();
        for _ in 0..16 {
            invocation = HostInvocation::for_call(None, Some(60000), Some(&invocation)).unwrap();
        }
        assert_eq!(
            HostInvocation::for_call(None, Some(60000), Some(&invocation))
                .err()
                .unwrap()
                .code,
            "CALL_DEPTH_EXCEEDED"
        );
    }

    #[test]
    fn process_shutdown_cancels_background_host_work_without_an_invocation_context() {
        let runtime = runtime();
        let root = TestRoot::new();
        let host = root.host(&runtime, "demo");
        let process_cancelled = Arc::new(AtomicBool::new(false));
        let invocation =
            HostInvocation::default().with_cancellation_parent(process_cancelled.clone());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            process_cancelled.store(true, Ordering::Release);
        });
        let result: Result<(), PluginError> = host.wait(&invocation, async {
            started_tx.send(()).unwrap();
            std::future::pending().await
        });
        assert_eq!(result.unwrap_err().code, "CANCELLED");
        shutdown.join().unwrap();
    }
}
