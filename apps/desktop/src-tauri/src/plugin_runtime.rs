//! One supervised stdio-NDJSON process per enabled plugin.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use wonderland_kernel::logging::warn;
use wonderland_plugin_protocol::{
    PluginError, PluginErrorMessage, PluginEvent, PluginHello, PluginRequest, PluginResult,
    PluginRuntimeState,
};

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_REQUESTS_PER_SESSION: usize = 65_536;
const MAX_PICKED_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PICKED_FILE_CHUNK_BYTES: u64 = 1024 * 1024;
const MAX_PICKED_FILE_INLINE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_EXPORTED_FILE_BYTES: usize = 20 * 1024 * 1024;
const FILE_HANDLE_TTL: Duration = Duration::from_secs(5 * 60);

pub(crate) struct PluginProcess {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Mutex<HashMap<String, Sender<Result<Value, PluginError>>>>,
    seen_request_ids: Mutex<HashSet<String>>,
    hello_waiter: Mutex<Option<SyncSender<Result<PluginHello, String>>>>,
    alive: AtomicBool,
    plugin_id: String,
    granted_capabilities: HashSet<String>,
    data_dir: PathBuf,
    export_dir: PathBuf,
    picked_files: Mutex<HashMap<String, PickedFile>>,
}

struct PickedFile {
    path: PathBuf,
    name: String,
    mime_type: String,
    max_bytes: u64,
    size: u64,
    expires_at: Instant,
}

impl PluginProcess {
    pub(crate) fn start(
        app: AppHandle,
        state: &PluginRuntimeState,
        executable: &std::path::Path,
        contract: Value,
        data_dir: PathBuf,
        documents_dir: PathBuf,
        contract_sha256: &str,
    ) -> Result<Arc<Self>, PluginError> {
        let plugin_id = state.manifest.id.clone();
        let granted_capabilities = state.granted_capabilities.clone();
        let mut command = Command::new(executable);
        command
            .current_dir(
                executable
                    .parent()
                    .and_then(std::path::Path::parent)
                    .unwrap_or(
                        executable
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new(".")),
                    ),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .env("WONDERLAND_PLUGIN_DATA_DIR", &data_dir);
        for key in ["PATH", "SystemRoot", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        // Plugin backends are console executables. A GUI host must suppress the
        // console window while keeping their piped stdio protocol available.
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        let mut child = command.spawn().map_err(|error| PluginError {
            code: "PLUGIN_START_FAILED".to_owned(),
            message: format!("Unable to start plugin backend: {error}"),
            details: None,
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| internal_error("Plugin stdin was not available."))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| internal_error("Plugin stdout was not available."))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| internal_error("Plugin stderr was not available."))?;
        let (hello_tx, hello_rx) = mpsc::sync_channel(1);
        let process = Arc::new(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            seen_request_ids: Mutex::new(HashSet::new()),
            hello_waiter: Mutex::new(Some(hello_tx)),
            alive: AtomicBool::new(true),
            plugin_id: plugin_id.clone(),
            granted_capabilities: granted_capabilities.iter().cloned().collect(),
            data_dir,
            export_dir: documents_dir
                .join("Wonderland Assistant")
                .join(&plugin_id)
                .join("Exports"),
            picked_files: Mutex::new(HashMap::new()),
        });

        let weak = Arc::downgrade(&process);
        thread::Builder::new()
            .name(format!("plugin-{plugin_id}-stdout"))
            .spawn(move || read_stdout(stdout, weak, app, contract))
            .map_err(|error| {
                internal_error(&format!("Cannot start plugin stdout reader: {error}"))
            })?;
        thread::Builder::new()
            .name(format!("plugin-{plugin_id}-stderr"))
            .spawn(move || read_stderr(stderr, plugin_id.clone()))
            .map_err(|error| {
                internal_error(&format!("Cannot start plugin stderr reader: {error}"))
            })?;

