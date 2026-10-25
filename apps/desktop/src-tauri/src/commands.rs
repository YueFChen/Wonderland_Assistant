use tauri::WebviewWindow;
use wonderland_kernel::KernelError;
use wonderland_kernel::logging::warn;

use crate::MAIN_WINDOW_LABEL;

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

fn log_failure<T>(command: &'static str, result: &Result<T, KernelError>) {
    if let Err(error) = result {
        warn!(command, code = error.code(), "命令失败");
    }
}
