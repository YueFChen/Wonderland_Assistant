//! Plugin-owned browser resources. No account, provider or login policy belongs here.
use crate::{
    host_environment::HostEnvironment,
    host_services::{HostInvocation, browser_url},
    plugin_runtime::plugin_error,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use wonderland_plugin_protocol::PluginError;

pub(crate) struct BrowserOptions {
    pub url: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub reset: bool,
}
struct Entry {
    label: String,
    origin: HostInvocation,
}
pub(crate) struct BrowserSessions {
    environment: Arc<dyn HostEnvironment>,
    profiles: PathBuf,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    runtime: tokio::runtime::Handle,
}
impl BrowserSessions {
    pub(crate) fn new(
        environment: Arc<dyn HostEnvironment>,
        data: &Path,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            environment,
            profiles: data.join(".browser-profiles"),
            entries: Arc::default(),
            runtime,
        }
    }
    pub(crate) fn call(
        &self,
        method: &str,
        params: &Value,
        invocation: &HostInvocation,
    ) -> Result<Value, PluginError> {
        // A visiting browser cannot open or inspect windows on the host desktop.
        if invocation.remote.is_some() {
            return Err(plugin_error(
                "INTERACTION_UNAVAILABLE",
                "Browser sessions require a local interactive host.",
            ));
        }
        invocation.check()?;
        if method == "core.browser.session.open" {
            return self.open(params, invocation);
        }
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| plugin_error("INVALID_INPUT", "Missing browser session ID."))?;
        let label = {
            let entries = self
                .entries
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Browser sessions are unavailable."))?;
            let entry = entries.get(id).ok_or_else(|| {
                plugin_error(
                    "BROWSER_SESSION_CLOSED",
                    "The browser session is closed or belongs to another plugin.",
                )
            })?;
            entry.origin.check()?;
            entry.label.clone()
        };
        match method {
            "core.browser.session.cookies" => self.environment.browser_cookies(&label),
            "core.browser.session.visibility" => {
                let visible = params
                    .get("visible")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| plugin_error("INVALID_INPUT", "Missing visibility state."))?;
                self.environment.browser_visibility(&label, visible)?;
                Ok(json!({"ok":true}))
            }
            "core.browser.session.navigate" => {
                self.environment
                    .browser_navigate(&label, &browser_url(params)?)?;
                Ok(json!({"ok":true}))
            }
            "core.browser.session.close" => {
                self.environment.browser_close(&label)?;
                self.entries.lock().unwrap().remove(id);
                Ok(json!({"ok":true}))
            }
            _ => Err(plugin_error(
                "METHOD_NOT_FOUND",
                "Unknown browser session operation.",
            )),
        }
    }
    fn open(&self, params: &Value, invocation: &HostInvocation) -> Result<Value, PluginError> {
        let url = browser_url(params)?;
        let profile = params
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or("default");
        if profile.is_empty()
            || profile.len() > 64
            || !profile
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Invalid browser profile name.",
            ));
        }
        let dimension = |name: &str, default: u32| -> Result<u32, PluginError> {
            match params.get(name) {
                None => Ok(default),
                Some(value) => value
                    .as_u64()
                    .filter(|v| *v > 0 && *v <= u32::MAX as u64)
                    .map(|v| v as u32)
                    .ok_or_else(|| {
                        plugin_error("INVALID_INPUT", "Invalid browser window dimensions.")
                    }),
            }
        };
        let options = BrowserOptions {
            url,
            title: params
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Browser")
                .to_owned(),
            width: dimension("width", 1000)?,
            height: dimension("height", 760)?,
            reset: params
                .get("reset")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        let id = crate::web_access::random_key()
            .map_err(|_| plugin_error("INTERNAL", "Cannot create a browser session."))?;
        let label = format!("plugin-browser-{id}");
        self.environment
            .browser_open(&label, &self.profiles.join(profile), &options)?;
        if let Err(error) = invocation.check() {
            let _ = self.environment.browser_close(&label);
            return Err(error);
        }
        self.entries.lock().unwrap().insert(
            id.clone(),
            Entry {
                label: label.clone(),
                origin: invocation.clone(),
            },
        );
        // Cancellation and process shutdown close resources even if the plugin stops polling.
        let entries = Arc::downgrade(&self.entries);
        let environment = self.environment.clone();
        let origin = invocation.clone();
        let token = id.clone();
        self.runtime.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some(entries) = entries.upgrade() else {
                    return;
                };
                let present = entries
                    .lock()
                    .map(|v| v.contains_key(&token))
                    .unwrap_or(false);
                if !present {
                    return;
                }
                if origin.check().is_err() {
                    let _ = environment.browser_close(&label);
                    if let Ok(mut entries) = entries.lock() {
                        entries.remove(&token);
                    }
                    return;
                }
            }
        });
        Ok(json!({"sessionId":id}))
    }
}
impl Drop for BrowserSessions {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.entries.lock() {
            for (_, entry) in entries.drain() {
                let _ = self.environment.browser_close(&entry.label);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Default)]
    struct Mock {
        opens: AtomicUsize,
        closes: AtomicUsize,
    }
    impl HostEnvironment for Mock {
        fn emit(&self, _: &str, _: Value) {}
        fn pick_file(&self, _: &[String]) -> Result<Option<PathBuf>, PluginError> {
            Ok(None)
        }
        fn open_url(&self, _: &str) -> Result<(), PluginError> {
            Ok(())
        }
        fn reveal_dir(&self, _: &Path) -> Result<(), PluginError> {
            Ok(())
        }
        fn browser_open(&self, _: &str, _: &Path, _: &BrowserOptions) -> Result<(), PluginError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn browser_cookies(&self, _: &str) -> Result<Value, PluginError> {
            Ok(json!([]))
        }
        fn browser_close(&self, _: &str) -> Result<(), PluginError> {
            self.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    #[tokio::test]
    async fn session_is_owned_and_cancelled_without_plugin_polling() {
        let environment = Arc::new(Mock::default());
        let runtime = tokio::runtime::Handle::current();
        let owner = BrowserSessions::new(
            environment.clone(),
            Path::new("unused-test-profile"),
            runtime.clone(),
        );
        let other = BrowserSessions::new(environment.clone(), Path::new("other"), runtime);
        let invocation = HostInvocation::for_call(None, None, None).unwrap();
        let params = owner
            .call(
                "core.browser.session.open",
                &json!({"url":"https://example.com"}),
                &invocation,
            )
            .unwrap();
        assert_eq!(
            other
                .call("core.browser.session.cookies", &params, &invocation)
                .unwrap_err()
                .code,
            "BROWSER_SESSION_CLOSED"
        );
        assert_eq!(
            owner
                .call(
                    "core.browser.session.open",
                    &json!({"url":"https://example.com","profile":"../other"}),
                    &invocation
                )
                .unwrap_err()
                .code,
            "INVALID_INPUT"
        );
        invocation.cancel();
        tokio::time::sleep(Duration::from_millis(220)).await;
        assert_eq!(environment.closes.load(Ordering::SeqCst), 1);
    }
}
