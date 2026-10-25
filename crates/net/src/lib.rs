//! Shared HTTP transport, proxy configuration and signed package downloads.

use std::fmt;
use std::sync::{Arc, RwLock};
#[cfg(test)]
use std::time::Duration;

pub use reqwest::Url;
use wonderland_kernel::KernelError;

pub mod distribution;
pub mod proxy;
pub use distribution::{DistributionClient, DistributionHttpError, DistributionSource};
pub use proxy::{
    ProxyConfiguration, ProxyCredentials, ProxyEnvironmentVariable, ProxyError, ProxyMode,
    ProxySettings,
};

/// An HTTP header value is bytes: legal repeated fields and non-UTF-8 values are retained.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpHeader {
    pub name: String,
    pub value: Vec<u8>,
}

impl fmt::Debug for HttpHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpHeader")
            .field("name", &self.name)
            .field("value_bytes", &self.value.len())
            .finish()
    }
}

/// Request-specific redirects. No policy is inferred from the request's business purpose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HttpRedirectPolicy {
    #[default]
    None,
    /// `None` follows an acyclic chain without an artificial hop limit.
    Follow { max_redirects: Option<usize> },
}

/// Files are consumed from their current position, with backpressure from the HTTP transport.
/// A file stream cannot be replayed automatically: a 307/308 requiring replay is returned
/// intact, even when redirects are enabled. The caller may explicitly reopen its source.
#[derive(Default)]
pub enum HttpRequestBody {
    #[default]
    Empty,
    Bytes(Vec<u8>),
    File(tokio::fs::File),
}

impl fmt::Debug for HttpRequestBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Bytes(bytes) => f.debug_tuple("Bytes").field(&bytes.len()).finish(),
            Self::File(_) => f.write_str("File(<stream>)"),
        }
    }
}

/// The host resolves credentials and owned file resources before entering this transport.
/// No account cookie, plugin key, distribution code or business retry is added here.
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<HttpHeader>,
    /// Host-injected credential headers survive same-origin redirects only. These are
    /// names, not values, so client caching never contains a request's credentials.
    pub credential_header_names: Vec<String>,
    pub query: Vec<(String, String)>,
    pub body: HttpRequestBody,
    /// A single absolute deadline covers sending, redirects and all subsequent body reads.
    pub deadline: Option<tokio::time::Instant>,
    pub redirects: HttpRedirectPolicy,
}

impl HttpRequest {
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: Vec::new(),
            credential_header_names: Vec::new(),
            query: Vec::new(),
            body: HttpRequestBody::Empty,
            deadline: None,
            redirects: HttpRedirectPolicy::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpFailureStage {
    Prepare,
    Send,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpFailureKind {
    InvalidUrl,
    InvalidMethod,
    InvalidHeader,
    ClientUnavailable,
    DeadlineExceeded,
    Connection,
    Timeout,
    Redirect,
    RequestFailed,
    BodyInterrupted,
}

/// Safe transport diagnostics; they contain no URL, header value, credential or body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpTransportError {
    pub stage: HttpFailureStage,
    pub kind: HttpFailureKind,
    /// `true` means Core cannot establish whether the upstream processed the operation.
    pub outcome_unknown: bool,
}

impl HttpTransportError {
    fn prepare(kind: HttpFailureKind) -> Self {
        Self {
            stage: HttpFailureStage::Prepare,
            kind,
            outcome_unknown: false,
        }
    }

    fn deadline(stage: HttpFailureStage) -> Self {
        Self {
            stage,
            kind: HttpFailureKind::DeadlineExceeded,
            outcome_unknown: stage != HttpFailureStage::Prepare,
        }
    }

    fn send(error: reqwest::Error, followed_response: bool) -> Self {
        let kind = if error.is_connect() {
            HttpFailureKind::Connection
        } else if error.is_timeout() {
            HttpFailureKind::Timeout
        } else if error.is_redirect() {
            HttpFailureKind::Redirect
        } else {
            HttpFailureKind::RequestFailed
        };
        // A failed connection to the original URL has not sent an HTTP request. A failure
        // on a redirect may follow already-processed requests, so its outcome remains unknown.
        let outcome_unknown = !error.is_connect() || followed_response;
        Self {
            stage: HttpFailureStage::Send,
            kind,
            outcome_unknown,
        }
    }
}

impl fmt::Display for HttpTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            HttpFailureKind::InvalidUrl => "HTTP URL is invalid",
            HttpFailureKind::InvalidMethod => "HTTP method is invalid",
            HttpFailureKind::InvalidHeader => "HTTP header is invalid",
            HttpFailureKind::ClientUnavailable => "HTTP transport or proxy is unavailable",
            HttpFailureKind::DeadlineExceeded => "The requested HTTP deadline expired",
            HttpFailureKind::Connection => "HTTP connection failed",
            HttpFailureKind::Timeout => "HTTP transport reported a timeout",
            HttpFailureKind::Redirect => "HTTP redirect failed",
            HttpFailureKind::RequestFailed => "HTTP request failed",
            HttpFailureKind::BodyInterrupted => "HTTP response body was interrupted",
        })?;
        if self.outcome_unknown {
            f.write_str("; the upstream outcome is unknown")?;
        }
        Ok(())
    }
}

