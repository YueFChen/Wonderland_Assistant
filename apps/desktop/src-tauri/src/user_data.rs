use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use wonderland_kernel::{AppContext, KernelError, UserDataState};

use crate::commands::{ensure_main_window, ipc};

#[tauri::command]
pub fn user_data_get(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<UserDataState, KernelError> {
    ensure_main_window(&window)?;
    ipc("user_data_get", || Ok(context.user_data()?.state()))
}

/// Schedule a directory migration for the next application startup.
#[tauri::command]
pub fn user_data_migrate_custom(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    path: String,
) -> Result<UserDataState, KernelError> {
    ensure_main_window(&window)?;
    ipc("user_data_migrate_custom", || {
        context.user_data()?.schedule_migration(Some(&path))
    })
}

#[tauri::command]
pub fn user_data_migrate_default(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<UserDataState, KernelError> {
    ensure_main_window(&window)?;
    ipc("user_data_migrate_default", || {
        context.user_data()?.schedule_migration(None)
    })
}

#[tauri::command]
pub fn user_data_cancel_migration(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<UserDataState, KernelError> {
    ensure_main_window(&window)?;
    ipc("user_data_cancel_migration", || {
        context.user_data()?.cancel_migration()
    })
}

/// 使用 Tauri 的系统目录选择器；取消选择不改变设置。
#[tauri::command]
pub async fn user_data_choose_directory(
    window: WebviewWindow,
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<Option<String>, KernelError> {
    ensure_main_window(&window)?;
    let current = context.user_data()?.state().active_path;
    let start = std::path::Path::new(&current)
        .parent()
        .unwrap_or_else(|| std::path::Path::new(&current));
    let picker = app
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("选择用户数据目录")
        .set_directory(start);
    let selected = tauri::async_runtime::spawn_blocking(move || picker.blocking_pick_folder())
        .await
        .map_err(|error| KernelError::Other(format!("目录选择器异常退出：{error}")))?;
    selected
        .map(|path| {
            path.into_path()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| KernelError::Other(format!("无法读取所选目录：{error}")))
        })
        .transpose()
}

/// 设置页统一的重启入口；启动引导会在装载各插件前执行已提交的迁移。
#[tauri::command]
pub fn settings_restart(window: WebviewWindow, app: AppHandle) -> Result<(), KernelError> {
    ensure_main_window(&window)?;
    app.restart()
}
