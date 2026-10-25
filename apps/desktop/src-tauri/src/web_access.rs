//! Opt-in HTTP host. Shared tools delegate declared dependency calls in one device context.
//! Only selected roots have direct browser entry points.
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use crate::download_sessions::{DownloadSessions, cookie, supplied_cookie};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRef, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Listener, Manager, WebviewWindow};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::{Semaphore, oneshot};
use wonderland_plugin_protocol::{PluginError, PluginRuntimeState};

use crate::plugin_manager::PluginManager;
use crate::remote_interaction::{InteractionNotification, RemoteBroker, RemoteContext};

const MAX_EVENTS: usize = 256;
const MAX_EVENT_BYTES: usize = 64 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebAccessOptions {
    /// `loopback` is suitable for a local HTTPS reverse proxy or tunnel agent.
    mode: String,
    port: u16,
    public_url: String,
    plugin_ids: BTreeSet<String>,
    /// Empty means no key is required; only the selected plugins are shared.
    #[serde(default)]
    access_key: String,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebAccessStatus {
    running: bool,
    options: Option<WebAccessOptions>,
    local_url: Option<String>,
    access_token: Option<String>,
    lan_addresses: Vec<LanAddress>,
    address_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LanAddress {
    interface_name: String,
    ip: Ipv4Addr,
    url: String,
}

fn usable_ipv4(ip: Ipv4Addr) -> bool {
    !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_multicast()
        && !ip.is_broadcast()
        && ip.octets()[0] != 0
        && ip.octets()[0] < 240
}

fn lan_addresses(interfaces: Vec<if_addrs::Interface>, port: u16) -> Vec<LanAddress> {
    let mut candidates = interfaces
        .into_iter()
        .filter_map(|interface| {
            let IpAddr::V4(ip) = interface.ip() else {
                return None;
            };
            (interface.is_oper_up() && usable_ipv4(ip)).then_some((
                ip,
                interface.is_p2p,
                interface.name,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        (!a.0.is_private(), a.1, a.0, &a.2).cmp(&(!b.0.is_private(), b.1, b.0, &b.2))
    });
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|(ip, _, _)| seen.insert(*ip))
        .map(|(ip, _, name)| LanAddress {
            interface_name: name,
            ip,
            url: format!("http://{ip}:{port}/"),
        })
        .collect()
}

#[derive(Default)]
pub struct WebAccessService {
    running: tokio::sync::Mutex<Option<RunningServer>>,
}

/// Local IPC only. The browser router has no route to start, stop or reconfigure itself.
pub fn run_cli(app: &AppHandle, args: &[String]) -> Result<Value, crate::cli_ipc::CliError> {
    use crate::cli_ipc::CliError;
    let service = app.state::<WebAccessService>();
    tauri::async_runtime::block_on(async {
        if args.first().is_some_and(|command| command == "downloads") {
            return service.manage_downloads(&args[1..]).await;
        }
        match args {
            [command] if command == "status" => {}
            [command, confirm] if command == "stop" && confirm == "--yes" => service
                .stop(app)
                .await
                .map_err(|message| CliError::new("WEB_ACCESS_BUSY", message))?,
            [command, flag, config, confirm]
                if command == "start" && flag == "--config-json" && confirm == "--yes" =>
            {
                let options = serde_json::from_str(config).map_err(|e| {
                    CliError::new("INVALID_REQUEST", format!("Invalid web options: {e}"))
                })?;
                service
                    .start(app.clone(), options)
                    .await
                    .map_err(|e| CliError::new("WEB_ACCESS_FAILED", e))?;
            }
            _ => {
                return Err(CliError::new(
                    "INVALID_REQUEST",
                    "Usage: wla web status | start --config-json <json> --yes | stop --yes | downloads list | downloads discard <download-id> --yes",
                ));
            }
        }
        Ok(json!(service.status().await))
    })
}

/// Invoked only by authenticated local CLI IPC, never by the remote browser router.
pub(crate) fn run_download_admin(
    session: Option<(&RemoteBroker, &DownloadSessions)>,
    args: &[String],
) -> Result<Value, crate::cli_ipc::CliError> {
    use crate::cli_ipc::CliError;
    match args {
        [command] if command == "list" => {
            let downloads = match session {
                Some((broker, _)) => broker.admin_downloads().map_err(CliError::from_plugin)?,
                None => json!([]),
            };
            Ok(json!({"downloads":downloads}))
        }
        [command, id, confirm]
            if command == "discard"
                && confirm == "--yes"
                && id.len() == 64
                && id.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            let (broker, credentials) = session.ok_or_else(|| {
                CliError::new("WEB_ACCESS_NOT_RUNNING", "Web sharing is not running.")
            })?;
            let owner = broker.discard_download(id).map_err(CliError::from_plugin)?;
            credentials.forget(&owner, id);
            Ok(json!({"downloadId":id,"discarded":true}))
        }
        _ => Err(CliError::new(
            "INVALID_REQUEST",
            "Usage: wla web downloads list | discard <download-id> --yes",
        )),
    }
}

struct RunningServer {
    options: WebAccessOptions,
    port: u16,
    access: Arc<Access>,
    shutdown: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<()>,
    event_listeners: Vec<tauri::EventId>,
}

struct Access {
    token: String,
    session_id: String,
    asset_key: String,
    active: AtomicBool,
    plugin_ids: BTreeSet<String>,
    events: Arc<Mutex<EventBuffer>>,
    calls: Arc<Semaphore>,
    interactions: Arc<RemoteBroker>,
    invocations: Mutex<HashMap<String, RemoteInvocation>>,
    downloads: DownloadSessions,
}

struct RemoteInvocation {
    owner: String,
    root: String,
    request_id: String,
    context: Arc<RemoteContext>,
}

struct InvocationRegistration {
    access: Arc<Access>,
    id: String,
}

/// The HTTP future owns cancellation; a detached blocking worker cannot keep a lost call alive.
struct RequestCancellation {
    context: Arc<RemoteContext>,
    cancel: Option<Box<dyn FnOnce() + Send>>,
}

impl RequestCancellation {
    fn completed(&mut self) {
        self.cancel = None;
    }
}

impl Drop for RequestCancellation {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            // Cancelling the context also covers a worker that has not registered with its process.
            self.context.cancel();
            cancel();
        }
    }
}

impl Drop for InvocationRegistration {
    fn drop(&mut self) {
        self.access.invocations.lock().unwrap().remove(&self.id);
    }
}

fn register_invocation(
    access: &Arc<Access>,
    invocation: RemoteInvocation,
) -> Result<InvocationRegistration, PluginError> {
    let id = format!(
        "web-{}",
        random_key().map_err(|message| PluginError {
            code: "INTERNAL".into(),
            message,
            details: None
        })?
    );
    let mut invocations = access.invocations.lock().unwrap();
    if invocations.values().any(|active| {
        active.owner == invocation.owner
            && active.root == invocation.root
            && active.request_id == invocation.request_id
    }) {
        return Err(PluginError {
            code: "INVALID_REQUEST".into(),
            message: "当前设备已有相同标识的调用。".into(),
            details: None,
        });
    }
    invocations.insert(id.clone(), invocation);
    Ok(InvocationRegistration {
        access: access.clone(),
        id,
    })
}

fn owned_invocation(access: &Access, owner: &str, root: &str, request_id: &str) -> Option<String> {
    access
        .invocations
        .lock()
        .unwrap()
        .iter()
        .find(|(_, invocation)| {
            invocation.owner == owner
                && invocation.root == root
                && invocation.request_id == request_id
        })
        .map(|(id, _)| id.clone())
}

/// Restore a caller's request ID only within the root invocation and its visiting device.
fn event_owner(access: &Access, payload: &mut Value) -> Option<Option<String>> {
    let Some(id) = payload
        .get("requestId")
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("web-"))
    else {
        return Some(None);
    };
    let invocations = access.invocations.lock().unwrap();
    let invocation = invocations.get(id)?;
    if payload.get("pluginId").and_then(Value::as_str) != Some(invocation.root.as_str()) {
        return None;
    }
    let owner = invocation.owner.clone();
    payload["requestId"] = json!(invocation.request_id);
    Some(Some(owner))
}

#[derive(Default)]
struct EventBuffer {
    sequence: u64,
    items: VecDeque<(u64, EventRecord)>,
}

struct EventRecord {
    owner: Option<String>,
    root: Option<String>,
    notification: Value,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WebNotification {
    PluginEvent { payload: Value },
    PluginState,
    InteractionChange { payload: InteractionNotification },
}

#[derive(Deserialize)]
struct EventIdentity {
    #[serde(rename = "pluginId")]
    plugin_id: String,
}

impl EventBuffer {
    fn push(&mut self, event: Value) {
        self.push_scoped(None, None, event);
    }

    fn push_scoped(&mut self, owner: Option<String>, root: Option<String>, notification: Value) {
        self.sequence += 1;
        self.items.push_back((
            self.sequence,
            EventRecord {
                owner,
                root,
                notification,
            },
        ));
        while self.items.len() > MAX_EVENTS {
            self.items.pop_front();
        }
    }

    #[cfg(test)]
    fn since(&self, after: Option<u64>) -> Value {
        self.since_filtered(after, |_| true)
    }