        let hello = json!({
            "protocol": "wonderland-plugin",
            "version": "1.0.0",
            "type": "hello",
            "role": "host",
            "pluginId": state.manifest.id,
            "coreVersion": env!("CARGO_PKG_VERSION"),
            "grantedCapabilities": granted_capabilities,
        });
        process.write_frame(&hello)?;
        let hello = hello_rx
            .recv_timeout(HANDSHAKE_TIMEOUT)
            .map_err(|_| PluginError {
                code: "PLUGIN_START_FAILED".to_owned(),
                message: "Plugin handshake timed out.".to_owned(),
                details: None,
            })?
            .map_err(|message| PluginError {
                code: "PLUGIN_START_FAILED".to_owned(),
                message,
                details: None,
            })?;
        if hello.protocol != "wonderland-plugin"
            || hello.version != "1.0.0"
            || hello.message_type != "hello"
            || hello.role != "plugin"
            || hello.plugin_id != state.manifest.id
            || hello.plugin_version != state.manifest.version
            || hello.contract_sha256 != contract_sha256
        {
            return Err(PluginError {
                code: "PLUGIN_START_FAILED".to_owned(),
                message: "Plugin handshake identity or contract does not match its manifest."
                    .to_owned(),
                details: None,
            });
        }
        Ok(process)
    }

    pub(crate) fn call(
        &self,
        request_id: &str,
        method: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<Value, PluginError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(plugin_error("PLUGIN_CRASHED", "Plugin process has exited."));
        }
        if request_id.is_empty() || request_id.len() > 128 {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Request ID must contain 1 to 128 characters.",
            ));
        }
        {
            let mut seen = self
                .seen_request_ids
                .lock()
                .map_err(|_| internal_error("Plugin request ID table is unavailable."))?;
            if seen.len() >= MAX_REQUESTS_PER_SESSION {
                return Err(plugin_error(
                    "RESOURCE_LIMIT",
                    "Plugin session reached its request limit and must be restarted.",
                ));
            }
            if !seen.insert(request_id.to_owned()) {
                return Err(plugin_error(
                    "INVALID_REQUEST",
                    "Request IDs cannot be reused during a plugin session.",
                ));
            }
        }
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| internal_error("Plugin request table is unavailable."))?;
            if pending.len() >= 128 {
                return Err(plugin_error(
                    "RESOURCE_LIMIT",
                    "Plugin has too many concurrent requests.",
                ));
            }
            pending.insert(request_id.to_owned(), tx);
        }
        let frame = json!({
            "protocol": "wonderland-plugin",
            "version": "1.0.0",
            "type": "request",
            "id": request_id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.write_frame(&frame) {
            self.remove_pending(request_id);
            return Err(error);
        }
        match rx.recv_timeout(Duration::from_millis(timeout_ms.clamp(1, MAX_TIMEOUT_MS))) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.cancel(request_id);
                self.remove_pending(request_id);
                Err(plugin_error(
                    "TIMEOUT",
                    "Plugin call exceeded its contract timeout.",
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(plugin_error(
                "PLUGIN_CRASHED",
                "Plugin process exited during the request.",
            )),
        }
    }

    pub(crate) fn cancel(&self, request_id: &str) -> Result<(), PluginError> {
        let active = self
            .pending
            .lock()
            .map_err(|_| internal_error("Plugin request table is unavailable."))?
            .contains_key(request_id);
        if !active {
            return Err(plugin_error(
                "CANCELLED",
                "No active plugin call has this request ID.",
            ));
        }
        self.write_frame(&json!({
            "protocol": "wonderland-plugin",
            "version": "1.0.0",
            "type": "cancel",
            "id": request_id,
        }))
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn write_frame(&self, frame: &Value) -> Result<(), PluginError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(plugin_error("PLUGIN_CRASHED", "Plugin process has exited."));
        }
        let bytes = serde_json::to_vec(frame)
            .map_err(|error| internal_error(&format!("Cannot encode protocol frame: {error}")))?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Protocol frame exceeds 32 MiB.",
            ));
        }
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| internal_error("Plugin stdin is unavailable."))?;
        stdin
            .write_all(&bytes)
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| {
                self.alive.store(false, Ordering::Release);
                plugin_error(
                    "PLUGIN_CRASHED",
                    &format!("Plugin process is not accepting input: {error}"),
                )
            })
    }

    fn remove_pending(&self, request_id: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(request_id);
        }
    }

    fn fail_pending(&self, error: PluginError) {
        let pending = self
            .pending
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for (_, sender) in pending {
            let _ = sender.send(Err(error.clone()));
        }
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.fail_pending(plugin_error(
            "PLUGIN_CRASHED",
            "Plugin process was stopped by Core.",
        ));
    }
}

