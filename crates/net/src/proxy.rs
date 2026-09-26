//! Core-managed HTTP proxy policy.
//!
//! System mode follows the platform proxy integration and standard proxy environment
//! variables. Direct and custom modes explicitly disable those implicit settings.

use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    #[default]
    System,
    Direct,
    Custom,
}

/// Persistable settings. Authentication is deliberately stored separately as a DPAPI blob by
/// the desktop host and is never part of this serializable structure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySettings {
    pub mode: ProxyMode,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub bypass: String,
}

impl ProxySettings {
    /// Validate and normalize a proxy setting without including caller input in errors.
    pub fn validated(mut self) -> Result<Self, ProxyError> {
        match self.mode {
            ProxyMode::System | ProxyMode::Direct => {
                self.address = None;
                self.bypass.clear();
            }
            ProxyMode::Custom => {
                let raw = self.address.as_deref().ok_or(ProxyError::InvalidAddress)?;
                if raw.len() > 2048 {
                    return Err(ProxyError::InvalidAddress);
                }
                let url = reqwest::Url::parse(raw).map_err(|_| ProxyError::InvalidAddress)?;
                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.path() != "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(ProxyError::InvalidAddress);
                }
                self.address = Some(url.origin().ascii_serialization());
                if self.bypass.len() > 2048
                    || self.bypass.chars().any(char::is_control)
                    || !self.bypass.is_ascii()
                    || self
                        .bypass
                        .chars()
                        .any(|ch| !(ch.is_ascii_alphanumeric() || ".,:/[]%* -_".contains(ch)))
                {
                    return Err(ProxyError::InvalidBypass);
                }
            }
        }
        Ok(self)
    }
}

/// Proxy credentials stay in memory only after the host decrypts its DPAPI-protected store.
#[derive(Clone)]
pub struct ProxyCredentials {
    username: String,
    password: String,
}

impl ProxyCredentials {
    pub fn new(username: String, password: String) -> Result<Self, ProxyError> {
        if username.is_empty()
            || password.is_empty()
            || username.len() > 512
            || password.len() > 2048
        {
            return Err(ProxyError::InvalidCredentials);
        }
        Ok(Self { username, password })
    }
}

impl std::fmt::Debug for ProxyCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProxyCredentials(<redacted>)")
    }
}

#[derive(Clone)]
pub struct ProxyConfiguration {
    settings: ProxySettings,
    credentials: Option<ProxyCredentials>,
}

impl std::fmt::Debug for ProxyConfiguration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfiguration")
            .field("settings", &self.settings)
            .field(
                "credentials",
                &self.credentials.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl ProxyConfiguration {
    pub fn new(
        settings: ProxySettings,
        credentials: Option<ProxyCredentials>,
    ) -> Result<Self, ProxyError> {
        let settings = settings.validated()?;
        if let Some(address) = &settings.address {
            reqwest::Proxy::all(address).map_err(|_| ProxyError::InvalidAddress)?;
        }
        Ok(Self {
            settings,
            credentials,
        })
    }

    pub fn settings(&self) -> &ProxySettings {
        &self.settings
    }

    pub fn has_credentials(&self) -> bool {
        self.credentials.is_some()
    }

    /// Apply this policy to any reqwest client builder, including Tauri's signed updater client.
    pub fn apply_to(&self, mut builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        match self.settings.mode {
            ProxyMode::System => builder,
            ProxyMode::Direct => builder.no_proxy(),
            ProxyMode::Custom => {
                let address = self
                    .settings
                    .address
                    .as_deref()
                    .expect("custom proxy settings are validated");
                let mut proxy = reqwest::Proxy::all(address)
                    .expect("validated proxy address must be accepted by reqwest");
                if let Some(credentials) = &self.credentials {
                    proxy = proxy.basic_auth(&credentials.username, &credentials.password);
                }
                if !self.settings.bypass.is_empty() {
                    proxy = proxy.no_proxy(reqwest::NoProxy::from_string(&self.settings.bypass));
                }
                builder = builder.no_proxy().proxy(proxy);
                builder
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyError {
    InvalidAddress,
    InvalidBypass,
    InvalidCredentials,
    Unavailable,
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidAddress => "代理地址无效；请填写不含账号或密码的 HTTP(S) 地址",
            Self::InvalidBypass => "代理绕过列表包含不支持的字符或过长",
            Self::InvalidCredentials => "代理账号或密码格式无效",
            Self::Unavailable => "代理设置暂不可用",
        })
    }
}

impl std::error::Error for ProxyError {}

#[derive(Clone)]
pub struct ProxySnapshot {
    pub revision: u64,
    pub configuration: ProxyConfiguration,
}

struct GlobalProxy {
    revision: u64,
    configuration: ProxyConfiguration,
}

fn global_proxy() -> &'static RwLock<GlobalProxy> {
    static GLOBAL: OnceLock<RwLock<GlobalProxy>> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        RwLock::new(GlobalProxy {
            revision: 0,
            configuration: ProxyConfiguration::new(ProxySettings::default(), None)
                .expect("default proxy configuration is valid"),
        })
    })
}

