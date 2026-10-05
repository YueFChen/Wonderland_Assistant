//! HTTP clients for plugin requests, Core accounts and signed package downloads.

use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::de::DeserializeOwned;
use wonderland_kernel::KernelError;
use wonderland_kernel::logging::{debug, warn};

pub mod proxy;
pub use proxy::{
    ProxyConfiguration, ProxyCredentials, ProxyEnvironmentVariable, ProxyError, ProxyMode,
    ProxySettings,
};

/// 出站请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Keep Core-mediated plugin responses below the plugin protocol's 32 MiB frame limit after
/// base64 encoding, while bounding memory use before serialization.
const MAX_HTTP_RESPONSE_BYTES: usize = 20 * 1024 * 1024;
const PLUGIN_CATALOG_URL: &str =
    "https://yuefchen.github.io/Wonderland_Plugin_Catalog/catalog/v2/index.json";
const PLUGIN_CATALOG_MAX_BYTES: usize = 2 * 1024 * 1024;
const PLUGIN_UPDATE_MANIFEST_MAX_BYTES: usize = 1024 * 1024;
const PLUGIN_PACKAGE_MAX_BYTES: usize = 100 * 1024 * 1024;

/// 固定 UA：米哈游接口对 UA 敏感，不使用随机值以免触发风控。
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";

/// Core account/platform requests keep their own fixed destinations.
///
/// 精确匹配主机名，不接受子域。
const CORE_PLATFORM_HOSTS: &[&str] = &[
    "api-takumi.mihoyo.com",
    "api-micreator.mihoyo.com",
    "bbs-api.miyoushe.com",
    "act-webstatic.mihoyo.com",
];

/// Parse once so the checked host is the same host reqwest will contact.
fn ensure_allowed(raw: &str, platform_only: bool) -> Result<reqwest::Url, KernelError> {
    let url = reqwest::Url::parse(raw).map_err(|_| KernelError::InvalidInput)?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let destination_allowed = if !platform_only {
        matches!(url.scheme(), "http" | "https") && !host.is_empty()
    } else {
        url.scheme() == "https"
            && url.port().is_none()
            && CORE_PLATFORM_HOSTS.contains(&host.as_str())
    };
    if destination_allowed
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
    {
        Ok(url)
    } else {
        warn!(host = %host, "出站主机或 URL 不符合白名单策略");
        Err(KernelError::InvalidInput)
    }
}

/// 出站 HTTP 客户端。
#[derive(Clone)]
pub struct HttpClient {
    cached: Arc<RwLock<Option<(u64, reqwest::Client)>>>,
    platform_only: bool,
}

impl HttpClient {
    pub fn new() -> Result<Self, KernelError> {
        let client = Self {
            platform_only: true,
            ..Self::unrestricted()
        };
        client.inner()?;
        Ok(client)
    }

    /// Plugin networking is a convenience service, not a process sandbox.
    pub fn unrestricted() -> Self {
        Self {
            cached: Arc::new(RwLock::new(None)),
            platform_only: false,
        }
    }

