//! Native interaction boundary. Host services can run without a desktop application.
use std::path::{Path, PathBuf};

use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tauri_plugin_dialog::DialogExt;
use wonderland_plugin_protocol::PluginError;

use crate::plugin_runtime::plugin_error;

pub(crate) trait HostEnvironment: Send + Sync {
    fn emit(&self, topic: &str, payload: Value);
    fn pick_file(&self, extensions: &[String]) -> Result<Option<PathBuf>, PluginError>;
    fn pick_paths(
        &self,
        _directory: bool,
        _extensions: &[String],
    ) -> Result<Option<Vec<PathBuf>>, PluginError> {
        Err(unavailable())
    }
    fn open_url(&self, url: &str) -> Result<(), PluginError>;
    fn reveal_dir(&self, path: &Path) -> Result<(), PluginError>;
    fn browser_open(
        &self,
        _label: &str,
        _profile: &Path,
        _options: &crate::browser_sessions::BrowserOptions,
    ) -> Result<(), PluginError> {
        Err(unavailable())
    }
    fn browser_cookies(&self, _label: &str) -> Result<Value, PluginError> {
        Err(unavailable())
    }
    fn browser_navigate(&self, _label: &str, _url: &str) -> Result<(), PluginError> {
        Err(unavailable())
    }
    fn browser_close(&self, _label: &str) -> Result<(), PluginError> {
        Err(unavailable())
    }
    fn browser_visibility(&self, _label: &str, _visible: bool) -> Result<(), PluginError> {
        Err(unavailable())
    }
}

pub(crate) struct DesktopEnvironment(AppHandle);

impl DesktopEnvironment {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self(app)
    }
}

impl HostEnvironment for DesktopEnvironment {
    fn pick_paths(
        &self,
        directory: bool,
        extensions: &[String],
    ) -> Result<Option<Vec<PathBuf>>, PluginError> {
        let mut picker = self.0.dialog().file();
        if !extensions.is_empty() {
            let extensions = extensions.iter().map(String::as_str).collect::<Vec<_>>();
            picker = picker.add_filter("Plugin files", &extensions);
        }
        let files = if directory {
            picker.blocking_pick_folder().map(|path| vec![path])
        } else {
            picker.blocking_pick_files()
        };
        files
            .map(|files| {
                files
                    .into_iter()
                    .map(|file| {
                        file.into_path()
                            .map_err(|_| plugin_error("INVALID_INPUT", "Selected path is invalid."))
                    })
                    .collect()
            })
            .transpose()
    }
    fn browser_visibility(&self, label: &str, visible: bool) -> Result<(), PluginError> {
        use tauri::Manager;
        let window = self.0.get_webview_window(label).ok_or_else(|| {
            plugin_error("BROWSER_SESSION_CLOSED", "The browser session is closed.")
        })?;
        (if visible {
            window.show()
        } else {
            window.hide()
        })
        .map_err(|_| plugin_error("BROWSER_ERROR", "Cannot change browser visibility."))
    }
    fn browser_open(
        &self,
        label: &str,
        profile: &Path,
        options: &crate::browser_sessions::BrowserOptions,
    ) -> Result<(), PluginError> {
        std::fs::create_dir_all(profile)
            .map_err(|_| plugin_error("BROWSER_ERROR", "Cannot prepare the browser profile."))?;
        let window = crate::browser_session_windows::create(&self.0, label, profile, options)
            .map_err(|_| plugin_error("BROWSER_ERROR", "Cannot create the browser session."))?;
        let result = (|| {
            if options.reset {
                window.clear_all_browsing_data().map_err(|_| {
                    plugin_error("BROWSER_ERROR", "Cannot reset the browser session.")
                })?;
            }
            window
                .navigate(
                    options
                        .url
                        .parse()
                        .map_err(|_| plugin_error("INVALID_INPUT", "Invalid browser URL."))?,
                )
                .map_err(|_| {
                    plugin_error("BROWSER_ERROR", "Cannot navigate the browser session.")
                })?;
            window
                .show()
                .map_err(|_| plugin_error("BROWSER_ERROR", "Cannot show the browser session."))
        })();
        if result.is_err() {
            let _ = window.destroy();
        }
        result
    }
    fn browser_cookies(&self, label: &str) -> Result<Value, PluginError> {
        use tauri::Manager;
        let window = self.0.get_webview_window(label).ok_or_else(|| {
            plugin_error("BROWSER_SESSION_CLOSED", "The browser session is closed.")
        })?;
        let cookies = window
            .cookies()
            .map_err(|_| plugin_error("BROWSER_ERROR", "Cannot read this browser session."))?;
        Ok(serde_json::json!(cookies.into_iter().map(|cookie| serde_json::json!({
            "name":cookie.name(),"value":cookie.value(),"domain":cookie.domain().unwrap_or_default(),"path":cookie.path().unwrap_or("/")
        })).collect::<Vec<_>>()))
    }
    fn browser_navigate(&self, label: &str, url: &str) -> Result<(), PluginError> {
        use tauri::Manager;
        let window = self.0.get_webview_window(label).ok_or_else(|| {
            plugin_error("BROWSER_SESSION_CLOSED", "The browser session is closed.")
        })?;
        window
            .navigate(
                url.parse()
                    .map_err(|_| plugin_error("INVALID_INPUT", "Invalid browser URL."))?,
            )
            .map_err(|_| plugin_error("BROWSER_ERROR", "Cannot navigate this browser session."))
    }
    fn browser_close(&self, label: &str) -> Result<(), PluginError> {
        use tauri::Manager;
        if let Some(window) = self.0.get_webview_window(label) {
            window
                .destroy()
                .map_err(|_| plugin_error("BROWSER_ERROR", "Cannot close this browser session."))?;
        }
        Ok(())
    }
    fn emit(&self, topic: &str, payload: Value) {
        let _ = self.0.emit(topic, payload);
    }

    fn pick_file(&self, extensions: &[String]) -> Result<Option<PathBuf>, PluginError> {
        let mut picker = self.0.dialog().file();
        if !extensions.is_empty() {
            let extensions = extensions.iter().map(String::as_str).collect::<Vec<_>>();
            picker = picker.add_filter("Plugin files", &extensions);
        }
        picker
            .blocking_pick_file()
            .map(|file| {
                file.into_path()
                    .map_err(|_| plugin_error("INVALID_INPUT", "Selected file path is invalid."))
            })
            .transpose()
    }

    fn open_url(&self, url: &str) -> Result<(), PluginError> {
        crate::reveal::open_url(url)
            .map_err(|_| plugin_error("INTERNAL", "Cannot open browser destination."))
    }

    fn reveal_dir(&self, path: &Path) -> Result<(), PluginError> {
        crate::reveal::open_dir(path)
            .map_err(|_| plugin_error("INTERNAL", "Cannot open plugin directory."))
    }
}

#[derive(Default)]
pub(crate) struct HeadlessEnvironment;

fn unavailable() -> PluginError {
    plugin_error(
        "INTERACTION_UNAVAILABLE",
        "This host has no interactive desktop.",
    )
}

impl HostEnvironment for HeadlessEnvironment {
    fn emit(&self, _topic: &str, _payload: Value) {}
    fn pick_file(&self, _extensions: &[String]) -> Result<Option<PathBuf>, PluginError> {
        Err(unavailable())
    }
    fn open_url(&self, _url: &str) -> Result<(), PluginError> {
        Err(unavailable())
    }
    fn reveal_dir(&self, _path: &Path) -> Result<(), PluginError> {
        Err(unavailable())
    }
}