impl std::error::Error for HttpTransportError {}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<HttpHeader>,
    pub final_url: String,
    pub content_length: Option<u64>,
    pub body: HttpResponseBody,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("header_count", &self.headers.len())
            .field("content_length", &self.content_length)
            .finish_non_exhaustive()
    }
}

/// Dropping or closing the body releases its response; chunks are read only on demand.
pub struct HttpResponseBody {
    response: Option<reqwest::Response>,
    deadline: Option<tokio::time::Instant>,
    failure: Option<HttpTransportError>,
}

impl HttpResponseBody {
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, HttpTransportError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let Some(response) = self.response.as_mut() else {
            return Ok(None);
        };
        let result = match self.deadline {
            Some(deadline) if deadline <= tokio::time::Instant::now() => {
                Err(HttpTransportError::deadline(HttpFailureStage::Body))
            }
            Some(deadline) => match tokio::time::timeout_at(deadline, response.chunk()).await {
                Ok(result) => result.map_err(|_| HttpTransportError {
                    stage: HttpFailureStage::Body,
                    kind: HttpFailureKind::BodyInterrupted,
                    outcome_unknown: true,
                }),
                Err(_) => Err(HttpTransportError::deadline(HttpFailureStage::Body)),
            },
            None => response.chunk().await.map_err(|_| HttpTransportError {
                stage: HttpFailureStage::Body,
                kind: HttpFailureKind::BodyInterrupted,
                outcome_unknown: true,
            }),
        };
        match result {
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())),
            Ok(None) => {
                self.response.take();
                Ok(None)
            }
            Err(error) => {
                self.response.take();
                self.failure = Some(error);
                Err(error)
            }
        }
    }

    pub fn close(&mut self) {
        self.response.take();
        self.failure = None;
    }
}

type CachedHttpTransport = Option<(u64, HttpRedirectPolicy, reqwest::Client)>;

/// General plugin HTTP transport. Distribution requests retain separate policy.
#[derive(Clone, Default)]
pub struct HttpTransport {
    cached: Arc<RwLock<CachedHttpTransport>>,
}

impl HttpTransport {
    pub fn new() -> Self {
        Self::default()
    }

    fn inner(&self, redirects: HttpRedirectPolicy) -> Result<reqwest::Client, HttpTransportError> {
        let unavailable = || HttpTransportError::prepare(HttpFailureKind::ClientUnavailable);
        let snapshot = proxy::current_proxy().map_err(|_| unavailable())?;
        let mut cached = self.cached.write().map_err(|_| unavailable())?;
        if let Some((revision, policy, client)) = cached.as_ref()
            && *revision == snapshot.revision
            && *policy == redirects
        {
            return Ok(client.clone());
        }
        let builder = reqwest::Client::builder()
            // Follow redirects here so arbitrary host-injected credential names are
            // stripped on origin changes, rather than reqwest's fixed list alone.
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            // Keep bytes and HTTP headers intact even if another workspace consumer enables
            // a reqwest automatic-decompression feature.
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd();
        // Reqwest otherwise installs its own 30-second unacknowledged-send limit on Linux.
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let builder = builder.tcp_user_timeout(None);
        let client = snapshot
            .configuration
            .apply_to(builder)
            .build()
            .map_err(|_| unavailable())?;
        *cached = Some((snapshot.revision, redirects, client.clone()));
        Ok(client)
    }

