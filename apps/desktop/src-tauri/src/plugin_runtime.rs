//! One supervised stdio-NDJSON process per enabled plugin.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wonderland_kernel::logging::{debug, error, info, trace, warn};
use wonderland_plugin_protocol::{
    PROTOCOL_ID, PROTOCOL_VERSION, PluginError, PluginErrorMessage, PluginEvent, PluginHello,
    PluginRequest, PluginResult, PluginRuntimeState,
};

use crate::host_services::{HostInvocation, HostServices, RuntimeServices};
use crate::remote_interaction::RemoteContext;

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_QUEUED_WRITES: usize = 16;
const MAX_QUEUED_WRITE_BYTES: usize = 64 * 1024 * 1024;

const PROCESS_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_PLUGIN_STDERR_LINE_BYTES: usize = 8 * 1024;

pub(crate) struct PluginProcess {
    child: Mutex<Child>,
    writes: SyncSender<QueuedWrite>,
    queued_write_bytes: Arc<AtomicUsize>,
    pending: Mutex<PendingRequests>,
    hello_waiter: Mutex<Option<SyncSender<Result<PluginHello, String>>>>,
    alive: AtomicBool,
    process_cancelled: Arc<AtomicBool>,
    plugin_id: String,
    host: Arc<HostServices>,
    invocation_contexts: Mutex<HashMap<String, HostInvocation>>,
    supports_service_context: bool,
    host_workers: Arc<tokio::sync::Semaphore>,
}

struct QueuedWrite {
    bytes: Vec<u8>,
    reserved_bytes: Arc<AtomicUsize>,
    invocation: Option<HostInvocation>,
}

impl Drop for QueuedWrite {
    fn drop(&mut self) {
        self.reserved_bytes
            .fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}

fn queue_write(
    sender: &SyncSender<QueuedWrite>,
    reserved_bytes: Arc<AtomicUsize>,
    bytes: Vec<u8>,
    invocation: Option<HostInvocation>,
) -> Result<(), PluginError> {
    reserved_bytes
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
            queued
                .checked_add(bytes.len())
                .filter(|total| *total <= MAX_QUEUED_WRITE_BYTES)
        })
        .map_err(|_| plugin_error("RESOURCE_LIMIT", "Plugin write buffer is full."))?;
    let queued = QueuedWrite {
        bytes,
        reserved_bytes,
        invocation,
    };
    match sender.try_send(queued) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err(plugin_error(
            "RESOURCE_LIMIT",
            "Plugin write queue is full.",
        )),
        Err(TrySendError::Disconnected(_)) => {
            Err(plugin_error("PLUGIN_CRASHED", "Plugin writer has exited."))
        }
    }
}

/// Only active calls need storage. Wire IDs never reuse a caller's ID, so a late reply
/// cannot complete a later call even when its caller reuses the same public ID.
#[derive(Default)]
struct PendingRequests {
    sequence: u64,
    calls: HashMap<String, PendingRequest>,
}

struct PendingRequest {
    caller_id: String,
    sender: Sender<Result<Value, PluginError>>,
}

impl PendingRequests {
    fn insert(
        &mut self,
        caller_id: &str,
        sender: Sender<Result<Value, PluginError>>,
    ) -> Result<String, PluginError> {
        if self.wire_id(caller_id).is_some() {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Request ID is already active.",
            ));
        }
        if self.calls.len() >= 128 {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Plugin has too many concurrent requests.",
            ));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| internal_error("Plugin request sequence is exhausted."))?;
        let wire_id = format!("core-{}", self.sequence);
        self.calls.insert(
            wire_id.clone(),
            PendingRequest {
                caller_id: caller_id.into(),
                sender,
            },
        );
        Ok(wire_id)
    }

    fn wire_id(&self, caller_id: &str) -> Option<String> {
        self.calls
            .iter()
            .find(|(_, call)| call.caller_id == caller_id)
            .map(|(wire_id, _)| wire_id.clone())
    }

    fn caller_id(&self, wire_id: &str) -> Option<String> {
        self.calls.get(wire_id).map(|call| call.caller_id.clone())
    }

    fn deliver(&mut self, wire_id: &str, result: Result<Value, PluginError>) {
        if let Some(call) = self.calls.remove(wire_id) {
            let _ = call.sender.send(result);
        }
    }
}