    fn since_filtered(&self, after: Option<u64>, visible: impl Fn(&EventRecord) -> bool) -> Value {
        let after = after.unwrap_or(self.sequence);
        let mut expected = after.saturating_add(1);
        let mut missed = after > self.sequence;
        for (id, _) in self.items.iter().filter(|(id, _)| *id > after) {
            missed |= *id != expected;
            expected = id.saturating_add(1);
        }
        missed |= expected <= self.sequence;
        json!({
            "cursor": self.sequence,
            "missed": missed,
            "events": self.items.iter().filter(|(id, event)| *id > after && visible(event)).map(|(_, event)| &event.notification).collect::<Vec<_>>()
        })
    }
}

#[derive(Clone)]
struct WebState {
    app: AppHandle,
    manager: PluginManager,
    access: Arc<Access>,
}

#[derive(Clone)]
struct DownloadState {
    manager: PluginManager,
    access: Arc<Access>,
}

impl FromRef<WebState> for DownloadState {
    fn from_ref(state: &WebState) -> Self {
        Self {
            manager: state.manager.clone(),
            access: state.access.clone(),
        }
    }
}

pub(crate) fn random_key() -> Result<String, String> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "无法生成访问密钥")?;
    Ok(hex::encode(bytes))
}

fn validate_options(options: &WebAccessOptions) -> Result<IpAddr, String> {
    let address = match options.mode.as_str() {
        "loopback" => Ipv4Addr::LOCALHOST,
        "lan" => Ipv4Addr::UNSPECIFIED,
        _ => return Err("请选择本机代理或局域网模式".into()),
    };
    if options.port == 0 {
        return Err("端口必须介于 1 与 65535 之间".into());
    }
    if options.access_key.len() > 1024 || options.access_key.chars().any(char::is_control) {
        return Err("访问密钥不能包含控制字符，且最多为 1024 字节".into());
    }
    if !options.public_url.is_empty() {
        let url =
            tauri::Url::parse(&options.public_url).map_err(|_| "公网入口必须为 HTTPS 地址")?;
        if url.scheme() != "https"
            || url.as_str().len() > 1024
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("公网入口必须为不含路径、凭据、查询参数的 HTTPS 地址".into());
        }
    }
    Ok(address.into())
}

impl WebAccessService {
    async fn manage_downloads(&self, args: &[String]) -> Result<Value, crate::cli_ipc::CliError> {
        let guard = self.running.lock().await;
        run_download_admin(
            guard.as_ref().map(|running| {
                (
                    running.access.interactions.as_ref(),
                    &running.access.downloads,
                )
            }),
            args,
        )
    }
    pub(crate) async fn upgrade_blockers(&self) -> Vec<String> {
        let guard = self.running.lock().await;
        let Some(running) = guard.as_ref() else {
            return Vec::new();
        };
        let mut blockers = running.access.interactions.upgrade_blockers();
        match running.access.invocations.lock() {
            Ok(invocations) if !invocations.is_empty() => blockers.push(format!(
                "还有 {} 个远程插件调用尚未结束。",
                invocations.len()
            )),
            Err(_) => blockers.push("远程调用状态不可用，暂不能安装更新。".into()),
            _ => {}
        }
        blockers
    }

    async fn status(&self) -> WebAccessStatus {
        let guard = self.running.lock().await;
        let mut status = guard
            .as_ref()
            .map_or_else(WebAccessStatus::default, |running| WebAccessStatus {
                running: running.access.active.load(Ordering::Acquire),
                options: Some(running.options.clone()),
                local_url: Some(format!("http://127.0.0.1:{}/", running.port)),
                access_token: (!running.access.token.is_empty())
                    .then(|| running.access.token.clone()),
                lan_addresses: Vec::new(),
                address_error: None,
            });
        drop(guard);
        if status.running
            && let Some(options) = &status.options
            && options.mode == "lan"
        {
            let port = options.port;
            match tauri::async_runtime::spawn_blocking(move || {
                if_addrs::get_if_addrs().map(|interfaces| lan_addresses(interfaces, port))
            })
            .await
            {
                Ok(Ok(addresses)) => status.lan_addresses = addresses,
                _ => {
                    status.address_error = Some("无法读取本机网卡地址，请检查网络连接后刷新".into())
                }
            }
        }
        status
    }

    async fn start(&self, app: AppHandle, mut options: WebAccessOptions) -> Result<(), String> {
        options.access_key = options.access_key.trim().to_owned();
        let address = validate_options(&options)?;
        let mut guard = self.running.lock().await;
        if guard.is_some() {
            return Err("请先关闭当前网页访问，再更改配置或生成新密钥".into());
        }
        let manager = app.state::<PluginManager>().inner().clone();
        for id in &options.plugin_ids {
            ensure_remote_plugin(&manager, id).map_err(|(_, error)| {
                error.0["message"]
                    .as_str()
                    .unwrap_or("插件不支持远程访问")
                    .to_owned()
            })?;
            manager.plugin_ui_launch_info(id).map_err(|e| e.message)?;
        }
        if app.asset_resolver().get("mobile.html".into()).is_none() {
            return Err("未找到手机页面资源，请先构建前端再开启网页访问".into());
        }
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(address, options.port))
            .await
            .map_err(|e| format!("网页访问端口无法监听：{e}"))?;
        let events = Arc::new(Mutex::new(EventBuffer::default()));
        let interaction_events = events.clone();
        let interactions = RemoteBroker::with_notify(Arc::new(move |owner, root, payload| {
            if let Ok(mut buffer) = interaction_events.lock() {
                buffer.push_scoped(
                    Some(owner.into()),
                    Some(root.into()),
                    json!(WebNotification::InteractionChange { payload }),
                );
            }
        }));
        let access = Arc::new(Access {
            token: options.access_key.clone(),
            session_id: random_key()?,
            asset_key: random_key()?,
            active: AtomicBool::new(true),
            plugin_ids: options.plugin_ids.clone(),
            events,
            calls: Arc::new(Semaphore::new(16)),
            interactions: Arc::new(interactions),
            invocations: Mutex::new(HashMap::new()),
            downloads: DownloadSessions::default(),
        });
        let event_access = access.clone();
        let event_listener = app.listen("plugin:event", move |event| {
            if !event_access.active.load(Ordering::Acquire) {
                return;
            }
            if event.payload().len() > MAX_EVENT_BYTES {
                // A skipped event is still a gap. Otherwise clients silently miss updates forever.
                if serde_json::from_str::<EventIdentity>(event.payload())
                    .is_ok_and(|event| event_access.plugin_ids.contains(&event.plugin_id))
                    && let Ok(mut buffer) = event_access.events.lock()
                {
                    buffer.sequence += 1;
                }
                return;
            }
            if let Ok(mut payload) = serde_json::from_str::<Value>(event.payload())
                && payload
                    .get("pluginId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| event_access.plugin_ids.contains(id))
            {
                let Some(owner) = event_owner(&event_access, &mut payload) else {
                    return;
                };
                let root = payload["pluginId"].as_str().map(str::to_owned);
                if let Ok(mut buffer) = event_access.events.lock() {
                    buffer.push_scoped(
                        owner,
                        root,
                        json!(WebNotification::PluginEvent { payload }),
                    );
                }
            }
        });
        let state_access = access.clone();
        let state_listener = app.listen("plugins:changed", move |_| {
            if state_access.active.load(Ordering::Acquire)
                && let Ok(mut buffer) = state_access.events.lock()
            {
                buffer.push(json!(WebNotification::PluginState));
            }
        });
        let state = WebState {
            app,
            manager,
            access: access.clone(),
        };
        let router = router(state);
        let (shutdown, receiver) = oneshot::channel();
        let task_access = access.clone();
        let task = tauri::async_runtime::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = receiver.await;
                })
                .await;
            task_access.active.store(false, Ordering::Release);
        });
        *guard = Some(RunningServer {
            port: options.port,
            options,
            access,
            shutdown,
            task,
            event_listeners: vec![event_listener, state_listener],
        });
        Ok(())
    }

    async fn stop(&self, app: &AppHandle) -> Result<(), String> {
        let mut guard = self.running.lock().await;
        if let Some(running) = guard.as_ref() {
            running
                .access
                .interactions
                .close_if_idle()
                .map_err(|blockers| blockers.join(" "))?;
        }
        if let Some(mut running) = guard.take() {
            running.access.active.store(false, Ordering::Release);
            running.access.interactions.close();
            let invocations: Vec<_> = running.access.invocations.lock().unwrap().drain().collect();
            let manager = app.state::<PluginManager>();
            for (id, invocation) in invocations {
                invocation.context.cancel();
                let _ = manager.cancel(&invocation.root, &id);
            }
            for listener in running.event_listeners {
                app.unlisten(listener);
            }
            let _ = running.shutdown.send(());
            if tokio::time::timeout(Duration::from_secs(2), &mut running.task)
                .await
                .is_err()
            {
                running.task.abort();
            }
        }
        Ok(())
    }
}

#[tauri::command]
pub async fn web_access_status(
    window: WebviewWindow,
    service: tauri::State<'_, WebAccessService>,
) -> Result<WebAccessStatus, String> {
    crate::commands::ensure_main_window(&window).map_err(|e| e.to_string())?;
    Ok(service.status().await)
}

#[tauri::command]
pub async fn web_access_start(
    app: AppHandle,
    window: WebviewWindow,
    service: tauri::State<'_, WebAccessService>,
    options: WebAccessOptions,
) -> Result<WebAccessStatus, String> {
    crate::commands::ensure_main_window(&window).map_err(|e| e.to_string())?;
    service.start(app, options).await?;
    Ok(service.status().await)
}

#[tauri::command]
pub async fn web_access_stop(
    app: AppHandle,
    window: WebviewWindow,
    service: tauri::State<'_, WebAccessService>,
) -> Result<WebAccessStatus, String> {
    crate::commands::ensure_main_window(&window).map_err(|e| e.to_string())?;
    service.stop(&app).await?;
    Ok(service.status().await)
}

