//! Small public SDK for a plugin backend speaking Wonderland Plugin Protocol v1.
//!
//! A single reader demultiplexes Core calls from replies to `core.*` service calls;
//! each plugin call runs on its own worker so long jobs do not block cancellation or
//! progress events. `HostClient` writes all frames through one synchronized writer.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod failure;
pub use failure::PluginFailure;
mod account;
pub use account::{AccountSnapshot, AccountSummary, AccountStatus, GameRoleSummary};
mod request;
pub use request::{ActiveRequestGuard, RequestTracker};

const PROTOCOL: &str = "wonderland-plugin";
const VERSION: &str = "1.0.0";
const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;
const SERVICE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl PluginError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), details: None }
    }
}

#[derive(Clone)]
pub struct HostClient {
    writer: Arc<Mutex<BufWriter<std::io::Stdout>>>,
    pending: Arc<Mutex<HashMap<String, mpsc::SyncSender<Result<Value, PluginError>>>>>,
    next_id: Arc<AtomicU64>,
}

impl HostClient {
    fn new(writer: Arc<Mutex<BufWriter<std::io::Stdout>>>) -> Self {
        Self { writer, pending: Arc::new(Mutex::new(HashMap::new())), next_id: Arc::new(AtomicU64::new(1)) }
    }

    /// Make a capability-gated Core service request.
    pub fn call_core(&self, method: &str, params: Value) -> Result<Value, PluginError> {
        let id = format!("p-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = mpsc::sync_channel(1);
        self.pending.lock().map_err(|_| PluginError::new("INTERNAL", "Plugin service table is unavailable."))?
            .insert(id.clone(), sender);
        let request = json!({"protocol": PROTOCOL, "version": VERSION, "type": "request", "id": id, "method": method, "params": params});
        if let Err(error) = write_frame(&self.writer, &request) {
            self.pending.lock().ok().and_then(|mut p| p.remove(&id));
            return Err(PluginError::new("PLUGIN_CRASHED", format!("Cannot send Core service request: {error}")));
        }
        match receiver.recv_timeout(SERVICE_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.pending.lock().ok().and_then(|mut p| p.remove(&id));
                Err(PluginError::new("TIMEOUT", "Core service request timed out."))
            }
        }
    }

    pub fn emit(&self, topic: &str, request_id: Option<&str>, payload: Value) -> Result<(), PluginError> {
        let frame = json!({"protocol": PROTOCOL, "version": VERSION, "type": "event", "topic": topic, "requestId": request_id, "payload": payload});
        write_frame(&self.writer, &frame).map_err(|error| PluginError::new("PLUGIN_CRASHED", format!("Cannot emit plugin event: {error}")))
    }
}

/// Serve a protocol backend until Core closes stdin.
pub fn serve<F>(
    plugin_id: &'static str,
    plugin_version: &'static str,
    contract: &'static str,
    dispatch: F,
) -> Result<(), String>
where
    F: Fn(HostClient, String, Value, Option<String>) -> Result<Value, PluginError> + Send + Sync + 'static,
{
    let writer = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));
    let host = HostClient::new(writer.clone());
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
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

    let dispatch = Arc::new(dispatch);
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
                let id = frame.get("id").and_then(Value::as_str).ok_or("Core reply has no id")?.to_owned();
                let sender = host.pending.lock().map_err(|_| "Plugin service table is unavailable")?.remove(&id);
                if let Some(sender) = sender {
                    let result: Result<Value, PluginError> = if frame.get("type").and_then(Value::as_str) == Some("result") {
                        frame.get("result").cloned().ok_or_else(|| PluginError::new("INVALID_RESPONSE", "Core result has no value."))
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
                let id = frame.get("id").and_then(Value::as_str).ok_or("Core request has no id")?.to_owned();
                let method = frame.get("method").and_then(Value::as_str).ok_or("Core request has no method")?.to_owned();
                let params = frame.get("params").cloned().unwrap_or_else(|| json!({}));
                let request_id = Some(id.clone());
                let host = host.clone();
                let dispatch = dispatch.clone();
                let writer = writer.clone();
                thread::spawn(move || {
                    let response = match dispatch(host, method, params, request_id) {
                        Ok(result) => json!({"protocol": PROTOCOL, "version": VERSION, "type": "result", "id": id, "result": result}),
                        Err(error) => json!({"protocol": PROTOCOL, "version": VERSION, "type": "error", "id": id, "error": error}),
                    };
                    let _ = write_frame(&writer, &response);
                });
            }
            Some("cancel") => {
                if let Some(id) = frame.get("id").and_then(Value::as_str) {
                    let host = host.clone();
                    let dispatch = dispatch.clone();
                    let request_id = id.to_owned();
                    thread::spawn(move || {
                        let _ = dispatch(host, "__cancel".to_owned(), json!({ "requestId": request_id }), None);
                    });
                }
            }
            _ => return Err("Unexpected Core protocol message".to_owned()),
        }
    }
}

fn read_frame<R: BufRead>(input: &mut R) -> Result<Value, String> {
    let mut line = Vec::new();
    let count = (&mut *input).take(MAX_FRAME_BYTES + 1).read_until(b'\n', &mut line).map_err(|error| error.to_string())?;
    if count == 0 { return Err("end of stream".to_owned()); }
    if count as u64 > MAX_FRAME_BYTES || line.last() != Some(&b'\n') { return Err("frame exceeded the protocol size limit".to_owned()); }
    line.pop();
    if line.last() == Some(&b'\r') { line.pop(); }
    serde_json::from_slice(&line).map_err(|error| error.to_string())
}

fn write_frame(writer: &Arc<Mutex<BufWriter<std::io::Stdout>>>, frame: &Value) -> std::io::Result<()> {
    let mut writer = writer.lock().map_err(|_| std::io::Error::other("stdout lock poisoned"))?;
    serde_json::to_writer(&mut *writer, frame)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}
