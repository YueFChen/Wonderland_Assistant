//! Classify transport failures without returning source messages, URLs or credentials.
use std::error::Error;

use crate::HttpFailureKind;

impl HttpFailureKind {
    pub fn from_reqwest(error: &reqwest::Error) -> Self {
        // A connection timeout satisfies both is_connect and is_timeout.
        if error.is_timeout() {
            return Self::Timeout;
        }
        if error.status() == Some(reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED) {
            return Self::ProxyAuthentication;
        }
        if let Some(kind) = error.source().and_then(|cause| known_cause(cause, 16)) {
            return kind;
        }
        if error.is_connect() {
            Self::Connection
        } else if error.is_redirect() {
            Self::Redirect
        } else {
            Self::RequestFailed
        }
    }

    /// Safe user-facing summary shared by distribution and updater callers.
    pub fn diagnostic_message(self) -> &'static str {
        match self {
            Self::Timeout | Self::DeadlineExceeded => "网络请求超时",
            Self::ProxyAuthentication => "代理认证失败，请检查代理账号和密码",
            Self::Dns => "无法解析目标或代理的主机名",
            Self::Tls => "TLS 握手或证书验证失败",
            Self::Connection => "无法连接目标或代理服务器",
            Self::BodyInterrupted => "响应读取中断",
            Self::ClientUnavailable => "网络客户端或代理配置不可用",
            _ => "网络请求失败",
        }
    }
}

fn known_cause(error: &(dyn Error + 'static), budget: usize) -> Option<HttpFailureKind> {
    if budget == 0 {
        return None;
    }
    if error.is::<rustls::Error>() {
        return Some(HttpFailureKind::Tls);
    }
    if let Some(io) = error.downcast_ref::<std::io::Error>() {
        if io.kind() == std::io::ErrorKind::TimedOut {
            return Some(HttpFailureKind::Timeout);
        }
        // io::Error::source can skip its wrapped error itself (notably rustls::Error).
        if let Some(cause) = io.get_ref() {
            return known_cause(cause, budget - 1);
        }
    }
    // hyper-util's TunnelError and ConnectError are not publicly exported.
    // Match only their fixed diagnostic markers, never arbitrary server text.
    match error.to_string().as_str() {
        "tunnel error: proxy authorization required" => Some(HttpFailureKind::ProxyAuthentication),
        "dns error" => Some(HttpFailureKind::Dns),
        _ => error
            .source()
            .and_then(|cause| known_cause(cause, budget - 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        sync::Arc,
        time::Duration,
    };

    struct FailedDns;
    impl reqwest::dns::Resolve for FailedDns {
        fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
            Box::pin(async { Err(std::io::Error::other("private resolver diagnostics").into()) })
        }
    }

    #[tokio::test]
    async fn dns_failure_is_classified_without_returning_resolver_text() {
        let error = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(FailedDns))
            .build()
            .unwrap()
            .get("https://private.example/")
            .send()
            .await
            .unwrap_err();
        let kind = HttpFailureKind::from_reqwest(&error);
        assert_eq!(kind, HttpFailureKind::Dns);
        assert!(!kind.diagnostic_message().contains("private"));
    }

    #[tokio::test]
    async fn connection_timeout_is_not_misclassified_as_connection_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut head = [0; 4096];
            let _ = stream.read(&mut head).unwrap();
            // Leave the TLS handshake unfinished so the connector times out.
            std::thread::sleep(Duration::from_millis(200));
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(100))
            .build()
            .unwrap()
            .get(format!("https://{address}/"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect());
        assert!(error.is_timeout());
        assert_eq!(
            HttpFailureKind::from_reqwest(&error),
            HttpFailureKind::Timeout
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn invalid_tls_record_is_classified_as_tls_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut hello = [0; 4096];
            let _ = stream.read(&mut hello).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        });
        let error = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
            .get(format!("https://{address}/"))
            .send()
            .await
            .unwrap_err();
        assert_eq!(
            HttpFailureKind::from_reqwest(&error),
            HttpFailureKind::Tls,
            "{error:?}"
        );
        server.join().unwrap();
    }
}