    pub async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpTransportError> {
        if request
            .deadline
            .is_some_and(|deadline| deadline <= tokio::time::Instant::now())
        {
            return Err(HttpTransportError::deadline(HttpFailureStage::Prepare));
        }
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| HttpTransportError::prepare(HttpFailureKind::InvalidMethod))?;
        let url = reqwest::Url::parse(&request.url)
            .map_err(|_| HttpTransportError::prepare(HttpFailureKind::InvalidUrl))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(HttpTransportError::prepare(HttpFailureKind::InvalidUrl));
        }
        let credential_headers = request
            .credential_header_names
            .iter()
            .map(|name| {
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| HttpTransportError::prepare(HttpFailureKind::InvalidHeader))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut headers = Vec::with_capacity(request.headers.len());
        for header in request.headers {
            let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
                .map_err(|_| HttpTransportError::prepare(HttpFailureKind::InvalidHeader))?;
            let mut value = reqwest::header::HeaderValue::from_bytes(&header.value)
                .map_err(|_| HttpTransportError::prepare(HttpFailureKind::InvalidHeader))?;
            if name == reqwest::header::AUTHORIZATION
                || name == reqwest::header::COOKIE
                || name == reqwest::header::PROXY_AUTHORIZATION
                || credential_headers.contains(&name)
            {
                value.set_sensitive(true);
            }
            headers.push((name, value));
        }
        let client = self.inner(request.redirects)?;
        let mut builder = client.request(method, url).query(&request.query);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        builder = match request.body {
            HttpRequestBody::Empty => builder,
            HttpRequestBody::Bytes(bytes) => builder.body(bytes),
            HttpRequestBody::File(file) => builder.body(reqwest::Body::from(file)),
        };
        let mut outgoing = builder
            .build()
            .map_err(|_| HttpTransportError::prepare(HttpFailureKind::RequestFailed))?;
        // Client construction and validation also spend the caller's original budget.
        if request
            .deadline
            .is_some_and(|deadline| deadline <= tokio::time::Instant::now())
        {
            return Err(HttpTransportError::deadline(HttpFailureStage::Prepare));
        }
        let mut previous = Vec::new();
        let response = loop {
            let current_url = outgoing.url().clone();
            let method = outgoing.method().clone();
            let headers = outgoing.headers().clone();
            // Non-replayable file uploads intentionally cannot follow 307/308.
            // 301/302 POST and 303 discard the payload and can create a GET/HEAD.
            let replay = outgoing.try_clone();
            let response = match request.deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, client.execute(outgoing))
                    .await
                    .map_err(|_| HttpTransportError::deadline(HttpFailureStage::Send))?
                    .map_err(|error| HttpTransportError::send(error, !previous.is_empty()))?,
                None => client
                    .execute(outgoing)
                    .await
                    .map_err(|error| HttpTransportError::send(error, !previous.is_empty()))?,
            };
            let HttpRedirectPolicy::Follow { max_redirects } = request.redirects else {
                break response;
            };
            let discard_payload = match response.status().as_u16() {
                301 | 302 => method == reqwest::Method::POST,
                303 => true,
                307 | 308 => false,
                _ => break response,
            };
            let next_request = if discard_payload {
                let next_method = if method == reqwest::Method::HEAD {
                    reqwest::Method::HEAD
                } else {
                    reqwest::Method::GET
                };
                let mut next = reqwest::Request::new(next_method, current_url.clone());
                *next.headers_mut() = headers;
                for name in [
                    reqwest::header::CONTENT_TYPE,
                    reqwest::header::CONTENT_LENGTH,
                    reqwest::header::CONTENT_ENCODING,
                    reqwest::header::TRANSFER_ENCODING,
                ] {
                    next.headers_mut().remove(name);
                }
                Some(next)
            } else {
                replay
            };
            let Some(mut next_request) = next_request else {
                break response;
            };
            let next_url = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|location| location.to_str().ok())
                .and_then(|location| current_url.join(location).ok());
            let Some(next_url) = next_url else {
                break response;
            };
            let mut current_resource = current_url.clone();
            current_resource.set_fragment(None);
            previous.push(current_resource);
            let mut next_resource = next_url.clone();
            next_resource.set_fragment(None);
            if !matches!(next_url.scheme(), "http" | "https")
                || previous.contains(&next_resource)
                || max_redirects.is_some_and(|max| previous.len() > max)
            {
                return Err(HttpTransportError {
                    stage: HttpFailureStage::Send,
                    kind: HttpFailureKind::Redirect,
                    outcome_unknown: true,
                });
            }
            if current_url.origin() != next_url.origin() {
                for name in [
                    reqwest::header::AUTHORIZATION,
                    reqwest::header::COOKIE,
                    reqwest::header::PROXY_AUTHORIZATION,
                    reqwest::header::WWW_AUTHENTICATE,
                    reqwest::header::HOST,
                ] {
                    next_request.headers_mut().remove(name);
                }
                next_request.headers_mut().remove("cookie2");
                for name in &credential_headers {
                    next_request.headers_mut().remove(name);
                }
            }
            *next_request.url_mut() = next_url;
            outgoing = next_request;
            // Drop the intermediate response before sending the next hop; no whole-body
            // aggregation or additional deadline is introduced by redirect processing.
        };
        Ok(HttpResponse {
            status: response.status().as_u16(),
            headers: response
                .headers()
                .iter()
                .map(|(name, value)| HttpHeader {
                    name: name.as_str().to_owned(),
                    value: value.as_bytes().to_vec(),
                })
                .collect(),
            final_url: response.url().to_string(),
            content_length: response.content_length(),
            body: HttpResponseBody {
                response: Some(response),
                deadline: request.deadline,
                failure: None,
            },
        })
    }
}