    fn inner(&self) -> Result<reqwest::Client, KernelError> {
        let snapshot = proxy::current_proxy()
            .map_err(|_| KernelError::Transport("代理设置暂不可用".to_owned()))?;
        if let Some((revision, client)) = self
            .cached
            .read()
            .map_err(|_| KernelError::Transport("HTTP 客户端暂不可用".to_owned()))?
            .as_ref()
            && *revision == snapshot.revision
        {
            return Ok(client.clone());
        }

        let builder = reqwest::Client::builder()
            .redirect(if !self.platform_only {
                reqwest::redirect::Policy::limited(10)
            } else {
                reqwest::redirect::Policy::none()
            })
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT);
        let client = snapshot
            .configuration
            .apply_to(builder)
            .build()
            .map_err(|_| KernelError::Transport("HTTP 客户端初始化失败".to_owned()))?;
        *self
            .cached
            .write()
            .map_err(|_| KernelError::Transport("HTTP 客户端暂不可用".to_owned()))? =
            Some((snapshot.revision, client.clone()));
        Ok(client)
    }

    /// GET with bounded retries for transient read-only failures.
    pub async fn get_bytes(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        query: &[(String, String)],
    ) -> Result<Vec<u8>, KernelError> {
        self.request_bytes("GET", url, headers, query, None).await
    }

    /// Account JSON requests retain their single-attempt behavior and error format.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<T, KernelError> {
        let url = ensure_allowed(url, self.platform_only)?;
        let response = self
            .request_builder(reqwest::Method::GET, url, headers, &[], None)?
            .send()
            .await
            .map_err(classify)?;
        let status = response.status();
        if !status.is_success() {
            return Err(KernelError::Transport(format!("HTTP 状态码 {status}")));
        }
        let body = read_limited_body(response, MAX_HTTP_RESPONSE_BYTES).await?;
        serde_json::from_slice(&body)
            .map_err(|error| KernelError::Transport(format!("响应解析失败：{error}")))
    }

    fn request_builder(
        &self,
        method: reqwest::Method,
        url: reqwest::Url,
        headers: &[(&str, &str)],
        query: &[(String, String)],
        body: Option<&str>,
    ) -> Result<reqwest::RequestBuilder, KernelError> {
        let post_json = method == reqwest::Method::POST
            && body.is_some()
            && !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
        let mut request = self.inner()?.request(method, url).query(query);
        if post_json {
            request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        }
        for (name, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| KernelError::InvalidInput)?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| KernelError::InvalidInput)?;
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request.body(body.to_owned());
        }
        Ok(request)
    }

    /// Only a GET without a request body is retried; mutations are sent once.
    pub async fn request_bytes(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        query: &[(String, String)],
        body: Option<&str>,
    ) -> Result<Vec<u8>, KernelError> {
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| KernelError::InvalidInput)?;
        let url = ensure_allowed(url, self.platform_only)?;
        let attempts = if method == reqwest::Method::GET && body.is_none() {
            3
        } else {
            1
        };
        for attempt in 0..attempts {
            let request =
                self.request_builder(method.clone(), url.clone(), headers, query, body)?;
            let result = async {
                let response = request.send().await.map_err(classify)?;
                read_limited_response(response, MAX_HTTP_RESPONSE_BYTES).await
            }
            .await;
            let retry = matches!(
                &result,
                Err(KernelError::Timeout
                    | KernelError::Connection
                    | KernelError::Http(429 | 500 | 502 | 503 | 504))
            );
            if let Err(error) = &result {
                if retry && attempt + 1 < attempts {
                    debug!(
                        code = error.code(),
                        attempt = attempt + 1,
                        "出站请求失败，退避后重试"
                    );
                } else {
                    warn!(code = error.code(), attempts = attempt + 1, "出站请求失败");
                }
            }
            if !retry || attempt + 1 == attempts {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
        }
        unreachable!()
    }
}

/// Fixed-origin catalog and release downloader. Redirects are limited to GitHub's release
/// asset hosts so catalog URLs cannot turn this client into an arbitrary network proxy.
#[derive(Clone)]
pub struct PluginCatalogClient {
    index: reqwest::Client,
    package: reqwest::Client,
}

impl PluginCatalogClient {
    pub fn new() -> Result<Self, KernelError> {
        let proxy = proxy::current_proxy()
            .map_err(|_| KernelError::Transport("代理设置暂不可用".to_owned()))?;
        let index = proxy
            .configuration
            .apply_to(
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(REQUEST_TIMEOUT)
                    .user_agent("WonderlandAssistant/PluginCatalog"),
            )
            .build()
            .map_err(|_| KernelError::Transport("HTTP 客户端初始化失败".to_owned()))?;
        let package = proxy
            .configuration
            .apply_to(
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::custom(|attempt| {
                        if attempt.previous().len() >= 5
                            || !is_allowed_release_redirect(attempt.url())
                        {
                            attempt.stop()
                        } else {
                            attempt.follow()
                        }
                    }))
                    .timeout(Duration::from_secs(120))
                    .user_agent("WonderlandAssistant/PluginCatalog"),
            )
            .build()
            .map_err(|_| KernelError::Transport("HTTP 客户端初始化失败".to_owned()))?;
        Ok(Self { index, package })
    }

    pub async fn get_index(&self) -> Result<Vec<u8>, KernelError> {
        let response = self
            .index
            .get(PLUGIN_CATALOG_URL)
            .send()
            .await
            .map_err(classify)?;
        read_limited_response(response, PLUGIN_CATALOG_MAX_BYTES).await
    }

    pub async fn download_package(&self, url: &str) -> Result<Vec<u8>, KernelError> {
        if !is_valid_plugin_release_url(url) {
            return Err(KernelError::InvalidInput);
        }
        let response = self.package.get(url).send().await.map_err(classify)?;
        read_limited_response(response, PLUGIN_PACKAGE_MAX_BYTES).await
    }

    pub async fn get_update_manifest(&self, url: &str) -> Result<Vec<u8>, KernelError> {
        if !is_valid_plugin_update_manifest_url(url) {
            return Err(KernelError::InvalidInput);
        }
        let response = self.package.get(url).send().await.map_err(classify)?;
        read_limited_response(response, PLUGIN_UPDATE_MANIFEST_MAX_BYTES).await
    }
}

