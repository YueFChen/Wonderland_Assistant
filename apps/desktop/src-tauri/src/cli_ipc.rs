//! Authenticated, loopback-only IPC used by `wla` to reach the running desktop Core.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State, WebviewWindow};
use wonderland_plugin_protocol::PluginError;

use crate::host_services::HostInvocation;
use crate::plugin_manager::PluginManager;

const ENDPOINT_FILE: &str = "wonderland-assistant-cli-v1.json";
const PROTOCOL_VERSION: u32 = 1;
const SENSITIVE_PAYLOAD_VERSION: u32 = 1;
const CANCELLATION_VERSION: u32 = 1;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_DISTRIBUTION_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const WAKE_TIMEOUT: Duration = Duration::from_secs(2);
const CLI_UI_EVENT: &str = "cli:ui-request";
const CLI_UI_CANCEL_EVENT: &str = "cli:ui-cancel";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl CliError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn from_plugin(error: PluginError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            details: error.details,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Endpoint {
    protocol_version: u32,
    port: u16,
    token: String,
    data_dir: PathBuf,
    process_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sensitive_payload_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancellation_version: Option<u32>,
}

/// A closed payload type, accepted only by `plugins distribution set-code --stdin`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum SensitivePayload {
    DistributionAccessCode(String),
}

impl std::fmt::Debug for SensitivePayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DistributionAccessCode([REDACTED])")
    }
}

impl SensitivePayload {
    pub(crate) fn access_code(&self) -> &str {
        match self {
            Self::DistributionAccessCode(code) => code,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    protocol_version: u32,
    request_id: String,
    token: String,
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sensitive_payload: Option<SensitivePayload>,
    #[serde(default, skip_serializing_if = "is_false")]
    cancel_on_disconnect: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Request")
            .field("protocol_version", &self.protocol_version)
            .field("request_id", &self.request_id)
            .field("token", &"[REDACTED]")
            .field("args", &self.args)
            .field("sensitive_payload", &self.sensitive_payload)
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    protocol_version: u32,
    request_id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<CliError>,
}

impl Response {
    fn success(request_id: String, data: Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    fn failure(request_id: String, error: CliError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            ok: false,
            data: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiRequest {
    request_id: String,
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
}

#[derive(Default)]
pub struct CliUiPending {
    requests: Mutex<HashMap<String, mpsc::Sender<Result<Value, CliError>>>>,
    queued_events: Mutex<Vec<UiRequest>>,
    ready: AtomicBool,
    closed: AtomicBool,
}

impl CliUiPending {
    fn request(
        &self,
        app: &AppHandle,
        args: Vec<String>,
        parent: &HostInvocation,
    ) -> Result<Value, CliError> {
        let timeout_ms = command_timeout_ms(&args)?;
        let invocation = parent
            .clone()
            .with_timeout_ms(timeout_ms)
            .map_err(CliError::from_plugin)?;
        let request_id = random_hex(16)?;
        let (tx, rx) = mpsc::channel();
        self.requests
            .lock()
            .map_err(|_| CliError::new("INTERNAL", "The CLI request queue is unavailable."))?
            .insert(request_id.clone(), tx);
        let event = UiRequest {
            request_id: request_id.clone(),
            args,
            timeout_ms,
        };
        let mut queued_events = self
            .queued_events
            .lock()
            .map_err(|_| CliError::new("INTERNAL", "The CLI request queue is unavailable."))?;
        if self.ready.load(Ordering::Acquire) {
            drop(queued_events);
            if let Err(error) = app.emit(CLI_UI_EVENT, event) {
                self.remove(&request_id);
                return Err(CliError::new(
                    "DESKTOP_UI_UNAVAILABLE",
                    format!("The desktop UI did not accept the request: {error}"),
                ));
            }
        } else {
            queued_events.push(event);
            drop(queued_events);
        }
        let response = loop {
            if self.closed.load(Ordering::Acquire) {
                break Err(CliError::new(
                    "UI_RESPONSE_LOST",
                    "The desktop Core closed before confirming the request.",
                ));
            }
            if let Err(error) = invocation.check() {
                break Err(CliError::from_plugin(error));
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(CliError::new(
                        "UI_RESPONSE_LOST",
                        "The desktop UI closed before confirming the request.",
                    ));
                }
            }
        };
        self.remove(&request_id);
        if response.is_err() {
            let _ = app.emit(
                CLI_UI_CANCEL_EVENT,
                serde_json::json!({"requestId":request_id}),
            );
        }
        response
    }

    fn remove(&self, request_id: &str) {
        if let Ok(mut requests) = self.requests.lock() {
            requests.remove(request_id);
        }
        if let Ok(mut queued) = self.queued_events.lock() {
            queued.retain(|event| event.request_id != request_id);
        }
    }

    fn complete(&self, request_id: &str, response: Result<Value, CliError>) -> Result<(), String> {
        let sender = self
            .requests
            .lock()
            .map_err(|_| "The CLI request queue is unavailable.".to_owned())?
            .remove(request_id)
            .ok_or_else(|| "The CLI request has expired or does not exist.".to_owned())?;
        sender
            .send(response)
            .map_err(|_| "The CLI request is no longer waiting for a response.".to_owned())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut requests) = self.requests.lock() {
            requests.clear();
        }
        if let Ok(mut queued) = self.queued_events.lock() {
            queued.clear();
        }
    }

    fn mark_ready(&self, app: &AppHandle) -> Result<(), String> {
        let events = {
            let mut queued = self
                .queued_events
                .lock()
                .map_err(|_| "The CLI request queue is unavailable.".to_owned())?;
            self.ready.store(true, Ordering::Release);
            std::mem::take(&mut *queued)
        };
        for event in events {
            if let Err(error) = app.emit(CLI_UI_EVENT, event.clone()) {
                self.complete(
                    &event.request_id,
                    Err(CliError::new(
                        "DESKTOP_UI_UNAVAILABLE",
                        format!("The desktop UI did not accept the request: {error}"),
                    )),
                )?;
            }
        }
        Ok(())
    }
}

pub struct CliIpcServer {
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    endpoint_path: PathBuf,
    pending: Arc<CliUiPending>,
}

impl CliIpcServer {
    pub fn start(
        app: &AppHandle,
        manager: PluginManager,
        data_dir: &Path,
        pending: Arc<CliUiPending>,
    ) -> Result<Self, CliError> {
        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .map_err(|error| {
                CliError::new(
                    "IPC_START_FAILED",
                    format!("Cannot start the local CLI service: {error}"),
                )
            })?;
        listener.set_nonblocking(true).map_err(|error| {
            CliError::new(
                "IPC_START_FAILED",
                format!("Cannot configure the local CLI service: {error}"),
            )
        })?;
        prevent_socket_inheritance(&listener)?;
        let port = listener
            .local_addr()
            .map_err(|error| CliError::new("IPC_START_FAILED", error.to_string()))?
            .port();
        let endpoint_path = endpoint_path();
        let endpoint = Endpoint {
            protocol_version: PROTOCOL_VERSION,
            port,
            token: random_hex(32)?,
            data_dir: data_dir.to_path_buf(),
            process_id: std::process::id(),
            sensitive_payload_version: Some(SENSITIVE_PAYLOAD_VERSION),
            cancellation_version: Some(CANCELLATION_VERSION),
        };
        write_endpoint(&endpoint_path, &endpoint)?;
        let expected_token = endpoint.token.clone();

        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let app = app.clone();
        let worker_pending = pending.clone();
        let worker = thread::Builder::new()
            .name("wonderland-cli-ipc".to_owned())
            .spawn(move || {
                while !thread_shutdown.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if prevent_socket_inheritance(&stream).is_err() {
                                continue;
                            }
                            let app = app.clone();
                            let manager = manager.clone();
                            let pending = Arc::clone(&worker_pending);
                            let expected_token = expected_token.clone();
                            thread::spawn(move || {
                                serve_client(stream, &app, &manager, &pending, &expected_token)
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(40));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(100)),
                    }
                }
            })
            .map_err(|error| {
                let _ = fs::remove_file(&endpoint_path);
                CliError::new(
                    "IPC_START_FAILED",
                    format!("Cannot start the local CLI worker: {error}"),
                )
            })?;
        Ok(Self {
            shutdown,
            worker: Some(worker),
            endpoint_path,
            pending,
        })
    }
}

impl Drop for CliIpcServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.pending.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.endpoint_path);
    }
}

