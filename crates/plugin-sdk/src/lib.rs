//! Small public SDK for a plugin backend speaking Wonderland Plugin Protocol v1.
//!
//! A single reader demultiplexes Core calls from replies to `core.*` service calls;
//! each plugin call runs on its own worker so long jobs do not block cancellation or
//! progress events. `HostClient` writes all frames through one synchronized writer.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub use wonderland_plugin_protocol::{FileResult, LocalFileState, RemoteFileState};
use wonderland_plugin_protocol::{PROTOCOL_ID, PROTOCOL_VERSION, PluginCallParams};

mod failure;
mod host;
pub use failure::PluginFailure;
pub use host::*;
mod request;
pub use request::{ActiveRequestGuard, RequestTracker};
mod lifecycle;
pub use lifecycle::ActivityGuard;

// The SDK and Core consume the same protocol version source so wire checks cannot drift.
const PROTOCOL: &str = PROTOCOL_ID;
const VERSION: &str = PROTOCOL_VERSION;
const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;
const PLUGIN_CALL_REPLY_MARGIN: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl PluginError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
}

#[derive(Clone)]
pub struct HostClient {
    writer: Arc<Mutex<BufWriter<std::io::Stdout>>>,
    pending: PendingCalls,
    next_id: Arc<AtomicU64>,
    service_context: Option<String>,
    closed: Arc<AtomicBool>,
    lifecycle_cleanup: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    restore_updates: Arc<Mutex<()>>,
    initializing: Arc<AtomicBool>,
}

type PendingCalls = Arc<Mutex<HashMap<String, mpsc::SyncSender<Result<Value, PluginError>>>>>;

struct TransportGuard {
    pending: PendingCalls,
    closed: Arc<AtomicBool>,
}

impl Drop for TransportGuard {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut pending) = self.pending.lock() {
            for (_, reply) in pending.drain() {
                let _ = reply.send(Err(PluginError::new(
                    "PLUGIN_CRASHED",
                    "Core service transport closed before returning a result.",
                )));
            }
        }
    }
}

impl HostClient {
    fn new(writer: Arc<Mutex<BufWriter<std::io::Stdout>>>) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            service_context: None,
            closed: Arc::new(AtomicBool::new(false)),
            lifecycle_cleanup: Arc::new(Mutex::new(None)),
            restore_updates: Arc::new(Mutex::new(())),
            initializing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Make a Core service request. Ordinary host services are available by default.
    pub fn call_core(&self, method: &str, params: Value) -> Result<Value, PluginError> {
        self.call_core_with_deadline(method, params, None)
    }

    /// Set an explicit service budget; ordinary calls have no implicit waiting deadline.
    pub fn call_core_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, PluginError> {
        self.call_core_with_deadline(method, params, Some(timeout))
    }

