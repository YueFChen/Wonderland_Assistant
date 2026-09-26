use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State, WebviewWindow};
use wonderland_kernel::KernelError;
use wonderland_kernel::logging::{info, warn};
use wonderland_net::{
    PluginCatalogClient, ProxyConfiguration, ProxyCredentials, ProxyEnvironmentVariable, ProxyMode,
    ProxySettings,
};

use crate::commands::ensure_main_window;
use crate::core_update::CoreUpdateState;

const SETTINGS_FILE: &str = "network-proxy.json";
const MAX_SETTINGS_BYTES: u64 = 16 * 1024;

/// Safe-to-display proxy settings. Secret values are never returned to the renderer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySettingsView {
    pub mode: ProxyMode,
    pub address: Option<String>,
    pub bypass: String,
    pub credentials_configured: bool,
    pub credentials_available: bool,
    pub credentials_match_proxy: bool,
    pub windows_system_proxy: WindowsSystemProxyState,
    pub environment_variables: Vec<ProxyEnvironmentVariable>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsSystemProxyState {
    pub supported: bool,
    pub enabled: bool,
    pub server_configured: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxySettingsFile {
    schema_version: u32,
    settings: ProxySettings,
    #[serde(default)]
    encrypted_credentials: Option<String>,
    #[serde(default)]
    credentials_proxy_address: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxySettingsFileWrite<'a> {
    schema_version: u32,
    settings: &'a ProxySettings,
    encrypted_credentials: &'a Option<String>,
    credentials_proxy_address: &'a Option<String>,
}

#[derive(Deserialize, Serialize)]
struct ProxyCredentialPayload {
    username: String,
    password: String,
}

#[derive(Default)]
struct RuntimeSettings {
    settings: ProxySettings,
    credentials: Option<ProxyCredentials>,
    encrypted_credentials: Option<String>,
    credentials_proxy_address: Option<String>,
    credentials_available: bool,
    warning: Option<String>,
}

/// App-scoped persisted proxy settings with credentials protected by Windows DPAPI.
pub struct NetworkProxyService {
    path: PathBuf,
    state: RwLock<RuntimeSettings>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySettingsUpdate {
    pub mode: ProxyMode,
    pub address: Option<String>,
    pub bypass: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub clear_credentials: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResult {
    pub success: bool,
    pub route: &'static str,
    pub outcome: &'static str,
    pub http_status: Option<u16>,
    pub duration_ms: u64,
}

impl NetworkProxyService {
    pub fn load(app_data_dir: &Path) -> Result<Self, KernelError> {
        let path = app_data_dir.join(SETTINGS_FILE);
        let mut runtime = RuntimeSettings {
            settings: ProxySettings::default(),
            credentials_available: true,
            ..RuntimeSettings::default()
        };

        let (file, warning) = load_proxy_settings_file(&path);
        runtime.warning = warning;
        if let Some(file) = file {
            runtime.settings = file.settings;
            runtime.encrypted_credentials = file.encrypted_credentials;
            runtime.credentials_proxy_address = file.credentials_proxy_address;
        }

        if let Some(sealed) = &runtime.encrypted_credentials {
            let decrypted = base64::engine::general_purpose::STANDARD
                .decode(sealed)
                .ok()
                .and_then(|bytes| wonderland_secret::unseal(&bytes).ok())
                .and_then(|bytes| serde_json::from_slice::<ProxyCredentialPayload>(&bytes).ok());
            match decrypted {
                Some(payload) => match ProxyCredentials::new(payload.username, payload.password) {
                    Ok(credentials) => runtime.credentials = Some(credentials),
                    Err(_) => {
                        runtime.credentials_available = false;
                        runtime.warning =
                            Some("代理认证信息无法读取；请重新输入凭据或清除认证信息。".to_owned());
                    }
                },
                None => {
                    runtime.credentials_available = false;
                    runtime.warning = Some(
                        "代理认证信息无法解密；请在当前 Windows 用户下重新输入或清除凭据。"
                            .to_owned(),
                    );
                }
            }
        }

        let active_credentials = credentials_for_settings(
            &runtime.settings,
            runtime.credentials_proxy_address.as_deref(),
            runtime.credentials.clone(),
        );
        let configuration = ProxyConfiguration::new(runtime.settings.clone(), active_credentials)
            .map_err(|_| KernelError::InvalidInput)?;
        wonderland_net::proxy::set_current_proxy(configuration)
            .map_err(|_| KernelError::Transport("代理设置暂不可用".to_owned()))?;

        Ok(Self {
            path,
            state: RwLock::new(runtime),
        })
    }

    pub fn view(&self) -> Result<ProxySettingsView, KernelError> {
        let state = self
            .state
            .read()
            .map_err(|_| KernelError::Transport("代理设置暂不可用".to_owned()))?;
        Ok(ProxySettingsView {
            mode: state.settings.mode,
            address: state.settings.address.clone(),
            bypass: state.settings.bypass.clone(),
            credentials_configured: state.encrypted_credentials.is_some(),
            credentials_available: state.credentials_available,
            credentials_match_proxy: credentials_for_settings(
                &state.settings,
                state.credentials_proxy_address.as_deref(),
                state.credentials.clone(),
            )
            .is_some(),
            windows_system_proxy: windows_system_proxy_state(),
            environment_variables: wonderland_net::proxy::proxy_environment_variables(),
            warning: state.warning.clone(),
        })
    }

    pub fn update(&self, update: ProxySettingsUpdate) -> Result<ProxySettingsView, KernelError> {
        let settings = ProxySettings {
            mode: update.mode,
            address: update.address,
            bypass: update.bypass,
        }
        .validated()
        .map_err(|_| KernelError::InvalidInput)?;

        let mut state = self
            .state
            .write()
            .map_err(|_| KernelError::Transport("代理设置暂不可用".to_owned()))?;

        let (credentials, encrypted_credentials, credentials_proxy_address) = if update
            .clear_credentials
        {
            (None, None, None)
        } else if !update.username.is_empty() || !update.password.is_empty() {
            let credential_proxy_address = if settings.mode == ProxyMode::Custom {
                settings.address.clone()
            } else {
                return Err(KernelError::InvalidInput);
            };
            let credentials =
                ProxyCredentials::new(update.username.clone(), update.password.clone())
                    .map_err(|_| KernelError::InvalidInput)?;
            let payload = ProxyCredentialPayload {
                username: update.username,
                password: update.password,
            };
            let plaintext = serde_json::to_vec(&payload).map_err(|_| KernelError::InvalidInput)?;
            let encrypted = wonderland_secret::seal(&plaintext)
                .map_err(|_| KernelError::Transport("代理认证信息无法保护".to_owned()))?;
            (
                Some(credentials),
                Some(base64::engine::general_purpose::STANDARD.encode(encrypted)),
                credential_proxy_address,
            )
        } else {
            (
                state.credentials.clone(),
                state.encrypted_credentials.clone(),
                state.credentials_proxy_address.clone(),
            )
        };

        let active_credentials = credentials_for_settings(
            &settings,
            credentials_proxy_address.as_deref(),
            credentials.clone(),
        );
        let configuration = ProxyConfiguration::new(settings.clone(), active_credentials)
            .map_err(|_| KernelError::InvalidInput)?;
        write_settings_file(
            &self.path,
            &settings,
            &encrypted_credentials,
            &credentials_proxy_address,
        )?;
        wonderland_net::proxy::set_current_proxy(configuration)
            .map_err(|_| KernelError::Transport("代理设置暂不可用".to_owned()))?;

        state.settings = settings;
        state.credentials = credentials;
        state.encrypted_credentials = encrypted_credentials;
        state.credentials_proxy_address = credentials_proxy_address;
        state.credentials_available =
            state.credentials.is_some() || state.encrypted_credentials.is_none();
        if state.credentials_available {
            state.warning = None;
        }
        drop(state);
        self.view()
    }
}

fn write_settings_file(
    path: &Path,
    settings: &ProxySettings,
    encrypted_credentials: &Option<String>,
    credentials_proxy_address: &Option<String>,
) -> Result<(), KernelError> {
    let bytes = serde_json::to_vec_pretty(&ProxySettingsFileWrite {
        schema_version: 1,
        settings,
        encrypted_credentials,
        credentials_proxy_address,
    })
    .map_err(|_| KernelError::InvalidResponse)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("{nonce}.tmp"));
    let backup = path.with_extension("json.bak");
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(storage)?;
    use std::io::Write;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(storage(error));
    }
    drop(file);

    if path.exists() {
        if backup.exists()
            && let Err(error) = fs::remove_file(&backup)
        {
            let _ = fs::remove_file(&temp);
            return Err(storage(error));
        }
        if let Err(error) = fs::rename(path, &backup) {
            let _ = fs::remove_file(&temp);
            return Err(storage(error));
        }
    }
    if let Err(error) = fs::rename(&temp, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temp);
        return Err(storage(error));
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn read_proxy_settings_file(path: &Path) -> Result<Option<ProxySettingsFile>, ()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if metadata.len() > MAX_SETTINGS_BYTES {
        return Err(());
    }
    let bytes = fs::read(path).map_err(|_| ())?;
    let mut file: ProxySettingsFile = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if file.schema_version != 1 {
        return Err(());
    }
    file.settings = file.settings.validated().map_err(|_| ())?;
    Ok(Some(file))
}

fn load_proxy_settings_file(path: &Path) -> (Option<ProxySettingsFile>, Option<String>) {
    let backup = path.with_extension("json.bak");
    match read_proxy_settings_file(path) {
        Ok(Some(file)) => {
            let _ = fs::remove_file(backup);
            (Some(file), None)
        }
        Ok(None) => match read_proxy_settings_file(&backup) {
            Ok(Some(file)) => {
                let recovered = fs::rename(&backup, path).is_ok();
                if recovered {
                    warn!("代理设置从备份恢复");
                    (Some(file), Some("代理设置已从备份恢复。".to_owned()))
                } else {
                    warn!("代理设置备份已载入，但无法写回主文件");
                    (
                        Some(file),
                        Some("代理设置从备份读取，但无法恢复主文件。".to_owned()),
                    )
                }
            }
            Ok(None) => (None, None),
            Err(()) => {
                warn!("代理设置备份无法读取，使用默认模式");
                (
                    None,
                    Some("代理设置备份无法读取，已使用默认模式。".to_owned()),
                )
            }
        },
        Err(()) => match read_proxy_settings_file(&backup) {
            Ok(Some(file)) => {
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let corrupt = path.with_extension(format!("json.corrupt-{nonce}"));
                let moved_corrupt = fs::rename(path, corrupt).is_ok();
                let restored = moved_corrupt && fs::rename(&backup, path).is_ok();
                if restored {
                    warn!("代理设置从备份恢复，损坏文件已保留");
                    (
                        Some(file),
                        Some("代理设置损坏，已从备份恢复；损坏文件已保留供检查。".to_owned()),
                    )
                } else {
                    warn!("代理设置备份已载入，但无法恢复主文件");
                    (
                        Some(file),
                        Some("代理设置从备份读取，但恢复主文件失败。".to_owned()),
                    )
                }
            }
            _ => {
                warn!("代理设置无法读取，使用默认模式");
                (None, Some("代理设置无法读取，已使用默认模式。".to_owned()))
            }
        },
    }
}

fn storage(error: std::io::Error) -> KernelError {
    KernelError::Transport(format!("代理设置保存失败：{}", error.kind()))
}

fn credentials_for_settings(
    settings: &ProxySettings,
    credential_proxy_address: Option<&str>,
    credentials: Option<ProxyCredentials>,
) -> Option<ProxyCredentials> {
    if settings.mode == ProxyMode::Custom && settings.address.as_deref() == credential_proxy_address
    {
        credentials
    } else {
        None
    }
}

#[tauri::command]
pub fn network_proxy_get(
    window: WebviewWindow,
    proxy: State<'_, NetworkProxyService>,
) -> Result<ProxySettingsView, KernelError> {
    ensure_main_window(&window)?;
    proxy.view()
}

#[tauri::command]
pub fn network_proxy_set(
    window: WebviewWindow,
    proxy: State<'_, NetworkProxyService>,
    updates: State<'_, CoreUpdateState>,
    update: ProxySettingsUpdate,
) -> Result<ProxySettingsView, KernelError> {
    ensure_main_window(&window)?;
    let view = proxy.update(update)?;
    updates.clear();
    let _ = window.emit("core-update-invalidated", ());
    Ok(view)
}

#[tauri::command]
pub async fn network_proxy_test(
    window: WebviewWindow,
    proxy: State<'_, NetworkProxyService>,
) -> Result<ProxyTestResult, KernelError> {
    ensure_main_window(&window)?;
    let view = proxy.view()?;
    let started = Instant::now();
    let result = match PluginCatalogClient::new() {
        Ok(client) => client.get_index().await,
        Err(error) => Err(error),
    };
    let (outcome, http_status) = match &result {
        Ok(_) => ("connected", None),
        Err(KernelError::Timeout) => ("timeout", None),
        Err(KernelError::Connection) => ("connection_failed", None),
        Err(KernelError::Http(407)) => ("proxy_auth_required", Some(407)),
        Err(KernelError::Http(status)) => ("http_error", Some(*status)),
        Err(KernelError::InvalidResponse) => ("invalid_response", None),
        Err(_) => ("request_failed", None),
    };
    let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    info!(
        route = proxy_route(view.mode),
        outcome,
        duration_ms = elapsed,
        "Core 目录网络连通性检查完成"
    );
    Ok(ProxyTestResult {
        success: result.is_ok(),
        route: proxy_route(view.mode),
        outcome,
        http_status,
        duration_ms: elapsed,
    })
}

fn proxy_route(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::System => "system",
        ProxyMode::Direct => "direct",
        ProxyMode::Custom => "custom",
    }
}

