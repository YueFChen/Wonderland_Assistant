//! Native plugin links use the same platform launcher as the Core browser service.
//!
//! Wry suppresses WebView2 popup requests unless a new-window handler is installed.
//! Returning `Deny` after launching the external browser avoids creating a WebView
//! whose opener or app privileges could be inherited by the destination page.

use tauri::{Manager, Runtime, Url, WebviewWindowBuilder, webview::NewWindowResponse};
use wonderland_kernel::logging::warn;

pub(crate) fn with_external_browser<'a, R: Runtime, M: Manager<R>>(
    builder: WebviewWindowBuilder<'a, R, M>,
) -> WebviewWindowBuilder<'a, R, M> {
    builder.on_new_window(|url, _features| {
        if let Some(destination) = external_web_url(&url)
            && let Err(error) = crate::reveal::open_url(&destination)
        {
            // URLs can contain credentials or user content; never include them in logs.
            warn!(error = %error, "Cannot open the requested external browser link");
        }
        NewWindowResponse::Deny
    })
}

fn external_web_url(url: &Url) -> Option<String> {
    crate::host_services::browser_url(&serde_json::json!({ "url": url.as_str() })).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_popups_reach_the_platform_launcher() {
        for raw in ["https://example.com/tool", "http://192.168.1.5:4321/page"] {
            assert!(external_web_url(&Url::parse(raw).unwrap()).is_some());
        }
        for raw in [
            "file:///C:/private.html",
            "javascript:alert(1)",
            "data:text/html,example",
            "about:blank",
            "plugin-asset://localhost/editor/ui/main.html",
            "mailto:someone@example.com",
            "https://user:password@example.com/tool",
        ] {
            assert!(external_web_url(&Url::parse(raw).unwrap()).is_none());
        }
    }
}