    fn call_core_with_deadline(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, PluginError> {
        // A startup callback must not wait on another plugin's business dispatch:
        // that dispatch may itself be waiting for this callback to finish.
        if method == "core.plugins.call" && self.initializing.load(Ordering::Acquire) {
            return Err(PluginError::new(
                "STARTUP_DEPENDENCY_CALL",
                "Call plugin dependencies after the startup callback returns, from a method or background task.",
            ));
        }
        let timeout_ms = timeout.map(duration_millis).transpose()?;
        let deadline = timeout
            .map(|timeout| {
                Instant::now().checked_add(timeout).ok_or_else(|| {
                    PluginError::new("INVALID_INPUT", "Service timeout is not representable.")
                })
            })
            .transpose()?;
        let id = format!("p-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| PluginError::new("INTERNAL", "Plugin service table is unavailable."))?;
        if self.closed.load(Ordering::Acquire) {
            return Err(PluginError::new(
                "PLUGIN_CRASHED",
                "Core service transport has closed.",
            ));
        }
        pending.insert(id.clone(), sender);
        drop(pending);
        let mut request = json!({"protocol": PROTOCOL, "version": VERSION, "type": "request", "id": id, "method": method, "params": params});
        // Nested calls retain the invocation's device and cancellation context.
        if let Some(context) = &self.service_context {
            request["serviceContext"] = json!(context);
        }
        if let Some(timeout_ms) = timeout_ms {
            request["timeoutMs"] = json!(timeout_ms);
        }
        if let Err(error) =
            write_service_request(&self.writer, &mut request, deadline, &self.closed)
        {
            self.pending.lock().ok().and_then(|mut p| p.remove(&id));
            return Err(host::interrupted_service(method, error));
        }
        let received = match deadline {
            Some(deadline) => receiver
                .recv_timeout(
                    deadline
                        .checked_add(PLUGIN_CALL_REPLY_MARGIN)
                        .unwrap_or(deadline)
                        .saturating_duration_since(Instant::now()),
                )
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => "TIMEOUT",
                    mpsc::RecvTimeoutError::Disconnected => "PLUGIN_CRASHED",
                }),
            None => receiver.recv().map_err(|_| "PLUGIN_CRASHED"),
        };
        let result = match received {
            Ok(result) => result,
            Err(code) => {
                self.pending.lock().ok().and_then(|mut p| p.remove(&id));
                Err(PluginError::new(
                    code,
                    if code == "TIMEOUT" {
                        "Core service request exceeded its explicit time budget."
                    } else {
                        "Core service transport closed before returning a result."
                    },
                ))
            }
        };
        result.map_err(|error| host::interrupted_service(method, error))
    }

    /// Call a target plugin method using its actual contract deadline.
    /// Manifest dependencies are diagnostics, not a local version or method allowlist.
    pub fn call_plugin(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginError> {
        self.call_plugin_request(plugin_id, method, params, None)
    }

    /// Call a target plugin method with a deadline that can only shorten its contract deadline.
    pub fn call_plugin_with_timeout(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<Value, PluginError> {
        duration_millis(Duration::from_millis(timeout_ms))?;
        self.call_plugin_request(plugin_id, method, params, Some(timeout_ms))
    }

    fn call_plugin_request(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
        timeout_ms: Option<u64>,
    ) -> Result<Value, PluginError> {
        let request = PluginCallParams {
            plugin_id: plugin_id.to_owned(),
            method: method.to_owned(),
            params,
            timeout_ms,
        };
        let params = serde_json::to_value(request)
            .map_err(|_| PluginError::new("INTERNAL", "Cannot serialize a direct plugin call."))?;
        self.call_core_with_deadline(
            "core.plugins.call",
            params,
            timeout_ms.map(Duration::from_millis),
        )
    }

    pub fn emit(
        &self,
        topic: &str,
        request_id: Option<&str>,
        payload: Value,
    ) -> Result<(), PluginError> {
        let frame = json!({"protocol": PROTOCOL, "version": VERSION, "type": "event", "topic": topic, "requestId": request_id, "payload": payload});
        write_frame(&self.writer, &frame).map_err(|error| {
            PluginError::new(
                "PLUGIN_CRASHED",
                format!("Cannot emit plugin event: {error}"),
            )
        })
    }
}

fn duration_millis(timeout: Duration) -> Result<u64, PluginError> {
    let milliseconds = u64::try_from(timeout.as_millis())
        .ok()
        .filter(|milliseconds| *milliseconds > 0)
        .filter(|_| Instant::now().checked_add(timeout).is_some())
        .ok_or_else(|| {
            PluginError::new("INVALID_INPUT", "Service timeout is not representable.")
        })?;
    Ok(milliseconds)
}

/// Serve a protocol backend until Core closes stdin.
pub fn serve<F>(
    plugin_id: &'static str,
    plugin_version: &'static str,
    contract: &'static str,
    dispatch: F,
) -> Result<(), String>
where
    F: Fn(HostClient, String, Value, Option<String>) -> Result<Value, PluginError>
        + Send
        + Sync
        + 'static,
{
    serve_with_startup(plugin_id, plugin_version, contract, |_| {}, dispatch)
}

/// Serve a protocol backend and invoke `startup` after the Core handshake succeeds.
///
/// The startup callback receives a cloneable host client so plugins can launch
/// background work that continues while the Core process remains alive.
/// Bind services and register work here; synchronous cross-plugin business calls
/// are rejected until this callback returns, so initialization cannot form a cycle.
pub fn serve_with_startup<F, S>(
    plugin_id: &'static str,
    plugin_version: &'static str,
    contract: &'static str,
    startup: S,
    dispatch: F,
) -> Result<(), String>
where
    F: Fn(HostClient, String, Value, Option<String>) -> Result<Value, PluginError>
        + Send
        + Sync
        + 'static,
    S: FnOnce(HostClient),
{
    let writer = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));
    let host = HostClient::new(writer.clone());
    host.initializing.store(true, Ordering::Release);
    let _transport_guard = TransportGuard {
        pending: host.pending.clone(),
        closed: host.closed.clone(),
    };
    let mut input = BufReader::new(std::io::stdin());
    let hello = read_frame(&mut input)?;
    if hello.get("protocol").and_then(Value::as_str) != Some(PROTOCOL)
        || hello.get("version").and_then(Value::as_str) != Some(VERSION)
        || hello.get("type").and_then(Value::as_str) != Some("hello")
        || hello.get("role").and_then(Value::as_str) != Some("host")
        || hello.get("pluginId").and_then(Value::as_str) != Some(plugin_id)
    {
        return Err("Host handshake identity or protocol does not match".to_owned());
    }
    let hash = sha256_hex(contract.as_bytes());
    write_frame(&writer, &json!({"protocol": PROTOCOL, "version": VERSION, "type": "hello", "role": "plugin", "pluginId": plugin_id, "pluginVersion": plugin_version, "contractSha256": hash}))
        .map_err(|error| format!("Cannot write plugin hello: {error}"))?;

