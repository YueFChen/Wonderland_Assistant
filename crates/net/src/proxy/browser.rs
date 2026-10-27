//! Native browser proxy policy. Credentials never appear in Chromium arguments.
use super::{ProxyConfiguration, ProxyCredentials, ProxyError, ProxyMode};

#[derive(Clone, Default)]
pub struct BrowserProxyPolicy {
    arguments: Option<String>,
    endpoints: Vec<(String, Option<ProxyCredentials>)>,
}

impl std::fmt::Debug for BrowserProxyPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserProxyPolicy")
            .field("arguments", &self.arguments)
            .field("proxy_count", &self.endpoints.len())
            .finish()
    }
}

impl ProxyConfiguration {
    pub fn browser_policy(&self) -> Result<BrowserProxyPolicy, ProxyError> {
        match self.settings.mode {
            ProxyMode::System => BrowserProxyPolicy::system(),
            ProxyMode::Direct => Ok(BrowserProxyPolicy::direct()),
            ProxyMode::Custom => {
                let address = self
                    .settings
                    .address
                    .as_deref()
                    .ok_or(ProxyError::InvalidAddress)?;
                let mut policy = BrowserProxyPolicy::default();
                let endpoint = policy.endpoint(address, self.credentials.clone())?;
                policy.arguments = Some(format!(
                    "--proxy-server=\"{endpoint}\" --proxy-bypass-list=\"{}\"",
                    bypass_rules(&self.settings.bypass)
                ));
                Ok(policy)
            }
        }
    }
}

impl BrowserProxyPolicy {
    fn direct() -> Self {
        Self {
            arguments: Some("--no-proxy-server".into()),
            endpoints: Vec::new(),
        }
    }

    /// Networking switches only. The host retains the WebView's other default switches.
    pub fn chromium_arguments(&self) -> Option<&str> {
        self.arguments.as_deref()
    }

    pub fn has_explicit_proxy(&self) -> bool {
        !self.endpoints.is_empty()
    }

    fn endpoint_for_challenge(&self, uri: &str) -> Option<&Option<ProxyCredentials>> {
        let url = reqwest::Url::parse(uri).ok()?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let origin = url.origin().ascii_serialization();
        self.endpoints
            .iter()
            .find(|(endpoint, _)| *endpoint == origin)
            .map(|(_, credentials)| credentials)
    }

    pub fn is_proxy_challenge(&self, uri: &str) -> bool {
        self.endpoint_for_challenge(uri).is_some()
    }

    /// Only a challenge for a configured proxy origin can obtain that proxy's credentials.
    pub fn credentials_for_challenge(&self, uri: &str) -> Option<(&str, &str)> {
        let credentials = self.endpoint_for_challenge(uri)?.as_ref()?;
        Some((&credentials.username, &credentials.password))
    }

