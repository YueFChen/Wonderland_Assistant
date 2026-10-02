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
use wonderland_kernel::logging::{debug, error, info, trace, warn};
use wonderland_plugin_protocol::{
    PROTOCOL_ID, PROTOCOL_VERSION, PluginError, PluginErrorMessage, PluginEvent, PluginHello,
    PluginRequest, PluginResult, PluginRuntimeState,
};

use crate::plugin_manager::PluginManager;
use crate::remote_interaction::RemoteContext;

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_REQUESTS_PER_SESSION: usize = 65_536;
const MAX_PICKED_FILE_CHUNK_BYTES: u64 = 1024 * 1024;
const MAX_PICKED_FILE_INLINE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_EXPORTED_FILE_BYTES: usize = 20 * 1024 * 1024;
const FILE_HANDLE_TTL: Duration = Duration::from_secs(5 * 60);
const PROCESS_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_PLUGIN_STDERR_LINE_BYTES: usize = 8 * 1024;

pub(crate) struct PluginProcess {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Mutex<HashMap<String, Sender<Result<Value, PluginError>>>>,
    seen_request_ids: Mutex<HashSet<String>>,
    hello_waiter: Mutex<Option<SyncSender<Result<PluginHello, String>>>>,
    alive: AtomicBool,
    plugin_id: String,
    granted_capabilities: HashSet<String>,
    network_public_hosts: Vec<String>,
    data_dir: PathBuf,
    export_dir: PathBuf,
    picked_files: Mutex<HashMap<String, PickedFile>>,
    invocation_contexts: Mutex<HashMap<String, Option<Arc<RemoteContext>>>>,
    supports_service_context: bool,
    remote_services: Arc<tokio::sync::Semaphore>,
}

struct PickedFile {
    path: PathBuf,
    name: String,
    mime_type: String,
    max_bytes: u64,
    size: u64,
    expires_at: Instant,
}

struct InvocationGuard<'a> {
    process: &'a PluginProcess,
    id: &'a str,
}

fn dispatch_remote_service(
    limit: Arc<tokio::sync::Semaphore>,
    work: impl FnOnce() + Send + 'static,
) -> Result<(), PluginError> {
    let permit = limit
        .try_acquire_owned()
        .map_err(|_| plugin_error("RESOURCE_LIMIT", "Too many remote host service requests."))?;
    thread::Builder::new()
        .name("remote-host-service".into())
        .spawn(move || {
            let _permit = permit;
            work();
        })
        .map_err(|_| plugin_error("RESOURCE_LIMIT", "Cannot start remote host service worker."))?;
    Ok(())
}