    // The receive thread starts before initialization, so a startup host call
    // can receive its reply. The main serve thread still owns initialization.
    let startup_result = Arc::new((Mutex::new(None::<Result<(), String>>), Condvar::new()));
    let reader_host = host.clone();
    let reader_writer = writer.clone();
    let initialized = startup_result.clone();
    let dispatch = Arc::new(dispatch);
    let reader = thread::Builder::new()
        .name("plugin-receiver".into())
        .spawn(move || {
            let _closed = TransportGuard {
                pending: reader_host.pending.clone(),
                closed: reader_host.closed.clone(),
            };
            serve_messages(input, reader_host, reader_writer, dispatch, initialized)
        })
        .map_err(|e| format!("Cannot start plugin receiver: {e}"))?;
    let startup_host = host.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| startup(startup_host)))
        .map_err(|panic| {
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_else(|| "Plugin startup callback failed.".into())
        });
    host.initializing.store(false, Ordering::Release);
    *startup_result.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
    startup_result.1.notify_all();
    result?;
    reader
        .join()
        .map_err(|_| "Plugin receiver stopped unexpectedly.".to_owned())?
}

type StartupResult = Arc<(Mutex<Option<Result<(), String>>>, Condvar)>;

fn serve_messages<F>(
    mut input: BufReader<std::io::Stdin>,
    host: HostClient,
    writer: Arc<Mutex<BufWriter<std::io::Stdout>>>,
    dispatch: Arc<F>,
    startup_result: StartupResult,
) -> Result<(), String>
where
    F: Fn(HostClient, String, Value, Option<String>) -> Result<Value, PluginError>
        + Send
        + Sync
        + 'static,
{
    let dispatch_cancellations = Arc::new(Mutex::new(HashMap::<String, Arc<AtomicBool>>::new()));
    loop {
        let frame = match read_frame(&mut input) {
            Ok(frame) => frame,
            Err(error) if error == "end of stream" => return Ok(()),
            Err(error) => return Err(format!("Cannot read Core frame: {error}")),
        };
        if frame.get("protocol").and_then(Value::as_str) != Some(PROTOCOL)
            || frame.get("version").and_then(Value::as_str) != Some(VERSION)
        {
            return Err("Core frame protocol version does not match".to_owned());
        }
        match frame.get("type").and_then(Value::as_str) {
            Some("result") | Some("error") => {
                let id = frame
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Core reply has no id")?
                    .to_owned();
                let sender = host
                    .pending
                    .lock()
                    .map_err(|_| "Plugin service table is unavailable")?
                    .remove(&id);
                if let Some(sender) = sender {
                    let result: Result<Value, PluginError> =
                        if frame.get("type").and_then(Value::as_str) == Some("result") {
                            frame.get("result").cloned().ok_or_else(|| {
                                PluginError::new("INVALID_RESPONSE", "Core result has no value.")
                            })
                        } else {
                            match serde_json::from_value::<PluginError>(
                                frame.get("error").cloned().unwrap_or(Value::Null),
                            ) {
                                Ok(error) => Err(error),
                                Err(_) => Err(PluginError::new(
                                    "INVALID_RESPONSE",
                                    "Core error frame is malformed.",
                                )),
                            }
                        };
                    let _ = sender.send(result);
                }
            }
            Some("request") => {
                let id = frame
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Core request has no id")?
                    .to_owned();
                let method = frame
                    .get("method")
                    .and_then(Value::as_str)
                    .ok_or("Core request has no method")?
                    .to_owned();
                let params = frame.get("params").cloned().unwrap_or_else(|| json!({}));
                let request_id = Some(id.clone());
                let mut host = host.clone();
                host.service_context = frame
                    .get("serviceContext")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let dispatch = dispatch.clone();
                let writer = writer.clone();
                let initialized = startup_result.clone();
                let cancellations = dispatch_cancellations.clone();
                let cancelled = Arc::new(AtomicBool::new(false));
                cancellations
                    .lock()
                    .unwrap()
                    .insert(id.clone(), cancelled.clone());
                thread::spawn(move || {
                    let ready = (|| {
                        let mut result = initialized.0.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if cancelled.load(Ordering::Acquire)
                                || host.closed.load(Ordering::Acquire)
                            {
                                return Err(PluginError::new(
                                    "CANCELLED",
                                    "Call ended before plugin initialization completed.",
                                ));
                            }
                            if let Some(result) = &*result {
                                return result.clone().map_err(|message| {
                                    PluginError::new("PLUGIN_START_FAILED", message)
                                });
                            }
                            result = initialized
                                .1
                                .wait_timeout(result, Duration::from_millis(20))
                                .unwrap_or_else(|e| e.into_inner())
                                .0;
                        }
                    })();
                    let response = match ready
                        .and_then(|()| dispatch(host, method, params, request_id))
                    {
                        Ok(result) => {
                            json!({"protocol": PROTOCOL, "version": VERSION, "type": "result", "id": id, "result": result})
                        }
                        Err(error) => {
                            json!({"protocol": PROTOCOL, "version": VERSION, "type": "error", "id": id, "error": error})
                        }
                    };
                    cancellations.lock().unwrap().remove(&id);
                    let _ = write_frame(&writer, &response);
                });
            }
            Some("cancel") => {
                if let Some(id) = frame.get("id").and_then(Value::as_str) {
                    if let Some(cancelled) = dispatch_cancellations.lock().unwrap().get(id) {
                        cancelled.store(true, Ordering::Release);
                    }
                    let host = host.clone();
                    let dispatch = dispatch.clone();
                    let request_id = id.to_owned();
                    thread::spawn(move || {
                        let _ = dispatch(
                            host,
                            "__cancel".to_owned(),
                            json!({ "requestId": request_id }),
                            None,
                        );
                    });
                }
            }
            _ => return Err("Unexpected Core protocol message".to_owned()),
        }
    }
}