fn router(state: WebState) -> Router {
    let api = Router::new()
        .route("/status", get(remote_status))
        .route("/plugins", get(plugins))
        .route("/launch/{id}", get(launch))
        .route("/call", post(call))
        .route("/cancel", post(cancel))
        .route("/events", get(events))
        .route("/interactions", get(interactions))
        .route(
            "/interactions/{id}/reply",
            post(interaction_reply).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route("/interactions/{id}/content", get(interaction_content))
        .route("/interactions/{id}/delivery", post(interaction_delivery))
        .merge(download_api_routes())
        .layer(middleware::from_fn_with_state(
            state.access.clone(),
            authenticate,
        ));
    Router::new()
        .nest("/api", api)
        .merge(native_download_routes())
        .route("/plugin-assets/{key}/{*path}", get(plugin_asset))
        .fallback(get(static_asset))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

fn download_api_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    DownloadState: FromRef<S>,
{
    Router::new()
        .route("/selected-files", get(selected_files))
        .route("/interactions/{id}/prepare", post(prepare_download))
        .route("/downloads/revoke", post(revoke_downloads))
}

fn native_download_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    DownloadState: FromRef<S>,
{
    Router::new().route(
        "/downloads/{session}/{owner}/{id}/{nonce}",
        get(native_download),
    )
}

fn authorized(headers: &HeaderMap, access: &Access) -> bool {
    access.active.load(Ordering::Acquire)
        && (access.token.is_empty() || supplied_key(headers).is_some_and(|key| key == access.token))
        && headers.get("origin").is_none_or(|value| value != "null")
        && headers
            .get("sec-fetch-site")
            .is_none_or(|value| value != "cross-site")
}

fn supplied_key(headers: &HeaderMap) -> Option<String> {
    if let Some(key) = headers.get("x-wonderland-key") {
        // UTF-8 keys cannot be represented directly in an HTTP header.
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(key.as_bytes())
            .ok()?;
        String::from_utf8(bytes).ok()
    } else {
        headers
            .get("authorization")?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")
            .map(str::to_owned)
    }
}

async fn authenticate(State(access): State<Arc<Access>>, request: Request, next: Next) -> Response {
    if !authorized(request.headers(), &access) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("cache-control", "no-store".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    if !headers.contains_key("content-security-policy") {
        headers.insert("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    }
    response
}

async fn remote_status(State(state): State<WebState>) -> Json<Value> {
    Json(
        json!({ "version": env!("CARGO_PKG_VERSION"), "transport": "web", "sessionId": state.access.session_id, "keyRequired": !state.access.token.is_empty(), "cursor": state.access.events.lock().map(|e| e.sequence).unwrap_or_default() }),
    )
}

async fn plugins(State(state): State<WebState>) -> Json<Value> {
    let mut snapshots = state.manager.snapshots();
    snapshots.retain(|s| {
        state.access.plugin_ids.contains(&s.manifest.id) && s.manifest.supports_remote_access()
    });
    for snapshot in &mut snapshots {
        snapshot.plugin_data_directory = None;
        snapshot.scan_diagnostic = None;
        snapshot.installation_source = None;
    }
    Json(json!(snapshots))
}

fn allowed(access: &Access, id: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if access.plugin_ids.contains(id) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(json!({"code":"NOT_SHARED", "message":"此插件未开放网页访问"})),
        ))
    }
}

fn ensure_remote_plugin(
    manager: &PluginManager,
    id: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    ensure_remote_snapshot(&manager.snapshots(), id)
}

fn ensure_remote_snapshot(
    snapshots: &[PluginRuntimeState],
    id: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    if snapshots.iter().any(|s| {
        s.manifest.id == id
            && s.manifest.supports_remote_access()
            && s.enabled
            && s.installation == wonderland_plugin_protocol::InstallationState::Installed
    }) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(
                json!({"code":"REMOTE_UNSUPPORTED", "message":"此插件未声明远程访问支持，不能共享。"}),
            ),
        ))
    }
}

fn allowed_plugin(
    manager: &PluginManager,
    access: &Access,
    id: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    allowed(access, id)?;
    ensure_remote_plugin(manager, id)
}

/// Dependency declarations authorize remote edges, while versions remain diagnostic metadata.
/// The visited set bounds cycles and includes transitive providers without exposing their UI.
fn remote_dependency_scope(snapshots: &[PluginRuntimeState], root: &str) -> BTreeSet<String> {
    let mut scope = BTreeSet::from([root.to_owned()]);
    let mut pending = vec![root.to_owned()];
    while let Some(id) = pending.pop() {
        let Some(consumer) = snapshots.iter().find(|snapshot| snapshot.manifest.id == id) else {
            continue;
        };
        for dependency in &consumer.manifest.dependencies {
            if dependency.methods.is_empty() || scope.contains(&dependency.plugin_id) {
                continue;
            }
            if snapshots.iter().any(|provider| {
                provider.manifest.id == dependency.plugin_id
                    && provider.enabled
                    && provider.installation
                        == wonderland_plugin_protocol::InstallationState::Installed
                    && provider.manifest.backend.supports_service_context == Some(true)
            }) {
                scope.insert(dependency.plugin_id.clone());
                pending.push(dependency.plugin_id.clone());
            }
        }
    }
    scope
}

fn allowed_owned_action(
    access: &Access,
    snapshots: &[PluginRuntimeState],
    root: &str,
    plugin: &str,
    kind: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    // Prepared bytes outlive their producing process. Their broker owner/root is authoritative.
    if kind == "download" {
        return allowed(access, root);
    }
    allowed_dependency(access, snapshots, root, plugin)?;
    // Eligibility can be cold; an already-issued interactive request still belongs
    // to its live producers. Only prepared download bytes outlive those instances.
    if [root, plugin].iter().all(|id| {
        snapshots.iter().any(|snapshot| {
            snapshot.manifest.id == *id
                && snapshot.runtime == wonderland_plugin_protocol::RuntimeState::Running
        })
    }) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"code":"INTERACTION_ENDED","message":"Producing plugin has stopped."})),
        ))
    }
}

fn allowed_dependency(
    access: &Access,
    snapshots: &[PluginRuntimeState],
    root: &str,
    plugin: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    allowed(access, root)?;
    ensure_remote_snapshot(snapshots, root)?;
    if remote_dependency_scope(snapshots, root).contains(plugin) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(
                json!({"code":"REMOTE_DEPENDENCY_NOT_DECLARED", "message":"交互不属于共享工具的声明依赖范围。"}),
            ),
        ))
    }
}

fn allowed_interaction(
    state: &WebState,
    owner: &str,
    id: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    allowed_interaction_for(&state.access, &state.manager, owner, id)
}

fn allowed_interaction_for(
    access: &Access,
    manager: &PluginManager,
    owner: &str,
    id: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    let (root, plugin, kind) = access.interactions.identity(owner, id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"message":"交互已过期或不属于当前设备。"})),
        )
    })?;
    allowed_owned_action(access, &manager.snapshots(), &root, &plugin, &kind)
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn client_id(headers: &HeaderMap) -> Result<String, (StatusCode, Json<Value>)> {
    headers
        .get("x-wonderland-client")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"message":"请刷新页面后重新连接，以启用远程设备交互。"})),
            )
        })
}

async fn interactions(State(state): State<WebState>, headers: HeaderMap) -> ApiResult {
    let mut actions = state.access.interactions.list(&client_id(&headers)?);
    let snapshots = state.manager.snapshots();
    actions.as_array_mut().unwrap().retain(|a| {
        a["rootPluginId"]
            .as_str()
            .zip(a["pluginId"].as_str())
            .is_some_and(|(root, plugin)| {
                allowed_owned_action(
                    &state.access,
                    &snapshots,
                    root,
                    plugin,
                    a["kind"].as_str().unwrap_or_default(),
                )
                .is_ok()
            })
    });
    Ok(Json(actions))
}

async fn selected_files(State(state): State<DownloadState>, headers: HeaderMap) -> ApiResult {
    Ok(Json(
        json!({"handles":state.access.interactions.selected_handles(&client_id(&headers)?)}),
    ))
}

async fn interaction_reply(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(value): Json<Value>,
) -> ApiResult {
    allowed_interaction(&state, &client_id(&headers)?, &id)?;
    state
        .access
        .interactions
        .reply(&client_id(&headers)?, &id, value)
        .map_err(plugin_error)?;
    state.access.downloads.forget(&client_id(&headers)?, &id);
    Ok(Json(Value::Null))
}

fn secure_download_cookie(headers: &HeaderMap) -> bool {
    headers
        .get("origin")
        .and_then(|header| header.to_str().ok())
        .is_some_and(|origin| origin.starts_with("https://"))
}
async fn prepare_download(
    State(state): State<DownloadState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let owner = client_id(&headers)?;
    allowed_interaction_for(&state.access, &state.manager, &owner, &id)?;
    let (_, _, kind) = state
        .access
        .interactions
        .identity(&owner, &id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"message":"The download has been removed."})),
            )
        })?;
    if kind != "download" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"message":"This action is not a download."})),
        ));
    }
    state
        .access
        .downloads
        .prune(|owner, id| state.access.interactions.identity(owner, id).is_some());
    let (token, nonce) = state.access.downloads.prepare(&owner, &id).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message":"Cannot prepare download."})),
        )
    })?;
    let url = format!(
        "/downloads/{}/{}/{}/{}",
        state.access.session_id, owner, id, nonce
    );
    let mut response = Json(json!({"url":url})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        cookie(
            &state.access.session_id,
            &owner,
            Some(&token),
            secure_download_cookie(&headers),
        )
        .parse()
        .unwrap(),
    );
    Ok(response)
}
async fn revoke_downloads(
    State(state): State<DownloadState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let owner = client_id(&headers)?;
    state.access.downloads.revoke(&owner);
    let mut response = Json(json!({"revoked":true})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        cookie(
            &state.access.session_id,
            &owner,
            None,
            secure_download_cookie(&headers),
        )
        .parse()
        .unwrap(),
    );
    Ok(response)
}

