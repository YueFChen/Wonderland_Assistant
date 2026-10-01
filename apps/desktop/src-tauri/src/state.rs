use std::error::Error;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use wonderland_account::FsAccountService;
use wonderland_config::FsSettingsService;
use wonderland_kernel::logging::{LogSettings, error, info};
use wonderland_kernel::{AccountService, AppContext, SettingsService, UserDataService};
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
    let (user_data, app_data_dir) = FsUserDataService::bootstrap(default_data_dir)?;
    let documents_dir = app_data_dir.clone();

    // Configure all Core-managed network clients before services construct their clients.
    let network_proxy = NetworkProxyService::load(&app_data_dir)?;

    let settings = FsSettingsService::load(app_data_dir.clone());
    let logging = init_logging(settings.logging(), &app_data_dir);

    info!(
        version = %app.package_info().version,
        dir = %app_data_dir.display(),
        "启动引导"
    );

    let account = load_account(app_data_dir.clone())?;
    let settings: Arc<dyn SettingsService> = Arc::new(settings);
    let user_data: Arc<dyn UserDataService> = Arc::new(user_data);
    info!("设置与账号服务已装载");

    Ok((
        AppContext {
            app_data_dir,
            documents_dir,
            account: Some(account),
            settings: Some(settings),
            user_data: Some(user_data),
        },
        logging,
        network_proxy,
    ))
}

/// 加载账号服务；失败时记录错误并返回启动错误。
fn load_account(app_data_dir: PathBuf) -> Result<Arc<dyn AccountService>, Box<dyn Error>> {
    match FsAccountService::load(app_data_dir) {
        Ok(account) => Ok(Arc::new(account)),
        Err(error) => {
            error!(code = error.code(), reason = %error, "账号服务装载失败");
            Err(error.into())
        }
    }
}

/// 安装日志系统；初始化失败时写入备用诊断，再返回 `None`。
fn init_logging(level: LogSettings, app_data_dir: &Path) -> Option<Logging> {
    let configured = level.level;
    match wonderland_logging::init(level, app_data_dir) {
        Ok(logging) => {
            info!(level = configured.as_str(), dir = %app_data_dir.display(), "日志已初始化");
            Some(logging)
        }
        Err(error) => {
            report_logging_init_failure(app_data_dir, &error);
            None
        }
    }
}

/// 日志订阅器还不可用时，同时尝试写 stderr 与应用数据目录中的备用文件。
fn report_logging_init_failure(app_data_dir: &Path, error: &dyn std::fmt::Display) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let error = error.to_string().replace(['\r', '\n'], " ");
    let diagnostic = format!(
        "timestamp_unix_seconds={timestamp} level=ERROR code=LOGGING_INIT_FAILED data_dir={} error={error}",
        app_data_dir.display()
    );

    let _ = writeln!(std::io::stderr().lock(), "日志系统初始化失败：{diagnostic}");

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(app_data_dir.join("wonderland-assistant-startup.log"))
    {
        let _ = writeln!(file, "{diagnostic}");
    }
}
