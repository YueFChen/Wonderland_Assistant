#[test]
#[ignore = "Requires the real direct-plugin process fixture"]
#[cfg(windows)]
fn failed_uninstall_cleanup_stays_discoverable_and_can_be_retried_without_losing_identity() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new(Arc::new(TestEnvironment::default()));
    let data = fixture.root.join("plugin-data/a");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("business"), b"retained").unwrap();
    let secrets = fixture.root.join("plugin-secrets/a");
    fs::create_dir_all(&secrets).unwrap();
    let secret = secrets.join("encrypted");
    fs::write(&secret, b"synthetic encrypted value").unwrap();
    {
        let mut state = fixture.manager.inner.state.lock().unwrap();
        state.preferences.publisher_identities["a"] = json!("ab".repeat(32));
        persist_preferences(&fixture.root, &state.preferences).unwrap();
    }
    assert_eq!(
        fixture
            .manager
            .cleanup_retained_plugin_data("a")
            .err()
            .unwrap()
            .code,
        "PLUGIN_STILL_INSTALLED"
    );
    let locked = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&secret)
        .unwrap();
    assert!(fixture.manager.uninstall("a", true).is_err());
    assert!(
        !fixture
            .manager
            .snapshots()
            .iter()
            .any(|state| state.manifest.id == "a")
    );
    assert!(
        fixture
            .manager
            .retained_plugin_data()
            .unwrap()
            .iter()
            .any(|item| item.id == "a")
    );
    assert!(
        publisher_key(&load_preferences(&fixture.root).unwrap(), "a")
            .unwrap()
            .is_some()
    );
    assert!(fixture.manager.cleanup_retained_plugin_data("a").is_err());
    drop(locked);
    assert!(
        !fixture
            .manager
            .cleanup_retained_plugin_data("a")
            .unwrap()
            .iter()
            .any(|item| item.id == "a")
    );
    assert!(
        publisher_key(&load_preferences(&fixture.root).unwrap(), "a")
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .manager
            .snapshots()
            .iter()
            .any(|state| state.manifest.id == "b")
    );
}

#[test]
#[ignore = "Requires the real direct-plugin process fixture"]
fn download_cancellation_cannot_interrupt_an_installation_commit() {
    let fixture = Fixture::new(Arc::new(TestEnvironment::default()));
    let (sender, receiver) = tokio::sync::watch::channel(false);
    fixture
        .manager
        .inner
        .catalog_installs
        .lock()
        .unwrap()
        .insert(
            "job".into(),
            CatalogInstallControl {
                plugin_id: "a".into(),
                cancel: sender,
                committing: false,
            },
        );
    assert!(fixture.manager.cancel_catalog_install("job").unwrap());
    assert!(*receiver.borrow());
    let (sender, receiver) = tokio::sync::watch::channel(false);
    fixture
        .manager
        .inner
        .catalog_installs
        .lock()
        .unwrap()
        .insert(
            "job".into(),
            CatalogInstallControl {
                plugin_id: "a".into(),
                cancel: sender,
                committing: true,
            },
        );
    assert!(!fixture.manager.cancel_catalog_install("job").unwrap());
    assert!(!*receiver.borrow());
    assert!(!fixture.manager.cancel_catalog_install("missing").unwrap());
}
use super::*;
use crate::host_environment::HostEnvironment;
use crate::remote_interaction::{RemoteBroker, RemoteContext};
use std::sync::mpsc;

#[derive(Default)]
pub(super) struct TestEnvironment {
    pub(super) events: Mutex<Vec<String>>,
    pub(super) pick_started: Option<mpsc::Sender<()>>,
    pub(super) install_started: Option<mpsc::Sender<()>>,
    pub(super) install_release: Option<Mutex<mpsc::Receiver<()>>>,
    pub(super) pick_release: Option<Mutex<mpsc::Receiver<()>>>,
}