struct NativeDownload {
    file: tokio::fs::File,
    _temporary: Arc<crate::byte_streams::TemporaryFile>,
    access: Arc<Access>,
    owner: String,
    id: String,
    transfer_id: String,
    credential: Option<(String, String)>,
    remaining: u64,
    completed: bool,
}
impl NativeDownload {
    fn new(
        access: Arc<Access>,
        owner: String,
        id: String,
        transfer: crate::remote_interaction::DownloadTransfer,
        credential: Option<(String, String)>,
    ) -> Result<Self, (StatusCode, Json<Value>)> {
        let file = std::fs::File::open(&transfer.file.path)
            .and_then(|file| {
                if file.metadata()?.len() != transfer.size {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Download file size changed.",
                    ));
                }
                Ok(file)
            })
            .map_err(|_| {
                let _ = access
                    .interactions
                    .delivery(&owner, &id, &transfer.transfer_id, false);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message":"Download file is unavailable."})),
                )
            })?;
        Ok(Self {
            file: tokio::fs::File::from_std(file),
            _temporary: transfer.file,
            access,
            owner,
            id,
            transfer_id: transfer.transfer_id,
            credential,
            remaining: transfer.size,
            completed: false,
        })
    }
}
impl AsyncRead for NativeDownload {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.completed {
            return Poll::Ready(Ok(()));
        }
        let live = self.access.active.load(Ordering::Acquire)
            && self
                .access
                .interactions
                .transfer_active(&self.owner, &self.id, &self.transfer_id)
            && self.credential.as_ref().is_none_or(|(token, nonce)| {
                self.access
                    .downloads
                    .authorized(&self.owner, token, &self.id, nonce)
            });
        if !live {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Download has been cancelled or revoked.",
            )));
        }
        let before = buffer.filled().len();
        let remaining = buffer.remaining();
        match Pin::new(&mut self.file).poll_read(context, buffer) {
            Poll::Ready(Ok(())) if remaining > 0 && before == buffer.filled().len() => {
                if self.remaining != 0 {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Download file was truncated during transfer.",
                    )));
                }
                self.completed = true;
                let _ = self.access.interactions.delivery(
                    &self.owner,
                    &self.id,
                    &self.transfer_id,
                    true,
                );
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Ok(())) => {
                let read = (buffer.filled().len() - before) as u64;
                let Some(remaining) = self.remaining.checked_sub(read) else {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Download file changed during transfer.",
                    )));
                };
                self.remaining = remaining;
                Poll::Ready(Ok(()))
            }
            result => result,
        }
    }
}
impl Drop for NativeDownload {
    fn drop(&mut self) {
        if !self.completed {
            let _ =
                self.access
                    .interactions
                    .delivery(&self.owner, &self.id, &self.transfer_id, false);
        }
    }
}
async fn native_download(
    State(state): State<DownloadState>,
    headers: HeaderMap,
    Path((session, owner, id, nonce)): Path<(String, String, String, String)>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let token = supplied_cookie(&headers).unwrap_or_default();
    if !state.access.active.load(Ordering::Acquire)
        || session != state.access.session_id
        || headers.get("origin").is_some_and(|value| value == "null")
        || headers
            .get("sec-fetch-site")
            .is_some_and(|value| value == "cross-site")
        || !state
            .access
            .downloads
            .authorized(&owner, token, &id, &nonce)
    {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"message":"Download requires the current browser session."})),
        ));
    }
    let admission = state.manager.admit_work().map_err(plugin_error)?;
    allowed_interaction_for(&state.access, &state.manager, &owner, &id)?;
    let transfer = state
        .access
        .interactions
        .download(&owner, &id)
        .map_err(plugin_error)?;
    let reader = NativeDownload::new(
        state.access.clone(),
        owner,
        id,
        transfer,
        Some((token.into(), nonce)),
    )?;
    drop(admission);
    let mut response = Body::from_stream(tokio_util::io::ReaderStream::new(reader)).into_response();
    response
        .headers_mut()
        .insert("content-type", "application/octet-stream".parse().unwrap());
    response
        .headers_mut()
        .insert("content-disposition", "attachment".parse().unwrap());
    Ok(response)
}

async fn interaction_content(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let admission = state.manager.admit_work().map_err(plugin_error)?;
    allowed_interaction(&state, &client_id(&headers)?, &id)?;
    let transfer = state
        .access
        .interactions
        .download(&client_id(&headers)?, &id)
        .map_err(plugin_error)?;
    // The broker counts this transfer until stream EOF or response cancellation.
    drop(admission);
    let transfer_id = transfer.transfer_id.clone();
    let reader = NativeDownload::new(
        state.access.clone(),
        client_id(&headers)?,
        id,
        transfer,
        None,
    )?;
    Ok((
        [
            ("content-type", "application/octet-stream"),
            ("content-disposition", "attachment"),
            ("x-wonderland-transfer", transfer_id.as_str()),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(reader)),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DownloadDelivery {
    transfer_id: String,
    received: bool,
}

async fn interaction_delivery(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<DownloadDelivery>,
) -> ApiResult {
    let owner = client_id(&headers)?;
    allowed_interaction(&state, &owner, &id)?;
    state
        .access
        .interactions
        .delivery(&owner, &id, &input.transfer_id, input.received)
        .map_err(plugin_error)?;
    Ok(Json(Value::Null))
}
fn plugin_error(error: PluginError) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!(error)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LaunchQuery {
    request_id: String,
    #[serde(default)]
    retry: bool,
}

async fn launch(
    State(state): State<DownloadState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(input): Query<LaunchQuery>,
) -> ApiResult {
    allowed_plugin(&state.manager, &state.access, &id)?;
    if !valid_request_id(&input.request_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"code":"INVALID_REQUEST","message":"Invalid request ID"})),
        ));
    }
    let owner = client_id(&headers)?;
    let permit = state
        .access
        .calls
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"message":"Too many active calls"})),
            )
        })?;
    let context = Arc::new(RemoteContext::new(
        state.access.interactions.clone(),
        owner.clone(),
        id.clone(),
    ));
    let registration = register_invocation(
        &state.access,
        RemoteInvocation {
            owner,
            root: id.clone(),
            request_id: input.request_id,
            context: context.clone(),
        },
    )
    .map_err(plugin_error)?;
    let request = registration.id.clone();
    let cancel_manager = state.manager.clone();
    let cancel_id = id.clone();
    let cancel_request = request.clone();
    let mut cancellation = RequestCancellation {
        context: context.clone(),
        cancel: Some(Box::new(move || {
            let _ = cancel_manager.cancel(&cancel_id, &cancel_request);
        })),
    };
    let asset_key = state.access.asset_key.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let _permit = permit;
        let mut invocation = crate::host_services::HostInvocation::default();
        invocation.remote = Some(context);
        state
            .manager
            .prepare_ui(&id, &request, input.retry, &invocation)
    })
    .await;
    if result.is_ok() {
        cancellation.completed();
    }
    let mut launch = result
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message":"Plugin activation failed"})),
            )
        })?
        .map_err(plugin_error)?;
    let path = tauri::Url::parse(&launch.url)
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message":"Invalid plugin URL"})),
            )
        })?
        .path()
        .to_owned();
    launch.url = format!("/plugin-assets/{}{path}", asset_key);
    Ok(Json(json!(launch)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginCall {
    plugin_id: String,
    request_id: String,
    method: String,
    params: Value,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

fn valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 120
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

async fn call(
    State(state): State<DownloadState>,
    headers: HeaderMap,
    Json(input): Json<PluginCall>,
) -> ApiResult {
    // Start an explicit caller budget before validation or blocking-pool admission.
    let budget = crate::host_services::HostInvocation::default()
        .with_timeout_ms(input.timeout_ms)
        .map_err(plugin_error)?;
    allowed_plugin(&state.manager, &state.access, &input.plugin_id)?;
    if !valid_request_id(&input.request_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"message":"Invalid request ID"})),
        ));
    }
    let permit = state
        .access
        .calls
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"message":"Too many active calls"})),
            )
        })?;
    let owner = client_id(&headers)?;
    let context = Arc::new(RemoteContext::new(
        state.access.interactions.clone(),
        owner.clone(),
        input.plugin_id.clone(),
    ));
    // Browser tabs may reuse a public request ID; the host ID and cancellation remain owner-bound.
    let registration = register_invocation(
        &state.access,
        RemoteInvocation {
            owner,
            root: input.plugin_id.clone(),
            request_id: input.request_id.clone(),
            context: context.clone(),
        },
    )
    .map_err(plugin_error)?;
    let host_id = registration.id.clone();
    let cancel_manager = state.manager.clone();
    let cancel_plugin = input.plugin_id.clone();
    let cancel_id = host_id.clone();
    let mut cancellation = RequestCancellation {
        context: context.clone(),
        cancel: Some(Box::new(move || {
            let _ = cancel_manager.cancel(&cancel_plugin, &cancel_id);
        })),
    };
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let _permit = permit;
        budget.check()?;
        let timeout_ms = budget.remaining().map(|remaining| {
            u64::try_from(
                remaining
                    .as_millis()
                    .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0)),
            )
            .unwrap_or(u64::MAX)
            .max(1)
        });
        state.manager.call_with_timeout_context(
            &input.plugin_id,
            &host_id,
            &input.method,
            input.params,
            timeout_ms,
            Some(context),
        )
    })
    .await;
    if result.is_ok() {
        cancellation.completed();
    }
    let value = result
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message":"Plugin call failed"})),
            )
        })?
        .map_err(plugin_error)?;
    Ok(Json(value))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginCancel {
    plugin_id: String,
    request_id: String,
}

async fn cancel(
    State(state): State<DownloadState>,
    headers: HeaderMap,
    Json(input): Json<PluginCancel>,
) -> ApiResult {
    allowed_plugin(&state.manager, &state.access, &input.plugin_id)?;
    if !valid_request_id(&input.request_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"message":"Invalid request ID"})),
        ));
    }
    let owner = client_id(&headers)?;
    let id = owned_invocation(&state.access, &owner, &input.plugin_id, &input.request_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"code":"NOT_FOUND", "message":"调用已结束或不属于当前设备。"})),
            )
        })?;
    state
        .manager
        .cancel(&input.plugin_id, &id)
        .map_err(plugin_error)?;
    Ok(Json(Value::Null))
}

