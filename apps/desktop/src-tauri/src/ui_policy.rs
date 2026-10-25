//! One policy for opaque plugin surfaces on native and remote hosts.

pub(crate) const IFRAME_SANDBOX: &str =
    "allow-scripts allow-forms allow-downloads allow-popups allow-popups-to-escape-sandbox";

/// Native assets use `/<plugin>/ui/`; remote assets include their share's asset key.
/// Two policies intersect: scripts must be both same-origin and inside this package.
/// This avoids trusting a reverse proxy's Host/scheme while denying sibling assets.
pub(crate) fn plugin_csp(plugin_id: &str, remote_asset_key: Option<&str>) -> String {
    if !valid_segment(plugin_id) || remote_asset_key.is_some_and(|key| !valid_segment(key)) {
        return "sandbox; default-src 'none'".into();
    }
    let script_prefix = remote_asset_key.map_or_else(
        || format!("/{plugin_id}/ui/"),
        |key| format!("/plugin-assets/{key}/{plugin_id}/ui/"),
    );
    let sandbox = if remote_asset_key.is_some() {
        format!("sandbox {IFRAME_SANDBOX}; ")
    } else {
        String::new()
    };
    let scripts = format!(
        "http://*:*{script_prefix} https://*:*{script_prefix} plugin-asset://*:*{script_prefix}"
    );
    // The LAN compatibility shim is Core-owned, fixed, and served at this exact path.
    let compatibility = if remote_asset_key.is_some() {
        " http://*:*/web-crypto.js https://*:*/web-crypto.js"
    } else {
        ""
    };
    format!(
        "{sandbox}default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' http: https: blob: data:; media-src 'self' http: https: blob: data:; font-src 'self' data:; connect-src http: https: ws: wss:; object-src 'none'; base-uri 'none'; form-action http: https:{}, script-src {scripts}{compatibility}",
        if remote_asset_key.is_some() {
            "; frame-ancestors 'self'"
        } else {
            ""
        }
    )
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_features_preserve_opaque_identity_and_packaged_scripts() {
        let policy = plugin_csp("sample_plugin", None);
        assert!(policy.contains("connect-src http: https: ws: wss:"));
        assert!(policy.contains("media-src 'self' http: https: blob: data:"));
        assert!(policy.contains("form-action http: https:"));
        assert!(policy.contains("script-src 'self'"));
        assert!(policy.contains("http://*:*/sample_plugin/ui/"));
        assert!(!policy.contains("allow-same-origin"));
        assert!(!policy.contains("unsafe-eval"));
        assert!(!policy.contains("script-src 'unsafe-inline'"));
    }

    #[test]
    fn remote_policy_matches_frame_flags_and_its_own_asset_prefix() {
        let policy = plugin_csp("editor", Some("share-key"));
        assert!(policy.starts_with(&format!("sandbox {IFRAME_SANDBOX};")));
        assert!(policy.contains("/plugin-assets/share-key/editor/ui/"));
        assert!(policy.contains("http://*:*/plugin-assets/share-key/editor/ui/"));
        assert!(policy.contains("https://*:*/plugin-assets/share-key/editor/ui/"));
        assert!(policy.contains("http://*:*/web-crypto.js"));
        assert!(policy.contains("https://*:*/web-crypto.js"));
        assert!(policy.contains("frame-ancestors 'self'"));
    }

    #[test]
    fn invalid_asset_identity_cannot_change_the_policy() {
        for plugin in ["", "../other", "editor; script-src *", "editor/ui"] {
            assert_eq!(plugin_csp(plugin, None), "sandbox; default-src 'none'");
        }
        assert_eq!(
            plugin_csp("editor", Some("bad/key")),
            "sandbox; default-src 'none'"
        );
    }
}