fn resolve_invocation_context(
    contexts: &HashMap<String, Option<Arc<RemoteContext>>>,
    id: Option<&str>,
    method: &str,
) -> Result<Option<Arc<RemoteContext>>, PluginError> {
    if let Some(id) = id {
        return contexts
            .get(id)
            .cloned()
            .ok_or_else(|| plugin_error("CANCELLED", "请求上下文已失效。"));
    }
    if contexts.values().any(Option::is_some)
        && (method.starts_with("core.files.")
            || method == "core.browser.open_official"
            || method == "core.services.invoke")
    {
        return Err(plugin_error(
            "REMOTE_CONTEXT_REQUIRED",
            "插件未传递请求上下文，无法安全判断交互设备。请更新插件。",
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod invocation_context_tests {
    use super::*;
    #[test]
    fn remote_waits_leave_dispatch_available_and_have_bounded_workers() {
        let limit = Arc::new(tokio::sync::Semaphore::new(1));
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        dispatch_remote_service(limit.clone(), move || {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
        })
        .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            dispatch_remote_service(limit.clone(), || {})
                .unwrap_err()
                .code,
            "RESOURCE_LIMIT"
        );
        finish_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while limit.available_permits() == 0 {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        dispatch_remote_service(limit, || {}).unwrap();
    }
    #[test]
    fn local_and_remote_concurrent_calls_are_routed_independently() {
        let remote = Arc::new(RemoteContext::new(
            Arc::new(crate::remote_interaction::RemoteBroker::default()),
            "browser".into(),
            "plugin".into(),
        ));
        let contexts =
            HashMap::from([("local".into(), None), ("web".into(), Some(remote.clone()))]);
        assert!(
            resolve_invocation_context(&contexts, Some("local"), "core.files.pick")
                .unwrap()
                .is_none()
        );
        assert!(Arc::ptr_eq(
            &resolve_invocation_context(&contexts, Some("web"), "core.files.pick")
                .unwrap()
                .unwrap(),
            &remote
        ));
        assert!(
            resolve_invocation_context(&contexts, Some("expired"), "core.files.export").is_err()
        );
        assert!(resolve_invocation_context(&contexts, None, "core.files.pick").is_err());
        assert!(
            resolve_invocation_context(&HashMap::new(), None, "core.files.pick")
                .unwrap()
                .is_none()
        );
    }
}
impl Drop for InvocationGuard<'_> {
    fn drop(&mut self) {
        if let Some(Some(context)) = self
            .process
            .invocation_contexts
            .lock()
            .unwrap()
            .remove(self.id)
        {
            context.cancel();
        }
    }
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
        let process_id = child.id();
        info!(
            plugin_id = %plugin_id,
            version = %state.manifest.version,
            process_id,
            phase = "process_spawn",
            "插件后端进程已创建"
        );
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
            network_public_hosts: state.manifest.network_public_hosts.clone(),
            data_dir,
            export_dir: documents_dir
                .join("Wonderland Assistant")
                .join(&plugin_id)
                .join("Exports"),
            picked_files: Mutex::new(HashMap::new()),
            invocation_contexts: Mutex::new(HashMap::new()),
            remote_services: Arc::new(tokio::sync::Semaphore::new(16)),
            supports_service_context: state
                .manifest
                .backend
                .supports_service_context
                .unwrap_or(false),
        });

        let weak = Arc::downgrade(&process);
        thread::Builder::new()
            .name(format!("plugin-{plugin_id}-stdout"))
            .spawn(move || read_stdout(stdout, weak, app, contract))
            .map_err(|error| {
                internal_error(&format!("Cannot start plugin stdout reader: {error}"))
            })?;
        let stderr_plugin_id = plugin_id.clone();
        thread::Builder::new()
            .name(format!("plugin-{plugin_id}-stderr"))
            .spawn(move || read_stderr(stderr, stderr_plugin_id))
            .map_err(|error| {
                internal_error(&format!("Cannot start plugin stderr reader: {error}"))
            })?;

        let hello = json!({
            "protocol": PROTOCOL_ID,
            "version": PROTOCOL_VERSION,
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
        if hello.protocol != PROTOCOL_ID
            || hello.version != PROTOCOL_VERSION
            || hello.message_type != "hello"
            || hello.role != "plugin"
            || hello.plugin_id != state.manifest.id
            || hello.plugin_version != state.manifest.version
            || hello.contract_sha256 != contract_sha256
        {
            warn!(
                plugin_id = %plugin_id,
                expected_version = %state.manifest.version,
                protocol_matches = hello.protocol == PROTOCOL_ID,
                protocol_version_matches = hello.version == PROTOCOL_VERSION,
                role_matches = hello.role == "plugin",
                plugin_id_matches = hello.plugin_id == state.manifest.id,
                plugin_version_matches = hello.plugin_version == state.manifest.version,
                contract_matches = hello.contract_sha256 == contract_sha256,
                phase = "handshake_validation",
                "插件握手身份或合约校验失败"
            );
            return Err(PluginError {
                code: "PLUGIN_START_FAILED".to_owned(),
                message: "Plugin handshake identity or contract does not match its manifest."
                    .to_owned(),
                details: None,
            });
        }
        info!(
            plugin_id = %plugin_id,
            version = %hello.plugin_version,
            protocol_version = hello.version,
            phase = "handshake",
            "插件握手成功"
        );
        Ok(process)
    }

    pub(crate) fn call(
        &self,
        request_id: &str,
        method: &str,
        params: Value,
        timeout_ms: u64,
        remote: Option<Arc<RemoteContext>>,
    ) -> Result<Value, PluginError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(plugin_error("PLUGIN_CRASHED", "Plugin process has exited."));
        }
        if remote.is_some() && !self.supports_service_context {
            return Err(plugin_error(
                "REMOTE_CONTEXT_REQUIRED",
                "此插件尚未声明远程请求上下文支持，请更新插件。宿主机本地操作不受影响。",
            ));
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
        let _invocation_guard = if self.supports_service_context {
            self.invocation_contexts
                .lock()
                .unwrap()
                .insert(request_id.into(), remote);
            Some(InvocationGuard {
                process: self,
                id: request_id,
            })
        } else {
            None
        };
        let mut frame = json!({
            "protocol": PROTOCOL_ID,
            "version": PROTOCOL_VERSION,
            "type": "request",
            "id": request_id,
            "method": method,
            "params": params,
        });
        if _invocation_guard.is_some() {
            frame["serviceContext"] = json!(request_id);
        }
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
        if let Some(Some(context)) = self.invocation_contexts.lock().unwrap().get(request_id) {
            context.cancel();
        }
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
            "protocol": PROTOCOL_ID,
            "version": PROTOCOL_VERSION,
            "type": "cancel",
            "id": request_id,
        }))
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// Stops the backend synchronously before Core removes its package or private data.
    pub(crate) fn stop(&self) -> Result<(), PluginError> {
        self.alive.store(false, Ordering::Release);
        let mut child = self
            .child
            .lock()
            .map_err(|_| internal_error("Plugin process handle is unavailable."))?;
        if let Err(error) = stop_child(&mut child) {
            self.fail_pending(plugin_error(
                "PLUGIN_STOPPED",
                "Plugin shutdown was requested by Core.",
            ));
            return Err(error);
        }
        self.fail_pending(plugin_error(
            "PLUGIN_STOPPED",
            "Plugin was stopped before its files were removed.",
        ));
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub(crate) fn terminate_for_test(&self) -> Result<(), PluginError> {
        self.stop()
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
                warn!(
                    plugin_id = %self.plugin_id,
                    phase = "protocol_write",
                    reason = %error,
                    "向插件后端发送协议消息失败"
                );
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

fn stop_child(child: &mut Child) -> Result<(), PluginError> {
    if child
        .try_wait()
        .map_err(|error| internal_error(&format!("Cannot inspect plugin process: {error}")))?
        .is_some()
    {
        return Ok(());
    }

    if let Err(kill_error) = child.kill()
        && child
            .try_wait()
            .map_err(|error| internal_error(&format!("Cannot inspect plugin process: {error}")))?
            .is_none()
    {
        return Err(internal_error(&format!(
            "Cannot stop plugin process: {kill_error}"
        )));
    }

    let deadline = Instant::now() + PROCESS_STOP_TIMEOUT;
    loop {
        if child
            .try_wait()
            .map_err(|error| internal_error(&format!("Cannot inspect plugin process: {error}")))?
            .is_some()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(internal_error(&format!(
                "Plugin process did not exit within {} seconds.",
                PROCESS_STOP_TIMEOUT.as_secs()
            )));
        }
        thread::sleep(PROCESS_STOP_POLL_INTERVAL);
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(child) = self.child.get_mut() {
            // Drop can run while a caller holds the plugin state lock. Never block there
            // waiting for process exit; `Child` closes its handle after this best-effort kill.
            if child.try_wait().is_ok_and(|status| status.is_none()) {
                let _ = child.kill();
            }
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
            if let Err(message) = &hello
                && let Some(process) = process.upgrade()
            {
                warn!(
                    plugin_id = %process.plugin_id,
                    phase = "handshake",
                    reason = %message,
                    "插件握手响应无效"
                );
            }
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
                            && request
                                .service_context
                                .as_deref()
                                .is_none_or(valid_request_id)
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
                let remote = request.service_context.as_ref().is_some_and(|id| {
                    process
                        .invocation_contexts
                        .lock()
                        .unwrap()
                        .get(id)
                        .is_some_and(Option::is_some)
                });
                if remote {
                    // A browser file picker must not block this plugin's stdout reader.
                    // Native host calls retain their existing synchronous path.
                    let worker = process.clone();
                    let app = app.clone();
                    let service_id = request.id.clone();
                    if let Err(error) =
                        dispatch_remote_service(process.remote_services.clone(), move || {
                            if let Err(error) = worker.reply_to_host_service(&app, &request) {
                                worker.fail_pending(error);
                            }
                        })
                        && let Err(error) = process.write_frame(&json!({
                            "protocol": PROTOCOL_ID, "version": PROTOCOL_VERSION,
                            "type": "error", "id": service_id, "error": error,
                        }))
                    {
                        break error;
                    }
                } else if let Err(error) = process.reply_to_host_service(&app, &request) {
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
        if process.alive.swap(false, Ordering::AcqRel) {
            warn!(
                plugin_id = %process.plugin_id,
                code = %exit_error.code,
                reason = %exit_error.message,
                phase = "protocol_stream",
                "插件后端通信流已结束"
            );
        }
        if let Ok(mut waiter) = process.hello_waiter.lock()
            && let Some(waiter) = waiter.take()
        {
            let _ = waiter.send(Err(exit_error.message.clone()));
        }
        process.fail_pending(exit_error);
    }
}

impl PluginProcess {
    fn reply_to_host_service(
        &self,
        app: &AppHandle,
        request: &PluginRequest,
    ) -> Result<(), PluginError> {
        let reply = match self.host_service(
            app,
            &request.method,
            &request.params,
            request.service_context.as_deref(),
        ) {
            Ok(result) => {
                json!({"protocol":PROTOCOL_ID,"version":PROTOCOL_VERSION,"type":"result","id":request.id,"result":result})
            }
            Err(error) => {
                json!({"protocol":PROTOCOL_ID,"version":PROTOCOL_VERSION,"type":"error","id":request.id,"error":error})
            }
        };
        self.write_frame(&reply)
    }

    fn host_service(
        &self,
        app: &AppHandle,
        method: &str,
        params: &Value,
        service_context: Option<&str>,
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
            "core.services.resolve" | "core.services.invoke" => "services.call",
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

        let remote = resolve_invocation_context(
            &self.invocation_contexts.lock().unwrap(),
            service_context,
            method,
        )?;
        if let Some(context) = &remote
            && (method.starts_with("core.files.") || method == "core.browser.open_official")
        {
            return context.service(method, params);
        }
        match method {
            "core.account.snapshot" => account_snapshot(app),
            "core.account.authed_get" => account_authed_get(app, params),
            "core.network.public" => network_public(params, &self.network_public_hosts),
            "core.network.model" => self.network_model(params),
            "core.secrets.plugin.get" => self.secret_get(params),
            "core.secrets.plugin.set" => self.secret_set(params),
            "core.secrets.plugin.delete" => self.secret_delete(params),
            "core.secrets.plugin.has" => self.secret_has(params),
            "core.files.pick" => self.pick_file(app, params),
            "core.files.read" => self.read_picked_file(params),
            "core.files.export" => self.export_file(params),
            "core.files.export_dir" => self.export_dir(),
            "core.files.reveal_own" => self.reveal_own(params),
            "core.browser.open_official" => open_official(params),
            "core.services.resolve" => {
                let service_id = params
                    .get("serviceId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| plugin_error("INVALID_INPUT", "Service ID is missing."))?;
                let manager = app.try_state::<PluginManager>().ok_or_else(|| {
                    plugin_error(
                        "SERVICE_UNAVAILABLE",
                        "Plugin service broker is unavailable.",
                    )
                })?;
                let resolution = manager.resolve_service(&self.plugin_id, service_id)?;
                serde_json::to_value(resolution).map_err(|_| {
                    plugin_error("INTERNAL", "Service resolution could not be serialized.")
                })
            }
            "core.services.invoke" => {
                let service_id = params
                    .get("serviceId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| plugin_error("INVALID_INPUT", "Service ID is missing."))?;
                let service_method = params
                    .get("method")
                    .and_then(Value::as_str)
                    .ok_or_else(|| plugin_error("INVALID_INPUT", "Service method is missing."))?;
                let service_params = params.get("params").cloned().unwrap_or_else(|| json!({}));
                let manager = app.try_state::<PluginManager>().ok_or_else(|| {
                    plugin_error(
                        "SERVICE_UNAVAILABLE",
                        "Plugin service broker is unavailable.",
                    )
                })?;
                if remote.is_some() {
                    manager.invoke_service_with_context(
                        &self.plugin_id,
                        service_id,
                        service_method,
                        service_params,
                        remote,
                    )
                } else {
                    manager.invoke_service(
                        &self.plugin_id,
                        service_id,
                        service_method,
                        service_params,
                    )
                }
            }
            _ => unreachable!(),
        }
    }

    fn pick_file(&self, app: &AppHandle, params: &Value) -> Result<Value, PluginError> {
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
        let mut dialog = app.dialog().file();
        if !extensions.is_empty() {
            dialog = dialog.add_filter("Plugin files", &extensions);
        }
        let file = dialog.blocking_pick_file();
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

/// Browser access is intentionally a set of named destinations. Plugins cannot ask the host to
/// open arbitrary URLs or pass through query strings and fragments.
fn open_official(params: &Value) -> Result<Value, PluginError> {
    let url = official_url(params)?;
    crate::reveal::open_url(&url).map_err(|error| {
        plugin_error(
            "INTERNAL",
            &format!("Cannot open official browser destination: {error}"),
        )
    })?;
    Ok(json!({ "ok": true }))
}

pub(crate) fn official_url(params: &Value) -> Result<String, PluginError> {
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
    Ok(url)
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
                    wonderland_kernel::KernelError::ResourceLimit => "RESOURCE_LIMIT",
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

fn network_public(params: &Value, allowed_hosts: &[String]) -> Result<Value, PluginError> {
    let method = params
        .get("method")
        .and_then(Value::as_str)
        .filter(|method| matches!(*method, "GET" | "POST"))
        .ok_or_else(|| plugin_error("INVALID_INPUT", "Public network method is invalid."))?;
    let url = params
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| {
            url.len() <= 2048
                && (url.starts_with("https://")
                    || allowed_hosts.is_empty() && url.starts_with("http://"))
        })
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
            wonderland_net::HttpClient::with_allowed_hosts(allowed_hosts.to_vec())
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
                wonderland_net::HttpClient::with_allowed_hosts(allowed_hosts.to_vec())
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
                wonderland_kernel::KernelError::ResourceLimit => "RESOURCE_LIMIT",
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
    let mut line = Vec::with_capacity(MAX_PLUGIN_STDERR_LINE_BYTES);
    loop {
        let truncated = match read_bounded_line(&mut reader, &mut line) {
            Ok(Some(truncated)) => truncated,
            Ok(None) => return,
            Err(error) => {
                warn!(plugin_id = %plugin_id, reason = %error, "读取插件后端标准错误失败");
                return;
            }
        };
        let diagnostic = String::from_utf8_lossy(&line);
        let diagnostic = diagnostic.trim_end_matches(['\r', '\n']);
        log_plugin_diagnostic(
            parse_plugin_log_level(diagnostic),
            &plugin_id,
            diagnostic,
            truncated,
        );
    }
}

/// 从 stderr 中至多缓冲一条限定长度的文本，并排空超出上限的部分。
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut read_any = false;
    let mut truncated = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(read_any.then_some(truncated));
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        let consumed = content_len + if newline.is_some() { 1 } else { 0 };
        let copied = content_len.min(MAX_PLUGIN_STDERR_LINE_BYTES - line.len());
        line.extend_from_slice(&available[..copied]);
        truncated |= copied < content_len;
        read_any = true;
        reader.consume(consumed);

        if newline.is_some() {
            return Ok(Some(truncated));
        }
    }
}

#[derive(Clone, Copy)]
enum PluginLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Parses common tracing prefixes and JSON `level` fields; unknown lines remain visible as info.
fn parse_plugin_log_level(line: &str) -> PluginLogLevel {
    if line.trim_start().starts_with('{')
        && let Ok(value) = serde_json::from_str::<Value>(line)
        && let Some(level) = value.get("level").and_then(Value::as_str)
        && let Some(level) = parse_plugin_log_level_token(level)
    {
        return level;
    }

    for token in line.split_whitespace().take(4) {
        let token = token
            .trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '_');
        let token = token.rsplit_once('=').map_or(token, |(_, value)| value);
        if let Some(level) = parse_plugin_log_level_token(token) {
            return level;
        }
    }

    PluginLogLevel::Info
}

fn parse_plugin_log_level_token(token: &str) -> Option<PluginLogLevel> {
    if token.eq_ignore_ascii_case("error") {
        Some(PluginLogLevel::Error)
    } else if token.eq_ignore_ascii_case("warn") || token.eq_ignore_ascii_case("warning") {
        Some(PluginLogLevel::Warn)
    } else if token.eq_ignore_ascii_case("info") {
        Some(PluginLogLevel::Info)
    } else if token.eq_ignore_ascii_case("debug") {
        Some(PluginLogLevel::Debug)
    } else if token.eq_ignore_ascii_case("trace") {
        Some(PluginLogLevel::Trace)
    } else {
        None
    }
}

fn log_plugin_diagnostic(
    level: PluginLogLevel,
    plugin_id: &str,
    diagnostic: &str,
    truncated: bool,
) {
    match level {
        PluginLogLevel::Error => error!(
            plugin_id = %plugin_id,
            diagnostic = %diagnostic,
            diagnostic_truncated = truncated,
            "插件后端诊断输出"
        ),
        PluginLogLevel::Warn => warn!(
            plugin_id = %plugin_id,
            diagnostic = %diagnostic,
            diagnostic_truncated = truncated,
            "插件后端诊断输出"
        ),
        PluginLogLevel::Info => info!(
            plugin_id = %plugin_id,
            diagnostic = %diagnostic,
            diagnostic_truncated = truncated,
            "插件后端诊断输出"
        ),
        PluginLogLevel::Debug => debug!(
            plugin_id = %plugin_id,
            diagnostic = %diagnostic,
            diagnostic_truncated = truncated,
            "插件后端诊断输出"
        ),
        PluginLogLevel::Trace => trace!(
            plugin_id = %plugin_id,
            diagnostic = %diagnostic,
            diagnostic_truncated = truncated,
            "插件后端诊断输出"
        ),
    }
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
    frame.get("protocol").and_then(Value::as_str) == Some(PROTOCOL_ID)
        && frame.get("version").and_then(Value::as_str) == Some(PROTOCOL_VERSION)
        && frame.get("type").and_then(Value::as_str).is_some()
}

fn valid_message(protocol: &str, version: &str, message_type: &str) -> bool {
    protocol == PROTOCOL_ID
        && version == PROTOCOL_VERSION
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
        "REMOTE_UNSUPPORTED",
        "REMOTE_CONTEXT_REQUIRED",
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

#[cfg(test)]
mod process_stop_tests {
    use super::*;

    #[test]
    fn stopping_a_running_child_is_bounded_and_collects_its_exit() {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "exec sleep 30"]);
            command
        };

        let mut child = command.spawn().expect("long-running test process starts");
        let started = Instant::now();
        stop_child(&mut child).expect("Core can stop the child process");

        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            child
                .try_wait()
                .expect("child status is readable")
                .is_some()
        );
    }
}
