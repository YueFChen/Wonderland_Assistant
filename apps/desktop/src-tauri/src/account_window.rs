//! 登录 WebView 窗口与 cookie 采集流程。

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use wonderland_account::{evaluate, select_mihoyo_cookies};
use wonderland_kernel::logging::{info, warn};
use wonderland_kernel::{
    AccountService, KernelError, LoginCancelReason, LoginOutcome, StoredCookie,
};
use wonderland_mihoyo::API_MICREATOR;

/// 账号状态变化事件；壳层据此刷新界面。
pub const ACCOUNT_CHANGED_EVENT: &str = "account-changed";

/// 登录窗口 label；同一时刻只允许一个登录流程。
const LOGIN_WINDOW_LABEL: &str = "account-login";

/// 创作者中心登录页 URL。
const LOGIN_URL: &str = "https://act.mihoyo.com/miliastra_wonderland/developer";

/// 凭据齐全后访问以补齐所需 cookie 的业务域。
const COOKIE_SETTLE_URL: &str = API_MICREATOR;

/// cookie 轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(600);

/// 整个登录流程的等待上限。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// 补访问业务域后的等待时长。
const SETTLE_WAIT: Duration = Duration::from_secs(2);

/// 取消请求标记：驱动线程优先于"窗口已关闭"判定，避免把取消误报为用户关闭。
static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 清除登录窗口会话，打开登录页并启动驱动线程。
pub fn open_login_window(
    app: &AppHandle,
    account: Arc<dyn AccountService>,
    app_data_dir: &Path,
) -> Result<(), KernelError> {
    CANCEL_REQUESTED.store(false, Ordering::SeqCst);

    let url: Url = LOGIN_URL
        .parse()
        .map_err(|error| KernelError::LoginWindow(format!("登录地址无效：{error}")))?;

    let window = match app.get_webview_window(LOGIN_WINDOW_LABEL) {
        Some(existing) => existing,
        None => {
            // Keep login cookies in a profile separate from the main application window.
            let profile_dir = app_data_dir.join("webview").join(LOGIN_WINDOW_LABEL);
            fs::create_dir_all(&profile_dir)
                .map_err(|error| KernelError::LoginWindow(error.to_string()))?;

            WebviewWindowBuilder::new(app, LOGIN_WINDOW_LABEL, WebviewUrl::External(url.clone()))
                .title("登录米哈游通行证")
                .inner_size(1000.0, 760.0)
                .data_directory(profile_dir)
                .build()
                .map_err(|error| KernelError::LoginWindow(error.to_string()))?
        }
    };

    let _ = window.hide();
    window
        .clear_all_browsing_data()
        .map_err(|error| KernelError::LoginWindow(error.to_string()))?;
    window
        .navigate(url)
        .map_err(|error| KernelError::LoginWindow(error.to_string()))?;
    let _ = window.show();

    info!("登录窗口已打开");
    spawn_driver(app.clone(), account);
    Ok(())
}

/// 请求结束进行中的登录流程。
pub fn request_cancel(app: &AppHandle, account: &Arc<dyn AccountService>) {
    CANCEL_REQUESTED.store(true, Ordering::SeqCst);

    if let Some(window) = app.get_webview_window(LOGIN_WINDOW_LABEL) {
        let _ = window.close();
        return;
    }

    info!("登录已取消");
    account.cancel_login(LoginCancelReason::Cancelled);
    let _ = app.emit(ACCOUNT_CHANGED_EVENT, ());
}

/// 驱动线程：轮询 cookie → 提交账号服务 → 收尾关闭窗口。
fn spawn_driver(app: AppHandle, account: Arc<dyn AccountService>) {
    thread::spawn(move || {
        // Read WebView cookies off the UI thread.
        match drive(&app, account.as_ref()) {
            Some(reason) => {
                info!(reason = ?reason, "登录流程未完成");
                account.cancel_login(reason);
            }
            None => info!("登录完成"),
        }

        if let Some(window) = app.get_webview_window(LOGIN_WINDOW_LABEL) {
            let _ = window.close();
        }
        let _ = app.emit(ACCOUNT_CHANGED_EVENT, ());
    });
}

/// 返回 `None` 表示登录完成；返回原因表示流程未走到终态。
fn drive(app: &AppHandle, account: &dyn AccountService) -> Option<LoginCancelReason> {
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    let mut settle_until: Option<Instant> = None;

    loop {
        if CANCEL_REQUESTED.load(Ordering::SeqCst) {
            return Some(LoginCancelReason::Cancelled);
        }

        let window = match app.get_webview_window(LOGIN_WINDOW_LABEL) {
            Some(window) => window,
            None => return Some(LoginCancelReason::WindowClosed),
        };

        if Instant::now() >= deadline {
            return Some(LoginCancelReason::Timeout);
        }

        let cookies = match read_cookies(&window) {
            Ok(cookies) => select_mihoyo_cookies(&cookies),
            Err(_) => {
                thread::sleep(POLL_INTERVAL);
                continue;
            }
        };

        match settle_until {
            None => {
                if evaluate(&cookies).is_some() {
                    // Log cookie names only; never include cookie values.
                    info!(
                        count = cookies.len(),
                        names = ?cookies
                            .iter()
                            .map(|cookie| cookie.name.as_str())
                            .collect::<Vec<_>>(),
                        "凭据已齐全"
                    );
                    let _ = window.hide();
                    if let Ok(url) = COOKIE_SETTLE_URL.parse() {
                        let _ = window.navigate(url);
                    }
                    settle_until = Some(Instant::now() + SETTLE_WAIT);
                }
            }
            Some(until) if Instant::now() >= until => match account.submit_login(cookies) {
                Ok(LoginOutcome::Completed { account_key }) => {
                    let _ = tauri::async_runtime::block_on(account.sync_account_info(&account_key));
                    let _ = app.emit(ACCOUNT_CHANGED_EVENT, ());
                    return None;
                }
                Ok(LoginOutcome::Pending) => settle_until = None,
                Err(error) => {
                    warn!(code = error.code(), reason = %error, "登录提交失败");
                    return Some(LoginCancelReason::Failed);
                }
            },
            Some(_) => {}
        }

        thread::sleep(POLL_INTERVAL);
    }
}

fn read_cookies(window: &WebviewWindow) -> Result<Vec<StoredCookie>, KernelError> {
    let cookies = window
        .cookies()
        .map_err(|error| KernelError::LoginWindow(error.to_string()))?;

    Ok(cookies
        .into_iter()
        .map(|cookie| StoredCookie {
            name: cookie.name().to_owned(),
            value: cookie.value().to_owned(),
            domain: cookie.domain().unwrap_or_default().to_owned(),
            path: cookie.path().unwrap_or("/").to_owned(),
        })
        .collect())
}
