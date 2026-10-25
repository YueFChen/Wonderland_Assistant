use std::error::Error;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use wonderland_config::FsSettingsService;
use wonderland_kernel::logging::{LogSettings, error, info};
use wonderland_kernel::{AppContext, SettingsService, UserDataService};
use wonderland_logging::Logging;
use wonderland_user_data::FsUserDataService;

use crate::network::NetworkProxyService;

/// 加载持久化服务并组装应用上下文。
pub fn bootstrap(
    app: &AppHandle,
) -> Result<(AppContext, Option<Logging>, NetworkProxyService), Box<dyn Error>> {
    bootstrap_with_data_dir(app, None)
}

/// Loads Core services from an alternate data root for CLI diagnostics and isolated test runs.
pub fn bootstrap_with_data_dir(
    app: &AppHandle,
    data_dir: Option<PathBuf>,
) -> Result<(AppContext, Option<Logging>, NetworkProxyService), Box<dyn Error>> {
    let default_data_dir = match data_dir {
        Some(data_dir) => data_dir,
        None => app.path().app_data_dir()?,
    };
    info!(version = %app.package_info().version, "桌面宿主启动");
    bootstrap_services(default_data_dir, true)
}

/// Load the same persistent services without creating a Tauri application or windows.
pub fn bootstrap_headless(
    default_data_dir: PathBuf,
) -> Result<(AppContext, Option<Logging>, NetworkProxyService), Box<dyn Error>> {
    bootstrap_services(default_data_dir, false)
}

fn bootstrap_services(
    default_data_dir: PathBuf,
    console_logging: bool,
) -> Result<(AppContext, Option<Logging>, NetworkProxyService), Box<dyn Error>> {
    let (user_data, app_data_dir) = FsUserDataService::bootstrap(default_data_dir)?;
    let documents_dir = app_data_dir.clone();

    // Configure all Core-managed network clients before services construct their clients.
    let network_proxy = NetworkProxyService::load(&app_data_dir)?;

    let settings = load_settings(&app_data_dir);
    let logging = init_logging(
        settings
            .as_ref()
            .map(|settings| settings.logging())
            .unwrap_or_default(),
        &app_data_dir,
        console_logging,
    );

    info!(
        dir = %app_data_dir.display(),
        "启动引导"
    );

    let user_data: Arc<dyn UserDataService> = Arc::new(user_data);
    info!("设置服务已装载");

    Ok((
        AppContext {
            app_data_dir,
            documents_dir,
            settings,
            user_data: Some(user_data),
        },
        logging,
        network_proxy,
    ))
}

fn load_settings(app_data_dir: &Path) -> Option<Arc<dyn SettingsService>> {
    match FsSettingsService::load(app_data_dir.to_owned()) {
        Ok(settings) => Some(Arc::new(settings)),
        Err(error) => {
            error!(code = error.code(), reason = %error, "设置服务不可用，保留现有设置和背景资源");
            None
        }
    }
}

/// 安装日志系统；初始化失败时写入备用诊断，再返回 `None`。
fn init_logging(level: LogSettings, app_data_dir: &Path, console: bool) -> Option<Logging> {
    let configured = level.level;
    let result = if console {
        wonderland_logging::init(level, app_data_dir)
    } else {
        wonderland_logging::init_file_only(level, app_data_dir)
    };
    match result {
        Ok(logging) => {
            info!(level = configured.as_str(), dir = %app_data_dir.display(), "日志已初始化");
            Some(logging)
        }
        Err(error) => {
            report_logging_init_failure(app_data_dir, &error, console);
            None
        }
    }
}

/// 日志订阅器还不可用时，同时尝试写 stderr 与应用数据目录中的备用文件。
fn report_logging_init_failure(app_data_dir: &Path, error: &dyn std::fmt::Display, console: bool) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let error = error.to_string().replace(['\r', '\n'], " ");
    let diagnostic = format!(
        "timestamp_unix_seconds={timestamp} level=ERROR code=LOGGING_INIT_FAILED data_dir={} error={error}",
        app_data_dir.display()
    );

    if console {
        let _ = writeln!(std::io::stderr().lock(), "日志系统初始化失败：{diagnostic}");
    }

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(app_data_dir.join("wonderland-assistant-startup.log"))
    {
        let _ = writeln!(file, "{diagnostic}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_settings_disable_writes_and_preserve_backgrounds() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "wla-settings-bootstrap-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("theme")).unwrap();
        let bytes = b"{\"backgrounds\": broken";
        std::fs::write(root.join("settings.json"), bytes).unwrap();
        std::fs::write(root.join("theme/bg-7.png"), b"retained image").unwrap();
        assert!(load_settings(&root).is_none());
        assert_eq!(std::fs::read(root.join("settings.json")).unwrap(), bytes);
        assert_eq!(
            std::fs::read(root.join("theme/bg-7.png")).unwrap(),
            b"retained image"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