fn read_stdout(
    stdout: std::process::ChildStdout,
    process: Weak<PluginProcess>,
    app: AppHandle,
    contract: Value,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    let mut first_frame = true;
    let exit_error = loop {
        line.clear();
        let result = (&mut reader)
            .take((MAX_FRAME_BYTES + 1) as u64)
            .read_until(b'\n', &mut line);
        let count = match result {
            Ok(0) => {
                break plugin_error("PLUGIN_CRASHED", "Plugin stdout closed.");
            }
            Ok(count) => count,
            Err(error) => {
                break plugin_error(
                    "PLUGIN_CRASHED",
                    &format!("Cannot read plugin stdout: {error}"),
                );
            }
        };
        if count > MAX_FRAME_BYTES || line.last() != Some(&b'\n') {
            break plugin_error(
                "RESOURCE_LIMIT",
                "Plugin emitted a frame larger than 32 MiB or omitted its newline.",
            );
        }
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let frame: Value = match serde_json::from_slice(&line) {
            Ok(frame) => frame,
            Err(error) => {
                break plugin_error(
                    "INVALID_REQUEST",
                    &format!("Plugin emitted invalid JSON: {error}"),
                );
            }
        };
        if !valid_envelope(&frame) {
            break plugin_error(
                "INCOMPATIBLE_PROTOCOL",
                "Plugin emitted an invalid protocol envelope.",
            );
        }
        if first_frame {
            first_frame = false;
            let hello =
                serde_json::from_value::<PluginHello>(frame).map_err(|error| error.to_string());
            if let Some(process) = process.upgrade()
                && let Ok(mut waiter) = process.hello_waiter.lock()
                && let Some(waiter) = waiter.take()
            {
                let _ = waiter
                    .send(hello.map_err(|message| format!("Invalid plugin handshake: {message}")));
            }
            continue;
        }
        let Some(process) = process.upgrade() else {
            return;
        };
        match frame.get("type").and_then(Value::as_str) {
            Some("result") => {
                let result = match serde_json::from_value::<PluginResult>(frame) {
                    Ok(result)
                        if valid_message(
                            &result.protocol,
                            &result.version,
                            &result.message_type,
                        ) && valid_request_id(&result.id) =>
                    {
                        result
                    }
                    _ => {
                        break plugin_error("INVALID_REQUEST", "Plugin result frame is malformed.");
                    }
                };
                process.deliver(&result.id, Ok(result.result));
            }
            Some("error") => {
                if frame.pointer("/error/details").is_none() {
                    break plugin_error(
                        "INVALID_REQUEST",
                        "Plugin error frame is missing required details.",
                    );
                }
                let mut message = match serde_json::from_value::<PluginErrorMessage>(frame) {
                    Ok(message)
                        if valid_message(
                            &message.protocol,
                            &message.version,
                            &message.message_type,
                        ) && valid_request_id(&message.id) =>
                    {
                        message
                    }
                    _ => break plugin_error("INVALID_REQUEST", "Plugin error frame is malformed."),
                };
                sanitize_plugin_error(&mut message.error);
                process.deliver(&message.id, Err(message.error));
            }
            Some("event") => {
                let request_id_present = frame.get("requestId").is_some();
                let event = match serde_json::from_value::<PluginEvent>(frame) {
                    Ok(event)
                        if valid_message(&event.protocol, &event.version, &event.message_type)
                            && event
                                .request_id
                                .as_ref()
                                .is_none_or(|id| valid_request_id(id)) =>
                    {
                        event
                    }
                    _ => break plugin_error("INVALID_REQUEST", "Plugin event frame is malformed."),
                };
                let topic = event.topic.as_str();
                let payload = &event.payload;
                let Some(schema) =
                    contract.pointer(&format!("/events/{}/payload", escape_json_pointer(topic)))
                else {
                    break plugin_error(
                        "INVALID_REQUEST",
                        "Plugin emitted an undeclared event topic.",
                    );
                };
                if let Err(error) = crate::plugin_schema::validate(payload, schema, &contract) {
                    break plugin_error(
                        "INVALID_RESPONSE",
                        &format!("Plugin event payload failed validation: {error}"),
                    );
                }
                if !request_id_present {
                    break plugin_error(
                        "INVALID_REQUEST",
                        "Plugin event requestId must be a string or null.",
                    );
                }
                let _ = app.emit(
                    "plugin:event",
                    json!({
                        "pluginId": process.plugin_id,
                        "topic": topic,
                        "requestId": event.request_id,
                        "payload": payload,
                    }),
                );
            }
            Some("request") => {
                let request = match serde_json::from_value::<PluginRequest>(frame) {
                    Ok(request)
                        if valid_message(
                            &request.protocol,
                            &request.version,
                            &request.message_type,
                        ) && valid_request_id(&request.id)
                            && valid_method(&request.method) =>
                    {
                        request
                    }
                    _ => {
                        break plugin_error(
                            "INVALID_REQUEST",
                            "Plugin service request frame is malformed.",
                        );
                    }
                };
                let reply = match process.host_service(&app, &request.method, &request.params) {
                    Ok(result) => json!({
                        "protocol": "wonderland-plugin",
                        "version": "1.0.0",
                        "type": "result",
                        "id": request.id,
                        "result": result,
                    }),
                    Err(error) => json!({
                        "protocol": "wonderland-plugin",
                        "version": "1.0.0",
                        "type": "error",
                        "id": request.id,
                        "error": error,
                    }),
                };
                if let Err(error) = process.write_frame(&reply) {
                    break error;
                }
            }
            Some("hello") | Some("cancel") | None | Some(_) => {
                break plugin_error(
                    "INVALID_REQUEST",
                    "Plugin emitted an unexpected protocol message.",
                );
            }
        }
    };

    if let Some(process) = process.upgrade() {
        process.alive.store(false, Ordering::Release);
        if let Ok(mut waiter) = process.hello_waiter.lock()
            && let Some(waiter) = waiter.take()
        {
            let _ = waiter.send(Err(exit_error.message.clone()));
        }
        process.fail_pending(exit_error);
    }
}