struct InvocationGuard<'a> {
    process: &'a PluginProcess,
    id: &'a str,
}

fn dispatch_host_service(
    limit: Arc<tokio::sync::Semaphore>,
    work: impl FnOnce() + Send + 'static,
) -> Result<(), PluginError> {
    let permit = limit
        .try_acquire_owned()
        .map_err(|_| plugin_error("RESOURCE_LIMIT", "Too many host service requests."))?;
    thread::Builder::new()
        .name("host-service".into())
        .spawn(move || {
            let _permit = permit;
            work();
        })
        .map_err(|_| plugin_error("RESOURCE_LIMIT", "Cannot start host service worker."))?;
    Ok(())
}

fn resolve_invocation_context(
    contexts: &HashMap<String, HostInvocation>,
    id: Option<&str>,
    method: &str,
) -> Result<Option<Arc<RemoteContext>>, PluginError> {
    if let Some(id) = id {
        return contexts
            .get(id)
            .map(|invocation| invocation.remote.clone())
            .ok_or_else(|| plugin_error("CANCELLED", "请求上下文已失效。"));
    }
    if contexts
        .values()
        .any(|invocation| invocation.remote.is_some())
        && (matches!(
            method,
            "core.files.pick"
                | "core.files.read"
                | "core.files.export"
                | "core.files.export_open"
                | "core.files.export_dir"
                | "core.files.reveal_own"
        ) || method == "core.browser.open"
            || method == "core.plugins.call")
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
    fn host_waits_leave_dispatch_available_and_have_bounded_workers() {
        let limit = Arc::new(tokio::sync::Semaphore::new(1));
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        dispatch_host_service(limit.clone(), move || {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
        })
        .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            dispatch_host_service(limit.clone(), || {})
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
        dispatch_host_service(limit, || {}).unwrap();
    }
    #[test]
    fn local_and_remote_concurrent_calls_are_routed_independently() {
        let remote = Arc::new(RemoteContext::new(
            Arc::new(crate::remote_interaction::RemoteBroker::default()),
            "browser".into(),
            "plugin".into(),
        ));
        let contexts = HashMap::from([
            ("local".into(), HostInvocation::default()),
            (
                "web".into(),
                HostInvocation::for_call(Some(remote.clone()), Some(5000), None).unwrap(),
            ),
        ]);
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
        assert!(resolve_invocation_context(&contexts, None, "core.files.export_open").is_err());
        assert!(
            resolve_invocation_context(&contexts, None, "core.files.open_read")
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_invocation_context(&contexts, None, "core.files.open_write")
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_invocation_context(&HashMap::new(), None, "core.files.pick")
                .unwrap()
                .is_none()
        );
    }
}
impl Drop for InvocationGuard<'_> {
    fn drop(&mut self) {
        if let Some(invocation) = self
            .process
            .invocation_contexts
            .lock()
            .unwrap()
            .remove(self.id)
        {
            invocation.cancel();
        }
        self.process.host.cleanup();
    }
}

// An unaccepted backend must exit before the installer restores files on Windows.
struct StartupGuard(Option<Arc<PluginProcess>>);
impl Drop for StartupGuard {
    fn drop(&mut self) {
        if let Some(process) = self.0.take() {
            let _ = process.stop();
        }
    }
}