// A caller-supplied metadata/error budget, also used by signed distribution requests.
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
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            KernelError::Timeout
        } else if error.is_connect() {
            KernelError::Connection
        } else {
            KernelError::Transport("响应读取失败".into())
        }
    })? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(KernelError::ResourceLimit);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn direct_transport(redirects: HttpRedirectPolicy) -> HttpTransport {
        let transport = HttpTransport::new();
        *transport.cached.write().unwrap() = Some((
            proxy::current_proxy().unwrap().revision,
            redirects,
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .unwrap(),
        ));
        transport
    }

    fn request_head(reader: &mut impl BufRead) -> (String, Vec<HttpHeader>) {
        let mut first = String::new();
        assert!(reader.read_line(&mut first).unwrap() > 0);
        let mut headers = Vec::new();
        loop {
            let mut line = Vec::new();
            assert!(reader.read_until(b'\n', &mut line).unwrap() > 0);
            if line == b"\r\n" {
                break;
            }
            let split = line.iter().position(|byte| *byte == b':').unwrap();
            headers.push(HttpHeader {
                name: String::from_utf8(line[..split].to_vec())
                    .unwrap()
                    .to_ascii_lowercase(),
                value: line[split + 1..].trim_ascii().to_vec(),
            });
        }
        (first.trim().to_owned(), headers)
    }

    fn values<'a>(headers: &'a [HttpHeader], name: &str) -> Vec<&'a [u8]> {
        headers
            .iter()
            .filter(|header| header.name == name)
            .map(|header| header.value.as_slice())
            .collect()
    }

    fn prepare_stream(stream: &TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
    }

    async fn collect(body: &mut HttpResponseBody) -> Vec<u8> {
        let mut all = Vec::new();
        while let Some(chunk) = body.chunk().await.unwrap() {
            all.extend_from_slice(&chunk);
        }
        all
    }

    #[tokio::test]
    async fn custom_method_binary_body_repeated_query_and_headers_keep_non_success_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/resource?existing=yes",
            listener.local_addr().unwrap()
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            let mut reader = BufReader::new(&mut stream);
            let (first, headers) = request_head(&mut reader);
            assert_eq!(first, "REPORT /resource?existing=yes&q=one&q=two HTTP/1.1");
            assert_eq!(
                values(&headers, "x-repeat"),
                [b"first".as_slice(), b"second"]
            );
            assert_eq!(values(&headers, "x-bytes"), [b"\xff".as_slice()]);
            assert!(values(&headers, "authorization").is_empty());
            assert!(values(&headers, "cookie").is_empty());
            assert!(values(&headers, "content-type").is_empty());
            let mut body = [0; 4];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(body, [0, 1, 0xff, 2]);
            drop(reader);
            stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 7\r\nX-Repeat: first\r\nX-Repeat: second\r\nX-Bytes: \xff\r\nContent-Length: 4\r\nConnection: close\r\n\r\n\0\x01\xff\x02").unwrap();
        });
        let transport = direct_transport(HttpRedirectPolicy::None);
        let mut request = HttpRequest::new("REPORT", &url);
        request.query = vec![("q".into(), "one".into()), ("q".into(), "two".into())];
        request.headers = vec![
            HttpHeader {
                name: "X-Repeat".into(),
                value: b"first".to_vec(),
            },
            HttpHeader {
                name: "X-Repeat".into(),
                value: b"second".to_vec(),
            },
            HttpHeader {
                name: "X-Bytes".into(),
                value: vec![0xff],
            },
        ];
        request.body = HttpRequestBody::Bytes(vec![0, 1, 0xff, 2]);
        let mut response = transport.send(request).await.unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(values(&response.headers, "retry-after"), [b"7".as_slice()]);
        assert_eq!(
            values(&response.headers, "x-repeat"),
            [b"first".as_slice(), b"second"]
        );
        assert_eq!(values(&response.headers, "x-bytes"), [b"\xff".as_slice()]);
        assert_eq!(response.final_url, format!("{url}&q=one&q=two"));
        assert_eq!(collect(&mut response.body).await, [0, 1, 0xff, 2]);
        assert!(response.body.chunk().await.unwrap().is_none());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn explicit_authentication_is_not_inherited_by_the_next_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for authenticated in [true, false] {
                let (mut stream, _) = listener.accept().unwrap();
                prepare_stream(&stream);
                let (_, headers) = request_head(&mut BufReader::new(&mut stream));
                if authenticated {
                    assert_eq!(
                        values(&headers, "authorization"),
                        [b"Bearer request-key".as_slice()]
                    );
                } else {
                    assert!(values(&headers, "authorization").is_empty());
                    assert!(values(&headers, "cookie").is_empty());
                }
                stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno").unwrap();
            }
        });
        let transport = direct_transport(HttpRedirectPolicy::None);
        for authenticated in [true, false] {
            let mut request = HttpRequest::new("GET", &url);
            if authenticated {
                request.headers.push(HttpHeader {
                    name: "Authorization".into(),
                    value: b"Bearer request-key".to_vec(),
                });
            }
            let mut response = transport.send(request).await.unwrap();
            assert_eq!(response.status, 401);
            assert_eq!(collect(&mut response.body).await, b"no");
        }
        server.join().unwrap();
    }

    #[tokio::test]
    async fn redirect_none_returns_complete_response_and_unlimited_follow_passes_ten_hops() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for expected in std::iter::once(0).chain(0..=12) {
                let (mut stream, _) = listener.accept().unwrap();
                prepare_stream(&stream);
                let (first, _) = request_head(&mut BufReader::new(&mut stream));
                assert_eq!(first, format!("GET /{expected} HTTP/1.1"));
                let response = if expected == 12 {
                    "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nend"
                        .to_owned()
                } else {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: /{}\r\nContent-Length: 4\r\nConnection: close\r\n\r\nhere",
                        expected + 1
                    )
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let mut response = direct_transport(HttpRedirectPolicy::None)
            .send(HttpRequest::new("GET", format!("{base}/0")))
            .await
            .unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(response.final_url, format!("{base}/0"));
        assert_eq!(collect(&mut response.body).await, b"here");
        let follow = HttpRedirectPolicy::Follow {
            max_redirects: None,
        };
        let mut request = HttpRequest::new("GET", format!("{base}/0"));
        request.redirects = follow;
        let mut response = direct_transport(follow).send(request).await.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.final_url, format!("{base}/12"));
        assert_eq!(collect(&mut response.body).await, b"end");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn redirect_loop_is_diagnosed_without_a_default_hop_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/a", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for (path, next) in [("/a", "/b"), ("/b", "/a")] {
                let (mut stream, _) = listener.accept().unwrap();
                prepare_stream(&stream);
                let (first, _) = request_head(&mut BufReader::new(&mut stream));
                assert_eq!(first, format!("GET {path} HTTP/1.1"));
                write!(stream, "HTTP/1.1 302 Found\r\nLocation: {next}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
        });
        let follow = HttpRedirectPolicy::Follow {
            max_redirects: None,
        };
        let mut request = HttpRequest::new("GET", url);
        request.redirects = follow;
        let error = direct_transport(follow).send(request).await.unwrap_err();
        assert_eq!(error.kind, HttpFailureKind::Redirect);
        assert_eq!(error.stage, HttpFailureStage::Send);
        assert!(error.outcome_unknown);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn cross_origin_redirect_strips_auth_and_cookie_but_preserves_final_response() {
        let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let last_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/first", first_listener.local_addr().unwrap());
        let final_url = format!("http://{}/last", last_listener.local_addr().unwrap());
        let expected_final = final_url.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = first_listener.accept().unwrap();
            prepare_stream(&stream);
            let (_, headers) = request_head(&mut BufReader::new(&mut stream));
            assert_eq!(
                values(&headers, "authorization"),
                [b"Bearer private-key".as_slice()]
            );
            assert_eq!(values(&headers, "cookie"), [b"session=private".as_slice()]);
            write!(stream, "HTTP/1.1 302 Found\r\nLocation: {final_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            let (mut stream, _) = last_listener.accept().unwrap();
            prepare_stream(&stream);
            let (_, headers) = request_head(&mut BufReader::new(&mut stream));
            assert!(values(&headers, "authorization").is_empty());
            assert!(values(&headers, "cookie").is_empty());
            stream.write_all(b"HTTP/1.1 418 Teapot\r\nX-Final: yes\r\nContent-Length: 3\r\nConnection: close\r\n\r\ntea").unwrap();
        });
        let follow = HttpRedirectPolicy::Follow {
            max_redirects: None,
        };
        let mut request = HttpRequest::new("GET", url);
        request.redirects = follow;
        request.headers = vec![
            HttpHeader {
                name: "Authorization".into(),
                value: b"Bearer private-key".to_vec(),
            },
            HttpHeader {
                name: "Cookie".into(),
                value: b"session=private".to_vec(),
            },
        ];
        let mut response = direct_transport(follow).send(request).await.unwrap();
        assert_eq!(response.status, 418);
        assert_eq!(response.final_url, expected_final);
        assert_eq!(collect(&mut response.body).await, b"tea");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn custom_secret_survives_same_origin_but_never_returns_after_cross_origin() {
        let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let other_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", first_listener.local_addr().unwrap());
        let other = format!("http://{}/other", other_listener.local_addr().unwrap());
        let initial = format!("{base}/first");
        let expected_final = format!("{base}/final");
        let server = thread::spawn(move || {
            for (path, location) in [("/first", "/second"), ("/second", other.as_str())] {
                let (mut stream, _) = first_listener.accept().unwrap();
                prepare_stream(&stream);
                let (first, headers) = request_head(&mut BufReader::new(&mut stream));
                assert_eq!(first, format!("GET {path} HTTP/1.1"));
                assert_eq!(values(&headers, "x-api-key"), [b"private-key".as_slice()]);
                write!(stream, "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            let (mut stream, _) = other_listener.accept().unwrap();
            prepare_stream(&stream);
            let (_, headers) = request_head(&mut BufReader::new(&mut stream));
            assert!(values(&headers, "x-api-key").is_empty());
            assert_eq!(values(&headers, "x-ordinary"), [b"retained".as_slice()]);
            write!(stream, "HTTP/1.1 302 Found\r\nLocation: {base}/final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            let (mut stream, _) = first_listener.accept().unwrap();
            prepare_stream(&stream);
            let (_, headers) = request_head(&mut BufReader::new(&mut stream));
            assert!(values(&headers, "x-api-key").is_empty());
            stream.write_all(b"HTTP/1.1 451 Unavailable\r\nX-Repeat: one\r\nX-Repeat: two\r\nContent-Length: 3\r\nConnection: close\r\n\r\nend").unwrap();
        });
        let follow = HttpRedirectPolicy::Follow {
            max_redirects: None,
        };
        let mut request = HttpRequest::new("GET", initial);
        request.redirects = follow;
        request.credential_header_names = vec!["X-API-Key".into()];
        request.headers = vec![
            HttpHeader {
                name: "x-api-key".into(),
                value: b"private-key".to_vec(),
            },
            HttpHeader {
                name: "X-Ordinary".into(),
                value: b"retained".to_vec(),
            },
        ];
        let mut response = direct_transport(follow).send(request).await.unwrap();
        assert_eq!(response.status, 451);
        assert_eq!(response.final_url, expected_final);
        assert_eq!(
            values(&response.headers, "x-repeat"),
            [b"one".as_slice(), b"two"]
        );
        assert_eq!(collect(&mut response.body).await, b"end");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn redirects_preserve_method_and_body_or_discard_them_as_http_requires() {
        for (status, original, expected, discard) in [
            (301, "POST", "GET", true),
            (302, "POST", "GET", true),
            (301, "PUT", "PUT", false),
            (302, "PUT", "PUT", false),
            (303, "PUT", "GET", true),
            (303, "HEAD", "HEAD", true),
            (307, "POST", "POST", false),
            (308, "POST", "POST", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/first", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                prepare_stream(&stream);
                let mut reader = BufReader::new(&mut stream);
                let (first, _) = request_head(&mut reader);
                assert_eq!(first, format!("{original} /first HTTP/1.1"));
                let mut body = [0; 7];
                reader.read_exact(&mut body).unwrap();
                assert_eq!(&body, b"payload");
                drop(reader);
                write!(stream, "HTTP/1.1 {status} Redirect\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                let (mut stream, _) = listener.accept().unwrap();
                prepare_stream(&stream);
                let mut reader = BufReader::new(&mut stream);
                let (first, headers) = request_head(&mut reader);
                assert_eq!(first, format!("{expected} /final HTTP/1.1"));
                if discard {
                    for name in ["content-type", "content-encoding", "transfer-encoding"] {
                        assert!(values(&headers, name).is_empty());
                    }
                    assert!(values(&headers, "content-length").is_empty());
                } else {
                    reader.read_exact(&mut body).unwrap();
                    assert_eq!(&body, b"payload");
                    assert_eq!(
                        values(&headers, "content-type"),
                        [b"application/test".as_slice()]
                    );
                }
                drop(reader);
                stream
                    .write_all(
                        b"HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            });
            let follow = HttpRedirectPolicy::Follow {
                max_redirects: None,
            };
            let mut request = HttpRequest::new(original, url);
            request.redirects = follow;
            request.headers = vec![HttpHeader {
                name: "Content-Type".into(),
                value: b"application/test".to_vec(),
            }];
            request.body = HttpRequestBody::Bytes(b"payload".to_vec());
            let mut response = direct_transport(follow).send(request).await.unwrap();
            assert_eq!(response.status, 409);
            assert!(collect(&mut response.body).await.is_empty());
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn redirect_limit_and_deadline_cover_the_complete_chain() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/first", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            request_head(&mut BufReader::new(&mut stream));
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let follow = HttpRedirectPolicy::Follow {
            max_redirects: Some(0),
        };
        let mut request = HttpRequest::new("GET", url);
        request.redirects = follow;
        let error = direct_transport(follow).send(request).await.unwrap_err();
        assert_eq!(error.kind, HttpFailureKind::Redirect);
        assert!(error.outcome_unknown);
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/first", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for status in [302, 200] {
                let (mut stream, _) = listener.accept().unwrap();
                prepare_stream(&stream);
                request_head(&mut BufReader::new(&mut stream));
                thread::sleep(Duration::from_millis(300));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Response\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        let follow = HttpRedirectPolicy::Follow {
            max_redirects: None,
        };
        let mut request = HttpRequest::new("GET", url);
        request.redirects = follow;
        request.deadline = Some(tokio::time::Instant::now() + Duration::from_millis(450));
        let error = direct_transport(follow).send(request).await.unwrap_err();
        assert_eq!(error.kind, HttpFailureKind::DeadlineExceeded);
        assert_eq!(error.stage, HttpFailureStage::Send);
        assert!(error.outcome_unknown);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn deadline_covers_the_whole_body_and_releases_the_response_on_expiry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            request_head(&mut BufReader::new(&mut stream));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\na")
                .unwrap();
            thread::sleep(Duration::from_millis(300));
            let _ = stream.write_all(b"b");
        });
        let mut request = HttpRequest::new("GET", url);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
        request.deadline = Some(deadline);
        let mut response = direct_transport(HttpRedirectPolicy::None)
            .send(request)
            .await
            .unwrap();
        assert_eq!(response.body.chunk().await.unwrap().unwrap(), b"a");
        tokio::time::sleep(Duration::from_millis(130)).await;
        let error = response.body.chunk().await.unwrap_err();
        assert_eq!(error.kind, HttpFailureKind::DeadlineExceeded);
        assert_eq!(error.stage, HttpFailureStage::Body);
        assert!(error.outcome_unknown);
        assert!(response.body.response.is_none());
        assert_eq!(response.body.chunk().await.unwrap_err(), error);
        response.body.close();
        assert!(response.body.chunk().await.unwrap().is_none());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn interrupted_post_is_sent_once_and_reports_unknown_outcome() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            let mut reader = BufReader::new(&mut stream);
            let (first, _) = request_head(&mut reader);
            assert_eq!(first, "POST / HTTP/1.1");
            let mut body = [0; 4];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"once");
            drop(reader);
            drop(stream);
            listener.set_nonblocking(true).unwrap();
            // Retain the listening socket so a hidden retry would be accepted, rather than
            // merely fail to connect after this server exits.
            for _ in 0..20 {
                assert!(
                    matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
                );
                thread::sleep(Duration::from_millis(10));
            }
        });
        let mut request = HttpRequest::new("POST", url);
        request.body = HttpRequestBody::Bytes(b"once".to_vec());
        let error = direct_transport(HttpRedirectPolicy::None)
            .send(request)
            .await
            .unwrap_err();
        assert_eq!(error.stage, HttpFailureStage::Send);
        assert!(error.outcome_unknown);
        assert!(!error.to_string().contains("once"));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn interrupted_response_preserves_received_chunks_then_reports_body_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            request_head(&mut BufReader::new(&mut stream));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nabc")
                .unwrap();
        });
        let mut response = direct_transport(HttpRedirectPolicy::None)
            .send(HttpRequest::new("GET", url))
            .await
            .unwrap();
        let mut received = Vec::new();
        let error = loop {
            match response.body.chunk().await {
                Ok(Some(chunk)) => received.extend_from_slice(&chunk),
                Ok(None) => panic!("truncated response must not become EOF"),
                Err(error) => break error,
            }
        };
        assert_eq!(received, b"abc");
        assert_eq!(error.stage, HttpFailureStage::Body);
        assert_eq!(error.kind, HttpFailureKind::BodyInterrupted);
        assert!(error.outcome_unknown);
        assert!(response.body.response.is_none());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn response_stream_exceeds_old_twenty_mib_limit_without_aggregation() {
        const SIZE: usize = 24 * 1024 * 1024;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            request_head(&mut BufReader::new(&mut stream));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {SIZE}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let block = [0x5a; 16 * 1024];
            for _ in 0..SIZE / block.len() {
                stream.write_all(&block).unwrap();
            }
        });
        let mut response = direct_transport(HttpRedirectPolicy::None)
            .send(HttpRequest::new("GET", url))
            .await
            .unwrap();
        assert_eq!(response.content_length, Some(SIZE as u64));
        assert!(response.body.deadline.is_none());
        let mut received = 0;
        while let Some(chunk) = response.body.chunk().await.unwrap() {
            assert!(chunk.iter().all(|byte| *byte == 0x5a));
            received += chunk.len();
        }
        assert_eq!(received, SIZE);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn streaming_file_upload_and_non_replayable_redirect_preserve_http_result() {
        const SIZE: usize = 24 * 1024 * 1024;
        let path = std::env::temp_dir().join(format!(
            "wonderland-http-stream-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            let block = [0x33; 16 * 1024];
            for _ in 0..SIZE / block.len() {
                file.write_all(&block).unwrap();
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/upload", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            let mut reader = BufReader::new(&mut stream);
            let (first, headers) = request_head(&mut reader);
            assert_eq!(first, "PUT /upload HTTP/1.1");
            assert_eq!(
                values(&headers, "content-length"),
                [SIZE.to_string().as_bytes()]
            );
            let mut received = 0;
            let mut block = [0; 16 * 1024];
            while received < SIZE {
                reader.read_exact(&mut block).unwrap();
                assert!(block.iter().all(|byte| *byte == 0x33));
                received += block.len();
            }
            drop(reader);
            stream.write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /replay\r\nContent-Length: 5\r\nConnection: close\r\n\r\nagain").unwrap();
        });
        let follow = HttpRedirectPolicy::Follow {
            max_redirects: None,
        };
        let mut request = HttpRequest::new("PUT", &url);
        request.redirects = follow;
        request.headers = vec![HttpHeader {
            name: "Content-Length".into(),
            value: SIZE.to_string().into_bytes(),
        }];
        request.body = HttpRequestBody::File(tokio::fs::File::open(&path).await.unwrap());
        let mut response = direct_transport(follow).send(request).await.unwrap();
        assert_eq!(response.status, 307);
        assert_eq!(response.final_url, url);
        assert_eq!(
            values(&response.headers, "location"),
            [b"/replay".as_slice()]
        );
        assert_eq!(collect(&mut response.body).await, b"again");
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn streaming_file_upload_follows_303_without_replaying_the_file() {
        let path = std::env::temp_dir().join(format!(
            "wonderland-http-303-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap()
            .write_all(b"payload")
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/upload", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            let mut reader = BufReader::new(&mut stream);
            let (first, _) = request_head(&mut reader);
            assert_eq!(first, "POST /upload HTTP/1.1");
            let mut body = [0; 7];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"payload");
            drop(reader);
            stream.write_all(b"HTTP/1.1 303 See Other\r\nLocation: /result\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            prepare_stream(&stream);
            let (first, headers) = request_head(&mut BufReader::new(&mut stream));
            assert_eq!(first, "GET /result HTTP/1.1");
            assert!(values(&headers, "content-length").is_empty());
            stream
                .write_all(
                    b"HTTP/1.1 418 Teapot\r\nContent-Length: 3\r\nConnection: close\r\n\r\ntea",
                )
                .unwrap();
        });
        let follow = HttpRedirectPolicy::Follow {
            max_redirects: None,
        };
        let mut request = HttpRequest::new("POST", url);
        request.redirects = follow;
        request.headers = vec![HttpHeader {
            name: "Content-Length".into(),
            value: b"7".to_vec(),
        }];
        request.body = HttpRequestBody::File(tokio::fs::File::open(&path).await.unwrap());
        let mut response = direct_transport(follow).send(request).await.unwrap();
        assert_eq!(response.status, 418);
        assert!(response.final_url.ends_with("/result"));
        assert_eq!(collect(&mut response.body).await, b"tea");
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn invalid_input_and_expired_preparation_never_claim_sent_outcome() {
        let transport = direct_transport(HttpRedirectPolicy::None);
        for (method, url, expected) in [
            ("GET", "file:///C:/private", HttpFailureKind::InvalidUrl),
            ("GET", "ftp://example.com/", HttpFailureKind::InvalidUrl),
            (
                "invalid method",
                "http://127.0.0.1:9/",
                HttpFailureKind::InvalidMethod,
            ),
        ] {
            let error = transport
                .send(HttpRequest::new(method, url))
                .await
                .unwrap_err();
            assert_eq!(error.stage, HttpFailureStage::Prepare);
            assert_eq!(error.kind, expected);
            assert!(!error.outcome_unknown);
        }
        let mut request = HttpRequest::new("GET", "http://127.0.0.1:9/");
        request.headers.push(HttpHeader {
            name: "Authorization".into(),
            value: b"secret\r\nInjected: yes".to_vec(),
        });
        let error = transport.send(request).await.unwrap_err();
        assert_eq!(error.kind, HttpFailureKind::InvalidHeader);
        assert!(!error.to_string().contains("secret"));
        let mut request = HttpRequest::new("GET", "http://127.0.0.1:9/");
        request.deadline = Some(tokio::time::Instant::now() - Duration::from_secs(1));
        let error = transport.send(request).await.unwrap_err();
        assert_eq!(error.stage, HttpFailureStage::Prepare);
        assert_eq!(error.kind, HttpFailureKind::DeadlineExceeded);
        assert!(!error.outcome_unknown);
    }

    #[tokio::test]
    async fn failed_initial_connection_without_redirects_is_known_not_sent() {
        let address = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let mut request = HttpRequest::new("POST", format!("http://{address}/?key=private"));
        request.body = HttpRequestBody::Bytes(b"request body".to_vec());
        request.deadline = Some(tokio::time::Instant::now() + Duration::from_secs(8));
        let error = direct_transport(HttpRedirectPolicy::None)
            .send(request)
            .await
            .unwrap_err();
        assert_eq!(error.kind, HttpFailureKind::Connection);
        assert_eq!(error.stage, HttpFailureStage::Send);
        assert!(!error.outcome_unknown);
        assert!(!error.to_string().contains("private"));
    }
}