impl PluginProcess {
    fn host_service(
        &self,
        app: &AppHandle,
        method: &str,
        params: &Value,
    ) -> Result<Value, PluginError> {
        let required_capability = match method {
            "core.account.snapshot" => "account.read",
            "core.account.authed_get" => "account.authed_get",
            "core.network.public" => "network.public",
            "core.network.model" => "network.model",
            "core.secrets.plugin.get"
            | "core.secrets.plugin.set"
            | "core.secrets.plugin.delete"
            | "core.secrets.plugin.has" => "secrets.plugin",
            "core.files.pick" | "core.files.read" => "files.pick",
            "core.files.export" | "core.files.export_dir" => "files.export",
            "core.files.reveal_own" => "files.reveal_own",
            "core.browser.open_official" => "browser.open_official",
            "core.knowledge.local_model" => "knowledge.local_model",
            _ => {
                return Err(plugin_error(
                    "METHOD_NOT_FOUND",
                    "Core service is not available.",
                ));
            }
        };
        if !self.granted_capabilities.contains(required_capability) {
            return Err(plugin_error(
                "UNAUTHORIZED",
                "The requested Core service capability has not been granted.",
            ));
        }
        if method == "core.network.model" && !self.granted_capabilities.contains("secrets.plugin") {
            return Err(plugin_error(
                "UNAUTHORIZED",
                "Model requests require the secrets.plugin capability.",
            ));
        }

        match method {
            "core.account.snapshot" => account_snapshot(app),
            "core.account.authed_get" => account_authed_get(app, params),
            "core.network.public" => network_public(params),
            "core.network.model" => self.network_model(params),
            "core.secrets.plugin.get" => self.secret_get(params),
            "core.secrets.plugin.set" => self.secret_set(params),
            "core.secrets.plugin.delete" => self.secret_delete(params),
            "core.secrets.plugin.has" => self.secret_has(params),
            "core.files.pick" => self.pick_file(app, params),
            "core.files.read" => self.read_picked_file(params),
            "core.files.export" => self.export_file(params),
            "core.files.export_dir" => self.export_dir(),
            "core.files.reveal_own" => self.reveal_own(app, params),
            "core.browser.open_official" => open_official(params),
            "core.knowledge.local_model" => knowledge_local_model(app),
            _ => unreachable!(),
        }
    }

