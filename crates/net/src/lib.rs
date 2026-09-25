//! 受控 HTTP 网络出口；统一处理超时、错误分类和主机白名单。

use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;
use wonderland_kernel::KernelError;
use wonderland_kernel::logging::{debug, warn};

/// 出站请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// 固定 UA：米哈游接口对 UA 敏感，不使用随机值以免触发风控。
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";

/// 允许出站的主机。
///
/// 精确匹配主机名，不接受子域。
const ALLOWED_HOSTS: &[&str] = &[
    "api-takumi.mihoyo.com",
    "api-micreator.mihoyo.com",
    "bbs-api.miyoushe.com",
    "act-webstatic.mihoyo.com",
];

/// 拒绝白名单之外的主机。
fn ensure_allowed(url: &str) -> Result<(), KernelError> {
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or(KernelError::InvalidInput)?
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ALLOWED_HOSTS.contains(&host.as_str()) {
        Ok(())
    } else {
        warn!(host = %host, "出站主机不在白名单");
        Err(KernelError::InvalidInput)
    }
}

/// 出站 HTTP 客户端。
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    pub fn new() -> Result<Self, KernelError> {
        let inner = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| KernelError::Transport(format!("HTTP 客户端初始化失败：{error}")))?;
        Ok(Self { inner })
    }

    /// 有限重试只针对只读 GET 的临时失败；不记录 URL、Cookie 或响应体。
    pub async fn get_bytes(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        query: &[(String, String)],
    ) -> Result<Vec<u8>, KernelError> {
        ensure_allowed(url)?;
        for attempt in 0..3 {
            let mut request = self.inner.get(url).query(query);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            let result = async {
                let response = request.send().await.map_err(classify)?;
                let status = response.status();
                if !status.is_success() {
                    return Err(KernelError::Http(status.as_u16()));
                }
                Ok(response.bytes().await.map_err(classify)?.to_vec())
            }
            .await;
            let retry = matches!(
                &result,
                Err(KernelError::Timeout
                    | KernelError::Connection
                    | KernelError::Http(429 | 500 | 502 | 503 | 504))
            );
            if let Err(error) = &result {
                if retry && attempt < 2 {
                    debug!(
                        code = error.code(),
                        attempt = attempt + 1,
                        "出站请求失败，退避后重试"
                    );
                } else {
                    warn!(code = error.code(), attempts = attempt + 1, "出站请求失败");
                }
            }
            if !retry || attempt == 2 {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
        }
        unreachable!()
    }

    /// GET 请求并把响应体解析为 JSON。
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<T, KernelError> {
        ensure_allowed(url)?;
        let mut request = self.inner.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        let response = request.send().await.map_err(classify)?;
        let status = response.status();
        let body = response.text().await.map_err(classify)?;

        if !status.is_success() {
            return Err(KernelError::Transport(format!("HTTP 状态码 {status}")));
        }

        serde_json::from_str(&body)
            .map_err(|error| KernelError::Transport(format!("响应解析失败：{error}")))
    }

    /// POST JSON，响应体原样返回（各接口的响应外壳不同，交给调用方解析）。
    ///
    /// **不重试**：POST 能否重放由调用方判断，这里只发一次。
    pub async fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<Vec<u8>, KernelError> {
        ensure_allowed(url)?;
        let mut request = self
            .inner
            .post(url)
            .header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        let response = request
            .body(body.to_owned())
            .send()
            .await
            .map_err(classify)?;
        let status = response.status();
        if !status.is_success() {
            return Err(KernelError::Http(status.as_u16()));
        }
        Ok(response.bytes().await.map_err(classify)?.to_vec())
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
#[derive(Debug, Clone)]
pub struct ModelEndpoint {
    /// 用户填写的基础地址，例如 `https://api.example.com/v1`。
    pub base_url: String,
    /// 单次请求超时。
    pub timeout: Duration,
    /// 是否允许本机回环（`http`）端点，仅供开发联调打开。
    pub allow_loopback: bool,
    /// 响应体大小上限。
    pub max_response_bytes: usize,
}

impl ModelEndpoint {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: MODEL_TIMEOUT,
            allow_loopback: false,
            max_response_bytes: MODEL_MAX_RESPONSE_BYTES,
        }
    }
}