/// Only accept immutable versioned GitHub Release asset URLs, never arbitrary HTTPS URLs.
pub fn is_valid_plugin_release_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(parts) = url.path_segments() else {
        return false;
    };
    let parts = parts.collect::<Vec<_>>();
    parts.len() == 6
        && parts[2] == "releases"
        && parts[3] == "download"
        && !parts[0].is_empty()
        && !parts[1].is_empty()
        && !parts[4].is_empty()
        && parts[0]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && parts[1]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        && parts[4]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        && parts[5].ends_with(".wplug")
        && parts[5]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// Only accept the stable latest-release manifest asset for a GitHub plugin repository.
pub fn is_valid_plugin_update_manifest_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(parts) = url.path_segments() else {
        return false;
    };
    let parts = parts.collect::<Vec<_>>();
    parts.len() == 6
        && parts[2] == "releases"
        && parts[3] == "latest"
        && parts[4] == "download"
        && !parts[0].is_empty()
        && !parts[1].is_empty()
        && parts[5].ends_with("-update.json")
        && parts[0]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && parts[1]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        && parts[5]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn is_allowed_release_redirect(url: &reqwest::Url) -> bool {
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    host == "github.com"
        || host == "release-assets.githubusercontent.com"
        || host == "objects.githubusercontent.com"
        || (host.starts_with("github-production-release-asset-")
            && host.ends_with(".s3.amazonaws.com"))
}

async fn read_limited_response(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, KernelError> {
    let status = response.status();
    if !status.is_success() {
        return Err(KernelError::Http(status.as_u16()));
    }
    read_limited_body(response, max_bytes).await
}

async fn read_limited_body(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, KernelError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(KernelError::ResourceLimit);
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(KernelError::ResourceLimit);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(error) => return Err(classify(error)),
        }
    }
}

/// 错误分类：区分超时、连接失败与其它，便于前端判别。
fn classify(error: reqwest::Error) -> KernelError {
    if error.is_timeout() {
        KernelError::Timeout
    } else if error.is_connect() {
        KernelError::Connection
    } else {
        KernelError::Transport("请求失败".to_owned())
    }
}

/// 模型生成远慢于普通接口，默认给足两分钟；按端点可覆盖。
const MODEL_TIMEOUT: Duration = Duration::from_secs(120);