    fn pick_file(&self, app: &AppHandle, params: &Value) -> Result<Value, PluginError> {
        let extensions = params
            .get("extensions")
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty() && items.len() <= 8)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "File picker extensions are invalid."))?
            .iter()
            .map(|item| item.as_str())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| plugin_error("INVALID_INPUT", "File picker extensions are invalid."))?;
        if extensions.iter().any(|extension| {
            !matches!(
                extension.to_ascii_lowercase().as_str(),
                "json" | "csv" | "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
            )
        }) {
            return Err(plugin_error("INVALID_INPUT", "File type is not supported."));
        }
        let max_bytes = params
            .get("maxBytes")
            .and_then(Value::as_u64)
            .filter(|size| (1..=MAX_PICKED_FILE_BYTES).contains(size))
            .ok_or_else(|| plugin_error("INVALID_INPUT", "File size limit is invalid."))?;
        let file = app
            .dialog()
            .file()
            .add_filter("Supported files", &extensions)
            .blocking_pick_file();
        let Some(file) = file else {
            return Ok(json!({ "cancelled": true }));
        };
        let path = file.into_path().map_err(|error| {
            plugin_error(
                "INVALID_INPUT",
                &format!("Selected file path is invalid: {error}"),
            )
        })?;
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
        if !extensions
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&extension))
        {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Selected file type does not match the requested file types.",
            ));
        }
        let mime_type = file_mime_type(&extension)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Selected file type is not supported."))?
            .to_owned();
        let token = file_handle_token()?;
        let mut picked_files = self
            .picked_files
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "File handle table is unavailable."))?;
        picked_files.retain(|_, picked| Instant::now() <= picked.expires_at);
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
                expires_at: Instant::now() + FILE_HANDLE_TTL,
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
        if Instant::now() > picked.expires_at {
            return Err(plugin_error("UNAUTHORIZED", "File handle has expired."));
        }
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
        file.take(picked.max_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                plugin_error("INTERNAL", &format!("Cannot read selected file: {error}"))
            })?;
        if bytes.len() as u64 > picked.max_bytes {
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
        let mut picked_files = self
            .picked_files
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "File handle table is unavailable."))?;
        let picked = picked_files
            .get(handle)
            .ok_or_else(|| plugin_error("UNAUTHORIZED", "File handle is invalid or expired."))?;
        if Instant::now() > picked.expires_at {
            picked_files.remove(handle);
            return Err(plugin_error("UNAUTHORIZED", "File handle has expired."));
        }
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
        if bytes.len() > MAX_EXPORTED_FILE_BYTES {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Export exceeds its size limit.",
            ));
        }
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
        Ok(json!({ "path": path.to_string_lossy() }))
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
        let temp = path.with_extension("tmp");
        fs::write(&temp, sealed)
            .map_err(|_| plugin_error("INTERNAL", "Plugin secret cannot be written."))?;
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|_| plugin_error("INTERNAL", "Plugin secret cannot be replaced."))?;
        }
        fs::rename(&temp, &path)
            .map_err(|_| plugin_error("INTERNAL", "Plugin secret cannot be committed."))?;
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

    fn network_model(&self, params: &Value) -> Result<Value, PluginError> {
        let secret_id = params
            .get("secretId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 120)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Model secret reference is invalid."))?;
        let base_url = params
            .get("baseUrl")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 2048)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Model endpoint is invalid."))?;
        let allow_loopback = params
            .get("allowLoopback")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let timeout_seconds = params
            .get("timeoutSeconds")
            .and_then(Value::as_u64)
            .filter(|value| (10..=600).contains(value))
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Model timeout is invalid."))?;
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .filter(|value| {
                value.starts_with('/') && !value.starts_with("//") && value.len() <= 256
            })
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Model request path is invalid."))?;
        let body = params
            .get("body")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 4 * 1024 * 1024)
            .ok_or_else(|| plugin_error("RESOURCE_LIMIT", "Model request body is too large."))?;
        let sealed = fs::read(self.secret_path(secret_id)?).map_err(|_| {
            plugin_error(
                "UNAUTHORIZED",
                "The selected model profile has no saved API key.",
            )
        })?;
        let api_key = String::from_utf8(wonderland_secret::unseal(&sealed).map_err(|_| {
            plugin_error(
                "INTERNAL",
                "The selected model API key cannot be decrypted.",
            )
        })?)
        .map_err(|_| plugin_error("INTERNAL", "The selected model API key is invalid."))?;
        let endpoint = wonderland_net::ModelEndpoint {
            base_url: base_url.to_owned(),
            timeout: Duration::from_secs(timeout_seconds),
            allow_loopback,
            ..wonderland_net::ModelEndpoint::new(base_url)
        };
        let client = wonderland_net::ModelClient::new(endpoint).map_err(model_service_error)?;
        let bytes = tauri::async_runtime::block_on(client.post_json(path, &api_key, body))
            .map_err(model_service_error)?;
        Ok(json!({ "contentBase64": base64::engine::general_purpose::STANDARD.encode(bytes) }))
    }

    fn reveal_own(&self, app: &AppHandle, params: &Value) -> Result<Value, PluginError> {
        let directory = match params.get("scope").and_then(Value::as_str) {
            Some("data") => &self.data_dir,
            Some("exports") => &self.export_dir,
            Some("knowledge_cache") if self.plugin_id == "knowledge_library" => {
                let context = app.state::<wonderland_kernel::AppContext>();
                let path = context.app_data_dir.join("plugins/knowledge/zh-cn/v1");
                fs::create_dir_all(&path).map_err(|error| {
                    plugin_error(
                        "INTERNAL",
                        &format!("Cannot create knowledge cache directory: {error}"),
                    )
                })?;
                return crate::reveal::open_dir(&path)
                    .map(|_| json!({ "ok": true }))
                    .map_err(|error| {
                        plugin_error(
                            "INTERNAL",
                            &format!("Cannot open knowledge cache directory: {error}"),
                        )
                    });
            }
            _ => return Err(plugin_error("INVALID_INPUT", "Reveal scope is invalid.")),
        };
        fs::create_dir_all(directory).map_err(|error| {
            plugin_error(
                "INTERNAL",
                &format!("Cannot create plugin directory: {error}"),
            )
        })?;
        crate::reveal::open_dir(directory).map_err(|error| {
            plugin_error(
                "INTERNAL",
                &format!("Cannot open plugin directory: {error}"),
            )
        })?;
        Ok(json!({ "ok": true }))
    }

    fn deliver(&self, request_id: &str, result: Result<Value, PluginError>) {
        if let Ok(mut pending) = self.pending.lock()
            && let Some(sender) = pending.remove(request_id)
        {
            let _ = sender.send(result);
        }
    }
}

