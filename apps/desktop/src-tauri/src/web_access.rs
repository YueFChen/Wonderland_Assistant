//! Opt-in HTTP host. Remote clients get only explicitly shared plugin capabilities;
//! native account, filesystem, installation and window commands never enter this router.
use std::collections::{BTreeSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
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
use tokio::sync::{Semaphore, oneshot};
use wonderland_plugin_protocol::PluginError;

use crate::plugin_manager::PluginManager;
use crate::remote_interaction::{RemoteBroker, RemoteContext};

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
        match args {
            [command] if command == "status" => {}
            [command, confirm] if command == "stop" && confirm == "--yes" => {
                service.stop(app).await
            }
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
                    "Usage: wla web status | start --config-json <json> --yes | stop --yes",
                ));
            }
        }
        Ok(json!(service.status().await))
    })
}

struct RunningServer {
    options: WebAccessOptions,
    port: u16,
    access: Arc<Access>,
    shutdown: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<()>,
    event_listener: tauri::EventId,
}

struct Access {
    token: String,
    session_id: String,
    asset_key: String,
    active: AtomicBool,
    plugin_ids: BTreeSet<String>,
    events: Mutex<EventBuffer>,
    calls: Arc<Semaphore>,
    interactions: Arc<RemoteBroker>,
}

#[derive(Default)]
struct EventBuffer {
    sequence: u64,
    items: VecDeque<(u64, Value)>,
}

#[derive(Deserialize)]
struct EventIdentity {
    #[serde(rename = "pluginId")]
    plugin_id: String,
}

impl EventBuffer {
    fn push(&mut self, event: Value) {
        self.sequence += 1;
        self.items.push_back((self.sequence, event));
        while self.items.len() > MAX_EVENTS {
            self.items.pop_front();
        }
    }

    fn since(&self, after: Option<u64>) -> Value {
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
            "events": self.items.iter().filter(|(id, _)| *id > after).map(|(_, event)| event).collect::<Vec<_>>()
        })
    }
}

#[derive(Clone)]
struct WebState {
    app: AppHandle,
    manager: PluginManager,
    access: Arc<Access>,
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
        let access = Arc::new(Access {
            token: options.access_key.clone(),
            session_id: random_key()?,
            asset_key: random_key()?,
            active: AtomicBool::new(true),
            plugin_ids: options.plugin_ids.clone(),
            events: Mutex::new(EventBuffer::default()),
            calls: Arc::new(Semaphore::new(16)),
            interactions: Arc::new(RemoteBroker::default()),
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
                && let Ok(mut buffer) = event_access.events.lock()
            {
                if let Some(id) = payload
                    .get("requestId")
                    .and_then(Value::as_str)
                    .and_then(|id| id.strip_prefix("web-"))
                {
                    payload["requestId"] = json!(id.to_owned());
                }
                buffer.push(payload);
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
            event_listener,
        });
        Ok(())
    }

    async fn stop(&self, app: &AppHandle) {
        let mut guard = self.running.lock().await;
        if let Some(mut running) = guard.take() {
            running.access.active.store(false, Ordering::Release);
            running.access.interactions.close();
            app.unlisten(running.event_listener);
            let _ = running.shutdown.send(());
            if tokio::time::timeout(Duration::from_secs(2), &mut running.task)
                .await
                .is_err()
            {
                running.task.abort();
            }
        }
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
    service.stop(&app).await;
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
        .layer(middleware::from_fn_with_state(
            state.access.clone(),
            authenticate,
        ));
    Router::new()
        .nest("/api", api)
        .route("/plugin-assets/{key}/{*path}", get(plugin_asset))
        .fallback(get(static_asset))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
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
    if manager
        .snapshots()
        .iter()
        .any(|s| s.manifest.id == id && s.manifest.supports_remote_access())
    {
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

fn allowed_plugin(state: &WebState, id: &str) -> Result<(), (StatusCode, Json<Value>)> {
    allowed(&state.access, id)?;
    ensure_remote_plugin(&state.manager, id)
}

fn allowed_interaction(
    state: &WebState,
    owner: &str,
    id: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    let plugin = state.access.interactions.plugin(owner, id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"message":"交互已过期或不属于当前设备。"})),
        )
    })?;
    allowed_plugin(state, &plugin)
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
    actions.as_array_mut().unwrap().retain(|a| {
        a["pluginId"]
            .as_str()
            .is_some_and(|id| allowed_plugin(&state, id).is_ok())
    });
    Ok(Json(actions))
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
    Ok(Json(Value::Null))
}

async fn interaction_content(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    allowed_interaction(&state, &client_id(&headers)?, &id)?;
    let bytes = state
        .access
        .interactions
        .download(&client_id(&headers)?, &id)
        .map_err(plugin_error)?;
    Ok((
        [
            ("content-type", "application/octet-stream"),
            ("content-disposition", "attachment"),
        ],
        bytes,
    )
        .into_response())
}
fn plugin_error(error: PluginError) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!(error)))
}

async fn launch(State(state): State<WebState>, Path(id): Path<String>) -> ApiResult {
    allowed_plugin(&state, &id)?;
    let mut launch = state
        .manager
        .plugin_ui_launch_info(&id)
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
    launch.url = format!("/plugin-assets/{}{path}", state.access.asset_key);
    Ok(Json(json!(launch)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginCall {
    plugin_id: String,
    request_id: String,
    method: String,
    params: Value,
}

fn valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 120
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

async fn call(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(input): Json<PluginCall>,
) -> ApiResult {
    allowed_plugin(&state, &input.plugin_id)?;
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
        owner,
        input.plugin_id.clone(),
    ));
    let value = tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        state.manager.call_with_context(
            &input.plugin_id,
            &format!("web-{}", input.request_id),
            &input.method,
            input.params,
            Some(context),
        )
    })
    .await
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

async fn cancel(State(state): State<WebState>, Json(input): Json<PluginCancel>) -> ApiResult {
    allowed_plugin(&state, &input.plugin_id)?;
    if !valid_request_id(&input.request_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"message":"Invalid request ID"})),
        ));
    }
    state
        .manager
        .cancel(&input.plugin_id, &format!("web-{}", input.request_id))
        .map_err(plugin_error)?;
    Ok(Json(Value::Null))
}

#[derive(Deserialize)]
struct EventQuery {
    after: Option<u64>,
}
async fn events(State(state): State<WebState>, Query(query): Query<EventQuery>) -> Json<Value> {
    let mut batch = state
        .access
        .events
        .lock()
        .map(|buffer| buffer.since(query.after))
        .unwrap_or_else(|_| json!({"events":[],"cursor":0,"missed":true}));
    batch["events"].as_array_mut().unwrap().retain(|event| {
        event["pluginId"]
            .as_str()
            .is_some_and(|id| allowed_plugin(&state, id).is_ok())
    });
    Json(batch)
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
            .is_some_and(|id| allowed_plugin(&state, id).is_ok())
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
        parts.headers.insert("content-security-policy", "sandbox allow-scripts; default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'".parse().unwrap());
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

    fn access() -> Access {
        Access {
            token: "secret".into(),
            session_id: "session".into(),
            asset_key: "asset".into(),
            active: AtomicBool::new(true),
            plugin_ids: BTreeSet::from(["shared".into()]),
            events: Mutex::new(EventBuffer::default()),
            calls: Arc::new(Semaphore::new(1)),
            interactions: Arc::new(RemoteBroker::default()),
        }
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