/// 默认响应体上限 4 MiB：正常对话响应远小于它，超限多半是异常端点。
const MODEL_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// 模型出口错误。
///
/// 变体划分服务于调用方的两个决策：**要不要重试**与**要不要落状态**。
/// `Transient` 可以直接重试；`Invalid` / `Config` 重试无意义；
/// `Indeterminate` 表示"远端可能已经处理"，重试前需要业务侧去重。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// 临时失败：超时、连接失败、HTTP 429 或 5xx。
    ///
    /// `retry_after` 在能解析到 `Retry-After`（整数秒）时给出，供调用方退避。
    Transient {
        status: Option<u16>,
        retry_after: Option<Duration>,
    },
    /// 响应不可用：其它非 2xx、响应体超过大小上限、不是合法 HTTP 响应。
    ///
    /// `String` 是可读原因，**不含响应体正文与密钥**（正文可能回显用户的输入）。
    Invalid(String),
    /// 端点或配置不合法。
    Config(String),
    /// 请求已发出但无法确认远端是否处理（例如收到响应头后读取响应体中断）。
    Indeterminate,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transient {
                status,
                retry_after,
            } => match status {
                Some(status) => {
                    write!(f, "模型端点暂时不可用（HTTP {status}），请稍后重试")?;
                    if let Some(retry_after) = retry_after {
                        write!(f, "，建议 {} 秒后重试", retry_after.as_secs())?;
                    }
                    Ok(())
                }
                None => write!(f, "模型端点连接失败或超时，请稍后重试"),
            },
            Self::Invalid(reason) => write!(f, "模型端点返回的响应无效：{reason}"),
            Self::Config(reason) => write!(f, "模型端点配置无效：{reason}"),
            Self::Indeterminate => write!(f, "模型请求结果未知，远端可能已处理，请勿直接重试"),
        }
    }
}

impl std::error::Error for ModelError {}

/// 用户自配的 OpenAI 兼容模型端点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEndpoint {
    /// 用户填写的基础地址，例如 `https://api.example.com/v1`。
    pub base_url: String,
    /// 单次请求超时。
    pub timeout: Duration,
    /// 响应体大小上限。
    pub max_response_bytes: usize,
}

impl ModelEndpoint {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: MODEL_TIMEOUT,
            max_response_bytes: MODEL_MAX_RESPONSE_BYTES,
        }
    }
}

/// 校验用户填写的模型端点地址，返回可直接用于拼接路径的 [`reqwest::Url`]。
///
/// Accept user-configured HTTP(S) endpoints, including LAN and loopback services.
pub fn validate_endpoint(base_url: &str) -> Result<reqwest::Url, ModelError> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|error| ModelError::Config(format!("地址无法解析：{error}")))?;

    if !url.username().is_empty() || url.password().is_some() {
        return Err(ModelError::Config("地址不得包含用户名或密码".to_owned()));
    }

    url.host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| ModelError::Config("地址缺少主机名".to_owned()))?;

    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(ModelError::Config(format!("不支持的协议：{other}")));
        }
    }

    Ok(url)
}

/// 把相对路径拼到已校验的 base 上。
///
/// 拒绝路径遍历和协议相对路径，并验证结果仍位于 base origin。
fn join_url(base: &reqwest::Url, path: &str) -> Result<reqwest::Url, ModelError> {
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(ModelError::Config(
            "请求路径必须是以 / 开头的绝对路径".to_owned(),
        ));
    }
    if path.split('/').any(|segment| segment == "..") {
        return Err(ModelError::Config("请求路径不得包含 ..".to_owned()));
    }

    let mut url = base.clone();
    let prefix = base.path().trim_end_matches('/');
    url.set_path(&format!("{prefix}{path}"));
    url.set_query(None);
    url.set_fragment(None);

    if url.origin() != base.origin() {
        return Err(ModelError::Config("请求地址与基础地址不同源".to_owned()));
    }
    Ok(url)
}

/// 受控模型出口客户端。
///
/// 与 [`HttpClient`] 并列但独立：不复用主机白名单，也不自动重试。
#[derive(Clone)]
pub struct ModelClient {
    inner: reqwest::Client,
    base: reqwest::Url,
    max_response_bytes: usize,
    origin: String,
}

impl ModelClient {
    pub fn new(endpoint: ModelEndpoint) -> Result<Self, ModelError> {
        let base = validate_endpoint(&endpoint.base_url)?;
        let proxy = proxy::current_proxy()
            .map_err(|_| ModelError::Config("代理设置暂不可用".to_owned()))?;
        let inner = proxy
            .configuration
            .apply_to(
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(endpoint.timeout),
            )
            .build()
            .map_err(|_| ModelError::Config("HTTP 客户端初始化失败".to_owned()))?;
        let origin = base.origin().ascii_serialization();
        Ok(Self {
            inner,
            base,
            max_response_bytes: endpoint.max_response_bytes,
            origin,
        })
    }