impl PluginProcess {
    pub(crate) fn start(
        services: RuntimeServices,
        state: &PluginRuntimeState,
        executable: &std::path::Path,
        contract: Value,
        data_dir: PathBuf,
        documents_dir: PathBuf,
        contract_sha256: &str,
    ) -> Result<Arc<Self>, PluginError> {
        let plugin_id = state.manifest.id.clone();
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
        if let Some(root) = data_dir.parent().and_then(std::path::Path::parent) {
            command.env("WONDERLAND_HOST_DATA_DIR", root);
        }
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
        let (writes, writer_rx) = mpsc::sync_channel(MAX_QUEUED_WRITES);
        let process = Arc::new(Self {
            child: Mutex::new(child),
            writes,
            queued_write_bytes: Arc::new(AtomicUsize::new(0)),
            pending: Mutex::new(PendingRequests::default()),
            hello_waiter: Mutex::new(Some(hello_tx)),
            alive: AtomicBool::new(true),
            process_cancelled: Arc::new(AtomicBool::new(false)),
            plugin_id: plugin_id.clone(),
            host: Arc::new(HostServices::new(
                plugin_id.clone(),
                data_dir,
                documents_dir,
                services,
            )),
            invocation_contexts: Mutex::new(HashMap::new()),
            host_workers: Arc::new(tokio::sync::Semaphore::new(16)),
            supports_service_context: state
                .manifest
                .backend
                .supports_service_context
                .unwrap_or(false),
        });

        let mut startup_guard = StartupGuard(Some(process.clone()));
        if let Some(manager) = process.host.services.manager.upgrade() {
            manager.register_starting_process(
                &plugin_id,
                process.host.services.startup_revision,
                process.clone(),
            )?;
        }
        let writer_process = Arc::downgrade(&process);
        thread::Builder::new()
            .name(format!("plugin-{plugin_id}-stdin"))
            .spawn(move || write_stdin(stdin, writer_rx, writer_process))
            .map_err(|error| {
                internal_error(&format!("Cannot start plugin stdin writer: {error}"))
            })?;
        let weak = Arc::downgrade(&process);
        thread::Builder::new()
            .name(format!("plugin-{plugin_id}-stdout"))
            .spawn(move || read_stdout(stdout, weak, contract))
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
        startup_guard.0.take();
        Ok(process)
    }

