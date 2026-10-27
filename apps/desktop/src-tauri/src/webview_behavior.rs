//! Desktop surfaces use application interactions, including inside plugin frames.

use tauri::{Runtime, WebviewWindow};

pub(crate) fn configure<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    #[cfg(windows)]
    {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2Settings3, ICoreWebView2Settings5, ICoreWebView2Settings6,
        };
        use windows::core::Interface;

        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        window.with_webview(move |webview| {
            // Native settings cover opaque/cross-origin frames and future navigations.
            // Browser accelerators are independent of page shortcuts and normal editing keys.
            let result = (|| -> windows::core::Result<()> {
                unsafe {
                    let settings = webview.controller().CoreWebView2()?.Settings()?;
                    settings.SetAreDefaultContextMenusEnabled(false)?;
                    settings.SetAreDevToolsEnabled(false)?;
                    settings.SetIsStatusBarEnabled(false)?;
                    settings.SetIsZoomControlEnabled(false)?;
                    settings
                        .cast::<ICoreWebView2Settings3>()?
                        .SetAreBrowserAcceleratorKeysEnabled(false)?;
                    settings
                        .cast::<ICoreWebView2Settings5>()?
                        .SetIsPinchZoomEnabled(false)?;
                    settings
                        .cast::<ICoreWebView2Settings6>()?
                        .SetIsSwipeNavigationEnabled(false)?;
                }
                Ok(())
            })();
            let _ = tx.send(result.is_ok());
        })?;
        if rx.recv() != Ok(true) {
            return Err(
                std::io::Error::other("Cannot configure application WebView behavior").into(),
            );
        }
    }
    #[cfg(not(windows))]
    let _ = window;
    Ok(())
}
