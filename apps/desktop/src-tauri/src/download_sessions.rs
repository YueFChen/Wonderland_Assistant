//! Download-only browser credentials; regular APIs never authenticate with this cookie.
use std::collections::HashMap;
use std::sync::Mutex;

struct Session {
    token: String,
    files: HashMap<String, String>,
}
#[derive(Default)]
pub(crate) struct DownloadSessions {
    sessions: Mutex<HashMap<String, Session>>,
}

impl DownloadSessions {
    pub(crate) fn prepare(&self, owner: &str, id: &str) -> Result<(String, String), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "Download sessions are unavailable.")?;
        if !sessions.contains_key(owner) {
            sessions.insert(
                owner.into(),
                Session {
                    token: crate::web_access::random_key()?,
                    files: HashMap::new(),
                },
            );
        }
        let session = sessions.get_mut(owner).unwrap();
        if !session.files.contains_key(id) {
            session
                .files
                .insert(id.into(), crate::web_access::random_key()?);
        }
        Ok((session.token.clone(), session.files[id].clone()))
    }
    pub(crate) fn authorized(&self, owner: &str, token: &str, id: &str, nonce: &str) -> bool {
        self.sessions.lock().is_ok_and(|sessions| {
            sessions
                .get(owner)
                .is_some_and(|s| s.token == token && s.files.get(id).is_some_and(|n| n == nonce))
        })
    }
    pub(crate) fn revoke(&self, owner: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(owner);
        }
    }
    pub(crate) fn forget(&self, owner: &str, id: &str) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(session) = sessions.get_mut(owner)
        {
            session.files.remove(id);
        }
    }
    pub(crate) fn prune(&self, mut live: impl FnMut(&str, &str) -> bool) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.retain(|owner, session| {
                session.files.retain(|id, _| live(owner, id));
                !session.files.is_empty()
            });
        }
    }
}

pub(crate) fn cookie_path(session: &str, owner: &str) -> String {
    format!("/downloads/{session}/{owner}/")
}
pub(crate) fn cookie(session: &str, owner: &str, token: Option<&str>, secure: bool) -> String {
    format!(
        "wonderland_download={}; Path={}; HttpOnly; SameSite=Strict{}{}",
        token.unwrap_or(""),
        cookie_path(session, owner),
        if secure { "; Secure" } else { "" },
        if token.is_none() { "; Max-Age=0" } else { "" }
    )
}
pub(crate) fn supplied_cookie(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| (name == "wonderland_download").then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copied_url_needs_the_matching_browser_cookie_and_client() {
        let sessions = DownloadSessions::default();
        let (a, nonce) = sessions.prepare("device-a", "file").unwrap();
        let (b, _) = sessions.prepare("device-b", "file").unwrap();
        assert!(sessions.authorized("device-a", &a, "file", &nonce));
        assert!(!sessions.authorized("device-a", &b, "file", &nonce));
        assert!(!sessions.authorized("device-b", &a, "file", &nonce));
        sessions.revoke("device-a");
        assert!(!sessions.authorized("device-a", &a, "file", &nonce));
    }
    #[test]
    fn cookies_partition_clients_and_use_https_only_secure() {
        assert_ne!(cookie_path("one", "a"), cookie_path("one", "b"));
        assert_ne!(cookie_path("one", "a"), cookie_path("two", "a"));
        assert!(cookie("s", "a", Some("key"), true).contains("; Secure"));
        assert!(!cookie("s", "a", Some("key"), false).contains("; Secure"));
        assert!(cookie("s", "a", None, false).contains("Max-Age=0"));
    }
    #[test]
    fn dismissing_or_pruning_one_file_preserves_other_live_credentials() {
        let sessions = DownloadSessions::default();
        let (token, one) = sessions.prepare("a", "one").unwrap();
        let (same, two) = sessions.prepare("a", "two").unwrap();
        assert_eq!(token, same);
        sessions.forget("a", "one");
        assert!(!sessions.authorized("a", &token, "one", &one));
        assert!(sessions.authorized("a", &token, "two", &two));
        sessions.prune(|owner, id| owner == "a" && id == "two");
        assert!(sessions.authorized("a", &token, "two", &two));
        sessions.prune(|_, _| false);
        let (new_token, _) = sessions.prepare("a", "two").unwrap();
        assert_ne!(token, new_token);
    }
}