#[derive(Deserialize)]
struct EventQuery {
    after: Option<u64>,
}
async fn events(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> ApiResult {
    let owner = client_id(&headers)?;
    // Expiry also enters this stream, even when a client no longer polls /interactions.
    state.access.interactions.expire();
    let roots: BTreeSet<_> = state
        .access
        .plugin_ids
        .iter()
        .filter(|id| allowed_plugin(&state.manager, &state.access, id).is_ok())
        .cloned()
        .collect();
    let batch = state
        .access
        .events
        .lock()
        .map(|buffer| {
            buffer.since_filtered(query.after, |event| {
                event.owner.as_ref().is_none_or(|id| id == &owner)
                    && event.root.as_ref().is_none_or(|root| roots.contains(root))
            })
        })
        .unwrap_or_else(|_| json!({"events":[],"cursor":0,"missed":true}));
    Ok(Json(batch))
}

async fn plugin_asset(
    State(state): State<WebState>,
    Path((key, path)): Path<(String, String)>,
) -> Response {
    if !state.access.active.load(Ordering::Acquire)
        || key != state.access.asset_key
        || !path
            .split('/')
            .next()
            .is_some_and(|id| allowed_plugin(&state.manager, &state.access, id).is_ok())
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let response = crate::plugin_manager::plugin_asset_response(&state.app, &path, "GET");
    let (mut parts, mut body) = response.into_parts();
    if parts
        .headers
        .get("content-type")
        .is_some_and(|value| value.to_str().unwrap_or("").starts_with("text/html"))
    {
        // Existing plugin SDKs use randomUUID, which HTTP LAN origins do not expose.
        body = inject_browser_compatibility(&body);
        parts.headers.insert(
            "content-security-policy",
            crate::ui_policy::plugin_csp(path.split('/').next().unwrap_or_default(), Some(&key))
                .parse()
                .unwrap(),
        );
    }
    Response::from_parts(parts, Body::from(body))
}

fn inject_browser_compatibility(body: &[u8]) -> Vec<u8> {
    let html = String::from_utf8_lossy(body);
    let lower = html.to_ascii_lowercase();
    let position = lower
        .find("<head")
        .and_then(|start| lower[start..].find('>').map(|end| start + end + 1))
        .or_else(|| {
            lower
                .find("<!doctype")
                .and_then(|start| lower[start..].find('>').map(|end| start + end + 1))
        })
        .unwrap_or(0);
    let mut patched = html.into_owned();
    patched.insert_str(position, "<script src=\"/web-crypto.js\"></script>");
    patched.into_bytes()
}

fn static_path(path: &str) -> Option<&str> {
    if path == "/" || path == "mobile.html" {
        return Some("mobile.html");
    }
    if path.contains(['%', '\\', ':'])
        || path
            .split('/')
            .any(|part| part == "." || part == ".." || part.is_empty())
    {
        return None;
    }
    if path.starts_with("assets/") || path == "web-crypto.js" {
        Some(path)
    } else {
        None
    }
}

async fn static_asset(State(state): State<WebState>, uri: Uri) -> Response {
    let path = if uri.path() == "/" {
        "/"
    } else {
        uri.path().trim_start_matches('/')
    };
    let Some(path) = static_path(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(asset) = state.app.asset_resolver().get(path.into()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            ("content-type", asset.mime_type),
            ("access-control-allow-origin", "*".to_owned()),
        ],
        asset.bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tokio::io::AsyncReadExt;

    fn access() -> Access {
        Access {
            token: "secret".into(),
            session_id: "session".into(),
            asset_key: "asset".into(),
            active: AtomicBool::new(true),
            plugin_ids: BTreeSet::from(["shared".into()]),
            events: Arc::new(Mutex::new(EventBuffer::default())),
            calls: Arc::new(Semaphore::new(1)),
            interactions: Arc::new(RemoteBroker::default()),
            invocations: Mutex::new(HashMap::new()),
            downloads: DownloadSessions::default(),
        }
    }

    fn snapshot(id: &str, dependencies: Value, remote: bool) -> PluginRuntimeState {
        serde_json::from_value(json!({
            "manifest": {
                "manifestVersion":3,"id":id,"name":id,"version":"1.0.0",
                "hostCompatibility":{"minCoreVersion":"0.0.0","maxCoreVersionExclusive":"999.0.0","protocol":{"minVersion":"1.0.0","maxVersionExclusive":"2.0.0"}},
                "platform":{"os":"windows","architecture":"x86_64","abi":"msvc"},
                "backend":{"entry":"backend/plugin.exe","transport":"stdio-ndjson-v1","supportsServiceContext":true},
                "contract":"contract.json","remoteAccess":remote,"dependencies":dependencies
            },
            "installation":"installed","enabled":true,"runtime":"running",
            "lastError":null,"installationSource":null,"dependencyIssues":["版本期望不符（仅提示）。"]
        })).unwrap()
    }

    fn dependency(id: &str) -> Value {
        json!({"pluginId":id,"versionReq":"^999.0.0","optional":false,"methods":["run"]})
    }

    fn prepared_reader(access: &Arc<Access>, bytes: &[u8]) -> (String, NativeDownload) {
        let parent = std::env::temp_dir().join("wonderland-web-stream-tests");
        std::fs::create_dir_all(&parent).unwrap();
        let (file, mut writer) = crate::byte_streams::TemporaryFile::create(&parent).unwrap();
        writer.write_all(bytes).unwrap();
        drop(writer);
        let context = RemoteContext::new(
            access.interactions.clone(),
            "device-a".into(),
            "shared".into(),
        );
        let result = context
            .download_file("result.bin".into(), file, bytes.len() as u64)
            .unwrap();
        let id = result["downloadId"].as_str().unwrap().to_owned();
        let transfer = access.interactions.download("device-a", &id).unwrap();
        (
            id.clone(),
            NativeDownload::new(access.clone(), "device-a".into(), id, transfer, None).unwrap(),
        )
    }

    #[tokio::test]
    async fn stream_eof_completes_delivery_but_a_dropped_response_keeps_the_result_pending() {
        let access = Arc::new(access());
        let (id, mut reader) = prepared_reader(&access, &vec![7; 1024 * 1024 + 17]);
        let mut first = [0; 16];
        assert_eq!(reader.read(&mut first).await.unwrap(), 16);
        assert!(!access.interactions.upgrade_blockers().is_empty());
        drop(reader);
        assert_eq!(access.interactions.list("device-a")[0]["state"], "ready");
        assert!(!access.interactions.upgrade_blockers().is_empty());
        let transfer = access.interactions.download("device-a", &id).unwrap();
        let mut reader = NativeDownload::new(
            access.clone(),
            "device-a".into(),
            id.clone(),
            transfer,
            None,
        )
        .unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, vec![7; 1024 * 1024 + 17]);
        assert_eq!(reader.read(&mut first).await.unwrap(), 0);
        assert!(access.interactions.upgrade_blockers().is_empty());
        assert_eq!(
            access.interactions.list("device-a")[0]["state"],
            "delivered"
        );
        drop(reader);
        access
            .interactions
            .reply("device-a", &id, Value::Null)
            .unwrap();
    }

    #[tokio::test]
    async fn empty_stream_eof_and_session_revocation_have_distinct_delivery_results() {
        let access = Arc::new(access());
        let (id, mut empty) = prepared_reader(&access, &[]);
        assert_eq!(empty.read(&mut [0]).await.unwrap(), 0);
        assert!(access.interactions.upgrade_blockers().is_empty());
        access
            .interactions
            .reply("device-a", &id, Value::Null)
            .unwrap();
        let (id, reader) = prepared_reader(&access, &[42; 128]);
        drop(reader);
        let transfer = access.interactions.download("device-a", &id).unwrap();
        let (token, nonce) = access.downloads.prepare("device-a", &id).unwrap();
        let mut reader = NativeDownload::new(
            access.clone(),
            "device-a".into(),
            id.clone(),
            transfer,
            Some((token, nonce)),
        )
        .unwrap();
        assert_eq!(reader.read(&mut [0; 8]).await.unwrap(), 8);
        access.downloads.revoke("device-a");
        assert_eq!(
            reader.read(&mut [0; 8]).await.unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        drop(reader);
        assert_eq!(access.interactions.list("device-a")[0]["state"], "ready");
        assert!(!access.interactions.upgrade_blockers().is_empty());
        access
            .interactions
            .reply("device-a", &id, Value::Null)
            .unwrap();
    }

    #[tokio::test]
    async fn local_download_admin_can_handle_an_abandoned_owner_and_cancel_an_active_stream() {
        let access = Arc::new(access());
        let (id, mut reader) = prepared_reader(&access, b"abandoned browser content");
        let path = reader._temporary.path.clone();
        assert_eq!(reader.read(&mut [0; 3]).await.unwrap(), 3);
        assert!(
            access
                .interactions
                .download("different-device", &id)
                .is_err()
        );
        let session = Some((access.interactions.as_ref(), &access.downloads));
        let listed = run_download_admin(session, &["list".into()]).unwrap();
        let item = &listed["downloads"][0];
        assert_eq!(item["id"], id);
        assert_eq!(item["owner"], "device-a");
        assert_eq!(item["state"], "transferring");
        assert_eq!(item["activeTransfers"], 1);
        let serialized = serde_json::to_string(&listed).unwrap();
        for omitted in [
            "abandoned browser content",
            "contentBase64",
            "token",
            "path",
        ] {
            assert!(!serialized.contains(omitted));
        }
        assert!(access.interactions.close_if_idle().is_err());
        let (token, nonce) = access.downloads.prepare("device-a", &id).unwrap();
        let args = ["discard".into(), id.clone(), "--yes".into()];
        assert_eq!(
            run_download_admin(session, &args).unwrap()["discarded"],
            true
        );
        assert!(!access.downloads.authorized("device-a", &token, &id, &nonce));
        assert_eq!(
            reader.read(&mut [0; 3]).await.unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        drop(reader);
        assert!(!path.exists());
        assert_eq!(
            run_download_admin(session, &["list".into()]).unwrap()["downloads"],
            json!([])
        );
        assert!(access.interactions.upgrade_blockers().is_empty());
        access.interactions.close_if_idle().unwrap();
        assert_eq!(
            run_download_admin(session, &args).unwrap_err().code,
            "NOT_FOUND"
        );
    }

    #[test]
    fn local_download_discard_requires_the_exact_command_and_confirmation() {
        let access = Arc::new(access());
        let (id, reader) = prepared_reader(&access, b"keep");
        drop(reader);
        let session = Some((access.interactions.as_ref(), &access.downloads));
        for args in [
            vec!["discard".into(), id.clone()],
            vec!["discard".into(), id.clone(), "--no".into()],
            vec!["discard".into(), "../file".into(), "--yes".into()],
            vec!["discard".into(), id.clone(), "--yes".into(), "extra".into()],
        ] {
            assert_eq!(
                run_download_admin(session, &args).unwrap_err().code,
                "INVALID_REQUEST"
            );
            assert_eq!(
                access
                    .interactions
                    .admin_downloads()
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
        assert_eq!(
            run_download_admin(None, &["list".into()]).unwrap()["downloads"],
            json!([])
        );
        assert_eq!(
            run_download_admin(None, &["discard".into(), id.clone(), "--yes".into()])
                .unwrap_err()
                .code,
            "WEB_ACCESS_NOT_RUNNING"
        );
        run_download_admin(session, &["discard".into(), id, "--yes".into()]).unwrap();
    }

    #[test]
    fn changed_download_file_cannot_be_marked_delivered() {
        let access = Arc::new(access());
        let (id, reader) = prepared_reader(&access, b"abc");
        drop(reader);
        let transfer = access.interactions.download("device-a", &id).unwrap();
        std::fs::write(&transfer.file.path, b"a").unwrap();
        assert!(
            NativeDownload::new(
                access.clone(),
                "device-a".into(),
                id.clone(),
                transfer,
                None
            )
            .is_err()
        );
        assert_eq!(access.interactions.list("device-a")[0]["state"], "ready");
        assert!(!access.interactions.upgrade_blockers().is_empty());
        access
            .interactions
            .reply("device-a", &id, Value::Null)
            .unwrap();
    }

    #[tokio::test]
    async fn abandoning_an_http_call_future_cancels_the_context_and_worker() {
        let access = Arc::new(access());
        let context = Arc::new(RemoteContext::new(
            access.interactions.clone(),
            "device-a".into(),
            "shared".into(),
        ));
        let worker_cancelled = Arc::new(AtomicBool::new(false));
        let signal = worker_cancelled.clone();
        let inside = context.clone();
        let (ready, started) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _cancellation = RequestCancellation {
                context: inside,
                cancel: Some(Box::new(move || {
                    signal.store(true, Ordering::Release);
                })),
            };
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(context.is_cancelled());
        assert!(worker_cancelled.load(Ordering::Acquire));
        let completed = Arc::new(RemoteContext::new(
            access.interactions.clone(),
            "device-a".into(),
            "shared".into(),
        ));
        let mut guard = RequestCancellation {
            context: completed.clone(),
            cancel: Some(Box::new(|| panic!("completed call must not cancel"))),
        };
        guard.completed();
        drop(guard);
        assert!(!completed.is_cancelled());
    }

    #[tokio::test]
    async fn a_closed_http_connection_drops_the_pending_call_cancellation_guard() {
        use tokio::io::AsyncWriteExt;
        #[derive(Clone)]
        struct PendingCall {
            context: Arc<RemoteContext>,
            ready: Arc<tokio::sync::Notify>,
            cancelled: Arc<tokio::sync::Notify>,
        }
        async fn pending_call(State(state): State<PendingCall>) -> ApiResult {
            let cancelled = state.cancelled.clone();
            let _guard = RequestCancellation {
                context: state.context,
                cancel: Some(Box::new(move || cancelled.notify_one())),
            };
            state.ready.notify_one();
            std::future::pending().await
        }
        let access = Arc::new(access());
        let state = PendingCall {
            context: Arc::new(RemoteContext::new(
                access.interactions.clone(),
                "device-a".into(),
                "shared".into(),
            )),
            ready: Arc::new(tokio::sync::Notify::new()),
            cancelled: Arc::new(tokio::sync::Notify::new()),
        };
        let router = Router::new()
            .route("/call", post(pending_call))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, stop) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stop.await;
                })
                .await
                .unwrap();
        });
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client.write_all(format!("POST /call HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), state.ready.notified())
            .await
            .unwrap();
        drop(client);
        tokio::time::timeout(Duration::from_secs(1), state.cancelled.notified())
            .await
            .unwrap();
        assert!(state.context.is_cancelled());
        shutdown.send(()).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn native_download_routes_require_browser_credentials_and_revoke_them_online() {
        use wonderland_kernel::AppContext;
        use wonderland_net::HttpHeader;

        async fn request(
            url: String,
            method: &str,
            headers: Vec<HttpHeader>,
        ) -> (u16, Vec<HttpHeader>, Vec<u8>) {
            use tokio::io::AsyncWriteExt;
            // A direct TCP client keeps this server test independent of concurrent proxy tests.
            let url = tauri::Url::parse(&url).unwrap();
            let host = url.host_str().unwrap();
            let port = url.port().unwrap();
            let mut socket = tokio::net::TcpStream::connect((host, port)).await.unwrap();
            let mut wire = format!(
                "{method} {} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Length: 0\r\n",
                url.path()
            );
            for header in headers {
                wire.push_str(&format!(
                    "{}: {}\r\n",
                    header.name,
                    std::str::from_utf8(&header.value).unwrap()
                ));
            }
            wire.push_str("\r\n");
            socket.write_all(wire.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
                .await
                .unwrap()
                .unwrap();
            let end = response
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .unwrap();
            let mut lines = std::str::from_utf8(&response[..end]).unwrap().split("\r\n");
            let status = lines
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse()
                .unwrap();
            let headers = lines
                .map(|line| {
                    let (name, value) = line.split_once(':').unwrap();
                    HttpHeader {
                        name: name.to_ascii_lowercase(),
                        value: value.trim().as_bytes().to_vec(),
                    }
                })
                .collect::<Vec<_>>();
            let mut remaining = &response[end + 4..];
            let mut body = Vec::new();
            if headers
                .iter()
                .any(|header| header.name == "transfer-encoding" && header.value == b"chunked")
            {
                loop {
                    let line = remaining
                        .windows(2)
                        .position(|bytes| bytes == b"\r\n")
                        .unwrap();
                    let length = usize::from_str_radix(
                        std::str::from_utf8(&remaining[..line])
                            .unwrap()
                            .split(';')
                            .next()
                            .unwrap(),
                        16,
                    )
                    .unwrap();
                    remaining = &remaining[line + 2..];
                    if length == 0 {
                        break;
                    }
                    body.extend_from_slice(&remaining[..length]);
                    assert_eq!(&remaining[length..length + 2], b"\r\n");
                    remaining = &remaining[length + 2..];
                }
            } else {
                body.extend_from_slice(remaining);
            }
            (status, headers, body)
        }
        fn header(name: &str, value: &str) -> HttpHeader {
            HttpHeader {
                name: name.into(),
                value: value.as_bytes().to_vec(),
            }
        }

        let root =
            std::env::temp_dir().join(format!("wla-download-routes-{}", random_key().unwrap()));
        std::fs::create_dir(&root).unwrap();
        let manager = PluginManager::load(
            &AppContext {
                app_data_dir: root.clone(),
                documents_dir: root.clone(),
                settings: None,
                user_data: None,
            },
            Arc::new(crate::host_environment::HeadlessEnvironment),
            tokio::runtime::Handle::current(),
            true,
        )
        .unwrap();
        let access = Arc::new(access());
        let owner = "a".repeat(64);
        let context =
            RemoteContext::new(access.interactions.clone(), owner.clone(), "shared".into());
        let expected = vec![23; 1024 * 1024 + 17];
        let (file, mut writer) = crate::byte_streams::TemporaryFile::create(&root).unwrap();
        writer.write_all(&expected).unwrap();
        drop(writer);
        let result = context
            .download_file("test.bin".into(), file, expected.len() as u64)
            .unwrap();
        let id = result["downloadId"].as_str().unwrap();
        let state = DownloadState {
            manager,
            access: access.clone(),
        };
        let api = download_api_routes()
            .layer(middleware::from_fn_with_state(access.clone(), authenticate));
        let router = Router::new()
            .nest("/api", api)
            .merge(native_download_routes())
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, stop) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stop.await;
                })
                .await
                .unwrap();
        });
        let main_headers = vec![
            header("authorization", "Bearer secret"),
            header("x-wonderland-client", &owner),
            header("origin", &origin),
        ];
        let prepare_url = format!("{origin}/api/interactions/{id}/prepare");
        assert_eq!(
            request(prepare_url.clone(), "POST", Vec::new()).await.0,
            401
        );
        let (status, headers, body) =
            request(prepare_url.clone(), "POST", main_headers.clone()).await;
        assert_eq!(status, 200);
        let set_cookie = std::str::from_utf8(
            &headers
                .iter()
                .find(|header| header.name == "set-cookie")
                .unwrap()
                .value,
        )
        .unwrap();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        assert!(set_cookie.contains(&format!("Path=/downloads/{}/{owner}/", access.session_id)));
        assert!(!set_cookie.contains("; Secure"));
        let browser_cookie = set_cookie.split(';').next().unwrap().to_owned();
        let download_url = format!(
            "{origin}{}",
            serde_json::from_slice::<Value>(&body).unwrap()["url"]
                .as_str()
                .unwrap()
        );
        assert!(!download_url.contains(&browser_cookie));
        assert_eq!(
            request(download_url.clone(), "GET", Vec::new()).await.0,
            401
        );
        assert_eq!(
            request(
                download_url.clone(),
                "GET",
                vec![header("cookie", &browser_cookie), header("origin", "null")]
            )
            .await
            .0,
            401
        );
        assert_eq!(
            request(
                download_url.clone(),
                "GET",
                vec![
                    header("cookie", &browser_cookie),
                    header("sec-fetch-site", "cross-site")
                ]
            )
            .await
            .0,
            401
        );
        let wrong_headers = vec![
            header("authorization", "Bearer secret"),
            header("x-wonderland-client", &"b".repeat(64)),
        ];
        assert_eq!(
            request(prepare_url.clone(), "POST", wrong_headers).await.0,
            404
        );
        let (status, headers, bytes) = request(
            download_url.clone(),
            "GET",
            vec![header("cookie", &browser_cookie)],
        )
        .await;
        assert_eq!(status, 200);
        assert!(headers.iter().any(|header| header.name == "content-disposition" && header.value == b"attachment"));
        assert_eq!(bytes, expected);
        assert!(access.interactions.upgrade_blockers().is_empty());
        assert_eq!(access.interactions.list(&owner)[0]["state"], "delivered");
        // A download-only cookie cannot authenticate any ordinary API, including revocation.
        let revoke_url = format!("{origin}/api/downloads/revoke");
        assert_eq!(
            request(
                revoke_url.clone(),
                "POST",
                vec![
                    header("cookie", &browser_cookie),
                    header("x-wonderland-client", &owner)
                ]
            )
            .await
            .0,
            401
        );
        let (status, headers, body) = request(revoke_url, "POST", main_headers).await;
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["revoked"],
            true
        );
        assert!(headers.iter().any(|header| {
            header.name == "set-cookie"
                && std::str::from_utf8(&header.value)
                    .unwrap()
                    .contains("Max-Age=0")
        }));
        assert_eq!(
            request(download_url, "GET", vec![header("cookie", &browser_cookie)])
                .await
                .0,
            401
        );
        access.interactions.reply(&owner, id, Value::Null).unwrap();
        shutdown.send(()).unwrap();
        server.await.unwrap();
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "Requires the actual direct-plugin fixture"]
    fn cold_remote_launch_authentication_calls_and_owner_bound_cancellation() {
        use crate::plugin_manager::direct_process_tests::Fixture;
        use wonderland_plugin_protocol::RuntimeState;

        async fn request(
            address: std::net::SocketAddr,
            method: &str,
            path: &str,
            key: bool,
            owner: &str,
            body: Value,
        ) -> (u16, Value) {
            use tokio::io::AsyncWriteExt;
            let body = if method == "GET" {
                String::new()
            } else {
                body.to_string()
            };
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let wire = format!(
                "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nX-Wonderland-Client: {owner}\r\n{}Content-Length: {}\r\n\r\n{body}",
                if key {
                    "Authorization: Bearer secret\r\n"
                } else {
                    ""
                },
                body.len()
            );
            socket.write_all(wire.as_bytes()).await.unwrap();
            let mut bytes = Vec::new();
            tokio::time::timeout(Duration::from_secs(8), socket.read_to_end(&mut bytes))
                .await
                .unwrap()
                .unwrap();
            let end = bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
            (
                status,
                if bytes.len() == end + 4 {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes[end + 4..]).unwrap()
                },
            )
        }

        let fixture = Fixture::cold();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut policy = access();
            policy.plugin_ids = BTreeSet::from(["a".into()]);
            let access = Arc::new(policy);
            let state = DownloadState { manager: fixture.manager.clone(), access: access.clone() };
            let router = Router::new()
                .route("/api/launch/{id}", get(launch))
                .route("/api/call", post(call))
                .route("/api/cancel", post(cancel))
                .layer(middleware::from_fn_with_state(access.clone(), authenticate))
                .with_state(state);
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let (shutdown, stop) = oneshot::channel();
            let server = tokio::spawn(async move {
                axum::serve(listener, router).with_graceful_shutdown(async { let _ = stop.await; }).await.unwrap();
            });
            let owner = "a".repeat(64);
            let foreign = "b".repeat(64);
            assert_eq!(request(address, "GET", "/api/launch/a?requestId=no-auth", false, &owner, Value::Null).await.0, 401);
            assert_eq!(request(address, "GET", "/api/launch/b?requestId=not-shared", true, &owner, Value::Null).await.0, 403);
            let forbidden = request(address, "POST", "/api/call", true, &owner, json!({"pluginId":"a","requestId":"local-only","method":"restore","params":{"required":true}})).await;
            assert_eq!(forbidden.1["code"], "LOCAL_ONLY");
            assert!(fixture.manager.snapshots().iter().all(|s| s.runtime == RuntimeState::Stopped));

            let launched = request(address, "GET", "/api/launch/a?requestId=first-ui", true, &owner, Value::Null).await;
            assert_eq!(launched.0, 200, "{launched:?}");
            assert!(launched.1["url"].as_str().unwrap().contains("/plugin-assets/"));
            assert_eq!(fixture.snapshot("a").runtime, RuntimeState::Running);
            assert_eq!(fixture.snapshot("b").runtime, RuntimeState::Stopped);
            let called = request(address, "POST", "/api/call", true, &owner, json!({"pluginId":"a","requestId":"chain","method":"echo","params":{"text":"cold remote chain"}})).await;
            assert_eq!(called, (200, json!("cold remote chain")));
            assert_eq!(fixture.snapshot("b").runtime, RuntimeState::Running);
            assert_eq!(fixture.snapshot("c").runtime, RuntimeState::Running);

            fixture.enabled("a", false);
            fixture.enabled("a", true);
            let data = fixture.root.join("plugin-data/a");
            std::fs::write(data.join("pause-before-hello"), b"owned HTTP test").unwrap();
            let caller = owner.clone();
            let pending = tokio::spawn(async move { request(address, "GET", "/api/launch/a?requestId=cancel-ui", true, &caller, Value::Null).await });
            fixture.wait_until(|| std::fs::read_dir(&data).unwrap().flatten().filter(|entry| entry.file_name().to_string_lossy().starts_with("started-")).count() == 2);
            assert_eq!(request(address, "POST", "/api/cancel", true, &foreign, json!({"pluginId":"a","requestId":"cancel-ui"})).await.0, 404);
            assert_eq!(request(address, "POST", "/api/cancel", true, &owner, json!({"pluginId":"a","requestId":"cancel-ui"})).await.0, 200);
            assert_eq!(pending.await.unwrap().1["code"], "CANCELLED");
            fixture.enabled("a", false);
            std::fs::remove_file(data.join("pause-before-hello")).unwrap();
            shutdown.send(()).unwrap();
            server.await.unwrap();
        });
    }

    #[test]
    fn shared_root_delegates_transitive_dependencies_without_exposing_provider_entries() {
        let snapshots = vec![
            snapshot("shared", json!([dependency("b")]), true),
            snapshot("b", json!([dependency("c")]), false),
            snapshot("c", json!([dependency("shared")]), false),
            snapshot("private", json!([]), true),
        ];
        let access = access();
        assert_eq!(
            remote_dependency_scope(&snapshots, "shared"),
            BTreeSet::from(["shared".into(), "b".into(), "c".into()])
        );
        for id in ["b", "c"] {
            assert!(allowed_dependency(&access, &snapshots, "shared", id).is_ok());
            assert!(allowed(&access, id).is_err()); // launch/call/assets/cancel direct entry
        }
        assert!(allowed_dependency(&access, &snapshots, "private", "private").is_err());
        assert!(allowed_dependency(&access, &snapshots, "shared", "private").is_err());
        let mut missing_context = snapshots.clone();
        missing_context[1].manifest.backend.supports_service_context = None;
        assert!(allowed_dependency(&access, &missing_context, "shared", "b").is_err());
        assert!(allowed_dependency(&access, &missing_context, "shared", "c").is_err());
    }

    #[test]
    fn prepared_download_survives_stopped_providers_without_exposing_their_entries() {
        let access = access();
        let root = RemoteContext::new(
            access.interactions.clone(),
            "device-a".into(),
            "shared".into(),
        );
        let result = root
            .for_plugin("b")
            .service(
                "core.files.export",
                &json!({"name":"result.json","contentBase64":"YWJj"}),
            )
            .unwrap();
        let id = result["downloadId"].as_str().unwrap();
        let mut snapshots = vec![
            snapshot("shared", json!([dependency("b")]), true),
            snapshot("b", json!([]), false),
        ];
        for snapshot in &mut snapshots {
            snapshot.runtime = wonderland_plugin_protocol::RuntimeState::Stopped;
        }
        let (root_id, provider_id, kind) = access.interactions.identity("device-a", id).unwrap();
        assert!(allowed_owned_action(&access, &snapshots, &root_id, &provider_id, &kind).is_ok());
        assert!(allowed_owned_action(&access, &snapshots, "shared", "b", "pick").is_err());
        assert!(allowed_owned_action(&access, &snapshots, "private", "b", "download").is_err());
        assert!(ensure_remote_snapshot(&snapshots, "shared").is_ok());
        snapshots[0].enabled = false;
        assert!(ensure_remote_snapshot(&snapshots, "shared").is_err());
        assert!(allowed(&access, "b").is_err()); // direct launch/call/assets remain private
        assert!(access.interactions.identity("device-b", id).is_none());
        assert!(access.interactions.download("device-b", id).is_err());
        let transfer = access.interactions.download("device-a", id).unwrap();
        assert_eq!(std::fs::read(&transfer.file.path).unwrap(), b"abc");
        access
            .interactions
            .delivery("device-a", id, &transfer.transfer_id, true)
            .unwrap();
        access
            .interactions
            .reply("device-a", id, Value::Null)
            .unwrap();
    }

    #[test]
    fn notification_cursor_filters_other_devices_and_retains_a_complete_resync_signal() {
        let mut buffer = EventBuffer::default();
        buffer.push_scoped(
            Some("device-a".into()),
            Some("shared".into()),
            json!({"kind":"interaction_change","payload":{"id":"a"}}),
        );
        buffer.push_scoped(
            Some("device-b".into()),
            Some("shared".into()),
            json!({"kind":"interaction_change","payload":{"id":"b"}}),
        );
        buffer.push(json!(WebNotification::PluginState));
        let batch = buffer.since_filtered(Some(0), |event| {
            event
                .owner
                .as_deref()
                .is_none_or(|owner| owner == "device-a")
        });
        assert_eq!(batch["cursor"], 3);
        assert_eq!(batch["missed"], false);
        assert_eq!(batch["events"].as_array().unwrap().len(), 2);
        assert_eq!(batch["events"][0]["payload"]["id"], "a");
        buffer.sequence += 1;
        assert_eq!(buffer.since_filtered(Some(3), |_| true)["missed"], true);
    }

    #[test]
    fn remote_request_events_and_cancellation_are_bound_to_owner_and_root() {
        let access = Arc::new(access());
        let invocation = |owner: &str, root: &str| RemoteInvocation {
            owner: owner.into(),
            root: root.into(),
            request_id: "same-request".into(),
            context: Arc::new(RemoteContext::new(
                access.interactions.clone(),
                owner.into(),
                root.into(),
            )),
        };
        let first = register_invocation(&access, invocation("device-a", "shared")).unwrap();
        let second = register_invocation(&access, invocation("device-b", "shared")).unwrap();
        assert_ne!(first.id, second.id);
        assert!(register_invocation(&access, invocation("device-a", "shared")).is_err());
        assert_eq!(
            owned_invocation(&access, "device-a", "shared", "same-request"),
            Some(first.id.clone())
        );
        assert_eq!(
            owned_invocation(&access, "device-c", "shared", "same-request"),
            None
        );
        assert_eq!(
            owned_invocation(&access, "device-a", "b", "same-request"),
            None
        );
        let mut event = json!({"pluginId":"shared","requestId":first.id});
        assert_eq!(
            event_owner(&access, &mut event),
            Some(Some("device-a".into()))
        );
        assert_eq!(event["requestId"], "same-request");
        assert_eq!(
            event_owner(&access, &mut json!({"pluginId":"b","requestId":first.id})),
            None
        );
        assert_eq!(
            event_owner(
                &access,
                &mut json!({"pluginId":"shared","requestId":"web-expired"})
            ),
            None
        );
        drop(first);
        assert_eq!(
            owned_invocation(&access, "device-a", "shared", "same-request"),
            None
        );
    }

    #[test]
    fn enumerated_addresses_are_live_ipv4_deduplicated_and_ready_to_copy() {
        fn interface(name: &str, ip: &str, up: bool) -> if_addrs::Interface {
            if_addrs::Interface {
                name: name.into(),
                addr: if_addrs::IfAddr::V4(if_addrs::Ifv4Addr {
                    ip: ip.parse().unwrap(),
                    netmask: Ipv4Addr::new(255, 255, 255, 0),
                    prefixlen: 24,
                    broadcast: None,
                }),
                index: None,
                is_p2p: false,
                oper_status: if up {
                    if_addrs::IfOperStatus::Up
                } else {
                    if_addrs::IfOperStatus::Down
                },
                #[cfg(windows)]
                adapter_name: name.into(),
            }
        }
        let addresses = lan_addresses(
            vec![
                interface("Wi-Fi", "192.168.1.20", true),
                interface("duplicate", "192.168.1.20", true),
                interface("Ethernet", "10.0.0.5", true),
                interface("disconnected", "192.168.2.20", false),
                interface("loopback", "127.0.0.1", true),
                interface("APIPA", "169.254.1.1", true),
            ],
            17890,
        );
        assert_eq!(addresses.len(), 2);
        assert_eq!(addresses[0].url, "http://10.0.0.5:17890/");
        assert_eq!(addresses[1].url, "http://192.168.1.20:17890/");
        for ip in [
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
        ] {
            assert!(!usable_ipv4(ip.parse().unwrap()));
        }
        assert!(lan_addresses(Vec::new(), 17890).is_empty());
    }

    #[test]
    fn event_stream_reports_skips_even_when_the_buffer_is_not_full() {
        let mut buffer = EventBuffer::default();
        buffer.push(json!({"n":1}));
        buffer.sequence += 1; // oversized event
        assert_eq!(buffer.since(Some(1))["missed"], true);
        buffer.push(json!({"n":3}));
        assert_eq!(buffer.since(Some(1))["missed"], true);
        assert_eq!(buffer.since(Some(1))["events"], json!([{"n":3}]));
        assert_eq!(buffer.since(Some(3))["missed"], false);
    }

    #[test]
    fn compatibility_script_preserves_doctype_and_precedes_modules() {
        let html = b"<!DOCTYPE html><html><head lang=\"zh\"><script type=\"module\" src=\"main.js\"></script></head></html>";
        let output = String::from_utf8(inject_browser_compatibility(html)).unwrap();
        assert!(output.starts_with("<!DOCTYPE html>"));
        assert!(output.find("web-crypto.js").unwrap() < output.find("main.js").unwrap());
    }

    #[test]
    fn requires_bearer_and_rejects_revoked_or_cross_site_access() {
        let access = access();
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, &access));
        headers.insert("cookie", "token=secret".parse().unwrap());
        assert!(!authorized(&headers, &access));
        headers.insert("authorization", "Bearer secret".parse().unwrap());
        assert!(authorized(&headers, &access));
        headers.insert("origin", "null".parse().unwrap());
        assert!(!authorized(&headers, &access));
        headers.remove("origin");
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!authorized(&headers, &access));
        headers.remove("sec-fetch-site");
        access.active.store(false, Ordering::Release);
        assert!(!authorized(&headers, &access));
    }

    #[test]
    fn optional_keys_allow_direct_access_and_support_utf8_keys() {
        let mut access = access();
        access.token.clear();
        let mut headers = HeaderMap::new();
        assert!(authorized(&headers, &access));
        access.active.store(false, Ordering::Release);
        assert!(!authorized(&headers, &access));
        access.active.store(true, Ordering::Release);
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!authorized(&headers, &access));
        headers.remove("sec-fetch-site");
        access.token = "我的 Core #密钥".into();
        assert!(!authorized(&headers, &access));
        headers.insert(
            "x-wonderland-key",
            base64::engine::general_purpose::STANDARD
                .encode(access.token.as_bytes())
                .parse()
                .unwrap(),
        );
        assert!(authorized(&headers, &access));
        headers.insert("x-wonderland-key", "not-base64!".parse().unwrap());
        assert!(!authorized(&headers, &access));
    }

    #[test]
    fn web_options_accept_an_omitted_empty_or_custom_key() {
        let mut value = json!({"mode":"lan","port":17890,"publicUrl":"","pluginIds":[]});
        let options: WebAccessOptions = serde_json::from_value(value.clone()).unwrap();
        assert!(options.access_key.is_empty());
        assert!(validate_options(&options).is_ok());
        for key in ["", "我的 Core #密钥"] {
            value["accessKey"] = json!(key);
            assert!(validate_options(&serde_json::from_value(value.clone()).unwrap()).is_ok());
        }
        for key in ["bad\nkey".to_owned(), "a".repeat(1025)] {
            value["accessKey"] = json!(key);
            assert!(validate_options(&serde_json::from_value(value.clone()).unwrap()).is_err());
        }
    }

    #[test]
    fn restricts_static_files_and_remote_plugins() {
        for path in [
            "../Cargo.toml",
            "assets/../../secret",
            "assets/%2e%2e/a",
            "assets/..\\a",
            "index.html",
            "api/unknown",
            "assets/C:/secret",
        ] {
            assert!(static_path(path).is_none(), "{path}");
        }
        assert_eq!(static_path("/"), Some("mobile.html"));
        assert!(static_path("assets/mobile.js").is_some());
        assert!(allowed(&access(), "shared").is_ok());
        assert!(allowed(&access(), "private").is_err());
        assert!(!valid_request_id("../id"));
    }

    #[test]
    fn validates_proxy_origin_and_binding() {
        let mut options = WebAccessOptions {
            mode: "loopback".into(),
            port: 17890,
            public_url: "https://core.example.com/".into(),
            plugin_ids: BTreeSet::new(),
            access_key: String::new(),
        };
        assert_eq!(
            validate_options(&options).unwrap(),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        for url in [
            "http://example.com",
            "https://a/b",
            "https://user:pass@a",
            "https://a/?token=x",
            "https://a/#token",
        ] {
            options.public_url = url.into();
            assert!(validate_options(&options).is_err(), "{url}");
        }
        options.public_url.clear();
        options.mode = "lan".into();
        assert_eq!(
            validate_options(&options).unwrap(),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        );
        options.port = 0;
        assert!(validate_options(&options).is_err());
    }

    #[test]
    fn event_history_is_bounded_and_reports_loss() {
        let mut buffer = EventBuffer::default();
        for n in 0..300 {
            buffer.push(json!({"n":n}));
        }
        assert_eq!(buffer.items.len(), MAX_EVENTS);
        assert_eq!(buffer.since(None)["events"], json!([]));
        assert_eq!(buffer.since(Some(0))["missed"], true);
        assert_eq!(buffer.since(Some(299))["events"], json!([{"n":299}]));
    }
}