pub struct SingleInstanceGuard {
    #[cfg(windows)]
    handle: usize,
}

impl SingleInstanceGuard {
    pub fn acquire(data_dir: &Path) -> Result<Self, CliError> {
        #[cfg(windows)]
        {
            use std::ptr::null;
            use windows_sys::Win32::Foundation::{
                CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE,
            };
            use windows_sys::Win32::System::Threading::CreateMutexW;

            let identity = fs::canonicalize(data_dir)
                .unwrap_or_else(|_| data_dir.to_path_buf())
                .to_string_lossy()
                .to_lowercase();
            let digest = hex::encode(Sha256::digest(identity.as_bytes()));
            let name: Vec<u16> = format!(r"Local\WonderlandAssistant.Core.{digest}")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `name` is NUL-terminated and lives through the call.
            let handle: HANDLE = unsafe { CreateMutexW(null(), 1, name.as_ptr()) };
            if handle.is_null() {
                return Err(CliError::new(
                    "INSTANCE_LOCK_FAILED",
                    "Core could not reserve its data directory.",
                ));
            }
            // SAFETY: GetLastError reads the calling thread's last Win32 error.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                // SAFETY: `handle` was returned by CreateMutexW.
                unsafe { CloseHandle(handle) };
                return Err(CliError::new(
                    "CORE_BUSY",
                    "Another Core process is using this data directory.",
                ));
            }
            Ok(Self {
                handle: handle as usize,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = data_dir;
            Ok(Self {})
        }
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
            use windows_sys::Win32::System::Threading::ReleaseMutex;
            // SAFETY: this process created and owns the mutex handle.
            unsafe {
                ReleaseMutex(self.handle as HANDLE);
                CloseHandle(self.handle as HANDLE);
            }
        }
    }
}