fn account_snapshot(app: &AppHandle) -> Result<Value, PluginError> {
    let context = app.state::<wonderland_kernel::AppContext>();
    let account = context
        .account
        .as_ref()
        .ok_or_else(|| plugin_error("NOT_RUNNING", "Core account service is unavailable."))?;
    serde_json::to_value(account.snapshot())
        .map_err(|_| plugin_error("INTERNAL", "Cannot serialize the Core account snapshot."))
}

fn knowledge_local_model(app: &AppHandle) -> Result<Value, PluginError> {
    let context = app.state::<wonderland_kernel::AppContext>();
    let cache_dir = context.app_data_dir.join("plugins/knowledge/zh-cn/v1");
    let models_dir = cache_dir.join("models");
    fs::create_dir_all(&models_dir).map_err(|error| {
        plugin_error(
            "INTERNAL",
            &format!("Cannot create the approved knowledge model directory: {error}"),
        )
    })?;
    Ok(json!({ "cacheDir": cache_dir, "modelsDir": models_dir }))
}

/// Browser access is intentionally a set of named destinations. Plugins cannot ask the host to
/// open arbitrary URLs or pass through query strings and fragments.
fn open_official(params: &Value) -> Result<Value, PluginError> {
    let url = match params.get("target").and_then(Value::as_str) {
        Some("document") => {
            let path_id = params
                .get("pathId")
                .and_then(Value::as_str)
                .filter(|value| {
                    !value.is_empty()
                        && value.len() <= 128
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                })
                .ok_or_else(|| {
                    plugin_error("INVALID_INPUT", "Official document identifier is invalid.")
                })?;
            format!("https://act.mihoyo.com/ys/ugc/tutorial/detail/{path_id}")
        }
        Some("onnx") => "https://github.com/microsoft/onnxruntime/releases".to_owned(),
        Some("model") => "https://huggingface.co/BAAI/bge-small-zh-v1.5".to_owned(),
        _ => {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Official browser destination is invalid.",
            ));
        }
    };
    crate::reveal::open_url(&url).map_err(|error| {
        plugin_error(
            "INTERNAL",
            &format!("Cannot open official browser destination: {error}"),
        )
    })?;
    Ok(json!({ "ok": true }))
}