#[cfg(windows)]
fn windows_system_proxy_state() -> WindowsSystemProxyState {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let Ok(internet_settings) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
    else {
        return WindowsSystemProxyState {
            supported: true,
            ..WindowsSystemProxyState::default()
        };
    };
    let enabled = internet_settings
        .get_value::<u32, _>("ProxyEnable")
        .unwrap_or(0)
        != 0;
    let server_configured = internet_settings
        .get_value::<String, _>("ProxyServer")
        .is_ok_and(|server| !server.trim().is_empty());
    WindowsSystemProxyState {
        supported: true,
        enabled,
        server_configured,
    }
}

#[cfg(not(windows))]
fn windows_system_proxy_state() -> WindowsSystemProxyState {
    WindowsSystemProxyState::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("wla-network-proxy-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn settings_file_never_contains_proxy_credentials_in_plaintext() {
        let root = temp_directory();
        let service = NetworkProxyService::load(&root).unwrap();
        #[cfg(windows)]
        let updated = service.update(ProxySettingsUpdate {
            mode: ProxyMode::Custom,
            address: Some("http://127.0.0.1:8080".to_owned()),
            bypass: "localhost".to_owned(),
            username: "private-user".to_owned(),
            password: "private-password".to_owned(),
            clear_credentials: false,
        });
        #[cfg(not(windows))]
        let updated = service.update(ProxySettingsUpdate {
            mode: ProxyMode::Custom,
            address: Some("http://127.0.0.1:8080".to_owned()),
            bypass: "localhost".to_owned(),
            username: String::new(),
            password: String::new(),
            clear_credentials: false,
        });
        assert!(updated.is_ok());
        let contents = fs::read_to_string(root.join(SETTINGS_FILE)).unwrap();
        assert!(!contents.contains("private-user"));
        assert!(!contents.contains("private-password"));
        #[cfg(windows)]
        {
            let view = updated.unwrap();
            assert!(view.credentials_available);
            assert!(view.credentials_match_proxy);
            let reloaded = NetworkProxyService::load(&root).unwrap();
            let view = reloaded.view().unwrap();
            assert!(view.credentials_available);
            assert!(view.credentials_match_proxy);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saved_proxy_credentials_are_only_used_for_their_bound_endpoint() {
        let credentials =
            ProxyCredentials::new("proxy-user".into(), "private-password".into()).unwrap();
        let first_proxy = ProxySettings {
            mode: ProxyMode::Custom,
            address: Some("http://127.0.0.1:8080".to_owned()),
            bypass: String::new(),
        }
        .validated()
        .unwrap();
        let second_proxy = ProxySettings {
            mode: ProxyMode::Custom,
            address: Some("https://proxy.example:8443".to_owned()),
            bypass: String::new(),
        }
        .validated()
        .unwrap();

        assert!(
            credentials_for_settings(
                &first_proxy,
                first_proxy.address.as_deref(),
                Some(credentials.clone()),
            )
            .is_some()
        );
        assert!(
            credentials_for_settings(
                &second_proxy,
                first_proxy.address.as_deref(),
                Some(credentials),
            )
            .is_none()
        );
    }

    #[test]
    fn interrupted_proxy_settings_replace_recovers_the_previous_file() {
        let root = temp_directory();
        let path = root.join(SETTINGS_FILE);
        let backup = path.with_extension("json.bak");
        let settings = ProxySettings {
            mode: ProxyMode::Custom,
            address: Some("http://127.0.0.1:8080".to_owned()),
            bypass: "localhost".to_owned(),
        }
        .validated()
        .unwrap();
        write_settings_file(&path, &settings, &None, &None).unwrap();
        fs::rename(&path, &backup).unwrap();

        let (recovered, warning) = load_proxy_settings_file(&path);
        let recovered = recovered.unwrap();
        assert_eq!(recovered.settings, settings);
        assert!(warning.is_some());
        assert!(path.is_file());
        assert!(!backup.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