pub fn try_send_to_desktop(
    args: &[String],
    requested_data_dir: Option<&Path>,
) -> Result<Option<Response>, CliError> {
    try_send_to_desktop_with_sensitive(args, requested_data_dir, None)
}

pub(crate) fn try_send_to_desktop_with_sensitive(
    args: &[String],
    requested_data_dir: Option<&Path>,
    sensitive_payload: Option<&SensitivePayload>,
) -> Result<Option<Response>, CliError> {
    let timeout = if crate::plugin_distribution_cli::is_network_command(args) {
        Some(crate::plugin_distribution_cli::COMMAND_TIMEOUT + Duration::from_secs(5))
    } else {
        command_timeout_ms(args)?.map(|milliseconds| {
            Duration::from_millis(milliseconds).saturating_add(Duration::from_secs(5))
        })
    };
    try_send_to_desktop_at(
        &endpoint_path(),
        args,
        requested_data_dir,
        timeout,
        sensitive_payload,
    )
}

fn try_send_to_desktop_with_timeout(
    args: &[String],
    requested_data_dir: Option<&Path>,
    response_timeout: Duration,
) -> Result<Option<Response>, CliError> {
    try_send_to_desktop_at(
        &endpoint_path(),
        args,
        requested_data_dir,
        Some(response_timeout),
        None,
    )
}

fn try_send_to_desktop_at(
    endpoint_path: &Path,
    args: &[String],
    requested_data_dir: Option<&Path>,
    response_timeout: Option<Duration>,
    sensitive_payload: Option<&SensitivePayload>,
) -> Result<Option<Response>, CliError> {
    if let Some(payload) = sensitive_payload {
        validate_sensitive_payload(args, Some(payload))?;
    }
    let contents = match fs::read(endpoint_path) {
        Ok(contents) => contents,
        Err(_) => return Ok(None),
    };
    let endpoint: Endpoint = match serde_json::from_slice::<Endpoint>(&contents) {
        Ok(endpoint) if endpoint.protocol_version == PROTOCOL_VERSION => endpoint,
        _ => {
            let _ = fs::remove_file(endpoint_path);
            return Ok(None);
        }
    };
    if requested_data_dir.is_some_and(|path| !same_path(path, &endpoint.data_dir)) {
        return Ok(None);
    }
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), endpoint.port);
    let mut stream = match TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    prevent_socket_inheritance(&stream)?;
    if sensitive_payload.is_some()
        && endpoint.sensitive_payload_version != Some(SENSITIVE_PAYLOAD_VERSION)
    {
        return Err(CliError::new(
            "IPC_PROTOCOL_MISMATCH",
            "The running Core does not support secure access-code input. Upgrade or close that desktop Core before retrying.",
        ));
    }
    stream
        .set_read_timeout(response_timeout)
        .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
    let request_id = random_hex(16)?;
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.clone(),
        token: endpoint.token,
        args: args.to_vec(),
        sensitive_payload: sensitive_payload.cloned(),
        cancel_on_disconnect: endpoint.cancellation_version == Some(CANCELLATION_VERSION),
    };
    let mut bytes = serde_json::to_vec(&request)
        .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
    if !request.cancel_on_disconnect {
        stream
            .shutdown(Shutdown::Write)
            .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
    }
    let max_bytes = if crate::plugin_distribution_cli::is_network_command(args) {
        MAX_DISTRIBUTION_RESPONSE_BYTES
    } else {
        MAX_MESSAGE_BYTES
    };
    let response = read_response(&mut stream, max_bytes, response_timeout).map_err(|error| {
        if matches!(error.code.as_str(), "IPC_READ_FAILED" | "IPC_PROTOCOL_ERROR") && crate::plugin_distribution_cli::is_network_command(args) {
            CliError::new("IPC_RESULT_UNKNOWN", "The running Core did not return a complete distribution result. Check plugin state before retrying; an installation may still complete.")
        } else { error }
    })?;
    if response.protocol_version != PROTOCOL_VERSION || response.request_id != request_id {
        return Err(CliError::new(
            "IPC_PROTOCOL_MISMATCH",
            "The running Core returned an incompatible CLI response.",
        ));
    }
    Ok(Some(response))
}

pub fn try_wake_desktop(data_dir: &Path) -> Result<Option<bool>, CliError> {
    let args = ["app".to_owned(), "wake".to_owned()];
    match try_send_to_desktop_with_timeout(&args, Some(data_dir), WAKE_TIMEOUT)? {
        Some(response) => Ok(Some(response.ok)),
        None => Ok(None),
    }
}