fn read_frame<R: BufRead>(input: &mut R) -> Result<Value, String> {
    let mut line = Vec::new();
    let count = (&mut *input)
        .take(MAX_FRAME_BYTES + 1)
        .read_until(b'\n', &mut line)
        .map_err(|error| error.to_string())?;
    if count == 0 {
        return Err("end of stream".to_owned());
    }
    if count as u64 > MAX_FRAME_BYTES || line.last() != Some(&b'\n') {
        return Err("frame exceeded the protocol size limit".to_owned());
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    serde_json::from_slice(&line).map_err(|error| error.to_string())
}

fn write_service_request(
    writer: &Arc<Mutex<BufWriter<std::io::Stdout>>>,
    request: &mut Value,
    deadline: Option<Instant>,
    closed: &AtomicBool,
) -> Result<(), PluginError> {
    let mut writer = loop {
        if closed.load(Ordering::Acquire) {
            return Err(PluginError::new(
                "PLUGIN_CRASHED",
                "Core service transport has closed.",
            ));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(PluginError::new(
                "TIMEOUT",
                "Core service budget expired before dispatch.",
            ));
        }
        match writer.try_lock() {
            Ok(writer) => break writer,
            Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(PluginError::new(
                    "PLUGIN_CRASHED",
                    "Core service writer is unavailable.",
                ));
            }
        }
    };
    if let Some(deadline) = deadline {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|value| !value.is_zero())
            .ok_or_else(|| {
                PluginError::new("TIMEOUT", "Core service budget expired before dispatch.")
            })?;
        let milliseconds =
            u64::try_from(remaining.as_nanos().div_ceil(1_000_000)).map_err(|_| {
                PluginError::new(
                    "INVALID_INPUT",
                    "Remaining service budget is not representable.",
                )
            })?;
        request["timeoutMs"] = json!(milliseconds);
    }
    serde_json::to_writer(&mut *writer, request)
        .map_err(std::io::Error::other)
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
        .map_err(|error| {
            PluginError::new(
                "PLUGIN_CRASHED",
                format!("Cannot send Core service request: {error}"),
            )
        })
}