    /// 端点 origin（`scheme://host[:port]`），供界面展示；不含路径与 userinfo。
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// 向 `base_url + path` 发送 JSON POST，响应体原样返回。
    ///
    /// 只设置内容类型与 `Authorization`；**不重试**（能否重放由调用方判断）。
    /// 日志只记主机与状态码，绝不记密钥、请求体或响应体。
    pub async fn post_json(
        &self,
        path: &str,
        bearer: &str,
        body: &str,
    ) -> Result<Vec<u8>, ModelError> {
        let url = join_url(&self.base, path)?;
        let host = url.host_str().unwrap_or_default().to_owned();

        let response = self
            .inner
            .post(url)
            .header("content-type", "application/json")
            .bearer_auth(bearer)
            .body(body.to_owned())
            .send()
            .await
            .map_err(|error| classify_model_error(error, &host))?;

        let status = response.status();
        if status.is_success() {
            let body = read_body(response, self.max_response_bytes, &host).await?;
            debug!(host = %host, status = status.as_u16(), "模型端点请求成功");
            return Ok(body);
        }

        let code = status.as_u16();
        if code == 429 || status.is_server_error() {
            let retry_after = parse_retry_after(response.headers());
            warn!(
                host = %host,
                status = code,
                retry_after_secs = retry_after.map(|delay| delay.as_secs()),
                "模型端点暂时不可用"
            );
            return Err(ModelError::Transient {
                status: Some(code),
                retry_after,
            });
        }

        warn!(host = %host, status = code, "模型端点返回非 2xx");
        Err(ModelError::Invalid(format!("服务返回 HTTP {code}")))
    }
}

/// HTTP request send errors.
///
/// 超时与连接失败多为瞬时问题，可重试；其余原因（TLS 协商、请求构造等）
/// 无法确认请求是否已被处理，保守归入 [`ModelError::Indeterminate`]。
fn classify_model_error(error: reqwest::Error, host: &str) -> ModelError {
    if error.is_timeout() {
        warn!(host = %host, "模型端点请求超时");
        ModelError::Transient {
            status: None,
            retry_after: None,
        }
    } else if error.is_connect() {
        warn!(host = %host, "模型端点连接失败");
        ModelError::Transient {
            status: None,
            retry_after: None,
        }
    } else {
        warn!(host = %host, "模型端点请求失败，结果未知");
        ModelError::Indeterminate
    }
}

/// 读取响应体并施加大小上限。
///
/// 分块累加而不是先看 `Content-Length`：后者可能缺失（chunked）也可能被伪造，
/// 只有实际收到的字节数才可靠。中途读取失败意味着已收到响应头却拿不全内容，
/// 远端很可能已经处理了请求 → [`ModelError::Indeterminate`]。
async fn read_body(
    mut response: reqwest::Response,
    max_response_bytes: usize,
    host: &str,
) -> Result<Vec<u8>, ModelError> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > max_response_bytes {
                    warn!(host = %host, "模型端点响应体超过大小上限");
                    return Err(ModelError::Invalid(format!(
                        "响应体超过 {max_response_bytes} 字节上限"
                    )));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(_) => {
                warn!(host = %host, "模型端点响应体读取中断");
                return Err(ModelError::Indeterminate);
            }
        }
    }
}

