use super::direct_process_tests::Fixture;
use super::lifecycle::LifecycleStore;
use super::*;
use crate::host_services::HostInvocation;
use std::sync::Barrier;
use wonderland_plugin_protocol::{
    BackgroundRestoreState as Restore, PluginActivationPolicy as Policy,
};

fn scratch() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "wla-lifecycle-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn lifecycle_control_repairs_preserve_original_bytes_and_unknown_records() {
    let root = scratch();
    let original = b"{not valid controls";
    fs::write(root.join("plugin-lifecycle.json"), original).unwrap();
    let store = LifecycleStore::load(&root);
    assert_eq!(store.restore("a"), Restore::Unknown);
    store.set_restore("a", true).unwrap();
    let backup = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().contains("corrupt-"))
        .unwrap();
    assert_eq!(fs::read(backup.path()).unwrap(), original);
    let reloaded = LifecycleStore::load(&root);
    assert_eq!(reloaded.restore("a"), Restore::Required);
    assert_eq!(reloaded.restore("b"), Restore::Unknown);
    reloaded.set_restore("a", false).unwrap();
    assert_eq!(
        LifecycleStore::load(&root).restore("a"),
        Restore::NotRequired
    );
    reloaded.remove("a").unwrap();
    assert_eq!(LifecycleStore::load(&root).restore("a"), Restore::Unknown);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lifecycle_policy_keeps_legacy_safe_and_background_unknown_distinct_from_false() {
    let root = scratch();
    let store = LifecycleStore::load(&root);
    let manifest: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/comment_collector_manifest.json"
    ))
    .unwrap();
    let mut state: PluginRuntimeState = serde_json::from_value(json!({"manifest":manifest,"installation":"installed","enabled":true,"runtime":"stopped","lastError":null})).unwrap();
    state.manifest.backend.activation = Some(Policy::OnDemand);
    assert!(!store.should_restore(&state));
    state.manifest.backend.activation = Some(Policy::BackgroundRestore);
    assert!(store.should_restore(&state));
    store.set_restore(&state.manifest.id, false).unwrap();
    assert!(!store.should_restore(&state));
    store.set_restore(&state.manifest.id, true).unwrap();
    assert!(store.should_restore(&state));
    state.manifest.backend.activation = Some(Policy::OnDemand);
    assert!(
        store.should_restore(&state),
        "changing metadata cannot discard an existing restore need"
    );
    state.enabled = false;
    assert!(!store.should_restore(&state));
    state.enabled = true;
    state.manifest.backend.activation = None;
    store.set_restore(&state.manifest.id, false).unwrap();
    assert!(store.should_restore(&state));
    assert_eq!(store.view(&state.manifest).policy_source, "legacy");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lifecycle_instance_counts_are_owned_and_replacement_admission_is_atomic() {
    let root = scratch();
    let store = Arc::new(LifecycleStore::load(&root));
    let admission = Arc::new(Mutex::new(PluginAdmission {
        frozen: false,
        active: 1,
    }));
    store.connect_instance("a", 1);
    let id = store
        .begin_activity("a", 1, "work".into(), PluginWorkGuard(admission.clone()))
        .unwrap();
    assert!(store.pause("a", false).is_err());
    store.connect_instance("a", 2);
    assert_eq!(store.report("a", 1, false).unwrap_err().code, "CANCELLED");
    store.report("a", 2, true).unwrap();
    store.end_activity("a", 2, &id);
    assert_eq!(admission.lock().unwrap().active, 1);
    store.clear_instance("a", 1);
    assert_eq!(admission.lock().unwrap().active, 0);
    store.report("a", 2, false).unwrap();
    let pause = store.pause("a", false).unwrap();
    assert!(store.enter_call("a").is_err());
    drop(pause);
    let call = store.enter_call("a").unwrap();
    assert!(store.pause("a", false).is_err());
    drop(call);
    assert!(store.pause("a", false).is_ok());
    fs::remove_dir_all(root).unwrap();
}