fn write_frame(
    writer: &Arc<Mutex<BufWriter<std::io::Stdout>>>,
    frame: &Value,
) -> std::io::Result<()> {
    let mut writer = writer
        .lock()
        .map_err(|_| std::io::Error::other("stdout lock poisoned"))?;
    serde_json::to_writer(&mut *writer, frame)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn explicit_budget_is_positive_and_not_capped_at_a_day() {
        assert!(duration_millis(Duration::ZERO).is_err());
        assert_eq!(
            duration_millis(Duration::from_secs(172_800)).unwrap(),
            172_800_000
        );
    }

    #[test]
    fn closed_transport_finishes_unbounded_service_waits() {
        let pending: PendingCalls = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = mpsc::sync_channel(1);
        pending.lock().unwrap().insert("waiting".into(), sender);
        let closed = Arc::new(AtomicBool::new(false));
        let guard = TransportGuard {
            pending: pending.clone(),
            closed: closed.clone(),
        };
        drop(guard);
        assert_eq!(receiver.recv().unwrap().unwrap_err().code, "PLUGIN_CRASHED");
        assert!(pending.lock().unwrap().is_empty());
        assert!(closed.load(Ordering::Acquire));
    }

    #[test]
    fn explicit_budget_includes_the_wait_for_the_writer() {
        let writer = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));
        let _held = writer.lock().unwrap();
        let queued = writer.clone();
        let worker = thread::spawn(move || {
            write_service_request(
                &queued,
                &mut json!({}),
                Some(Instant::now() + Duration::from_millis(5)),
                &AtomicBool::new(false),
            )
        });
        assert_eq!(worker.join().unwrap().unwrap_err().code, "TIMEOUT");
    }

    #[test]
    fn transport_close_releases_an_unbounded_writer_wait() {
        let writer = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));
        let _held = writer.lock().unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let worker_writer = writer.clone();
        let worker_closed = closed.clone();
        let worker = thread::spawn(move || {
            write_service_request(&worker_writer, &mut json!({}), None, &worker_closed)
        });
        closed.store(true, Ordering::Release);
        assert_eq!(worker.join().unwrap().unwrap_err().code, "PLUGIN_CRASHED");
    }
}