pub fn current_proxy() -> Result<ProxySnapshot, ProxyError> {
    let current = global_proxy().read().map_err(|_| ProxyError::Unavailable)?;
    Ok(ProxySnapshot {
        revision: current.revision,
        configuration: current.configuration.clone(),
    })
}

pub fn set_current_proxy(configuration: ProxyConfiguration) -> Result<u64, ProxyError> {
    let mut current = global_proxy()
        .write()
        .map_err(|_| ProxyError::Unavailable)?;
    current.revision = current.revision.wrapping_add(1);
    current.configuration = configuration;
    Ok(current.revision)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEnvironmentVariable {
    pub name: &'static str,
    pub configured: bool,
}

/// Report which standard proxy variables are present without exposing their values.
pub fn proxy_environment_variables() -> Vec<ProxyEnvironmentVariable> {
    ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"]
        .into_iter()
        .map(|name| ProxyEnvironmentVariable {
            name,
            configured: std::env::var_os(name).is_some()
                || std::env::var_os(name.to_ascii_lowercase()).is_some(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn proxy_address_rejects_embedded_credentials_and_paths() {
        let long_address = format!("http://127.0.0.1/{}", "a".repeat(2048));
        for address in [
            "http://user:password@127.0.0.1:8080",
            "http://127.0.0.1:8080/path",
            "ftp://127.0.0.1:8080",
            "http://127.0.0.1:8080/?token=secret",
            long_address.as_str(),
        ] {
            let settings = ProxySettings {
                mode: ProxyMode::Custom,
                address: Some(address.to_owned()),
                bypass: String::new(),
            };
            assert_eq!(settings.validated(), Err(ProxyError::InvalidAddress));
        }
    }

    #[test]
    fn credentials_are_redacted_from_debug_output() {
        let credentials = ProxyCredentials::new("proxy-user".into(), "top-secret".into()).unwrap();
        assert!(!format!("{credentials:?}").contains("top-secret"));
        assert!(!format!("{credentials:?}").contains("proxy-user"));
    }

    #[test]
    fn custom_proxy_sends_auth_without_connecting_directly() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET http://example.invalid/ HTTP/1.1"));
            assert!(request.lines().any(|line| {
                line.eq_ignore_ascii_case("proxy-authorization: Basic cHJveHktdXNlcjp0b3Atc2VjcmV0")
            }));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });

        let config = ProxyConfiguration::new(
            ProxySettings {
                mode: ProxyMode::Custom,
                address: Some(address),
                bypass: String::new(),
            },
            Some(ProxyCredentials::new("proxy-user".into(), "top-secret".into()).unwrap()),
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime.block_on(async {
            let client = config
                .apply_to(reqwest::Client::builder().timeout(std::time::Duration::from_secs(3)))
                .build()
                .unwrap();
            client.get("http://example.invalid/").send().await.unwrap()
        });
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        server.join().unwrap();
    }

    #[test]
    fn direct_mode_bypasses_environment_and_custom_proxy_settings() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET /health HTTP/1.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let config = ProxyConfiguration::new(
            ProxySettings {
                mode: ProxyMode::Direct,
                address: None,
                bypass: String::new(),
            },
            None,
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime.block_on(async {
            let client = config
                .apply_to(reqwest::Client::builder().timeout(std::time::Duration::from_secs(3)))
                .build()
                .unwrap();
            client
                .get(format!("http://{target}/health"))
                .send()
                .await
                .unwrap()
        });
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        server.join().unwrap();
    }
}