fn data(fixture: &Fixture, id: &str) -> PathBuf {
    fixture.root.join("plugin-data").join(id)
}
fn starts(fixture: &Fixture, id: &str) -> usize {
    fs::read_dir(data(fixture, id))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("started-"))
                .count()
        })
        .unwrap_or(0)
}
fn pause_before_hello(fixture: &Fixture, id: &str) {
    fs::create_dir_all(data(fixture, id)).unwrap();
    fs::write(
        data(fixture, id).join("pause-before-hello"),
        b"owned test gate",
    )
    .unwrap();
}
fn health(manager: &PluginManager, id: &str, request: &str) -> Result<Value, PluginError> {
    manager.call(id, request, "health", json!({}))
}

fn incoming(fixture: &Fixture, name: &str) -> PathBuf {
    let source = fixture.root.join("installed-plugins/a/0.0.0");
    let target = fixture.root.join(name);
    for path in [
        "manifest.json",
        "contract.json",
        "checksums.json",
        "backend/fixture.exe",
        "ui/index.html",
    ] {
        let destination = target.join(path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source.join(path), destination).unwrap();
    }
    target
}

fn update_checksums(package: &Path) {
    let path = package.join("checksums.json");
    let mut checksums: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for file in checksums["files"].as_array_mut().unwrap() {
        file["sha256"] = json!(hex::encode(Sha256::digest(
            fs::read(package.join(file["path"].as_str().unwrap())).unwrap()
        )));
    }
    fs::write(path, serde_json::to_vec(&checksums).unwrap()).unwrap();
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture; optional benchmark receipt path via WLA_LIFECYCLE_REPORT"]
fn lifecycle_records_comparable_startup_and_first_use_measurements() {
    fn processes(fixture: &Fixture) -> Value {
        let pids: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .flat_map(|id| {
                fs::read_dir(data(fixture, id))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .strip_prefix("started-")
                            .and_then(|id| id.parse::<u32>().ok())
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        if pids.is_empty() {
            return json!({"count":0,"workingSetBytes":0,"privateBytes":0,"threads":0,"handles":0,"cpuSeconds":0});
        }
        // Optional Windows measurements keep the product free of a performance dependency.
        let script = format!(
            "$p=@(Get-Process -Id {} -ErrorAction SilentlyContinue); @{{count=$p.Count;workingSetBytes=($p|Measure-Object WorkingSet64 -Sum).Sum;privateBytes=($p|Measure-Object PrivateMemorySize64 -Sum).Sum;threads=($p|ForEach-Object {{$_.Threads.Count}}|Measure-Object -Sum).Sum;handles=($p|Measure-Object Handles -Sum).Sum;cpuSeconds=($p|Measure-Object CPU -Sum).Sum}}|ConvertTo-Json -Compress",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .unwrap();
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    }
    let baseline = Fixture::cold();
    for id in ["a", "b", "c"] {
        baseline.policy(id, Some("on_startup"));
    }
    let start = Instant::now();
    baseline.manager.start_enabled();
    baseline.wait_until(|| {
        baseline
            .manager
            .snapshots()
            .iter()
            .all(|s| s.runtime == RuntimeState::Running)
    });
    let eager_ms = start.elapsed().as_secs_f64() * 1000.0;
    let eager_processes = processes(&baseline);
    drop(baseline);

    let demand = Fixture::cold();
    let start = Instant::now();
    demand.manager.start_enabled();
    let demand_ms = start.elapsed().as_secs_f64() * 1000.0;
    let cold_processes = processes(&demand);
    let start = Instant::now();
    health(&demand.manager, "a", "first-use").unwrap();
    let first_use_ms = start.elapsed().as_secs_f64() * 1000.0;
    let used_processes = processes(&demand);
    let start = Instant::now();
    for i in 0..10 {
        health(&demand.manager, "a", &format!("steady-{i}")).unwrap();
    }
    let steady_ms = start.elapsed().as_secs_f64() * 100.0;
    let receipt = json!({"scope":"Same debug Core manager and three synthetic backend packages; child process metrics only, not full desktop or production plugin benchmarks", "eager":{"startupMs":eager_ms,"processes":eager_processes},"onDemand":{"startupMs":demand_ms,"beforeUse":cold_processes,"firstUseMs":first_use_ms,"afterOneUse":used_processes,"meanSteadyCallMs":steady_ms}});
    assert_eq!(receipt["eager"]["processes"]["count"], 3);
    assert_eq!(receipt["onDemand"]["beforeUse"]["count"], 0);
    assert_eq!(receipt["onDemand"]["afterOneUse"]["count"], 1);
    if let Some(path) = std::env::var_os("WLA_LIFECYCLE_REPORT") {
        fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    }
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_install_keeps_cold_packages_cold_and_retained_controls_survive_reinstall() {
    let fixture = Fixture::cold();
    let source = incoming(&fixture, "cold-input");
    fixture
        .manager
        .inner
        .lifecycle
        .set_restore("a", false)
        .unwrap();
    fs::create_dir_all(data(&fixture, "a")).unwrap();
    fs::write(data(&fixture, "a").join("user-content"), b"keep").unwrap();
    fixture.manager.uninstall("a", false).unwrap();
    let retained = fixture.manager.retained_plugin_data().unwrap();
    assert!(retained.iter().any(|item| item.id == "a"));
    assert_eq!(
        fixture.manager.inner.lifecycle.restore("a"),
        Restore::NotRequired
    );
    fixture.manager.install(source, true, true, None).unwrap();
    assert!(fixture.snapshot("a").enabled);
    assert_eq!(fixture.snapshot("a").runtime, RuntimeState::Stopped);
    assert_eq!(starts(&fixture, "a"), 0);
    assert_eq!(
        fs::read(data(&fixture, "a").join("user-content")).unwrap(),
        b"keep"
    );
    fixture.manager.uninstall("a", true).unwrap();
    assert_eq!(
        fixture.manager.inner.lifecycle.restore("a"),
        Restore::Unknown
    );
    assert!(
        !fixture
            .manager
            .retained_plugin_data()
            .unwrap()
            .iter()
            .any(|item| item.id == "a")
    );
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_warm_same_version_update_restarts_and_failed_update_restores_original_runtime() {
    let fixture = Fixture::cold();
    health(&fixture.manager, "a", "warm").unwrap();
    let original_revision = fixture.snapshot("a").lifecycle.unwrap().package_revision;
    let source = incoming(&fixture, "warm-input");
    fs::write(
        source.join("ui/index.html"),
        b"<!doctype html><title>updated fixture</title>",
    )
    .unwrap();
    update_checksums(&source);
    fixture.manager.install(source, true, true, None).unwrap();
    assert_eq!(fixture.snapshot("a").runtime, RuntimeState::Running);
    let revision = fixture.snapshot("a").lifecycle.unwrap().package_revision;
    assert_ne!(revision, original_revision);
    fixture.manager.retry_runtime("a").unwrap();
    assert_eq!(
        fixture.snapshot("a").lifecycle.unwrap().package_revision,
        revision
    );
    let bad = incoming(&fixture, "broken-input");
    fs::write(bad.join("backend/fixture.exe"), b"invalid executable").unwrap();
    update_checksums(&bad);
    assert_eq!(
        fixture
            .manager
            .install(bad, true, true, None)
            .unwrap_err()
            .code,
        "PLUGIN_START_FAILED"
    );
    assert_eq!(fixture.snapshot("a").runtime, RuntimeState::Running);
    assert_eq!(
        fixture.snapshot("a").lifecycle.unwrap().package_revision,
        revision
    );
    assert_eq!(
        health(&fixture.manager, "a", "after-rollback").unwrap(),
        json!("a")
    );
    assert!(
        fs::read_to_string(fixture.root.join("installed-plugins/a/0.0.0/ui/index.html"))
            .unwrap()
            .contains("updated fixture")
    );
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_standalone_cli_enable_and_inventory_never_restore_unrelated_backends() {
    let mut fixture = Fixture::cold();
    fixture.policy("a", Some("on_startup"));
    fixture.policy("b", Some("background_restore"));
    fixture
        .manager
        .inner
        .lifecycle
        .set_restore("b", true)
        .unwrap();
    fixture.restart_as(false);
    fixture.manager.start_enabled();
    fixture.manager.set_enabled("a", false).unwrap();
    fixture.manager.set_enabled("a", true).unwrap();
    assert!(
        fixture
            .manager
            .snapshots()
            .iter()
            .all(|s| s.runtime == RuntimeState::Stopped)
    );
    assert_eq!(starts(&fixture, "a"), 0);
    assert_eq!(starts(&fixture, "b"), 0);
    assert_eq!(
        health(&fixture.manager, "c", "cli-call").unwrap(),
        json!("c")
    );
    assert_eq!(starts(&fixture, "c"), 1);
    assert_eq!(starts(&fixture, "b"), 0);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_startup_dependency_calls_cannot_deadlock_initialization() {
    let fixture = Fixture::cold();
    fs::create_dir_all(data(&fixture, "a")).unwrap();
    fs::write(data(&fixture, "a").join("startup-direct"), b"owned fixture").unwrap();
    assert_eq!(
        health(&fixture.manager, "a", "initialize").unwrap(),
        json!("a")
    );
    assert_eq!(
        fs::read_to_string(data(&fixture, "a").join("startup-direct-denied")).unwrap(),
        "STARTUP_DEPENDENCY_CALL"
    );
    assert_eq!(starts(&fixture, "b"), 0);
    assert_eq!(
        fixture
            .manager
            .call(
                "a",
                "initialized",
                "direct",
                json!({
                    "pluginId":"b", "method":"health", "params":{}
                })
            )
            .unwrap(),
        json!("b")
    );
    assert_eq!(starts(&fixture, "b"), 1);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_cold_metadata_ui_and_method_calls_activate_only_used_targets() {
    let fixture = Fixture::cold();
    fixture.manager.start_enabled();
    assert!(
        fixture
            .manager
            .snapshots()
            .iter()
            .all(|s| s.runtime == RuntimeState::Stopped)
    );
    fixture.manager.plugin_ui_launch_info("a").unwrap();
    assert_eq!(starts(&fixture, "a"), 0);
    assert_eq!(
        fixture
            .manager
            .call("a", "bad", "unknown", json!({}))
            .unwrap_err()
            .code,
        "METHOD_NOT_FOUND"
    );
    assert_eq!(
        fixture
            .manager
            .call("a", "invalid", "echo", json!({}))
            .unwrap_err()
            .code,
        "INVALID_INPUT"
    );
    assert_eq!(starts(&fixture, "a"), 0);
    let launch = fixture
        .manager
        .prepare_ui("a", "ui-a", false, &HostInvocation::default())
        .unwrap();
    assert!(!launch.url.is_empty());
    assert_eq!(starts(&fixture, "a"), 1);
    assert_eq!(starts(&fixture, "b"), 0);
    assert_eq!(starts(&fixture, "c"), 0);
    assert_eq!(
        fixture
            .manager
            .call("a", "chain", "echo", json!({"text":"chain works"}))
            .unwrap(),
        json!("chain works")
    );
    assert_eq!(starts(&fixture, "b"), 1);
    assert_eq!(starts(&fixture, "c"), 1);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_concurrent_first_calls_and_ui_share_one_handshake() {
    let fixture = Fixture::cold();
    pause_before_hello(&fixture, "a");
    let barrier = Arc::new(Barrier::new(13));
    let threads: Vec<_> = (0..12)
        .map(|i| {
            let manager = fixture.manager.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                if i % 2 == 0 {
                    health(&manager, "a", &format!("call-{i}")).map(|_| ())
                } else {
                    manager
                        .prepare_ui("a", &format!("ui-{i}"), false, &HostInvocation::default())
                        .map(|_| ())
                }
            })
        })
        .collect();
    barrier.wait();
    fixture.wait_until(|| starts(&fixture, "a") == 1);
    fs::remove_file(data(&fixture, "a").join("pause-before-hello")).unwrap();
    for call in threads {
        call.join().unwrap().unwrap();
    }
    assert_eq!(starts(&fixture, "a"), 1);
    assert_eq!(starts(&fixture, "b"), 0);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_cancelled_wait_does_not_kill_another_waiter_or_dispatch_late_work() {
    let fixture = Fixture::cold();
    pause_before_hello(&fixture, "a");
    let manager = fixture.manager.clone();
    let cancelled = thread::spawn(move || {
        manager.call("a", "cancelled", "start_task", json!({"milliseconds":10}))
    });
    fixture.wait_until(|| starts(&fixture, "a") == 1);
    let manager = fixture.manager.clone();
    let surviving = thread::spawn(move || health(&manager, "a", "surviving"));
    fixture.manager.cancel("a", "cancelled").unwrap();
    assert_eq!(cancelled.join().unwrap().unwrap_err().code, "CANCELLED");
    fs::remove_file(data(&fixture, "a").join("pause-before-hello")).unwrap();
    assert_eq!(surviving.join().unwrap().unwrap(), json!("a"));
    assert_eq!(starts(&fixture, "a"), 1);
    assert!(!data(&fixture, "a").join("task-started").exists());
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_deadline_includes_startup_and_never_replays_expired_work() {
    let fixture = Fixture::cold();
    pause_before_hello(&fixture, "a");
    let manager = fixture.manager.clone();
    let request = thread::spawn(move || {
        manager.call_with_timeout_context(
            "a",
            "deadline",
            "start_task",
            json!({"milliseconds":10}),
            Some(100),
            None,
        )
    });
    fixture.wait_until(|| starts(&fixture, "a") == 1);
    assert_eq!(request.join().unwrap().unwrap_err().code, "TIMEOUT");
    fs::remove_file(data(&fixture, "a").join("pause-before-hello")).unwrap();
    assert_eq!(health(&fixture.manager, "a", "next").unwrap(), json!("a"));
    assert!(!data(&fixture, "a").join("task-started").exists());
    assert_eq!(starts(&fixture, "a"), 1);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_disabling_during_handshake_stops_the_candidate_and_invalidates_waiters() {
    let fixture = Fixture::cold();
    pause_before_hello(&fixture, "a");
    let manager = fixture.manager.clone();
    let request = thread::spawn(move || health(&manager, "a", "starting"));
    fixture.wait_until(|| starts(&fixture, "a") == 1);
    fixture.manager.set_enabled("a", false).unwrap();
    assert!(matches!(
        request.join().unwrap().unwrap_err().code.as_str(),
        "CANCELLED" | "DISABLED"
    ));
    fixture.wait_until(|| fixture.snapshot("a").runtime == RuntimeState::Stopped);
    assert!(!fixture.snapshot("a").enabled);
    assert!(
        fixture.manager.inner.state.lock().unwrap().plugins["a"]
            .process
            .is_none()
    );
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_failed_start_requires_explicit_retry_and_retains_enable_intent() {
    let fixture = Fixture::cold();
    let binary = fixture
        .root
        .join("installed-plugins/a/0.0.0/backend/fixture.exe");
    let original = fs::read(&binary).unwrap();
    fs::remove_file(&binary).unwrap();
    assert_eq!(
        health(&fixture.manager, "a", "fail").unwrap_err().code,
        "PLUGIN_START_FAILED"
    );
    assert!(fixture.snapshot("a").enabled);
    fs::write(&binary, original).unwrap();
    assert_eq!(
        health(&fixture.manager, "a", "no-retry").unwrap_err().code,
        "PLUGIN_START_FAILED"
    );
    assert_eq!(starts(&fixture, "a"), 0);
    fixture.manager.retry_runtime("a").unwrap();
    assert_eq!(
        health(&fixture.manager, "a", "retried").unwrap(),
        json!("a")
    );
    assert_eq!(starts(&fixture, "a"), 1);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_detached_work_blocks_replacement_and_upgrade_until_release() {
    let fixture = Fixture::cold();
    fixture
        .manager
        .call("a", "task", "start_task", json!({"milliseconds":400}))
        .unwrap();
    assert_eq!(
        fixture.snapshot("a").lifecycle.unwrap().active_activities,
        1
    );
    assert_eq!(
        fixture.manager.inner.state.lock().unwrap().plugins["a"]
            .process
            .as_ref()
            .unwrap()
            .active_request_count(),
        0
    );
    assert_eq!(
        fixture.manager.begin_upgrade().err().unwrap().code,
        "UPGRADE_BUSY"
    );
    assert!(
        fixture
            .manager
            .upgrade_blockers()
            .iter()
            .any(|message| message.contains("fixture detached task"))
    );
    assert_eq!(
        fixture.manager.stop_for_reinstall("a").unwrap_err().code,
        "PLUGIN_BUSY"
    );
    fixture.wait_until(|| {
        data(&fixture, "a").join("task-finished").exists()
            && fixture.snapshot("a").lifecycle.unwrap().active_activities == 0
    });
    let upgrade = fixture.manager.begin_upgrade().unwrap();
    assert_eq!(
        health(&fixture.manager, "a", "frozen").unwrap_err().code,
        "UPGRADE_IN_PROGRESS"
    );
    drop(upgrade);
    health(&fixture.manager, "a", "unfrozen").unwrap();
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_upgrade_freeze_rejects_a_deferred_background_admission_before_work() {
    let fixture = Fixture::cold();
    fixture
        .manager
        .call("a", "defer", "defer_task", json!({}))
        .unwrap();
    let upgrade = fixture.manager.begin_upgrade().unwrap();
    fs::write(data(&fixture, "a").join("admit-deferred"), b"admit").unwrap();
    fixture.wait_until(|| data(&fixture, "a").join("deferred-denied").exists());
    assert_eq!(
        fs::read_to_string(data(&fixture, "a").join("deferred-denied")).unwrap(),
        "UPGRADE_IN_PROGRESS"
    );
    assert!(!data(&fixture, "a").join("deferred-started").exists());
    drop(upgrade);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_unknown_background_is_reconciled_once_and_required_jobs_survive_restart() {
    let mut fixture = Fixture::cold();
    fixture.policy("a", Some("background_restore"));
    fixture.manager.start_enabled();
    fixture.wait_until(|| fixture.manager.inner.lifecycle.restore("a") == Restore::NotRequired);
    fixture.restart();
    fixture.manager.start_enabled();
    assert_eq!(fixture.snapshot("a").runtime, RuntimeState::Stopped);
    assert_eq!(starts(&fixture, "a"), 1);
    fixture
        .manager
        .call(
            "a",
            "enable-background",
            "restore",
            json!({"required":true}),
        )
        .unwrap();
    fixture.restart();
    fixture.manager.start_enabled();
    fixture.wait_until(|| fixture.snapshot("a").runtime == RuntimeState::Running);
    assert_eq!(
        fixture.manager.inner.lifecycle.restore("a"),
        Restore::Required
    );
    assert_eq!(starts(&fixture, "a"), 3);
    assert_eq!(starts(&fixture, "b"), 0);
    fixture.manager.set_enabled("a", false).unwrap();
    fixture.restart();
    fixture.manager.start_enabled();
    assert!(!fixture.snapshot("a").enabled);
    assert_eq!(starts(&fixture, "a"), 3);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_legacy_fallback_does_not_prestart_its_unused_declared_dependencies() {
    let fixture = Fixture::cold();
    fixture.policy("a", None);
    fixture.manager.start_enabled();
    fixture.wait_until(|| fixture.snapshot("a").runtime == RuntimeState::Running);
    assert_eq!(
        fixture.snapshot("a").lifecycle.unwrap().policy_source,
        "legacy"
    );
    assert_eq!(starts(&fixture, "b"), 0);
    assert_eq!(starts(&fixture, "c"), 0);
}

#[test]
#[ignore = "Requires the actual direct-plugin fixture"]
fn lifecycle_uncommitted_and_stale_instance_reports_cannot_clear_old_restore_intent() {
    let fixture = Fixture::cold();
    fixture
        .manager
        .call("a", "enable", "restore", json!({"required":true}))
        .unwrap();
    let pending_install = fixture.manager.inner.lifecycle.begin_install("a");
    let manager = fixture.manager.clone();
    let request = thread::spawn(move || {
        manager.call("a", "staged-report", "restore", json!({"required":false}))
    });
    fixture.wait_until(|| {
        fs::read_to_string(data(&fixture, "a").join("restore-state")).unwrap() == "false"
    });
    assert_eq!(
        fixture.manager.inner.lifecycle.restore("a"),
        Restore::Required
    );
    fixture.manager.set_enabled("a", false).unwrap();
    assert!(request.join().unwrap().is_err());
    drop(pending_install);
    assert_eq!(
        fixture.manager.inner.lifecycle.restore("a"),
        Restore::Required
    );
}
