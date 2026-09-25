//! 日志设置与日志目录命令。

use tauri::{AppHandle, Manager, State, WebviewWindow};
use wonderland_kernel::logging::{LogLevel, LogSettings, log_dir};
use wonderland_kernel::{AppContext, KernelError};
use wonderland_logging::Logging;

/// 返回当前日志设置。
#[tauri::command]
pub fn logging_get(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<LogSettings, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("logging_get", || Ok(context.settings()?.logging()))
}

/// 返回应用日志目录。
#[tauri::command]
pub fn logging_dir(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<String, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    Ok(log_dir(&context.app_data_dir)
        .to_string_lossy()
        .into_owned())
}

/// 保存并应用指定的日志级别。
#[tauri::command]
pub fn logging_set_level(
    app: AppHandle,
    window: WebviewWindow,
    level: LogLevel,
) -> Result<LogSettings, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("logging_set_level", || {
        let settings = app
            .state::<AppContext>()
            .settings()?
            .set_logging(LogSettings { level })?;

        // 即使日志初始化失败，也保存新的日志级别。
        if let Some(logging) = app.try_state::<Logging>() {
            logging.set_level(settings.level)?;
        }
        Ok(settings)
    })
}

/// 在文件管理器中打开应用日志目录。
#[tauri::command]
pub fn logging_reveal_dir(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<(), KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("logging_reveal_dir", || {
        let dir = log_dir(&context.app_data_dir);
        if !dir.is_dir() {
            return Err(KernelError::Other(format!(
                "日志目录还不存在：{}",
                dir.display()
            )));
        }
        crate::reveal::open_dir(&dir)
    })
}