impl HostEnvironment for TestEnvironment {
    fn emit(&self, topic: &str, payload: Value) {
        self.events.lock().unwrap().push(topic.into());
        if topic == "plugins:install-progress"
            && payload["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["status"] == "running"))
            && let (Some(started), Some(release)) = (&self.install_started, &self.install_release)
        {
            started.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(15))
                .unwrap();
        }
    }
    fn pick_file(&self, _extensions: &[String]) -> Result<Option<PathBuf>, PluginError> {
        if let (Some(started), Some(release)) = (&self.pick_started, &self.pick_release) {
            started.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            Ok(None)
        } else {
            Err(plugin_error(
                "INTERACTION_UNAVAILABLE",
                "Test host has no desktop.",
            ))
        }
    }
    fn open_url(&self, _url: &str) -> Result<(), PluginError> {
        Err(plugin_error(
            "INTERACTION_UNAVAILABLE",
            "Test host has no desktop.",
        ))
    }
    fn reveal_dir(&self, _path: &Path) -> Result<(), PluginError> {
        Err(plugin_error(
            "INTERACTION_UNAVAILABLE",
            "Test host has no desktop.",
        ))
    }
}

pub(crate) struct Fixture {
    pub(crate) manager: PluginManager,
    environment: Arc<TestEnvironment>,
    pub(crate) root: PathBuf,
    _runtime: tokio::runtime::Runtime,
}

impl Fixture {
    pub(super) fn new(environment: Arc<TestEnvironment>) -> Self {
        Self::new_with_stalled_plugin(environment, false)
    }

    fn new_with_stalled_plugin(environment: Arc<TestEnvironment>, stalled: bool) -> Self {
        Self::with_start_mode(environment, stalled, true)
    }

    pub(crate) fn cold() -> Self {
        Self::with_start_mode(Arc::new(TestEnvironment::default()), false, false)
    }

    pub(crate) fn enabled(&self, id: &str, enabled: bool) {
        self.manager.set_enabled(id, enabled).unwrap();
    }