fn account_authed_get(app: &AppHandle, params: &Value) -> Result<Value, PluginError> {
    let account_key = params
        .get("accountKey")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 100)
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Account key is invalid."))?;
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with('/') && !value.starts_with("//") && value.len() <= 512)
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Account request path is invalid."))?;
    let query = params
        .get("query")
        .and_then(Value::as_array)
        .filter(|items| items.len() <= 32)
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Account request query is invalid."))?
        .iter()
        .map(|item| {
            let pair = item
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| {
                    plugin_error("INVALID_INPUT", "Account request query is invalid.")
                })?;
            let key = pair[0]
                .as_str()
                .filter(|value| value.len() <= 100)
                .ok_or_else(|| {
                    plugin_error("INVALID_INPUT", "Account request query is invalid.")
                })?;
            let value = pair[1]
                .as_str()
                .filter(|value| value.len() <= 512)
                .ok_or_else(|| {
                    plugin_error("INVALID_INPUT", "Account request query is invalid.")
                })?;
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    let context = app.state::<wonderland_kernel::AppContext>();
    let account = context
        .account
        .as_ref()
        .ok_or_else(|| plugin_error("NOT_RUNNING", "Core account service is unavailable."))?;
    let bytes = tauri::async_runtime::block_on(account.authed_get(account_key, path, &query))
        .map_err(|error| {
            plugin_error(
                match error {
                    wonderland_kernel::KernelError::NotLoggedIn => {
                        "PLUGIN_MY_WONDERLAND_NOT_LOGGED_IN"
                    }
                    wonderland_kernel::KernelError::AccountNotFound(_) => "INVALID_INPUT",
                    wonderland_kernel::KernelError::SessionExpired => {
                        "PLUGIN_MY_WONDERLAND_SESSION_EXPIRED"
                    }
                    wonderland_kernel::KernelError::InvalidInput => "UNAUTHORIZED",
                    wonderland_kernel::KernelError::Timeout => "TIMEOUT",
                    wonderland_kernel::KernelError::Http(_) => "PLUGIN_MY_WONDERLAND_HTTP_ERROR",
                    _ => "INTERNAL",
                },
                "Core account request failed.",
            )
        })?;
    Ok(json!({ "contentBase64": base64::engine::general_purpose::STANDARD.encode(bytes) }))
}

fn network_public(params: &Value) -> Result<Value, PluginError> {
    let method = params
        .get("method")
        .and_then(Value::as_str)
        .filter(|method| matches!(*method, "GET" | "POST"))
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network method is invalid."))?;
    let url = params
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| url.len() <= 2048 && url.starts_with("https://"))
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network URL is invalid."))?;
    let headers = params
        .get("headers")
        .and_then(Value::as_array)
        .filter(|headers| headers.len() <= 16)
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network headers are invalid."))?
        .iter()
        .map(|item| {
            let pair = item
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| {
                    plugin_error("INVALID_INPUT", "Public network headers are invalid.")
                })?;
            let name = pair[0].as_str().ok_or_else(|| {
                plugin_error("INVALID_INPUT", "Public network headers are invalid.")
            })?;
            let value = pair[1].as_str().ok_or_else(|| {
                plugin_error("INVALID_INPUT", "Public network headers are invalid.")
            })?;
            let normalized = name.to_ascii_lowercase();
            if !matches!(
                normalized.as_str(),
                "user-agent"
                    | "origin"
                    | "referer"
                    | "x-rpc-client_type"
                    | "x-rpc-language"
                    | "accept"
            ) || value.len() > 1024
                || value.chars().any(char::is_control)
            {
                return Err(plugin_error(
                    "INVALID_INPUT",
                    "Public network header is not allowed.",
                ));
            }
            Ok((normalized, value.to_owned()))
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    let query = params
        .get("query")
        .and_then(Value::as_array)
        .filter(|items| items.len() <= 32)
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network query is invalid."))?
        .iter()
        .map(|item| {
            let pair = item
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network query is invalid."))?;
            let key = pair[0]
                .as_str()
                .filter(|value| value.len() <= 100)
                .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network query is invalid."))?;
            let value = pair[1]
                .as_str()
                .filter(|value| value.len() <= 512)
                .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network query is invalid."))?;
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    let bytes = match method {
        "GET" => tauri::async_runtime::block_on(
            wonderland_net::HttpClient::new()
                .map_err(|_| plugin_error("INTERNAL", "Public network client is unavailable."))?
                .get_bytes(
                    url,
                    &headers
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str()))
                        .collect::<Vec<_>>(),
                    &query,
                ),
        ),
        "POST" => {
            if !query.is_empty() {
                return Err(plugin_error(
                    "INVALID_INPUT",
                    "POST query parameters must be part of the URL.",
                ));
            }
            let body = params
                .get("body")
                .and_then(Value::as_str)
                .filter(|body| body.len() <= 1_048_576)
                .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network body is invalid."))?;
            tauri::async_runtime::block_on(
                wonderland_net::HttpClient::new()
                    .map_err(|_| plugin_error("INTERNAL", "Public network client is unavailable."))?
                    .post_json(
                        url,
                        &headers
                            .iter()
                            .map(|(name, value)| (name.as_str(), value.as_str()))
                            .collect::<Vec<_>>(),
                        body,
                    ),
            )
        }
        _ => unreachable!(),
    }
    .map_err(|error| {
        plugin_error(
            match error {
                wonderland_kernel::KernelError::Timeout => "TIMEOUT",
                wonderland_kernel::KernelError::InvalidInput => "UNAUTHORIZED",
                wonderland_kernel::KernelError::Http(_) => "PLUGIN_NETWORK_HTTP_ERROR",
                _ => "PLUGIN_NETWORK_REQUEST_FAILED",
            },
            "Public network request failed.",
        )
    })?;
    Ok(json!({ "contentBase64": base64::engine::general_purpose::STANDARD.encode(bytes) }))
}

