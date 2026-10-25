//! HTTP transport for the single configured plugin distribution origin.
//! Credentials never cross redirects, and errors never include server text or URLs.

use std::fmt;
use std::time::Duration;

use reqwest::{StatusCode, Url};
use serde::Deserialize;

pub const CATALOG_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const UPDATE_MAX_BYTES: usize = 1024 * 1024;
pub const PACKAGE_MAX_BYTES: usize = 100 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DistributionSource {
    origin: String,
    debug_loopback: bool,
}

impl DistributionSource {
    pub fn parse(raw: &str) -> Result<Self, DistributionHttpError> {
        Self::parse_origin(raw, false)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn debug_loopback(raw: &str) -> Result<Self, DistributionHttpError> {
        let source = Self::parse_origin(raw, true)?;
        if !source.debug_loopback {
            return Err(error("INVALID_DISTRIBUTION_SOURCE"));
        }
        Ok(source)
    }

    fn parse_origin(raw: &str, allow_loopback: bool) -> Result<Self, DistributionHttpError> {
        let raw = raw.trim();
        let url = Url::parse(raw).map_err(|_| error("INVALID_DISTRIBUTION_SOURCE"))?;
        let loopback = url
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "[::1]" || host == "::1");
        let debug_loopback = allow_loopback && url.scheme() == "http" && loopback;
        if raw.len() > 2048
            || raw.bytes().any(|byte| byte.is_ascii_control())
            || raw.contains('\\')
            || (url.scheme() != "https" && !debug_loopback)
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(error("INVALID_DISTRIBUTION_SOURCE"));
        }
        Ok(Self {
            origin: url.origin().ascii_serialization(),
            debug_loopback,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.origin
    }

    pub fn update_url(&self, plugin_id: &str) -> String {
        self.path(&["v1", "plugins", plugin_id, "update"])
    }

    pub fn package_url(&self, plugin_id: &str, version: &str) -> String {
        self.path(&["v1", "plugins", plugin_id, "versions", version, "package"])
    }

    pub fn accepts_url(&self, raw: &str) -> bool {
        let Ok(url) = Url::parse(raw) else {
            return false;
        };
        !raw.contains('\\')
            && !raw.bytes().any(|byte| byte.is_ascii_control())
            && url.origin().ascii_serialization() == self.origin
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    }

    fn path(&self, segments: &[&str]) -> String {
        let mut url = Url::parse(&self.origin).expect("validated distribution origin");
        url.path_segments_mut()
            .expect("HTTPS origins have path segments")
            .clear()
            .extend(segments.iter().copied());
        url.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DistributionHttpError {
    pub code: &'static str,
    pub status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
}

impl fmt::Display for DistributionHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for DistributionHttpError {}

fn error(code: &'static str) -> DistributionHttpError {
    DistributionHttpError {
        code,
        status: None,
        retry_after_seconds: None,
    }
}

#[derive(Clone)]
pub struct DistributionClient {
    source: DistributionSource,
    client: reqwest::Client,
}

impl DistributionClient {
    pub fn new(source: DistributionSource) -> Result<Self, DistributionHttpError> {
        let proxy = crate::proxy::current_proxy().map_err(|_| error("DISTRIBUTION_UNAVAILABLE"))?;
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .user_agent("WonderlandAssistant/PluginDistribution-v1");
        let builder = if source.debug_loopback {
            builder.no_proxy()
        } else {
            proxy.configuration.apply_to(builder)
        };
        let client = builder
            .build()
            .map_err(|_| error("DISTRIBUTION_UNAVAILABLE"))?;
        Ok(Self { source, client })
    }

    pub fn source(&self) -> &DistributionSource {
        &self.source
    }

    pub async fn access(&self, code: Option<&str>) -> Result<Vec<u8>, DistributionHttpError> {
        self.get(&self.source.path(&["v1", "access"]), code, 16 * 1024, false)
            .await
    }

    pub async fn catalog(&self, code: Option<&str>) -> Result<Vec<u8>, DistributionHttpError> {
        self.get(
            &self.source.path(&["v1", "catalog"]),
            code,
            CATALOG_MAX_BYTES,
            false,
        )
        .await
    }

    pub async fn update(
        &self,
        plugin_id: &str,
        code: Option<&str>,
    ) -> Result<Vec<u8>, DistributionHttpError> {
        if !valid_id(plugin_id) {
            return Err(error("INVALID_REQUEST"));
        }
        self.get(
            &self.source.update_url(plugin_id),
            code,
            UPDATE_MAX_BYTES,
            false,
        )
        .await
    }

    pub async fn package(
        &self,
        plugin_id: &str,
        version: &str,
        code: Option<&str>,
    ) -> Result<Vec<u8>, DistributionHttpError> {
        self.package_with_progress(plugin_id, version, code, &|_, _| {})
            .await
    }

    pub async fn package_with_progress(
        &self,
        plugin_id: &str,
        version: &str,
        code: Option<&str>,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
    ) -> Result<Vec<u8>, DistributionHttpError> {
        if !valid_id(plugin_id)
            || version.is_empty()
            || version.len() > 128
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte))
        {
            return Err(error("INVALID_REQUEST"));
        }
        self.get_with_progress(
            &self.source.package_url(plugin_id, version),
            code,
            PACKAGE_MAX_BYTES,
            true,
            Some(progress),
        )
        .await
    }

    async fn get(
        &self,
        url: &str,
        code: Option<&str>,
        limit: usize,
        package: bool,
    ) -> Result<Vec<u8>, DistributionHttpError> {
        self.get_with_progress(url, code, limit, package, None)
            .await
    }

    async fn get_with_progress(
        &self,
        url: &str,
        code: Option<&str>,
        limit: usize,
        package: bool,
        progress: Option<&(dyn Fn(u64, Option<u64>) + Send + Sync)>,
    ) -> Result<Vec<u8>, DistributionHttpError> {
        let mut request = self.client.get(url);
        if let Some(code) = code {
            let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {code}"))
                .map_err(|_| error("INVALID_REQUEST"))?;
            value.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        if package {
            request = request.timeout(Duration::from_secs(120));
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| error("DISTRIBUTION_UNAVAILABLE"))?;
        let status = response.status();
        if status.is_success() {
            if let Some(progress) = progress {
                let total = response.content_length();
                if total.is_some_and(|value| value > limit as u64) {
                    return Err(error("RESOURCE_LIMIT"));
                }
                let mut bytes = Vec::new();
                progress(0, total);
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| error("DISTRIBUTION_UNAVAILABLE"))?
                {
                    if chunk.len() > limit.saturating_sub(bytes.len()) {
                        return Err(error("RESOURCE_LIMIT"));
                    }
                    bytes.extend_from_slice(&chunk);
                    progress(bytes.len() as u64, total);
                }
                return Ok(bytes);
            }
            return crate::read_limited_body(response, limit)
                .await
                .map_err(|error_value| {
                    if matches!(error_value, wonderland_kernel::KernelError::ResourceLimit) {
                        error("RESOURCE_LIMIT")
                    } else {
                        error("DISTRIBUTION_UNAVAILABLE")
                    }
                });
        }
        let retry_after_seconds = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(|seconds| seconds.min(3600));
        let code = match status {
            StatusCode::UNAUTHORIZED => {
                #[derive(Deserialize)]
                struct Envelope {
                    error: RemoteError,
                }
                #[derive(Deserialize)]
                struct RemoteError {
                    code: String,
                }
                let parsed = crate::read_limited_body(response, 16 * 1024)
                    .await
                    .ok()
                    .and_then(|body| serde_json::from_slice::<Envelope>(&body).ok());
                match parsed.as_ref().map(|body| body.error.code.as_str()) {
                    Some("ACCESS_CODE_EXPIRED") => "ACCESS_CODE_EXPIRED",
                    Some("ACCESS_CODE_REVOKED") => "ACCESS_CODE_REVOKED",
                    _ => "ACCESS_CODE_INVALID",
                }
            }
            StatusCode::NOT_FOUND => "NOT_AVAILABLE",
            StatusCode::TOO_MANY_REQUESTS => "RATE_LIMITED",
            _ if status.is_server_error() => "DISTRIBUTION_UNAVAILABLE",
            _ => "INVALID_DISTRIBUTION_RESPONSE",
        };
        Err(DistributionHttpError {
            code,
            status: Some(status.as_u16()),
            retry_after_seconds,
        })
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_origin_rejects_paths_credentials_http_and_queries() {
        assert_eq!(
            DistributionSource::parse("https://example.test/")
                .unwrap()
                .as_str(),
            "https://example.test"
        );
        for source in [
            "http://127.0.0.1:8080",
            "https://user@example.test",
            "https://example.test/v1",
            "https://example.test?x=1",
            "https://example.test#x",
            "https:\\example.test",
            "file:///x",
        ] {
            assert!(DistributionSource::parse(source).is_err(), "{source}");
        }
        assert!(DistributionSource::debug_loopback("http://127.0.0.1:8080").is_ok());
        assert!(DistributionSource::debug_loopback("http://[::1]:8080").is_ok());
        assert!(DistributionSource::debug_loopback("http://example.test").is_err());
        assert!(DistributionSource::debug_loopback("https://example.test").is_err());
    }

    #[test]
    fn signed_addresses_are_same_origin_and_path_segments_are_encoded() {
        let source = DistributionSource::parse("https://example.test").unwrap();
        assert_eq!(
            source.update_url("editor"),
            "https://example.test/v1/plugins/editor/update"
        );
        assert_eq!(
            source.package_url("editor", "1.2.3"),
            "https://example.test/v1/plugins/editor/versions/1.2.3/package"
        );
        assert!(source.accepts_url("https://example.test/notes"));
        for url in [
            "https://other.test/notes",
            "https://example.test/notes?code=secret",
            "https://user@example.test/notes",
            "https://example.test/#secret",
        ] {
            assert!(!source.accepts_url(url));
        }
    }

    fn server(response: String) -> (DistributionClient, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let source = DistributionSource::debug_loopback(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 2048];
            while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (DistributionClient::new(source).unwrap(), task)
    }

    #[tokio::test]
    async fn redirects_never_forward_the_bearer_code() {
        let (client, server) = server("HTTP/1.1 302 Found\r\nLocation: https://other.test/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
        let error = client
            .catalog(Some("test-code-never-redirected"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "INVALID_DISTRIBUTION_RESPONSE");
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /v1/catalog HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-code-never-redirected")
        );
    }

    #[tokio::test]
    async fn errors_keep_known_codes_and_bounded_retry_without_server_text() {
        let secret = "test-code-must-not-appear-in-errors";
        let body =
            format!("{{\"error\":{{\"code\":\"ACCESS_CODE_REVOKED\",\"message\":\"{secret}\"}}}}");
        let (client, task) = server(format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let error = client.access(Some(secret)).await.unwrap_err();
        assert_eq!(error.code, "ACCESS_CODE_REVOKED");
        assert!(!format!("{error:?} {error}").contains(secret));
        task.join().unwrap();

        let (client, task) = server("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 999999999\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
        let error = client.catalog(None).await.unwrap_err();
        assert_eq!(error.code, "RATE_LIMITED");
        assert_eq!(error.retry_after_seconds, Some(3600));
        task.join().unwrap();

        let (client, task) = server(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        );
        assert_eq!(
            client.update("hidden", None).await.unwrap_err().code,
            "NOT_AVAILABLE"
        );
        task.join().unwrap();
    }

    #[tokio::test]
    async fn chunked_success_obeys_limits_without_content_length() {
        let (client, task) = server("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n4\r\nefgh\r\n0\r\n\r\n".into());
        let error = client
            .get(&client.source.path(&["v1", "catalog"]), None, 5, false)
            .await
            .unwrap_err();
        assert_eq!(error.code, "RESOURCE_LIMIT");
        task.join().unwrap();
    }
}