pub fn try_wake_desktop_legacy(data_dir: &Path) -> Result<Option<bool>, CliError> {
    let args = ["app".to_owned(), "open".to_owned()];
    match try_send_to_desktop_with_timeout(&args, Some(data_dir), WAKE_TIMEOUT)? {
        Some(response) => Ok(Some(response.ok)),
        None => Ok(None),
    }
}

pub fn print_response(response: Response) -> i32 {
    let rendered = serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_owned());
    if response.ok {
        println!("{rendered}");
        0
    } else {
        eprintln!("{rendered}");
        response
            .error
            .as_ref()
            .map_or(1, |error| exit_code_for(&error.code))
    }
}

pub fn print_success(data: Value) {
    let response = Response::success(String::new(), data);
    println!(
        "{}",
        serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_owned())
    );
}

pub fn print_error(error: CliError) -> i32 {
    print_response(Response::failure(String::new(), error))
}

pub fn launch_desktop() -> Result<(), CliError> {
    let current_executable = std::env::current_exe()
        .map_err(|error| CliError::new("APP_NOT_FOUND", error.to_string()))?;
    let mut candidates = Vec::new();
    if let Some(parent) = current_executable.parent() {
        candidates.push(parent.join("Wonderland Assistant.exe"));
        candidates.push(parent.join("wonderland-desktop.exe"));
        if let Some(resources_parent) = parent.parent() {
            candidates.push(resources_parent.join("Wonderland Assistant.exe"));
            candidates.push(resources_parent.join("wonderland-desktop.exe"));
            if let Some(install_root) = resources_parent.parent() {
                candidates.push(install_root.join("Wonderland Assistant.exe"));
                candidates.push(install_root.join("wonderland-desktop.exe"));
            }
        }
    }
    let executable = candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            CliError::new(
                "APP_NOT_FOUND",
                "The desktop Core executable was not found next to the CLI.",
            )
        })?;
    std::process::Command::new(executable)
        .spawn()
        .map(|_| ())
        .map_err(|error| CliError::new("APP_START_FAILED", error.to_string()))
}

pub fn exit_code_for(error_code: &str) -> i32 {
    match error_code {
        "INVALID_REQUEST" | "CONFIRMATION_REQUIRED" => 2,
        "CORE_BUSY" => 3,
        _ => 1,
    }
}

fn command_timeout_ms(args: &[String]) -> Result<Option<u64>, CliError> {
    let options_start = if args.first().is_some_and(|value| value == "plugins")
        && args.get(1).is_some_and(|value| value == "call")
    {
        Some(4)
    } else if args.first().is_some_and(|value| value == "ui")
        && args.get(1).is_some_and(|value| value == "command")
        && args.get(2).is_some_and(|value| value == "run")
    {
        Some(5)
    } else {
        None
    };
    let Some(mut index) = options_start else {
        return Ok(Some(REQUEST_TIMEOUT.as_millis() as u64));
    };
    let mut timeout = None;
    while index < args.len() {
        if args[index] == "--input-json" {
            index += 2;
            continue;
        }
        if args[index] == "--timeout-ms" {
            if timeout.is_some() {
                return Err(CliError::new(
                    "INVALID_REQUEST",
                    "--timeout-ms may be specified once.",
                ));
            }
            index += 1;
            let milliseconds = args
                .get(index)
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .filter(|value| {
                    Instant::now()
                        .checked_add(Duration::from_millis(*value))
                        .is_some()
                })
                .ok_or_else(|| {
                    CliError::new(
                        "INVALID_REQUEST",
                        "--timeout-ms must be a representable positive integer.",
                    )
                })?;
            timeout = Some(milliseconds);
        }
        index += 1;
    }
    Ok(timeout)
}

struct DisconnectMonitor {
    finished: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl DisconnectMonitor {
    fn start(stream: &TcpStream, invocation: HostInvocation) -> Result<Self, CliError> {
        let stream = stream
            .try_clone()
            .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
        prevent_socket_inheritance(&stream)?;
        stream
            .set_nonblocking(false)
            .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let worker = thread::Builder::new()
            .name("cli-disconnect".into())
            .spawn(move || {
                let mut byte = [0_u8; 1];
                while !done.load(Ordering::Acquire) {
                    match stream.peek(&mut byte) {
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::Interrupted
                            ) =>
                        {
                            continue;
                        }
                        // New clients keep their write half open while waiting. EOF is a real
                        // cancellation; extra input after the closed request is invalid too.
                        Ok(_) | Err(_) => {
                            invocation.cancel();
                            break;
                        }
                    }
                }
            })
            .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
        Ok(Self {
            finished,
            worker: Some(worker),
        })
    }
}

