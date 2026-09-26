//! Authenticated, loopback-only IPC used by `wla` to reach the running desktop Core.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use wonderland_plugin_protocol::PluginError;

use crate::plugin_manager::PluginManager;

const ENDPOINT_FILE: &str = "wonderland-assistant-cli-v1.json";
const PROTOCOL_VERSION: u32 = 1;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const CLI_UI_EVENT: &str = "cli:ui-request";

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

    fn from_plugin(error: PluginError) -> Self {
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    protocol_version: u32,
    request_id: String,
    token: String,
    args: Vec<String>,
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
}

#[derive(Default)]
pub struct CliUiPending {
    requests: Mutex<HashMap<String, mpsc::Sender<Result<Value, CliError>>>>,
    queued_events: Mutex<Vec<UiRequest>>,
    ready: AtomicBool,
}

impl CliUiPending {
    fn request(&self, app: &AppHandle, args: Vec<String>) -> Result<Value, CliError> {
        let request_id = random_hex(16)?;
        let (tx, rx) = mpsc::channel();
        self.requests
            .lock()
            .map_err(|_| CliError::new("INTERNAL", "The CLI request queue is unavailable."))?
            .insert(request_id.clone(), tx);
        let event = UiRequest {
            request_id: request_id.clone(),
            args,
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
        let response = rx.recv_timeout(REQUEST_TIMEOUT);
        self.remove(&request_id);
        match response {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(CliError::new(
                "UI_RESPONSE_TIMEOUT",
                "The desktop UI did not confirm the change before the request timed out.",
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(CliError::new(
                "UI_RESPONSE_LOST",
                "The desktop UI closed before confirming the request.",
            )),
        }
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
        };
        write_endpoint(&endpoint_path, &endpoint)?;
        let expected_token = endpoint.token.clone();

        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let app = app.clone();
        let worker = thread::Builder::new()
            .name("wonderland-cli-ipc".to_owned())
            .spawn(move || {
                while !thread_shutdown.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let app = app.clone();
                            let manager = manager.clone();
                            let pending = Arc::clone(&pending);
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
        })
    }
}

impl Drop for CliIpcServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
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
    let endpoint_path = endpoint_path();
    let contents = match fs::read(&endpoint_path) {
        Ok(contents) => contents,
        Err(_) => return Ok(None),
    };
    let endpoint: Endpoint = match serde_json::from_slice::<Endpoint>(&contents) {
        Ok(endpoint) if endpoint.protocol_version == PROTOCOL_VERSION => endpoint,
        _ => {
            let _ = fs::remove_file(&endpoint_path);
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
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
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
    };
    let mut bytes = serde_json::to_vec(&request)
        .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| CliError::new("IPC_FAILED", error.to_string()))?;
    let response = read_response(&mut stream)?;
    if response.protocol_version != PROTOCOL_VERSION || response.request_id != request_id {
        return Err(CliError::new(
            "IPC_PROTOCOL_MISMATCH",
            "The running Core returned an incompatible CLI response.",
        ));
    }
    Ok(Some(response))
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

pub fn plugin_error(error: PluginError) -> i32 {
    print_error(CliError::from_plugin(error))
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

fn serve_client(
    mut stream: TcpStream,
    app: &AppHandle,
    manager: &PluginManager,
    pending: &CliUiPending,
    expected_token: &str,
) {
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
        let result = dispatch(app, manager, pending, &request.args);
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
) -> Result<Value, CliError> {
    if args[0] == "ui"
        && args.get(1).is_some_and(|arg| arg == "command")
        && args.get(2).is_some_and(|arg| arg == "list")
    {
        let arguments = args.iter().cloned().map(Into::into).collect::<Vec<_>>();
        return crate::plugin_manager::run_cli_command(app, manager, &arguments, true)
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
        if args[0] == "app"
            && args.get(1).is_some_and(|arg| arg == "open")
            && let Some(window) = app.get_webview_window(crate::MAIN_WINDOW_LABEL)
        {
            let _ = window.show();
            let _ = window.set_focus();
        }
        return pending.request(app, args.to_vec());
    }
    let arguments = args.iter().cloned().map(Into::into).collect::<Vec<_>>();
    match crate::plugin_manager::run_cli_command(app, manager, &arguments, true) {
        Ok(data) => {
            if args.first().is_some_and(|arg| arg == "plugins")
                && (args.get(1).is_some_and(|arg| {
                    matches!(arg.as_str(), "install" | "enable" | "disable" | "remove")
                }) || (args.get(1).is_some_and(|arg| arg == "permissions")
                    && args.get(2).is_some_and(|arg| arg == "set")))
            {
                let _ = app.emit("plugins:changed", ());
            }
            Ok(data)
        }
        Err(error) => Err(CliError::from_plugin(error)),
    }
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
        .map_err(|error| CliError::new("INVALID_REQUEST", format!("Invalid CLI request: {error}")))
}

fn read_response(stream: &mut TcpStream) -> Result<Response, CliError> {
    let mut bytes = Vec::new();
    stream
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| CliError::new("IPC_READ_FAILED", error.to_string()))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(CliError::new(
            "IPC_MESSAGE_TOO_LARGE",
            "The Core response is larger than the supported limit.",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| CliError::new("IPC_PROTOCOL_ERROR", error.to_string()))
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
}