    fn with_start_mode(environment: Arc<TestEnvironment>, stalled: bool, prestart: bool) -> Self {
        static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let executable = std::env::var_os("WLA_DIRECT_PLUGIN_FIXTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| checkout.join("target/debug/wonderland-direct-plugin-fixture.exe"));
        assert!(
            executable.is_file(),
            "Build tests/fixtures/direct-plugin first or set WLA_DIRECT_PLUGIN_FIXTURE."
        );
        let root = std::env::temp_dir().join(format!(
            "wla-direct-process-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).unwrap();
        let mut preferences = Preferences::default();
        let contract =
            fs::read(checkout.join("tests/fixtures/direct-plugin/contract.json")).unwrap();
        for (id, target) in [("a", "b"), ("b", "c"), ("c", "a")]
            .into_iter()
            .chain(stalled.then_some(("blocked", "")))
        {
            let package = root.join("installed-plugins").join(id).join("0.0.0");
            fs::create_dir_all(package.join("backend")).unwrap();
            fs::copy(&executable, package.join("backend/fixture.exe")).unwrap();
            fs::write(package.join("contract.json"), &contract).unwrap();
            let mut dependencies = if id == "blocked" {
                Vec::new()
            } else {
                vec![
                    json!({"pluginId":target,"versionReq":"^9.0.0","optional":false,
                    "methods":["echo","health","direct","delay","loop"]}),
                ]
            };
            if id == "a" {
                dependencies.push(json!({"pluginId":"missing","versionReq":"*","optional":false,"methods":["echo"]}));
            }
            let mut manifest = json!({"manifestVersion":3,"id":id,"name":format!("Fixture {id}"),"version":"0.0.0",
                "hostCompatibility":{"minCoreVersion":"0.0.0","maxCoreVersionExclusive":"1.0.0",
                    "protocol":{"minVersion":"1.0.0","maxVersionExclusive":"2.0.0"}},
                "platform":{"os":"windows","architecture":"x86_64","abi":"msvc"},
                "backend":{"entry":"backend/fixture.exe","transport":"stdio-ndjson-v1","supportsServiceContext":true},
                "contract":"contract.json","remoteAccess":id == "a","dependencies":dependencies});
            if !prestart {
                manifest["backend"]["activation"] = json!("on_demand");
                manifest["ui"] = json!({"entry":"ui/index.html","bridgeCompatibility":{"minVersion":"1.1.0","maxVersionExclusive":"2.0.0"},"integrations":[],"contributions":[{"id":"main","kind":"activity","title":"Main","defaultOrder":0}]});
                fs::create_dir_all(package.join("ui")).unwrap();
                fs::write(
                    package.join("ui/index.html"),
                    b"<!doctype html><title>fixture</title>",
                )
                .unwrap();
            }
            fs::write(
                package.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .unwrap();
            let mut paths = vec!["backend/fixture.exe", "contract.json", "manifest.json"];
            if !prestart {
                paths.push("ui/index.html");
            }
            let checksums = paths.into_iter().map(|path| {
                json!({"path":path,"sha256":hex::encode(Sha256::digest(fs::read(package.join(path)).unwrap()))})
            }).collect::<Vec<_>>();
            fs::write(
                package.join("checksums.json"),
                serde_json::to_vec(&json!({"algorithm":"sha256","files":checksums})).unwrap(),
            )
            .unwrap();
            preferences.enabled_plugins.insert(id.into(), true);
            preferences
                .active_versions
                .insert(id.into(), "0.0.0".into());
        }
        persist_preferences(&root, &preferences).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let context = AppContext {
            app_data_dir: root.clone(),
            documents_dir: root.clone(),
            settings: None,
            user_data: None,
        };
        let manager = PluginManager::load(
            &context,
            environment.clone(),
            runtime.handle().clone(),
            true,
        )
        .unwrap();
        if prestart {
            for id in ["a", "b", "c"]
                .into_iter()
                .chain(stalled.then_some("blocked"))
            {
                manager.inner.start_one(id).unwrap();
            }
        }
        Self {
            manager,
            environment,
            root,
            _runtime: runtime,
        }
    }

    pub(super) fn restart(&mut self) {
        self.restart_as(true);
    }

    pub(super) fn restart_as(&mut self, desktop_host: bool) {
        self.manager.shutdown();
        let context = AppContext {
            app_data_dir: self.root.clone(),
            documents_dir: self.root.clone(),
            settings: None,
            user_data: None,
        };
        self.manager = PluginManager::load(
            &context,
            self.environment.clone(),
            self._runtime.handle().clone(),
            desktop_host,
        )
        .unwrap();
    }

    pub(super) fn policy(&self, id: &str, policy: Option<&str>) {
        let package = self.root.join("installed-plugins").join(id).join("0.0.0");
        let manifest_path = package.join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        if let Some(policy) = policy {
            manifest["backend"]["activation"] = json!(policy);
        } else {
            manifest["backend"]
                .as_object_mut()
                .unwrap()
                .remove("activation");
        }
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let path = package.join("checksums.json");
        let mut checksums: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for file in checksums["files"].as_array_mut().unwrap() {
            file["sha256"] = json!(hex::encode(Sha256::digest(
                fs::read(package.join(file["path"].as_str().unwrap())).unwrap()
            )));
        }
        fs::write(path, serde_json::to_vec(&checksums).unwrap()).unwrap();
        self.manager.refresh().unwrap();
    }

    pub(crate) fn snapshot(&self, id: &str) -> PluginRuntimeState {
        self.manager
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.manifest.id == id)
            .unwrap()
    }

    pub(crate) fn wait_until(&self, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "Timed out waiting for fixture state."
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.manager.shutdown();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "Build the independent direct-plugin fixture first"]
fn failed_preference_commit_restores_same_version_package_and_keeps_data() {
    let fixture = Fixture::new(Arc::new(TestEnvironment::default()));
    let installed = fixture.root.join("installed-plugins/a/0.0.0");
    let incoming = fixture.root.join("same-version-input");
    let original = fs::read(installed.join("backend/fixture.exe")).unwrap();
    let original_preferences = fs::read(fixture.root.join(MANAGER_CONFIG_FILE)).unwrap();
    let marker = fixture.root.join("plugin-data/a/retention-marker");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, b"retained user data").unwrap();
    fs::create_dir_all(incoming.join("backend")).unwrap();
    for file in ["manifest.json", "contract.json"] {
        fs::copy(installed.join(file), incoming.join(file)).unwrap();
    }
    fs::write(
        incoming.join("backend/fixture.exe"),
        b"replacement must never survive a failed metadata commit",
    )
    .unwrap();
    let files=["backend/fixture.exe","contract.json","manifest.json"].into_iter().map(|path|json!({"path":path,"sha256":hex::encode(Sha256::digest(fs::read(incoming.join(path)).unwrap()))})).collect::<Vec<_>>();
    fs::write(
        incoming.join("checksums.json"),
        serde_json::to_vec(&json!({"algorithm":"sha256","files":files})).unwrap(),
    )
    .unwrap();
    let blocked = fixture.root.join(format!(".{MANAGER_CONFIG_FILE}.tmp"));
    fs::create_dir(&blocked).unwrap();
    let result = fixture.manager.install_transaction(
        incoming,
        true,
        None,
        None,
        PluginInstallSource {
            kind: "local".into(),
            origin: "fixture-local-development".into(),
            author: None,
        },
        true,
    );
    assert_eq!(
        result.unwrap_err().code,
        "PUBLISHER_IDENTITY_PERSIST_FAILED"
    );
    assert_eq!(
        fs::read(installed.join("backend/fixture.exe")).unwrap(),
        original
    );
    assert_eq!(
        fs::read(fixture.root.join(MANAGER_CONFIG_FILE)).unwrap(),
        original_preferences
    );
    assert_eq!(fs::read(marker).unwrap(), b"retained user data");
    assert_eq!(fixture.snapshot("a").runtime, RuntimeState::Running);
    assert_eq!(
        publisher_key(&load_preferences(&fixture.root).unwrap(), "a").unwrap(),
        None
    );
    fs::remove_dir(blocked).unwrap();
}

#[test]
#[ignore = "Build the independent direct-plugin fixture first"]
fn failed_online_start_rolls_back_first_publisher_pin_with_the_old_installation() {
    let fixture = Fixture::new(Arc::new(TestEnvironment::default()));
    let incoming = fixture.root.join("incoming");
    fs::create_dir_all(incoming.join("backend")).unwrap();
    let installed = fixture.root.join("installed-plugins/a/0.0.0");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(installed.join("manifest.json")).unwrap()).unwrap();
    manifest["version"] = json!("0.0.1");
    manifest["author"] = json!("Fixture author");
    fs::write(
        incoming.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::copy(
        installed.join("contract.json"),
        incoming.join("contract.json"),
    )
    .unwrap();
    fs::write(
        incoming.join("backend/fixture.exe"),
        b"invalid executable for rollback test",
    )
    .unwrap();
    let files=["backend/fixture.exe","contract.json","manifest.json"].into_iter().map(|path|json!({"path":path,"sha256":hex::encode(Sha256::digest(fs::read(incoming.join(path)).unwrap()))})).collect::<Vec<_>>();
    let checksums = serde_json::to_vec(&json!({"algorithm":"sha256","files":files})).unwrap();
    fs::write(incoming.join("checksums.json"), &checksums).unwrap();
    let signature = wonderland_plugin_signing::sign(&checksums, &[42; 32]).unwrap();
    fs::write(
        incoming.join("signature.json"),
        serde_json::to_vec(&signature).unwrap(),
    )
    .unwrap();
    let result = fixture.manager.install_transaction(
        incoming,
        false,
        None,
        Some(&signature.public_key),
        PluginInstallSource {
            kind: "catalog".into(),
            origin: "https://fixture.example".into(),
            author: Some("Fixture author".into()),
        },
        true,
    );
    assert!(result.is_err());
    assert_eq!(fixture.snapshot("a").manifest.version, "0.0.0");
    let preferences = load_preferences(&fixture.root).unwrap();
    assert_eq!(publisher_key(&preferences, "a").unwrap(), None);
    assert_eq!(preferences.active_versions["a"], "0.0.0");
}

#[test]
#[ignore = "Build the independent direct-plugin fixture and set WLA_DIRECT_PLUGIN_FIXTURE if needed"]
fn actual_process_chain_keeps_dependency_diagnostics_soft_and_validates_real_contracts() {
    let fixture = Fixture::new(Arc::new(TestEnvironment::default()));
    {
        let mut state = fixture.manager.inner.state.lock().unwrap();
        for id in ["a", "b"] {
            state.plugins.get_mut(id).unwrap().contract["methods"]["health"]["localOnly"] =
                json!(true);
        }
    }
    let remote_policy = Arc::new(RemoteContext::new(
        Arc::new(RemoteBroker::default()),
        "policy-test".into(),
        "a".into(),
    ));
    assert_eq!(
        fixture
            .manager
            .call_with_context(
                "a",
                "local-policy-direct",
                "health",
                json!({}),
                Some(remote_policy.clone())
            )
            .unwrap_err()
            .code,
        "LOCAL_ONLY"
    );
    assert_eq!(
        fixture
            .manager
            .invoke_plugin("a", "b", "health", json!({}), None, Some(remote_policy))
            .unwrap_err()
            .code,
        "LOCAL_ONLY"
    );
    assert!(
        fixture
            .manager
            .call("a", "local-health", "health", json!({}))
            .is_ok()
    );
    assert!(
        fixture
            .snapshot("a")
            .dependency_issues
            .iter()
            .any(|issue| issue.contains("missing"))
    );
    assert!(
        fixture
            .snapshot("a")
            .dependency_issues
            .iter()
            .any(|issue| issue.contains("version"))
    );
    assert!(
        fixture
            .snapshot("a")
            .dependency_issues
            .iter()
            .any(|issue| issue.contains("cycle"))
    );
    assert_eq!(
        fixture
            .manager
            .call("a", "echo", "echo", json!({"text":"three processes"}))
            .unwrap(),
        "three processes"
    );
    let remote = Arc::new(RemoteContext::new(
        Arc::new(RemoteBroker::default()),
        "browser".into(),
        "a".into(),
    ));
    assert!(!fixture.snapshot("b").manifest.supports_remote_access());
    assert_eq!(
        fixture
            .manager
            .call_with_context(
                "a",
                "remote",
                "echo",
                json!({"text":"hidden dependencies"}),
                Some(remote)
            )
            .unwrap(),
        "hidden dependencies"
    );

    for (params, code) in [
        (
            json!({"pluginId":"b","method":"missing_method","params":{}}),
            "METHOD_NOT_FOUND",
        ),
        (
            json!({"pluginId":"b","method":"echo","params":{"text":7}}),
            "INVALID_INPUT",
        ),
        (
            json!({"pluginId":"b","method":"bad_result","params":{}}),
            "INVALID_RESPONSE",
        ),
        (
            json!({"pluginId":"missing","method":"echo","params":{"text":"x"}}),
            "NOT_INSTALLED",
        ),
    ] {
        assert_eq!(
            fixture
                .manager
                .call("a", code, "direct", params)
                .unwrap_err()
                .code,
            code
        );
    }
    assert_eq!(
        fixture
            .manager
            .call("a", "headless", "pick", json!({}))
            .unwrap_err()
            .code,
        "INTERACTION_UNAVAILABLE"
    );
    for index in 0..3 {
        assert_eq!(
            fixture
                .manager
                .call(
                    "a",
                    &format!("undeclared-{index}"),
                    "direct",
                    json!({"pluginId":"c","method":"health","params":{}})
                )
                .unwrap(),
            "c"
        );
    }
    let snapshot = fixture.snapshot("a");
    assert_eq!(
        snapshot
            .dependency_issues
            .iter()
            .filter(|issue| issue.contains("Runtime call 'c.health'"))
            .count(),
        1
    );
    assert!(
        snapshot
            .dependency_issues
            .iter()
            .any(|issue| issue.contains("Runtime call 'b.bad_result'")
                && issue.contains("method expectation"))
    );

    let before = fixture.environment.events.lock().unwrap().len();
    fixture.manager.set_enabled("c", false).unwrap();
    assert!(fixture.environment.events.lock().unwrap().len() > before);
    assert_eq!(fixture.snapshot("a").runtime, RuntimeState::Running);
    assert_eq!(fixture.snapshot("b").runtime, RuntimeState::Running);
    assert_eq!(
        fixture
            .manager
            .call("a", "provider-stopped", "echo", json!({"text":"x"}))
            .unwrap_err()
            .code,
        "DISABLED"
    );
    // Missing and disabled declarations do not block restarting the consumer.
    fixture.manager.set_enabled("a", false).unwrap();
    fixture.manager.set_enabled("a", true).unwrap();
    fixture.manager.inner.start_one("a").unwrap();
    assert_eq!(fixture.snapshot("a").runtime, RuntimeState::Running);
    assert_eq!(
        fixture
            .manager
            .call("a", "still-running", "health", json!({}))
            .unwrap(),
        "a"
    );
    let before = fixture.environment.events.lock().unwrap().len();
    fixture.manager.snapshots();
    fixture.manager.snapshots();
    assert_eq!(fixture.environment.events.lock().unwrap().len(), before);
}

#[test]
#[ignore = "Build the independent direct-plugin fixture and set WLA_DIRECT_PLUGIN_FIXTURE if needed"]
fn actual_local_cycles_are_bounded_and_parent_cancellation_and_budget_reach_children() {
    let fixture = Fixture::new(Arc::new(TestEnvironment::default()));
    assert_eq!(
        fixture
            .manager
            .call("a", "cycle", "loop", json!({}))
            .unwrap_err()
            .code,
        "CALL_DEPTH_EXCEEDED"
    );
    fixture.wait_until(|| fixture.manager.upgrade_blockers().is_empty());
    let nested = json!({"pluginId":"b","method":"direct","params":{"pluginId":"c","method":"delay","params":{"milliseconds":3000}}});
    let manager = fixture.manager.clone();
    let pending = thread::spawn(move || manager.call("a", "cancel-chain", "direct", nested));
    fixture.wait_until(|| {
        fixture.manager.inner.state.lock().unwrap().plugins["c"]
            .process
            .as_ref()
            .unwrap()
            .active_request_count()
            > 0
    });
    fixture.manager.cancel("a", "cancel-chain").unwrap();
    assert_eq!(pending.join().unwrap().unwrap_err().code, "CANCELLED");
    fixture.wait_until(|| fixture.manager.upgrade_blockers().is_empty());
    let started = Instant::now();
    assert_eq!(
        fixture
            .manager
            .invoke_plugin(
                "c",
                "a",
                "direct",
                json!({"pluginId":"b","method":"delay","params":{"milliseconds":3000}}),
                Some(100),
                None
            )
            .unwrap_err()
            .code,
        "TIMEOUT"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    fixture.wait_until(|| fixture.manager.upgrade_blockers().is_empty());
    assert_eq!(
        fixture
            .manager
            .call("a", "after-cancel", "health", json!({}))
            .unwrap(),
        "a"
    );
}

#[test]
#[ignore = "Build the independent direct-plugin fixture and set WLA_DIRECT_PLUGIN_FIXTURE if needed"]
fn slow_native_host_work_does_not_block_stdout_and_upgrade_freeze_closes_admission() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let environment = Arc::new(TestEnvironment {
        events: Mutex::default(),
        pick_started: Some(started_tx),
        pick_release: Some(Mutex::new(release_rx)),
        ..Default::default()
    });
    let fixture = Fixture::new(environment);
    let manager = fixture.manager.clone();
    let slow = thread::spawn(move || manager.call("a", "slow-picker", "pick", json!({})));
    started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        fixture
            .manager
            .call("a", "concurrent-fast", "health", json!({}))
            .unwrap(),
        "a"
    );
    assert_eq!(
        fixture.manager.begin_upgrade().err().unwrap().code,
        "UPGRADE_BUSY"
    );
    fixture.manager.cancel("a", "slow-picker").unwrap();
    assert_eq!(slow.join().unwrap().unwrap_err().code, "CANCELLED");
    // The native dialog worker is still active even after its originating plugin call ended.
    assert!(!fixture.manager.upgrade_blockers().is_empty());
    release_tx.send(()).unwrap();
    fixture.wait_until(|| fixture.manager.upgrade_blockers().is_empty());
    let upgrade = fixture.manager.begin_upgrade().unwrap();
    assert_eq!(
        fixture
            .manager
            .call("a", "frozen", "health", json!({}))
            .unwrap_err()
            .code,
        "UPGRADE_IN_PROGRESS"
    );
    assert_eq!(
        fixture.manager.inner.start_one("a").unwrap_err().code,
        "UPGRADE_IN_PROGRESS"
    );
    assert_eq!(
        fixture.manager.admit_work().err().unwrap().code,
        "UPGRADE_IN_PROGRESS"
    );
    drop(upgrade);
    assert_eq!(
        fixture
            .manager
            .call("a", "unfrozen", "health", json!({}))
            .unwrap(),
        "a"
    );
    let manager_clone = fixture.manager.clone();
    let processes = fixture
        .manager
        .inner
        .state
        .lock()
        .unwrap()
        .plugins
        .values()
        .filter_map(|record| record.process.clone())
        .collect::<Vec<_>>();
    assert_eq!(processes.len(), 3);
    fixture.manager.shutdown();
    assert!(processes.iter().all(|process| !process.is_alive()));
    assert_eq!(
        manager_clone
            .call("a", "after-shutdown", "health", json!({}))
            .unwrap_err()
            .code,
        "SERVICE_UNAVAILABLE"
    );
}

#[test]
#[ignore = "Build the independent direct-plugin fixture and set WLA_DIRECT_PLUGIN_FIXTURE if needed"]
fn unread_stdin_preserves_timeouts_cancellation_and_shutdown_without_holding_state() {
    let fixture = Fixture::new_with_stalled_plugin(Arc::new(TestEnvironment::default()), true);
    assert!(
        fixture
            .root
            .join("plugin-data/blocked/stdin-paused")
            .is_file()
    );
    let process = fixture.manager.inner.state.lock().unwrap().plugins["blocked"]
        .process
        .clone()
        .unwrap();
    let manager = fixture.manager.clone();
    let (timeout_tx, timeout_rx) = mpsc::channel();
    let large_call = thread::spawn(move || {
        let result = manager.call(
            "blocked",
            "blocked-large-frame",
            "large",
            json!({"text":"x".repeat(2 * 1024 * 1024)}),
        );
        timeout_tx.send(result).unwrap();
    });
    let timed_out = timeout_rx.recv_timeout(Duration::from_secs(3));
    if timed_out.is_err() {
        fixture.manager.shutdown();
    }
    assert_eq!(timed_out.unwrap().unwrap_err().code, "TIMEOUT");
    large_call.join().unwrap();
    // The pipe writer is still blocked, so an ended IPC request alone cannot permit upgrade.
    assert!(process.active_request_count() > 0);
    assert_eq!(
        fixture.manager.begin_upgrade().err().unwrap().code,
        "UPGRADE_BUSY"
    );

    let manager = fixture.manager.clone();
    let (call_tx, call_rx) = mpsc::channel();
    let call = thread::spawn(move || {
        call_tx
            .send(manager.call("blocked", "cancel-queued-frame", "health", json!({})))
            .unwrap();
    });
    fixture.wait_until(|| process.has_active_request_for_test("cancel-queued-frame"));
    let manager = fixture.manager.clone();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let cancel = thread::spawn(move || {
        cancel_tx
            .send(manager.cancel("blocked", "cancel-queued-frame"))
            .unwrap();
    });
    let cancelled = cancel_rx.recv_timeout(Duration::from_secs(1));
    if cancelled.is_err() {
        // Keep a failed lock-order regression bounded without needing manager state.
        process.stop().unwrap();
    }
    cancelled.unwrap().unwrap();
    cancel.join().unwrap();
    assert_eq!(
        call_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .code,
        "CANCELLED"
    );
    call.join().unwrap();
    assert_eq!(fixture.manager.snapshots().len(), 4);
    assert_eq!(
        fixture
            .manager
            .call("a", "other-plugin", "health", json!({}))
            .unwrap(),
        "a"
    );
    let stopped = Instant::now();
    fixture.manager.shutdown();
    assert!(stopped.elapsed() < Duration::from_secs(3));
    assert!(!process.is_alive());
    fixture.wait_until(|| process.active_request_count() == 0);
}