    fn endpoint(
        &mut self,
        raw: &str,
        credentials: Option<ProxyCredentials>,
    ) -> Result<String, ProxyError> {
        let raw = if raw.contains("://") {
            raw.to_owned()
        } else {
            format!("http://{raw}")
        };
        let url = reqwest::Url::parse(&raw).map_err(|_| ProxyError::InvalidAddress)?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(ProxyError::InvalidAddress);
        }
        let credentials = match credentials {
            Some(credentials) => Some(credentials),
            None if !url.username().is_empty() || url.password().is_some() => {
                Some(ProxyCredentials {
                    username: percent_encoding::percent_decode_str(url.username())
                        .decode_utf8()
                        .map_err(|_| ProxyError::InvalidCredentials)?
                        .into_owned(),
                    password: percent_encoding::percent_decode_str(
                        url.password().unwrap_or_default(),
                    )
                    .decode_utf8()
                    .map_err(|_| ProxyError::InvalidCredentials)?
                    .into_owned(),
                })
            }
            None => None,
        };
        if credentials
            .as_ref()
            .is_some_and(|value| value.username.contains('\0') || value.password.contains('\0'))
        {
            return Err(ProxyError::InvalidCredentials);
        }
        let origin = url.origin().ascii_serialization();
        self.endpoints.push((origin.clone(), credentials));
        Ok(origin)
    }

    fn system() -> Result<Self, ProxyError> {
        let env = |name: &str| {
            std::env::var(name)
                .or_else(|_| std::env::var(name.to_ascii_lowercase()))
                .unwrap_or_default()
        };
        let http = env("HTTP_PROXY");
        let https = env("HTTPS_PROXY");
        let all = env("ALL_PROXY");
        let no = env("NO_PROXY");
        if std::env::var_os("REQUEST_METHOD").is_some() {
            return Ok(Self::direct());
        }
        if [&http, &https, &all, &no]
            .iter()
            .all(|value| value.is_empty())
        {
            // Let WebView2 use the user's native system proxy integration.
            return Ok(Self::default());
        }
        let (system_proxy, system_bypass) = windows_manual_proxy();
        Self::from_environment(&http, &https, &all, &no, &system_proxy, &system_bypass)
    }

    fn from_environment(
        http: &str,
        https: &str,
        all: &str,
        no: &str,
        system: &str,
        system_bypass: &str,
    ) -> Result<Self, ProxyError> {
        let mut policy = Self::default();
        let mut mappings = Vec::new();
        for (scheme, explicit) in [("http", http), ("https", https)] {
            let system_endpoint = if system.contains('=') {
                system
                    .split(';')
                    .filter_map(|entry| entry.trim().split_once('='))
                    .find(|(name, _)| *name == scheme)
                    .map(|(_, value)| value)
                    .unwrap_or_default()
            } else {
                system
            };
            // Match reqwest: per-scheme environment, then Windows manual proxy, then ALL_PROXY.
            let raw = [explicit, system_endpoint, all]
                .into_iter()
                .find(|value| !value.is_empty());
            let endpoint = match raw {
                Some(raw) => policy.endpoint(raw, None)?,
                None => "direct://".into(),
            };
            mappings.push(format!("{scheme}={endpoint}"));
        }
        if policy.endpoints.is_empty() {
            return Ok(Self::direct());
        }
        let bypass = if no.is_empty() { system_bypass } else { no };
        policy.arguments = Some(format!(
            "--proxy-server=\"{}\" --proxy-bypass-list=\"{}\"",
            mappings.join(";"),
            bypass_rules(bypass)
        ));
        Ok(policy)
    }
}

fn windows_manual_proxy() -> (String, String) {
    #[cfg(windows)]
    {
        use winreg::{RegKey, enums::HKEY_CURRENT_USER};
        if let Ok(settings) = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
            && settings.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0
        {
            return (
                settings.get_value("ProxyServer").unwrap_or_default(),
                settings
                    .get_value::<String, _>("ProxyOverride")
                    .unwrap_or_default()
                    .replace(';', ",")
                    .replace("*.", ""),
            );
        }
    }
    (String::new(), String::new())
}

