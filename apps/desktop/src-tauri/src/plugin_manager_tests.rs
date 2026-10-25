use super::*;

#[test]
fn uninstall_clears_only_the_selected_plugin_preferences() {
    let mut preferences = Preferences::default();
    preferences.enabled_plugins.insert("editor".into(), true);
    preferences.enabled_plugins.insert("other".into(), true);
    preferences.publisher_identities = json!({"editor":"a".repeat(64)});
    preferences
        .active_versions
        .insert("editor".into(), "1.0.0".into());
    clear_plugin_preferences(&mut preferences, "editor");
    assert!(!preferences.active_versions.contains_key("editor"));
    assert!(preferences.enabled_plugins.contains_key("other"));
    assert_eq!(
        publisher_key(&preferences, "editor").unwrap(),
        Some("a".repeat(64))
    );
}

#[test]
fn damaged_publisher_records_are_preserved_without_disabling_old_plugins() {
    for original in [json!({"editor":42}), json!(["damaged"])] {
        let preferences: Preferences=serde_json::from_value(json!({"schemaVersion":4,"enabledPlugins":{"editor":true},"publisherIdentities":original})).unwrap();
        assert!(preferences.enabled_plugins["editor"]);
        assert_eq!(
            publisher_key(&preferences, "editor").unwrap_err().code,
            "PUBLISHER_IDENTITY_UNAVAILABLE"
        );
        assert_eq!(
            serde_json::to_value(&preferences).unwrap()["publisherIdentities"],
            original
        );
    }
    assert_eq!(
        publisher_key(&Preferences::default(), "editor").unwrap(),
        None
    );
}

#[test]
fn source_switch_requires_review_of_existing_data_and_secrets() {
    let old = local_source("C:/plugins/editor.wplug");
    let new = PluginInstallSource {
        kind: "catalog".into(),
        origin: "https://github.com/example/editor".into(),
        author: Some("Example".into()),
    };
    assert!(install_source_changed(None, &new));
    assert!(install_source_changed(Some(&old), &new));
    assert!(!install_source_changed(Some(&new), &new));
    assert!(!check_install_source(None, None, &new, false).unwrap());
    assert_eq!(
        check_install_source(Some("1.0.0"), Some(&old), &new, false)
            .unwrap_err()
            .code,
        "SOURCE_CHANGE_APPROVAL_REQUIRED"
    );
    assert!(check_install_source(Some("1.0.0"), None, &new, true).unwrap());
}

#[test]
fn old_preferences_preserve_identity_and_back_up_original_bytes() {
    let root = std::env::temp_dir().join(format!(
        "wla-config-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    ));
    fs::create_dir_all(&root).unwrap();
    let original = br#"{"schemaVersion":3,"enabledPlugins":{"editor":true},"grantedCapabilities":{"editor":["files.pick"]},"activeVersions":{"editor":"1.0.0"}}"#;
    fs::write(root.join(MANAGER_CONFIG_FILE), original).unwrap();
    let preferences = load_preferences(&root).unwrap();
    assert_eq!(preferences.schema_version, 4);
    assert_eq!(preferences.active_versions["editor"], "1.0.0");
    assert!(preferences.enabled_plugins["editor"]);
    let backup = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "bak"))
        .unwrap();
    assert_eq!(fs::read(backup).unwrap(), original);
    let saved = fs::read_to_string(root.join(MANAGER_CONFIG_FILE)).unwrap();
    assert!(!saved.contains("grantedCapabilities"));
    load_preferences(&root).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_config_is_not_replaced_or_moved() {
    let root = std::env::temp_dir().join(format!(
        "wla-invalid-config-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    ));
    fs::create_dir_all(&root).unwrap();
    let original = b"{broken";
    fs::write(root.join(MANAGER_CONFIG_FILE), original).unwrap();
    assert!(load_preferences(&root).is_err());
    assert_eq!(fs::read(root.join(MANAGER_CONFIG_FILE)).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn plugin_data_remains_by_default_and_is_deleted_only_when_requested() {
    let data_directory = std::env::temp_dir().join(format!(
        "wonderland-plugin-data-removal-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::create_dir_all(&data_directory).unwrap();
    let data_file = data_directory.join("user-state.json");
    fs::write(&data_file, b"user data").unwrap();

    apply_plugin_data_removal(&data_directory, false).unwrap();
    assert_eq!(fs::read(&data_file).unwrap(), b"user data");

    apply_plugin_data_removal(&data_directory, true).unwrap();
    assert!(!data_directory.exists());
}

fn local_source(origin: &str) -> PluginInstallSource {
    PluginInstallSource {
        kind: "local".into(),
        origin: origin.into(),
        author: None,
    }
}

#[test]
fn optional_deadlines_accept_null_and_long_budgets_without_a_policy_cap() {
    assert_eq!(optional_timeout_ms(None).unwrap(), None);
    assert_eq!(optional_timeout_ms(Some(&Value::Null)).unwrap(), None);
    assert_eq!(
        optional_timeout_ms(Some(&json!(172_800_000))).unwrap(),
        Some(172_800_000)
    );
    for value in [json!(0), json!(-1), json!("100")] {
        assert!(optional_timeout_ms(Some(&value)).is_err());
    }
    assert_eq!(earliest_timeout(None, None), None);
    assert_eq!(earliest_timeout(None, Some(100)), Some(100));
    assert_eq!(earliest_timeout(Some(200), Some(100)), Some(100));
    assert_eq!(remaining_timeout(None, Instant::now()).unwrap(), None);
    let expired = Instant::now() - Duration::from_millis(10);
    assert_eq!(
        remaining_timeout(Some(1), expired).unwrap_err().code,
        "TIMEOUT"
    );
}
