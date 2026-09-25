//! 主题与自定义背景库的 IPC 入口。
//!
//! 转发主题和背景库命令至设置服务。

use tauri::{State, WebviewWindow};
use wonderland_kernel::{AppContext, BackgroundAsset, KernelError, ThemeSettings, ThemeState};

#[tauri::command]
pub fn theme_get(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<ThemeState, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("theme_get", || context.settings()?.theme_state())
}

#[tauri::command]
pub fn theme_set(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    settings: ThemeSettings,
) -> Result<ThemeState, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("theme_set", || context.settings()?.set_theme(settings))
}

/// 背景库清单（不含图片数据，缩略图按需经 [`theme_background_url`] 取）。
#[tauri::command]
pub fn theme_backgrounds(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<Vec<BackgroundAsset>, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("theme_backgrounds", || context.settings()?.backgrounds())
}

#[tauri::command]
pub fn theme_background_url(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    id: String,
) -> Result<Option<String>, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("theme_background_url", || {
        context.settings()?.background_url(&id)
    })
}

/// 导入背景图并设为当前背景。`name` 为原始文件名。
#[tauri::command]
pub fn theme_add_background(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    data_url: String,
    name: String,
) -> Result<ThemeState, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("theme_add_background", || {
        context.settings()?.add_background(&data_url, &name)
    })
}

#[tauri::command]
pub fn theme_select_background(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    id: String,
) -> Result<ThemeState, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("theme_select_background", || {
        context.settings()?.select_background(&id)
    })
}

#[tauri::command]
pub fn theme_remove_background(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    id: String,
) -> Result<ThemeState, KernelError> {
    crate::commands::ensure_main_window(&window)?;
    crate::commands::ipc("theme_remove_background", || {
        context.settings()?.remove_background(&id)
    })
}
