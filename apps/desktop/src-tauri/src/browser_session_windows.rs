//! Popup windows are part of their browser session, sharing its WebView environment.
//! They receive no Core IPC permissions and are destroyed with the owning window.

use std::{
    collections::HashSet,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tauri::{
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry,
    webview::NewWindowResponse,
};

#[derive(Default)]
struct PopupState {
    closed: bool,
    labels: HashSet<String>,
}

struct PopupFamily {
    root: String,
    title: String,
    sequence: AtomicU64,
    state: Mutex<PopupState>,
}

impl PopupFamily {
    fn new(root: &str, title: &str) -> Self {
        Self {
            root: root.into(),
            title: title.into(),
            sequence: AtomicU64::new(0),
            state: Mutex::default(),
        }
    }

    fn next_label(&self) -> String {
        format!(
            "{}-popup-{}",
            self.root,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn track(&self, label: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return false;
        }
        state.labels.insert(label.into());
        true
    }

    fn forget(&self, label: &str) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .labels
            .remove(label);
    }

    fn close(&self) -> Vec<String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        state.labels.drain().collect()
    }
}

fn session_destination(url: &Url) -> bool {
    // OAuth pages commonly pre-open about:blank and navigate their returned WindowProxy later.
    url.as_str() == "about:blank"
        || crate::host_services::browser_url(&serde_json::json!({"url": url.as_str()})).is_ok()
}

fn with_popups<'a>(
    builder: WebviewWindowBuilder<'a, Wry, AppHandle>,
    app: AppHandle,
    family: Arc<PopupFamily>,
) -> WebviewWindowBuilder<'a, Wry, AppHandle> {
    builder
        .on_navigation(session_destination)
        .on_new_window(move |url, features| {
            if !session_destination(&url) {
                return NewWindowResponse::Deny;
            }
            let label = family.next_label();
            let child = WebviewWindowBuilder::new(
                &app,
                &label,
                WebviewUrl::External("about:blank".parse().unwrap()),
            )
            .title(&family.title)
            .inner_size(900.0, 700.0)
            // Preserves window.opener and the exact parent WebView2 environment/cookie store.
            .window_features(features);
            let child = match with_popups(child, app.clone(), family.clone()).build() {
                Ok(child) => child,
                Err(_) => {
                    wonderland_kernel::logging::warn!("Cannot create a browser-session popup");
                    return NewWindowResponse::Deny;
                }
            };
            if !family.track(&label) {
                let _ = child.destroy();
                return NewWindowResponse::Deny;
            }
            let family = family.clone();
            child.on_window_event(move |event| {
                if matches!(event, WindowEvent::Destroyed) {
                    family.forget(&label);
                }
            });
            NewWindowResponse::Create { window: child }
        })
}

pub(crate) fn create(
    app: &AppHandle,
    label: &str,
    profile: &Path,
    options: &crate::browser_sessions::BrowserOptions,
) -> Result<WebviewWindow, tauri::Error> {
    let family = Arc::new(PopupFamily::new(label, &options.title));
    let builder = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::External("about:blank".parse().unwrap()),
    )
    .title(&options.title)
    .inner_size(f64::from(options.width), f64::from(options.height))
    .visible(false)
    .data_directory(profile.to_path_buf());
    let window = with_popups(builder, app.clone(), family.clone()).build()?;
    let app = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            for label in family.close() {
                if let Some(child) = app.get_webview_window(&label) {
                    let _ = child.destroy();
                }
            }
        }
    });
    Ok(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_session_popups_allow_web_destinations_and_blank_openers() {
        for url in [
            "about:blank",
            "https://example.com/oauth",
            "http://127.0.0.1:8080/callback",
        ] {
            assert!(session_destination(&Url::parse(url).unwrap()));
        }
        for url in [
            "file:///C:/private.html",
            "javascript:alert(1)",
            "data:text/html,test",
            "tauri://localhost",
            "plugin-asset://localhost/a/ui/",
            "https://user:password@example.com",
        ] {
            assert!(!session_destination(&Url::parse(url).unwrap()));
        }
    }

    #[test]
    fn session_cleanup_drains_all_descendants_and_prevents_late_windows() {
        let family = PopupFamily::new("session", "Browser");
        let first = family.next_label();
        let second = family.next_label();
        assert_ne!(first, second);
        assert!(family.track(&first));
        assert!(family.track(&second));
        family.forget(&first); // Closing just one popup leaves the session usable.
        let nested = family.next_label();
        assert!(family.track(&nested));
        let mut remaining = family.close();
        remaining.sort();
        let mut expected = vec![second, nested];
        expected.sort();
        assert_eq!(remaining, expected);
        assert!(!family.track(&family.next_label()));
        assert!(family.close().is_empty());
    }
}