fn bypass_rules(raw: &str) -> String {
    // Core's explicit bypass list is authoritative, including for localhost.
    let mut rules = vec!["<-loopback>".to_owned()];
    for entry in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if entry == "*" {
            rules.push("*".into());
        } else if let Ok(ip) = entry.parse::<std::net::IpAddr>() {
            rules.push(match ip {
                std::net::IpAddr::V4(ip) => ip.to_string(),
                std::net::IpAddr::V6(ip) => format!("[{ip}]"),
            });
        } else if let Some((ip, prefix)) = entry.split_once('/') {
            if let Ok(ip) = ip.parse::<std::net::IpAddr>()
                && prefix
                    .parse::<u8>()
                    .is_ok_and(|prefix| prefix <= if ip.is_ipv4() { 32 } else { 128 })
            {
                rules.push(format!("{ip}/{prefix}"));
            }
        } else {
            let domain = entry.strip_prefix('.').unwrap_or(entry);
            // reqwest supports only a whole-entry wildcard. Do not turn ignored input
            // into a broader Chromium wildcard or an injected command-line switch.
            if !domain.is_empty()
                && domain
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            {
                rules.push(domain.to_owned());
                rules.push(format!("*.{domain}"));
            }
        }
    }
    rules.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProxySettings;

    #[test]
    fn custom_proxy_credentials_are_bound_to_the_proxy_and_never_in_arguments() {
        for scheme in ["http", "https"] {
            let origin = format!("{scheme}://proxy.example:8443");
            let policy = ProxyConfiguration::new(
                ProxySettings {
                    mode: ProxyMode::Custom,
                    address: Some(origin.clone()),
                    bypass: String::new(),
                },
                Some(
                    ProxyCredentials::new("private-user".into(), "private-password".into())
                        .unwrap(),
                ),
            )
            .unwrap()
            .browser_policy()
            .unwrap();
            assert_eq!(
                policy.credentials_for_challenge(&origin),
                Some(("private-user", "private-password"))
            );
            for uri in [
                "http://website.example/",
                "http://proxy.example:9999/",
                "https://proxy.example:8443/login",
                "https://proxy.example:8443/?challenge=1",
            ] {
                assert_eq!(policy.credentials_for_challenge(uri), None);
            }
            let safe = format!("{policy:?} {}", policy.chromium_arguments().unwrap());
            assert!(!safe.contains("private-user"));
            assert!(!safe.contains("private-password"));
            assert!(!safe.contains("direct://"));
            assert!(policy.chromium_arguments().unwrap().contains("<-loopback>"));
        }
    }

    #[test]
    fn bypass_translation_keeps_apex_subdomain_and_ip_rules_without_injected_switches() {
        let rules = bypass_rules(
            ".example.com,127.0.0.1,::1,192.168.0.0/16,fe80::/10,*.ignored.example,\" --no-proxy-server",
        );
        for expected in [
            "example.com",
            "*.example.com",
            "127.0.0.1",
            "[::1]",
            "192.168.0.0/16",
            "fe80::/10",
        ] {
            assert!(rules.split(';').any(|rule| rule == expected));
        }
        assert!(!rules.contains("ignored.example"));
        assert!(!rules.contains("--no-proxy-server"));
        assert!(!rules.contains('"'));
    }

    #[test]
    fn environment_proxy_authentication_and_protocol_precedence_are_preserved() {
        let policy = BrowserProxyPolicy::from_environment(
            "http://env%2Duser:env%3Apassword@env.example:8080",
            "",
            "http://all.example:9000",
            ".example.test",
            "http=system-http.example:8000;https=system-https.example:8001",
            "localhost",
        )
        .unwrap();
        let arguments = policy.chromium_arguments().unwrap();
        assert!(
            arguments
                .contains("http=http://env.example:8080;https=http://system-https.example:8001")
        );
        assert!(!arguments.contains("all.example"));
        assert!(!arguments.contains("system-http.example"));
        assert!(!arguments.contains("env%"));
        assert!(!arguments.contains("password"));
        assert_eq!(
            policy.credentials_for_challenge("http://env.example:8080/"),
            Some(("env-user", "env:password"))
        );
        assert!(arguments.contains("example.test;*.example.test"));
        assert!(!arguments.contains("localhost"));
        let policy =
            BrowserProxyPolicy::from_environment("", "", "http://all.example:9000", "", "", "")
                .unwrap();
        assert!(
            policy
                .chromium_arguments()
                .unwrap()
                .contains("http=http://all.example:9000;https=http://all.example:9000")
        );
    }

    #[test]
    fn invalid_environment_proxy_fails_without_silently_using_direct() {
        assert!(
            BrowserProxyPolicy::from_environment("not a valid proxy", "", "", "", "", "").is_err()
        );
        assert!(
            BrowserProxyPolicy::from_environment("socks5://proxy.example:1080", "", "", "", "", "")
                .is_err()
        );
        assert_eq!(
            ProxyConfiguration::new(
                ProxySettings {
                    mode: ProxyMode::Direct,
                    ..ProxySettings::default()
                },
                None
            )
            .unwrap()
            .browser_policy()
            .unwrap()
            .chromium_arguments(),
            Some("--no-proxy-server")
        );
    }
}