impl Drop for DisconnectMonitor {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve_client(
    mut stream: TcpStream,
    app: &AppHandle,
    manager: &PluginManager,
    pending: &CliUiPending,
    expected_token: &str,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let response = Response::failure(String::new(), error);
            let _ = write_response(&mut stream, &response);
            return;
        }
    };
    let response = if request.protocol_version != PROTOCOL_VERSION {
        Response::failure(
            request.request_id,
            CliError::new(
                "IPC_PROTOCOL_MISMATCH",
                "The CLI and Core use different IPC protocol versions.",
            ),
        )
    } else if !token_matches(request.token.as_bytes(), expected_token.as_bytes()) {
        Response::failure(
            request.request_id,
            CliError::new(
                "IPC_AUTH_FAILED",
                "The CLI request could not be authenticated.",
            ),
        )
    } else if request.args.is_empty() {
        Response::failure(
            request.request_id,
            CliError::new("INVALID_REQUEST", "A CLI command is required."),
        )
    } else {
        let invocation = match command_timeout_ms(&request.args).and_then(|timeout| {
            HostInvocation::for_call(None, timeout, None).map_err(CliError::from_plugin)
        }) {
            Ok(invocation) => invocation,
            Err(error) => {
                let _ = write_response(&mut stream, &Response::failure(request.request_id, error));
                return;
            }
        };
        let _disconnect = if request.cancel_on_disconnect {
            match DisconnectMonitor::start(&stream, invocation.clone()) {
                Ok(monitor) => Some(monitor),
                Err(error) => {
                    let _ =
                        write_response(&mut stream, &Response::failure(request.request_id, error));
                    return;
                }
            }
        } else {
            None
        };
        let result = validate_sensitive_payload(&request.args, request.sensitive_payload.as_ref())
            .and_then(|()| {
                dispatch(
                    app,
                    manager,
                    pending,
                    &request.args,
                    request.sensitive_payload.as_ref(),
                    &invocation,
                )
            });
        match result {
            Ok(data) => Response::success(request.request_id, data),
            Err(error) => Response::failure(request.request_id, error),
        }
    };
    let _ = write_response(&mut stream, &response);
}

fn dispatch(
    app: &AppHandle,
    manager: &PluginManager,
    pending: &CliUiPending,
    args: &[String],
    sensitive_payload: Option<&SensitivePayload>,
    invocation: &HostInvocation,
) -> Result<Value, CliError> {
    if let Some(result) = crate::plugin_distribution_cli::run(
        manager,
        args,
        sensitive_payload.map(SensitivePayload::access_code),
    ) {
        return result.map_err(CliError::from_plugin);
    }
    if args[0] == "web" {
        return crate::web_access::run_cli(app, &args[1..]);
    }
    if args[0] == "app" && args.get(1).is_some_and(|arg| arg == "wake") && args.len() == 2 {
        crate::tray::show_main_window(app)
            .map_err(|error| CliError::new("WINDOW_UNAVAILABLE", error))?;
        return Ok(serde_json::json!({
            "desktopRunning": true,
            "windowShown": true,
        }));
    }
    if args[0] == "app" && args.get(1).is_some_and(|arg| arg == "open") {
        crate::tray::show_main_window(app)
            .map_err(|error| CliError::new("WINDOW_UNAVAILABLE", error))?;
        return pending.request(app, args.to_vec(), invocation);
    }
    if args.len() == 2 && args[0] == "app" && args[1] == "activate" {
        crate::tray::show_main_window(app)
            .map_err(|error| CliError::new("WINDOW_UNAVAILABLE", error))?;
        return Ok(serde_json::json!({ "desktopRunning": true, "windowShown": true }));
    }
    if args[0] == "ui"
        && args.get(1).is_some_and(|arg| arg == "command")
        && args.get(2).is_some_and(|arg| arg == "list")
    {
        let arguments = args.iter().cloned().map(Into::into).collect::<Vec<_>>();
        return crate::plugin_manager::run_cli_command(manager, &arguments, true)
            .map_err(CliError::from_plugin);
    }
    if args[0] == "ui"
        && args.get(1).is_some_and(|arg| arg == "command")
        && args.get(2).is_some_and(|arg| arg == "run")
    {
        crate::plugin_manager::validate_cli_ui_command(manager, args)
            .map_err(CliError::from_plugin)?;
    }
    if args[0] == "app" || args[0] == "ui" {
        return pending.request(app, args.to_vec(), invocation);
    }
    let arguments = args.iter().cloned().map(Into::into).collect::<Vec<_>>();
    crate::plugin_manager::run_cli_command_with_invocation(
        manager,
        &arguments,
        true,
        Some(invocation),
    )
    .map_err(CliError::from_plugin)
}

#[tauri::command]
pub fn cli_ui_ack(
    window: WebviewWindow,
    pending: State<'_, Arc<CliUiPending>>,
    request_id: String,
    result: Value,
    error: Option<CliError>,
) -> Result<(), String> {
    crate::commands::ensure_main_window(&window).map_err(|error| error.to_string())?;
    pending.complete(
        &request_id,
        match error {
            Some(error) => Err(error),
            None => Ok(result),
        },
    )
}

#[tauri::command]
pub fn cli_ui_ready(
    window: WebviewWindow,
    pending: State<'_, Arc<CliUiPending>>,
    app: AppHandle,
) -> Result<(), String> {
    crate::commands::ensure_main_window(&window).map_err(|error| error.to_string())?;
    pending.mark_ready(&app)
}

