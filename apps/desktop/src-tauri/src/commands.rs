use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use wonderland_kernel::logging::warn;
use wonderland_kernel::{Account, AccountSnapshot, AppContext, KernelError, LoginCancelReason};

use crate::MAIN_WINDOW_LABEL;
use crate::account_window::{self, ACCOUNT_CHANGED_EVENT};

/// 验证命令调用来自主窗口。
pub(crate) fn ensure_main_window(window: &WebviewWindow) -> Result<(), KernelError> {
    if window.label() == MAIN_WINDOW_LABEL {
        Ok(())
    } else {
        Err(KernelError::Other("拒绝来自非主窗口的调用".to_owned()))
    }
}

/// 记录 IPC 命令错误及其稳定错误代码。
///
/// 记录命令名称与稳定错误代码，不记录可能包含本地路径的错误详情。
pub(crate) fn ipc<T>(
    command: &'static str,
    body: impl FnOnce() -> Result<T, KernelError>,
) -> Result<T, KernelError> {
    let result = body();
    log_failure(command, &result);
    result
}

/// async 命令的同一出口。
pub(crate) async fn ipc_async<T, F>(command: &'static str, body: F) -> Result<T, KernelError>
where
    F: std::future::Future<Output = Result<T, KernelError>>,
{
    let result = body.await;
    log_failure(command, &result);
    result
}

fn log_failure<T>(command: &'static str, result: &Result<T, KernelError>) {
    if let Err(error) = result {
        warn!(command, code = error.code(), "命令失败");
    }
}

#[tauri::command]
pub fn account_snapshot(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<AccountSnapshot, KernelError> {
    ensure_main_window(&window)?;
    crate::commands::ipc("account_snapshot", || Ok(context.account()?.snapshot()))
}

/// 打开登录窗口。
#[tauri::command]
pub async fn account_login_start(app: AppHandle, window: WebviewWindow) -> Result<(), KernelError> {
    ensure_main_window(&window)?;
    crate::commands::ipc_async("account_login_start", async {
        let (account, app_data_dir) = {
            let context = app.state::<AppContext>();
            (context.account()?.clone(), context.app_data_dir.clone())
        };

        account.begin_login()?;
        if let Err(error) = account_window::open_login_window(&app, account.clone(), &app_data_dir)
        {
            account.cancel_login(LoginCancelReason::Failed);
            return Err(error);
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn account_sync(
    app: AppHandle,
    window: WebviewWindow,
    context: State<'_, AppContext>,
    account_key: String,
) -> Result<Account, KernelError> {
    ensure_main_window(&window)?;
    crate::commands::ipc_async("account_sync", async {
        let account = context.account()?.clone();
        let updated = account.sync_account_info(&account_key).await?;
        let _ = app.emit(ACCOUNT_CHANGED_EVENT, ());
        Ok(updated)
    })
    .await
}

#[tauri::command]
pub fn account_login_cancel(
    app: AppHandle,
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<(), KernelError> {
    ensure_main_window(&window)?;
    crate::commands::ipc("account_login_cancel", || {
        account_window::request_cancel(&app, context.account()?);
        Ok(())
    })
}

#[tauri::command]
pub fn account_switch(
    app: AppHandle,
    window: WebviewWindow,
    context: State<'_, AppContext>,
    account_key: String,
) -> Result<(), KernelError> {
    ensure_main_window(&window)?;
    crate::commands::ipc("account_switch", || {
        context.account()?.switch_account(&account_key)?;
        let _ = app.emit(ACCOUNT_CHANGED_EVENT, ());
        Ok(())
    })
}

#[tauri::command]
pub fn account_remove(
    app: AppHandle,
    window: WebviewWindow,
    context: State<'_, AppContext>,
    account_key: String,
) -> Result<(), KernelError> {
    ensure_main_window(&window)?;
    crate::commands::ipc("account_remove", || {
        context.account()?.remove_account(&account_key)?;
        let _ = app.emit(ACCOUNT_CHANGED_EVENT, ());
        Ok(())
    })
}