fn model_service_error(error: wonderland_net::ModelError) -> PluginError {
    match error {
        wonderland_net::ModelError::Transient {
            status: Some(status),
            ..
        } => plugin_error(
            "PLUGIN_TRANSLATOR_MODEL_TRANSIENT",
            &format!("Model service returned HTTP {status}."),
        ),
        wonderland_net::ModelError::Transient { status: None, .. } => {
            plugin_error("TIMEOUT", "Model service timed out or could not connect.")
        }
        wonderland_net::ModelError::Invalid(_) => plugin_error(
            "PLUGIN_TRANSLATOR_MODEL_INVALID",
            "Model service response was invalid.",
        ),
        wonderland_net::ModelError::Config(_) => {
            plugin_error("INVALID_INPUT", "Model endpoint configuration is invalid.")
        }
        wonderland_net::ModelError::Indeterminate => plugin_error(
            "PLUGIN_TRANSLATOR_MODEL_INDETERMINATE",
            "Model request completed without a confirmed response.",
        ),
    }
}

fn read_stderr(stderr: ChildStderr, plugin_id: String) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let diagnostic: String = line.chars().take(2_048).collect();
                warn!(plugin_id = %plugin_id, diagnostic = %diagnostic.trim_end(), "插件后端诊断输出");
            }
        }
    }
}

fn file_handle_token() -> Result<String, PluginError> {
    let mut bytes = [0_u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| plugin_error("INTERNAL", "Cannot create a file access handle."))?;
    Ok(hex::encode(bytes))
}

fn file_mime_type(extension: &str) -> Option<&'static str> {
    match extension {
        "json" => Some("application/json"),
        "csv" => Some("text/csv"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn safe_export_name(name: &str) -> bool {
    name.len() <= 180
        && matches!(
            Path::new(name)
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("json" | "csv" | "xlsx")
        )
        && Path::new(name).file_name().and_then(|file| file.to_str()) == Some(name)
        && !name
            .chars()
            .any(|character| character.is_control() || matches!(character, ':' | '/' | '\\'))
}

fn valid_envelope(frame: &Value) -> bool {
    frame.get("protocol").and_then(Value::as_str) == Some("wonderland-plugin")
        && frame.get("version").and_then(Value::as_str) == Some("1.0.0")
        && frame.get("type").and_then(Value::as_str).is_some()
}

fn valid_message(protocol: &str, version: &str, message_type: &str) -> bool {
    protocol == "wonderland-plugin"
        && version == "1.0.0"
        && matches!(
            message_type,
            "hello" | "request" | "result" | "error" | "event" | "cancel"
        )
}

fn valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty() && request_id.len() <= 128
}

fn valid_method(method: &str) -> bool {
    method.split('.').all(|part| {
        !part.is_empty()
            && part.as_bytes()[0].is_ascii_lowercase()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

fn sanitize_plugin_error(error: &mut PluginError) {
    const KNOWN: &[&str] = &[
        "INVALID_REQUEST",
        "METHOD_NOT_FOUND",
        "INVALID_INPUT",
        "INVALID_RESPONSE",
        "UNAUTHORIZED",
        "NOT_INSTALLED",
        "DISABLED",
        "NOT_RUNNING",
        "INCOMPATIBLE_CORE",
        "INCOMPATIBLE_PROTOCOL",
        "PLUGIN_START_FAILED",
        "PLUGIN_CRASHED",
        "TIMEOUT",
        "CANCELLED",
        "RESOURCE_LIMIT",
        "INTERNAL",
    ];
    if !KNOWN.contains(&error.code.as_str()) && !error.code.starts_with("PLUGIN_") {
        error.code = "INTERNAL".to_owned();
    }
    error.message = error
        .message
        .chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .take(2_048)
        .collect();
    error.details = None;
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

pub(crate) fn plugin_error(code: &str, message: &str) -> PluginError {
    PluginError {
        code: code.to_owned(),
        message: message.to_owned(),
        details: None,
    }
}

fn internal_error(message: &str) -> PluginError {
    plugin_error("INTERNAL", message)
}