fn endpoint_path() -> PathBuf {
    std::env::temp_dir().join(ENDPOINT_FILE)
}

fn write_endpoint(path: &Path, endpoint: &Endpoint) -> Result<(), CliError> {
    let bytes = serde_json::to_vec(endpoint)
        .map_err(|error| CliError::new("IPC_START_FAILED", error.to_string()))?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, bytes)
        .map_err(|error| CliError::new("IPC_START_FAILED", error.to_string()))?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| CliError::new("IPC_START_FAILED", error.to_string()))?;
    }
    fs::rename(temporary, path)
        .map_err(|error| CliError::new("IPC_START_FAILED", error.to_string()))
}

fn read_request(stream: &mut TcpStream) -> Result<Request, CliError> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| CliError::new("IPC_READ_FAILED", error.to_string()))?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > MAX_MESSAGE_BYTES {
            return Err(CliError::new(
                "IPC_MESSAGE_TOO_LARGE",
                "The CLI request is larger than the supported limit.",
            ));
        }
        if let Some(newline) = buffer[..count].iter().position(|byte| *byte == b'\n') {
            bytes.extend_from_slice(&buffer[..newline]);
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| CliError::new("INVALID_REQUEST", "The CLI request has an invalid format."))
}

#[cfg(windows)]
fn prevent_socket_inheritance<T: std::os::windows::io::AsRawSocket>(
    socket: &T,
) -> Result<(), CliError> {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    // A backend started during an IPC request must not retain that authenticated
    // connection or delay its EOF. SOCKET is a Windows kernel handle here.
    if unsafe { SetHandleInformation(socket.as_raw_socket() as _, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(CliError::new(
            "IPC_FAILED",
            "Cannot isolate the IPC socket from child processes.",
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn prevent_socket_inheritance<T>(_socket: &T) -> Result<(), CliError> {
    Ok(())
}

fn read_response(
    stream: &mut TcpStream,
    max_bytes: usize,
    timeout: Option<Duration>,
) -> Result<Response, CliError> {
    // A socket's read timeout is per read. Decrease it after every chunk so a
    // trickling response cannot keep a finite CLI operation alive indefinitely.
    let deadline = timeout
        .map(|timeout| {
            Instant::now().checked_add(timeout).ok_or_else(|| {
                CliError::new("INVALID_REQUEST", "The IPC timeout is not representable.")
            })
        })
        .transpose()?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        if remaining.is_some_and(|remaining| remaining.is_zero()) {
            return Err(CliError::new(
                "IPC_READ_FAILED",
                "The Core response exceeded its time budget.",
            ));
        }
        stream
            .set_read_timeout(remaining)
            .map_err(|error| CliError::new("IPC_READ_FAILED", error.to_string()))?;
        let count = stream
            .read(&mut buffer)
            .map_err(|error| CliError::new("IPC_READ_FAILED", error.to_string()))?;
        if count == 0 {
            break;
        }
        let newline = buffer[..count].iter().position(|byte| *byte == b'\n');
        let frame_bytes = newline.unwrap_or(count);
        if bytes.len().saturating_add(frame_bytes) > max_bytes {
            return Err(CliError::new(
                "IPC_MESSAGE_TOO_LARGE",
                "The Core response is larger than the supported limit.",
            ));
        }
        bytes.extend_from_slice(&buffer[..frame_bytes]);
        if let Some(newline) = newline {
            if buffer[newline + 1..count]
                .iter()
                .any(|byte| !byte.is_ascii_whitespace())
            {
                return Err(CliError::new(
                    "IPC_PROTOCOL_ERROR",
                    "Unexpected bytes after the IPC response.",
                ));
            }
            break;
        }
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| CliError::new("IPC_PROTOCOL_ERROR", error.to_string()))
}

fn validate_sensitive_payload(
    args: &[String],
    payload: Option<&SensitivePayload>,
) -> Result<(), CliError> {
    let expects_code = crate::plugin_distribution_cli::needs_access_code_stdin(args)
        .map_err(CliError::from_plugin)?;
    if expects_code != payload.is_some() {
        return Err(CliError::new(
            "INVALID_REQUEST",
            "Only the access-code command accepts its required dedicated sensitive payload.",
        ));
    }
    if let Some(payload) = payload {
        crate::plugin_distribution_cli::validate_access_code(payload.access_code())
            .map_err(CliError::from_plugin)?;
    }
    Ok(())
}

fn write_response(stream: &mut TcpStream, response: &Response) -> Result<(), CliError> {
    let mut bytes = serde_json::to_vec(response)
        .map_err(|error| CliError::new("IPC_PROTOCOL_ERROR", error.to_string()))?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .map_err(|error| CliError::new("IPC_WRITE_FAILED", error.to_string()))
}

fn random_hex(byte_count: usize) -> Result<String, CliError> {
    let mut bytes = vec![0; byte_count];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| CliError::new("RANDOM_FAILED", "Cannot create a secure CLI request token."))?;
    Ok(hex::encode(bytes))
}

fn token_matches(left: &[u8], right: &[u8]) -> bool {
    left == right
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_and_error_responses_have_one_stable_json_shape() {
        let success = serde_json::to_value(Response::success(
            "request".to_owned(),
            json!({"plugins": []}),
        ))
        .unwrap();
        assert_eq!(success["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(success["ok"], true);
        assert_eq!(success["data"]["plugins"], json!([]));
        assert!(success.get("error").is_none());

        let failure = serde_json::to_value(Response::failure(
            "request".to_owned(),
            CliError::new("INVALID_REQUEST", "bad command"),
        ))
        .unwrap();
        assert_eq!(failure["ok"], false);
        assert_eq!(failure["error"]["code"], "INVALID_REQUEST");
        assert_eq!(failure["error"]["message"], "bad command");
        assert!(failure.get("data").is_none());
    }

    fn code_args() -> Vec<String> {
        ["plugins", "distribution", "set-code", "--stdin", "--yes"]
            .map(str::to_owned)
            .into()
    }

    fn socket_endpoint(capability: Option<u32>) -> (PathBuf, TcpListener) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let path = std::env::temp_dir().join(format!(
            "wla-sensitive-endpoint-{}.json",
            random_hex(16).unwrap()
        ));
        let endpoint = Endpoint {
            protocol_version: PROTOCOL_VERSION,
            port: listener.local_addr().unwrap().port(),
            token: "private-ipc-token".into(),
            data_dir: std::env::temp_dir(),
            process_id: std::process::id(),
            sensitive_payload_version: capability,
            cancellation_version: None,
        };
        fs::write(&path, serde_json::to_vec(&endpoint).unwrap()).unwrap();
        (path, listener)
    }

    #[test]
    fn sensitive_payload_is_closed_redacted_and_only_valid_for_set_code() {
        let payload = SensitivePayload::DistributionAccessCode("private-access-code".into());
        assert!(validate_sensitive_payload(&code_args(), Some(&payload)).is_ok());
        assert!(validate_sensitive_payload(&code_args(), None).is_err());
        for args in [
            vec!["app".into(), "open".into()],
            vec!["plugins".into(), "list".into()],
        ] {
            assert!(validate_sensitive_payload(&args, Some(&payload)).is_err());
        }
        assert!(
            serde_json::from_value::<SensitivePayload>(
                json!({"kind":"arbitraryJson", "value":{"command":"anything"}})
            )
            .is_err()
        );
        let request = Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "test".into(),
            token: "private-ipc-token".into(),
            args: code_args(),
            sensitive_payload: Some(payload),
            cancel_on_disconnect: false,
        };
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("private-access-code"));
        assert!(!rendered.contains("private-ipc-token"));
    }

    #[test]
    fn active_legacy_endpoint_rejects_secret_without_sending_it_or_deleting_endpoint() {
        let (path, listener) = socket_endpoint(None);
        let original = fs::read(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let payload = SensitivePayload::DistributionAccessCode("private-access-code".into());
        let result = try_send_to_desktop_at(
            &path,
            &code_args(),
            None,
            Some(Duration::from_secs(2)),
            Some(&payload),
        )
        .unwrap_err();
        assert_eq!(result.code, "IPC_PROTOCOL_MISMATCH");
        assert!(!result.message.contains("private-access-code"));
        assert!(server.join().unwrap().is_empty());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn supported_endpoint_sends_secret_only_in_dedicated_payload() {
        let (path, listener) = socket_endpoint(Some(SENSITIVE_PAYLOAD_VERSION));
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream).unwrap();
            assert_eq!(request.args, code_args());
            assert!(
                !request
                    .args
                    .iter()
                    .any(|arg| arg.contains("private-access-code"))
            );
            let payload = request.sensitive_payload.as_ref().unwrap();
            assert_eq!(payload.access_code(), "private-access-code");
            validate_sensitive_payload(&request.args, Some(payload)).unwrap();
            write_response(
                &mut stream,
                &Response::success(request.request_id, json!({"credentialConfigured":true})),
            )
            .unwrap();
        });
        let payload = SensitivePayload::DistributionAccessCode("private-access-code".into());
        let response = try_send_to_desktop_at(
            &path,
            &code_args(),
            None,
            Some(Duration::from_secs(2)),
            Some(&payload),
        )
        .unwrap()
        .unwrap();
        assert!(response.ok);
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains("private-access-code")
        );
        server.join().unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn download_admin_commands_cross_authenticated_local_ipc_without_a_new_remote_route() {
        use crate::download_sessions::DownloadSessions;
        use crate::remote_interaction::{RemoteBroker, RemoteContext};
        let broker = Arc::new(RemoteBroker::default());
        let credentials = Arc::new(DownloadSessions::default());
        let context = RemoteContext::new(broker.clone(), "abandoned-owner".into(), "editor".into());
        let result = context
            .service(
                "core.files.export",
                &json!({"name":"keep.anything","contentBase64":"YWJj"}),
            )
            .unwrap();
        let id = result["downloadId"].as_str().unwrap().to_owned();
        context.cancel();
        for args in [
            vec!["web".into(), "downloads".into(), "list".into()],
            vec![
                "web".into(),
                "downloads".into(),
                "discard".into(),
                id.clone(),
                "--yes".into(),
            ],
        ] {
            let (path, listener) = socket_endpoint(Some(SENSITIVE_PAYLOAD_VERSION));
            let worker_broker = broker.clone();
            let worker_credentials = credentials.clone();
            let expected_args = args.clone();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream).unwrap();
                assert_eq!(request.token, "private-ipc-token");
                assert_eq!(request.args, expected_args);
                assert!(request.sensitive_payload.is_none());
                let output = crate::web_access::run_download_admin(
                    Some((&worker_broker, &worker_credentials)),
                    &request.args[2..],
                )
                .unwrap();
                write_response(&mut stream, &Response::success(request.request_id, output))
                    .unwrap();
            });
            let response =
                try_send_to_desktop_at(&path, &args, None, Some(Duration::from_secs(2)), None)
                    .unwrap()
                    .unwrap();
            assert!(response.ok);
            if args[2] == "list" {
                assert_eq!(
                    response.data.unwrap()["downloads"][0]["owner"],
                    "abandoned-owner"
                );
            } else {
                assert_eq!(response.data.unwrap()["discarded"], true);
            }
            server.join().unwrap();
            fs::remove_file(path).unwrap();
        }
        assert!(broker.upgrade_blockers().is_empty());
    }

    #[test]
    fn complete_response_does_not_wait_for_socket_eof_after_cold_child_start() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let (done, wait) = mpsc::channel();
        let worker = thread::spawn(move || {
            write_response(
                &mut server,
                &Response::success(
                    "cold".into(),
                    serde_json::json!({"text":"x".repeat(20_000)}),
                ),
            )
            .unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        let response = read_response(
            &mut client,
            MAX_MESSAGE_BYTES,
            Some(Duration::from_millis(500)),
        );
        done.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(
            response.unwrap().data.unwrap()["text"]
                .as_str()
                .unwrap()
                .len(),
            20_000
        );
    }

    #[cfg(windows)]
    #[test]
    fn accepted_ipc_sockets_are_not_inherited_by_plugin_processes() {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        prevent_socket_inheritance(&listener).unwrap();
        prevent_socket_inheritance(&client).unwrap();
        prevent_socket_inheritance(&server).unwrap();
        let mut flags = HANDLE_FLAG_INHERIT;
        assert_ne!(
            unsafe { GetHandleInformation(server.as_raw_socket() as _, &mut flags) },
            0
        );
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    }

    #[test]
    fn malformed_sensitive_request_errors_do_not_echo_wire_input() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let failure = read_request(&mut stream).unwrap_err();
            assert_eq!(failure.code, "INVALID_REQUEST");
            assert!(!failure.message.contains("private-access-code"));
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(b"{\"sensitivePayload\":{\"kind\":\"private-access-code\"}}\n")
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn response_chunks_do_not_reset_the_total_deadline() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for _ in 0..8 {
                if stream.write_all(b" ").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        let mut client = TcpStream::connect(address).unwrap();
        let started = Instant::now();
        let failure =
            read_response(&mut client, 1024, Some(Duration::from_millis(70))).unwrap_err();
        assert_eq!(failure.code, "IPC_READ_FAILED");
        assert!(started.elapsed() < Duration::from_millis(180));
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn method_and_ui_commands_have_only_explicit_budgets() {
        let mut args = vec![
            "plugins".into(),
            "call".into(),
            "fixture".into(),
            "method".into(),
            "--yes".into(),
        ];
        assert_eq!(command_timeout_ms(&args).unwrap(), None);
        args.extend(["--timeout-ms".into(), "172800000".into()]);
        assert_eq!(command_timeout_ms(&args).unwrap(), Some(172_800_000));
        let mut ui = vec![
            "ui".into(),
            "command".into(),
            "run".into(),
            "fixture/view".into(),
            "run".into(),
        ];
        assert_eq!(command_timeout_ms(&ui).unwrap(), None);
        ui.extend(["--timeout-ms".into(), "0".into()]);
        assert!(command_timeout_ms(&ui).is_err());
    }

    #[test]
    fn unbounded_response_completes_when_server_replies() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(50));
            write_response(
                &mut stream,
                &Response::success("unbounded".into(), json!({"done":true})),
            )
            .unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        assert!(read_response(&mut client, 1024, None).unwrap().ok);
        server.join().unwrap();
    }

    #[test]
    fn connection_close_cancels_an_unbounded_invocation() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let invocation = HostInvocation::for_call(None, None, None).unwrap();
        let monitor = DisconnectMonitor::start(&server, invocation.clone()).unwrap();
        assert!(invocation.check().is_ok());
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(1);
        while invocation.check().is_ok() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(invocation.check().unwrap_err().code, "CANCELLED");
        drop(monitor);
    }
}