/// 校验用户填写的模型端点地址，返回可直接用于拼接路径的 [`reqwest::Url`]。
///
/// 规则集中在收口处：只有 `https`（或用户显式允许、且主机确为回环的 `http`）能通过。
/// 这样"用户自配主机"不会变成一条绕过传输安全的后门。
pub fn validate_endpoint(base_url: &str, allow_loopback: bool) -> Result<reqwest::Url, ModelError> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|error| ModelError::Config(format!("地址无法解析：{error}")))?;

    if !url.username().is_empty() || url.password().is_some() {
        return Err(ModelError::Config("地址不得包含用户名或密码".to_owned()));
    }

    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| ModelError::Config("地址缺少主机名".to_owned()))?;

    match url.scheme() {
        "https" => {}
        "http" if allow_loopback && is_loopback_host(host) => {}
        "http" => {
            return Err(ModelError::Config(
                "仅允许 https；本机联调需显式允许回环地址".to_owned(),
            ));
        }
        other => {
            return Err(ModelError::Config(format!("不支持的协议：{other}")));
        }
    }

    Ok(url)
}

/// 回环主机判定。
///
/// `Url::host_str` 对 IPv6 会带上方括号，因此 `::1` 会以 `[::1]` 出现。
fn is_loopback_host(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "[::1]" | "::1"
    )
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
        let base = validate_endpoint(&endpoint.base_url, endpoint.allow_loopback)?;
        let inner = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(endpoint.timeout)
            .build()
            .map_err(|error| ModelError::Config(format!("HTTP 客户端初始化失败：{error}")))?;
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
    fn ok(base_url: &str, allow_loopback: bool) -> String {
        validate_endpoint(base_url, allow_loopback)
            .expect("应当校验通过")
            .to_string()
    }

    fn config_err(base_url: &str, allow_loopback: bool) -> String {
        match validate_endpoint(base_url, allow_loopback) {
            Err(ModelError::Config(reason)) => reason,
            other => panic!("期望 Config，实际为 {other:?}"),
        }
    }

    #[test]
    fn https_with_and_without_path_prefix_passes() {
        assert_eq!(
            ok("https://api.example.com/v1", false),
            "https://api.example.com/v1"
        );
        assert_eq!(
            ok("https://api.example.com", false),
            "https://api.example.com/"
        );
    }

    #[test]
    fn plain_http_is_rejected_by_default() {
        config_err("http://api.example.com/v1", false);
    }

    #[test]
    fn loopback_http_allowed_only_when_enabled() {
        config_err("http://127.0.0.1:8080/v1", false);
        config_err("http://localhost:1/v1", false);
        assert_eq!(
            ok("http://127.0.0.1:8080/v1", true),
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(ok("http://localhost:1/v1", true), "http://localhost:1/v1");
        assert_eq!(ok("http://[::1]:8080/v1", true), "http://[::1]:8080/v1");
        config_err("http://api.example.com/v1", true);
    }

    #[test]
    fn loopback_ip_without_flag_fails() {
        config_err("http://127.0.0.1", false);
    }

    #[test]
    fn non_http_scheme_userinfo_and_missing_host_are_rejected() {
        config_err("ftp://x/", false);
        config_err("https://user:pw@api.example.com/v1", false);
        config_err("", false);
        config_err("https://", false);
        config_err("file:///tmp/model", false);
    }

    #[test]
    fn join_url_keeps_prefix_without_double_slash() {
        let base = validate_endpoint("https://api.example.com/v1", false).unwrap();
        let joined = join_url(&base, "/chat/completions").unwrap();
        assert_eq!(
            joined.as_str(),
            "https://api.example.com/v1/chat/completions"
        );
        let bare = validate_endpoint("https://api.example.com", false).unwrap();
        assert_eq!(
            join_url(&bare, "/chat/completions").unwrap().as_str(),
            "https://api.example.com/chat/completions"
        );
        let trailing = validate_endpoint("https://api.example.com/v1/", false).unwrap();
        assert_eq!(
            join_url(&trailing, "/chat/completions").unwrap().as_str(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn join_url_rejects_traversal_and_scheme_relative_paths() {
        let base = validate_endpoint("https://api.example.com/v1", false).unwrap();
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
        let mut endpoint = ModelEndpoint::new("http://127.0.0.1:8080/v1");
        endpoint.allow_loopback = true;
        let client = ModelClient::new(endpoint).unwrap();
        assert_eq!(client.origin(), "http://127.0.0.1:8080");
    }
}