    pub(crate) fn call_with_parent(
        &self,
        request_id: &str,
        method: &str,
        params: Value,
        timeout_ms: Option<u64>,
        remote: Option<Arc<RemoteContext>>,
        parent: Option<&HostInvocation>,
    ) -> Result<Value, PluginError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(plugin_error("PLUGIN_CRASHED", "Plugin process has exited."));
        }
        if remote.is_some() && !self.supports_service_context {
            return Err(plugin_error(
                "REMOTE_UNSUPPORTED",
                "此插件未声明调用上下文支持，不能参与远程调用。",
            ));
        }
        if request_id.is_empty() || request_id.len() > 128 {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Request ID must contain 1 to 128 characters.",
            ));
        }
        let invocation = HostInvocation::for_call(remote, timeout_ms, parent)?;
        let (tx, rx) = mpsc::channel();
        let wire_id = self
            .pending
            .lock()
            .map_err(|_| internal_error("Plugin request table is unavailable."))?
            .insert(request_id, tx)?;
        self.invocation_contexts
            .lock()
            .unwrap()
            .insert(wire_id.clone(), invocation.clone());
        let _invocation_guard = InvocationGuard {
            process: self,
            id: &wire_id,
        };
        let mut frame = json!({
            "protocol": PROTOCOL_ID,
            "version": PROTOCOL_VERSION,
            "type": "request",
            "id": wire_id,
            "method": method,
            "params": params,
        });
        if self.supports_service_context {
            frame["serviceContext"] = json!(wire_id);
        }
        if let Err(error) = self.queue_frame(&frame, Some(invocation.clone())) {
            self.remove_pending(&wire_id);
            return Err(error);
        }
        trace!(plugin_id = %self.plugin_id, request_id, phase = "call_queued", "插件请求已入队");
        loop {
            if let Err(error) = invocation.check() {
                let _ = self.cancel_wire(&wire_id);
                self.remove_pending(&wire_id);
                return Err(error);
            }
            let remaining = invocation.remaining().unwrap_or(Duration::from_millis(20));
            match rx.recv_timeout(remaining.min(Duration::from_millis(20))) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(plugin_error(
                        "PLUGIN_CRASHED",
                        "Plugin process exited during the request.",
                    ));
                }
            }
        }
    }

    pub(crate) fn cancel(&self, request_id: &str) -> Result<(), PluginError> {
        let wire_id = self
            .pending
            .lock()
            .map_err(|_| internal_error("Plugin request table is unavailable."))?
            .wire_id(request_id)
            .ok_or_else(|| {
                plugin_error("CANCELLED", "No active plugin call has this request ID.")
            })?;
        self.cancel_wire(&wire_id)
    }

    fn cancel_wire(&self, request_id: &str) -> Result<(), PluginError> {
        if let Some(invocation) = self.invocation_contexts.lock().unwrap().get(request_id) {
            invocation.cancel();
        }
        self.host.cleanup();
        let active = self
            .pending
            .lock()
            .map_err(|_| internal_error("Plugin request table is unavailable."))?
            .calls
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
        self.cancel_host_invocations();
        self.close_lifecycle();
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
        self.queue_frame(frame, None)
    }

    fn close_lifecycle(&self) {
        if let Some(manager) = self.host.services.manager.upgrade() {
            manager.lifecycle_instance_closed(&self.plugin_id, self.host.services.instance_id);
        }
    }

    fn queue_frame(
        &self,
        frame: &Value,
        invocation: Option<HostInvocation>,
    ) -> Result<(), PluginError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(plugin_error("PLUGIN_CRASHED", "Plugin process has exited."));
        }
        let mut bytes = serde_json::to_vec(frame)
            .map_err(|error| internal_error(&format!("Cannot encode protocol frame: {error}")))?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(plugin_error(
                "RESOURCE_LIMIT",
                "Protocol frame exceeds 32 MiB.",
            ));
        }
        bytes.push(b'\n');
        queue_write(
            &self.writes,
            self.queued_write_bytes.clone(),
            bytes,
            invocation,
        )
    }

    fn remove_pending(&self, request_id: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.calls.remove(request_id);
        }
    }

    fn fail_pending(&self, error: PluginError) {
        let pending = self
            .pending
            .lock()
            .map(|mut pending| std::mem::take(&mut pending.calls))
            .unwrap_or_default();
        for (_, call) in pending {
            let _ = call.sender.send(Err(error.clone()));
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
        self.cancel_host_invocations();
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

/// One process-owned pipe writer; no strong process reference survives a blocking write.
fn write_stdin(
    mut stdin: ChildStdin,
    receiver: Receiver<QueuedWrite>,
    process: Weak<PluginProcess>,
) {
    while let Ok(queued) = receiver.recv() {
        if !process.upgrade().is_some_and(|process| process.is_alive()) {
            return;
        }
        if queued
            .invocation
            .as_ref()
            .is_some_and(|invocation| invocation.check().is_err())
        {
            continue;
        }
        if let Err(error) = stdin.write_all(&queued.bytes).and_then(|()| stdin.flush()) {
            if let Some(process) = process.upgrade() {
                protocol_ended(
                    &process,
                    plugin_error(
                        "PLUGIN_CRASHED",
                        &format!("Plugin process is not accepting input: {error}"),
                    ),
                    "protocol_write",
                );
            }
            return;
        }
    }
}

fn protocol_ended(process: &Arc<PluginProcess>, error: PluginError, phase: &'static str) {
    process.close_lifecycle();
    if process.alive.swap(false, Ordering::AcqRel) {
        warn!(
            plugin_id = %process.plugin_id,
            code = %error.code,
            reason = %error.message,
            phase,
            "插件后端通信流已结束"
        );
    }
    if let Ok(mut waiter) = process.hello_waiter.lock()
        && let Some(waiter) = waiter.take()
    {
        let _ = waiter.send(Err(error.message.clone()));
    }
    process.fail_pending(error.clone());
    process.cancel_host_invocations();
    if let Some(manager) = process.host.services.manager.upgrade() {
        manager.process_exited(&process.plugin_id, process, error);
    }
}

fn read_stdout(stdout: std::process::ChildStdout, process: Weak<PluginProcess>, contract: Value) {
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
                let request_id = match event.request_id.as_deref() {
                    Some(id) => match process
                        .pending
                        .lock()
                        .ok()
                        .and_then(|pending| pending.caller_id(id))
                    {
                        Some(id) => Some(id),
                        None => continue, // Expired calls cannot publish events into a newer call.
                    },
                    None => None,
                };
                process.host.services.environment.emit(
                    "plugin:event",
                    json!({
                        "pluginId": process.plugin_id,
                        "topic": topic,
                        "requestId": request_id,
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
                let admission = process
                    .host
                    .services
                    .manager
                    .upgrade()
                    .ok_or_else(|| {
                        plugin_error("SERVICE_UNAVAILABLE", "Plugin manager is unavailable.")
                    })
                    .and_then(|manager| manager.admit_work());
                let admission = match admission {
                    Ok(admission) => admission,
                    Err(error) => {
                        process.host.cleanup();
                        if let Err(error) = process.reply_to_host_service(&request.id, Err(error)) {
                            break error;
                        }
                        continue;
                    }
                };
                let host = process.host.clone();
                let invocation = process.host_invocation(&request);
                let worker = Arc::downgrade(&process);
                let service_id = request.id.clone();
                if let Err(error) = dispatch_host_service(process.host_workers.clone(), move || {
                    let _admission = admission;
                    let result = invocation.and_then(|invocation| {
                        host.call(&request.method, &request.params, &invocation)
                    });
                    host.cleanup();
                    if let Some(process) = worker.upgrade()
                        && let Err(error) = process.reply_to_host_service(&request.id, result)
                    {
                        process.fail_pending(error);
                    }
                }) {
                    process.host.cleanup();
                    if let Err(error) = process.reply_to_host_service(&service_id, Err(error)) {
                        break error;
                    }
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
        protocol_ended(&process, exit_error, "protocol_stream");
    }
}

impl PluginProcess {
    fn host_invocation(&self, request: &PluginRequest) -> Result<HostInvocation, PluginError> {
        let contexts = self
            .invocation_contexts
            .lock()
            .map_err(|_| internal_error("Invocation table is unavailable."))?;
        let remote = resolve_invocation_context(
            &contexts,
            request.service_context.as_deref(),
            &request.method,
        )?;
        let mut invocation = match request.service_context.as_deref() {
            Some(id) => contexts
                .get(id)
                .cloned()
                .ok_or_else(|| plugin_error("CANCELLED", "Host invocation has ended."))?,
            None => HostInvocation::default(),
        };
        invocation.remote = remote;
        invocation = invocation.with_timeout_ms(request.timeout_ms)?;
        Ok(invocation.with_cancellation_parent(self.process_cancelled.clone()))
    }

    fn reply_to_host_service(
        &self,
        id: &str,
        result: Result<Value, PluginError>,
    ) -> Result<(), PluginError> {
        let reply = match result {
            Ok(result) => {
                json!({"protocol":PROTOCOL_ID,"version":PROTOCOL_VERSION,"type":"result","id":id,"result":result})
            }
            Err(error) => {
                json!({"protocol":PROTOCOL_ID,"version":PROTOCOL_VERSION,"type":"error","id":id,"error":error})
            }
        };
        self.write_frame(&reply)
    }

    fn cancel_host_invocations(&self) {
        self.process_cancelled.store(true, Ordering::Release);
        if let Ok(contexts) = self.invocation_contexts.lock() {
            for invocation in contexts.values() {
                invocation.cancel();
            }
        }
        self.host.cleanup();
    }

    pub(crate) fn active_request_count(&self) -> usize {
        self.pending.lock().map_or(0, |pending| pending.calls.len())
            + 16usize.saturating_sub(self.host_workers.available_permits())
            + usize::from(self.queued_write_bytes.load(Ordering::Acquire) > 0)
    }

    #[cfg(test)]
    pub(crate) fn has_active_request_for_test(&self, request_id: &str) -> bool {
        self.pending
            .lock()
            .is_ok_and(|pending| pending.wire_id(request_id).is_some())
    }

    fn deliver(&self, request_id: &str, result: Result<Value, PluginError>) {
        trace!(plugin_id = %self.plugin_id, request_id, phase = "reply_received", "收到插件响应");
        if let Ok(mut pending) = self.pending.lock() {
            pending.deliver(request_id, result);
        }
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
        "REMOTE_DEPENDENCY_NOT_DECLARED",
        "CALL_DEPTH_EXCEEDED",
        "INTERACTION_UNAVAILABLE",
        "SERVICE_UNAVAILABLE",
        "SECRET_NOT_FOUND",
        "NOT_LOGGED_IN",
        "SESSION_EXPIRED",
        "HTTP_ERROR",
        "NETWORK_ERROR",
        "MODEL_TRANSIENT",
        "MODEL_INVALID",
        "MODEL_INDETERMINATE",
        "UPGRADE_IN_PROGRESS",
        "UPGRADE_BUSY",
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

#[cfg(test)]
mod network_and_request_tests {
    use super::*;

    #[test]
    fn writer_backpressure_bounds_frames_and_bytes_and_releases_failed_reservations() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let bytes = Arc::new(AtomicUsize::new(0));
        queue_write(&sender, bytes.clone(), vec![1, 2, 3], None).unwrap();
        assert_eq!(bytes.load(Ordering::Acquire), 3);
        assert_eq!(
            queue_write(&sender, bytes.clone(), vec![4, 5], None)
                .unwrap_err()
                .code,
            "RESOURCE_LIMIT"
        );
        assert_eq!(bytes.load(Ordering::Acquire), 3);
        drop(receiver.recv().unwrap());
        assert_eq!(bytes.load(Ordering::Acquire), 0);
        bytes.store(MAX_QUEUED_WRITE_BYTES, Ordering::Release);
        assert_eq!(
            queue_write(&sender, bytes.clone(), vec![1], None)
                .unwrap_err()
                .code,
            "RESOURCE_LIMIT"
        );
        assert_eq!(bytes.load(Ordering::Acquire), MAX_QUEUED_WRITE_BYTES);
        bytes.store(0, Ordering::Release);
        drop(receiver);
        assert_eq!(
            queue_write(&sender, bytes.clone(), vec![1], None)
                .unwrap_err()
                .code,
            "PLUGIN_CRASHED"
        );
        assert_eq!(bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn relayed_host_errors_keep_their_generic_codes_and_sanitize_diagnostics() {
        for code in [
            "CALL_DEPTH_EXCEEDED",
            "INTERACTION_UNAVAILABLE",
            "SECRET_NOT_FOUND",
            "MODEL_INVALID",
            "UPGRADE_IN_PROGRESS",
        ] {
            let mut error = plugin_error(code, "host failure\u{0001}");
            error.details = Some(json!({"private":"redacted"}));
            sanitize_plugin_error(&mut error);
            assert_eq!(error.code, code);
            assert_eq!(error.message, "host failure");
            assert!(error.details.is_none());
        }
    }

    #[test]
    fn completed_call_ids_can_be_reused_without_delivering_stale_replies_or_events() {
        let mut pending = PendingRequests::default();
        let (first_tx, first_rx) = mpsc::channel();
        let first = pending.insert("caller", first_tx).unwrap();
        let (duplicate_tx, _) = mpsc::channel();
        assert_eq!(
            pending.insert("caller", duplicate_tx).unwrap_err().code,
            "INVALID_REQUEST"
        );
        assert_eq!(pending.caller_id(&first).as_deref(), Some("caller"));
        pending.deliver(&first, Ok(json!("first")));
        assert_eq!(first_rx.recv().unwrap().unwrap(), json!("first"));
        let (second_tx, second_rx) = mpsc::channel();
        let second = pending.insert("caller", second_tx).unwrap();
        assert_ne!(first, second);
        assert_eq!(pending.wire_id("caller"), Some(second.clone()));
        assert!(pending.caller_id(&first).is_none());
        pending.deliver(&first, Ok(json!("late")));
        assert_eq!(second_rx.try_recv().unwrap_err(), mpsc::TryRecvError::Empty);
        pending.deliver(&second, Ok(json!("second")));
        assert_eq!(second_rx.recv().unwrap().unwrap(), json!("second"));
    }

    #[test]
    fn plugin_sessions_outlive_the_old_65536_request_limit_with_bounded_storage() {
        let mut pending = PendingRequests::default();
        for _ in 0..65_537 {
            let (sender, receiver) = mpsc::channel();
            let id = pending.insert("caller", sender).unwrap();
            pending.deliver(&id, Ok(Value::Null));
            assert!(receiver.recv().unwrap().is_ok());
        }
        assert!(pending.calls.is_empty());
    }

    #[test]
    fn browser_urls_accept_lan_ports_queries_and_fragments_and_reject_legacy_calls() {
        use crate::host_services::browser_url;
        let url = "http://192.168.1.20:8080/tools?q=example#section";
        assert_eq!(browser_url(&json!({"url": url})).unwrap(), url);
        assert!(browser_url(&json!({"target":"onnx"})).is_err());
        for url in [
            "file:///C:/test",
            "javascript:alert(1)",
            "https://user:password@example.com",
        ] {
            assert!(browser_url(&json!({"url":url})).is_err());
        }
    }
}