/// 解析 `Retry-After` 的整数秒形式；HTTP-date 形式不解析（调用方按默认退避处理）。
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let seconds = value.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_requests_keep_fixed_https_destinations() {
        let allowed = ensure_allowed("https://API-TAKUMI.MIHOYO.COM:443/path", true).unwrap();
        assert_eq!(allowed.host_str(), Some("api-takumi.mihoyo.com"));
        for host in CORE_PLATFORM_HOSTS {
            assert!(ensure_allowed(&format!("https://{host}/"), true).is_ok());
        }
        for raw in [
            r"https://evil.com\@api-takumi.mihoyo.com/path",
            "http://api-takumi.mihoyo.com/path",
            "https://user@api-takumi.mihoyo.com/path",
            "https://api-takumi.mihoyo.com:8443/path",
            "https://api-takumi.mihoyo.com/path#fragment",
            "https://api.example.com/resource",
        ] {
            assert!(
                matches!(ensure_allowed(raw, true), Err(KernelError::InvalidInput)),
                "{raw}"
            );
        }
    }

    #[test]
    fn plugin_requests_allow_http_ips_and_ports_but_reject_non_http_urls() {
        for url in [
            "http://127.0.0.1:8080/",
            "https://api.example.com:8443/path",
        ] {
            assert!(ensure_allowed(url, false).is_ok(), "{url}");
        }
        for url in [
            "file:///C:/secret",
            "ftp://example.com/file",
            "https://user@example.com/",
        ] {
            assert!(ensure_allowed(url, false).is_err(), "{url}");
        }
    }

    #[test]
    fn update_manifest_url_is_limited_to_latest_github_release_assets() {
        assert!(is_valid_plugin_update_manifest_url(
            "https://github.com/YueFChen/my_wonderland/releases/latest/download/my_wonderland-update.json"
        ));
        for raw in [
            "http://github.com/YueFChen/my_wonderland/releases/latest/download/my_wonderland-update.json",
            "https://github.com.evil.example/YueFChen/my_wonderland/releases/latest/download/my_wonderland-update.json",
            "https://github.com/YueFChen/my_wonderland/releases/download/v1/my_wonderland-update.json",
            "https://github.com/YueFChen/my_wonderland/releases/latest/download/my_wonderland-update.json?redirect=evil",
            "https://github.com/YueFChen/my_wonderland/releases/latest/download/update.json",
        ] {
            assert!(
                !is_valid_plugin_update_manifest_url(raw),
                "URL should be rejected: {raw}"
            );
        }
    }

    #[tokio::test]
    async fn response_body_limit_is_enforced_without_content_length() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n4\r\nefgh\r\n0\r\n\r\n",
                )
                .unwrap();
        });

        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        assert!(matches!(
            read_limited_body(response, 5).await,
            Err(KernelError::ResourceLimit)
        ));
        server.join().unwrap();
    }

    fn ok(base_url: &str) -> String {
        validate_endpoint(base_url)
            .expect("应当校验通过")
            .to_string()
    }

    fn config_err(base_url: &str) -> String {
        match validate_endpoint(base_url) {
            Err(ModelError::Config(reason)) => reason,
            other => panic!("期望 Config，实际为 {other:?}"),
        }
    }

    #[test]
    fn https_with_and_without_path_prefix_passes() {
        assert_eq!(
            ok("https://api.example.com/v1"),
            "https://api.example.com/v1"
        );
        assert_eq!(ok("https://api.example.com"), "https://api.example.com/");
    }

    #[test]
    fn user_model_endpoints_accept_http_ips_and_custom_ports_without_flags() {
        for url in [
            "http://api.example.com:8080/v1",
            "http://192.168.1.10:11434/v1",
            "http://127.0.0.1:8080/v1",
            "http://localhost:1/v1",
            "http://[::1]:8080/v1",
            "https://api.example.com:8443/v1",
        ] {
            assert_eq!(ok(url), url);
        }
    }

    #[test]
    fn non_http_scheme_userinfo_and_missing_host_are_rejected() {
        config_err("ftp://x/");
        config_err("https://user:pw@api.example.com/v1");
        config_err("");
        config_err("https://");
        config_err("file:///tmp/model");
    }

    #[test]
    fn join_url_keeps_prefix_without_double_slash() {
        let base = validate_endpoint("https://api.example.com/v1").unwrap();
        let joined = join_url(&base, "/chat/completions").unwrap();
        assert_eq!(
            joined.as_str(),
            "https://api.example.com/v1/chat/completions"
        );
        let bare = validate_endpoint("https://api.example.com").unwrap();
        assert_eq!(
            join_url(&bare, "/chat/completions").unwrap().as_str(),
            "https://api.example.com/chat/completions"
        );
        let trailing = validate_endpoint("https://api.example.com/v1/").unwrap();
        assert_eq!(
            join_url(&trailing, "/chat/completions").unwrap().as_str(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn join_url_rejects_traversal_and_scheme_relative_paths() {
        let base = validate_endpoint("https://api.example.com/v1").unwrap();
        assert!(matches!(
            join_url(&base, "/../admin"),
            Err(ModelError::Config(_))
        ));
        assert!(matches!(
            join_url(&base, "//evil.example.com/x"),
            Err(ModelError::Config(_))
        ));
        assert!(matches!(
            join_url(&base, "chat/completions"),
            Err(ModelError::Config(_))
        ));
    }

    #[test]
    fn origin_has_no_path_or_userinfo() {
        let client = ModelClient::new(ModelEndpoint::new("https://api.example.com/v1")).unwrap();
        assert_eq!(client.origin(), "https://api.example.com");
        let endpoint = ModelEndpoint::new("http://127.0.0.1:8080/v1");
        let client = ModelClient::new(endpoint).unwrap();
        assert_eq!(client.origin(), "http://127.0.0.1:8080");
    }
}

#[cfg(test)]
mod plugin_http_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn direct_client() -> HttpClient {
        let client = HttpClient::unrestricted();
        let revision = proxy::current_proxy().unwrap().revision;
        // Loopback integration tests must not depend on the workstation's system proxy.
        *client.cached.write().unwrap() = Some((
            revision,
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::limited(10))
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        ));
        client
    }

    fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        let size = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; size];
        reader.read_exact(&mut body).unwrap();
        (
            headers.to_ascii_lowercase(),
            String::from_utf8(body).unwrap(),
        )
    }

    #[tokio::test]
    async fn plugin_requests_support_auth_custom_headers_post_queries_and_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for path in ["/start?existing=1&q=two", "/next"] {
                let (mut stream, _) = listener.accept().unwrap();
                let (headers, body) = read_request(&mut stream);
                assert!(headers.starts_with(&format!("post {path} http/1.1")));
                assert!(headers.contains("authorization: bearer plugin-key"));
                assert!(headers.contains("x-plugin-custom: yes"));
                assert!(headers.contains("content-type: text/plain"));
                assert!(!headers.contains("application/json"));
                assert_eq!(body, "payload");
                let response = if path.starts_with("/start") {
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let client = direct_client();
        let bytes = client
            .request_bytes(
                "POST",
                &format!("{base}/start?existing=1"),
                &[
                    ("Authorization", "Bearer plugin-key"),
                    ("X-Plugin-Custom", "yes"),
                    ("Content-Type", "text/plain"),
                ],
                &[("q".into(), "two".into())],
                Some("payload"),
            )
            .await
            .unwrap();
        assert_eq!(bytes, b"ok");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn plugin_client_reuses_connections_for_put_patch_and_delete() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            // All three requests must arrive over the same accepted connection.
            let (mut stream, _) = listener.accept().unwrap();
            for method in ["put", "patch", "delete"] {
                let (headers, _) = read_request(&mut stream);
                assert!(headers.starts_with(&format!("{method} / http/1.1")));
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .unwrap();
            }
        });
        let client = direct_client();
        for method in ["PUT", "PATCH", "DELETE"] {
            assert_eq!(
                client
                    .request_bytes(method, &url, &[], &[], None)
                    .await
                    .unwrap(),
                b"ok"
            );
        }
        server.join().unwrap();
    }

    #[tokio::test]
    async fn transient_failures_retry_only_get_without_a_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for (method, status, expected_body) in [
                ("get", "503 Service Unavailable", ""),
                ("get", "200 OK", ""),
                ("post", "503 Service Unavailable", "{}"),
                ("get", "503 Service Unavailable", "payload"),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let (headers, body) = read_request(&mut stream);
                assert!(headers.starts_with(&format!("{method} / http/1.1")));
                assert_eq!(body, expected_body);
                if method == "post" {
                    assert!(headers.contains("content-type: application/json"));
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        });
        let client = direct_client();
        assert!(client.get_bytes(&url, &[], &[]).await.unwrap().is_empty());
        for (method, body) in [("POST", "{}"), ("GET", "payload")] {
            assert!(matches!(
                client
                    .request_bytes(method, &url, &[], &[], Some(body))
                    .await,
                Err(KernelError::Http(503))
            ));
        }
        server.join().unwrap();
    }
}
