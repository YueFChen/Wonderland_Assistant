//! Generic plugin host commands and lifecycle state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use wonderland_kernel::AppContext;
use wonderland_kernel::logging::{error, info, trace, warn};
use wonderland_plugin_protocol::{
    HostCompatibility, InstallationState, PluginBackend, PluginError, PluginFailure,
    PluginInstallSource, PluginManifest, PluginPlatform, PluginRuntimeState, PluginScanDiagnostic,
    PluginUi, PluginUiContribution, PluginUiContributionKind, ProtocolCompatibility, RuntimeState,
    UI_BRIDGE_SUPPORTED_VERSIONS,
};

use crate::host_environment::HostEnvironment;
use crate::host_services::RuntimeServices;
use crate::plugin_distribution::{
    CatalogInstallMode, CatalogInstallRequest, DistributionConfigureRequest,
    DistributionSettingsView, PluginCatalogEntry, PluginCatalogSnapshot, PluginDistributionService,
};
use crate::{plugin_package, plugin_runtime::PluginProcess};

#[path = "plugin_cli.rs"]
mod cli;
pub(crate) use cli::run_cli_command_with_invocation;
pub use cli::{run_cli_command, validate_cli_ui_command};
#[cfg(all(test, windows))]
#[path = "plugin_process_tests.rs"]
pub(crate) mod direct_process_tests;
#[cfg(test)]
#[path = "plugin_manager_tests.rs"]
mod source_tests;

#[path = "plugin_activation.rs"]
mod activation;
#[path = "plugin_lifecycle.rs"]
mod lifecycle;
#[cfg(test)]
#[path = "plugin_lifecycle_tests.rs"]
mod lifecycle_tests;
#[path = "plugin_local_install.rs"]
pub(crate) mod local_install;
#[path = "plugin_retained_data.rs"]
pub(crate) mod retained_data;

const MANAGER_CONFIG_FILE: &str = "plugin-manager.json";
const LEGACY_CONFIG_FILE: &str = "plugins.json";
const PLUGIN_SCAN_RETRY_DELAYS_SECS: [u64; 6] = [1, 2, 4, 8, 12, 20];
const PLUGIN_SCAN_RETRY_LIMIT: u8 = PLUGIN_SCAN_RETRY_DELAYS_SECS.len() as u8 + 1;
const PLUGIN_SCAN_RETRY_WINDOW: Duration = Duration::from_secs(60);
static NEXT_PLUGIN_SCAN_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_PLUGIN_INSTALL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUiLaunchInfo {
    pub(crate) url: String,
    bridge_versions: Vec<String>,
}

#[derive(Clone)]
pub struct PluginManager {
    inner: Arc<PluginManagerInner>,
}

#[derive(Clone, Default)]
pub(crate) struct WeakPluginManager {
    inner: Weak<PluginManagerInner>,
}

impl WeakPluginManager {
    pub(crate) fn upgrade(&self) -> Option<PluginManager> {
        self.inner.upgrade().map(|inner| PluginManager { inner })
    }
}

struct PluginChangeNotification(Arc<dyn HostEnvironment>);

impl Drop for PluginChangeNotification {
    fn drop(&mut self) {
        self.0.emit("plugins:changed", Value::Null);
    }
}

#[derive(Default)]
struct PluginAdmission {
    frozen: bool,
    active: usize,
}

pub(crate) struct PluginWorkGuard(Arc<Mutex<PluginAdmission>>);
impl Drop for PluginWorkGuard {
    fn drop(&mut self) {
        if let Ok(mut admission) = self.0.lock() {
            admission.active = admission.active.saturating_sub(1);
        }
    }
}

pub(crate) struct PluginUpgradeGuard(Arc<Mutex<PluginAdmission>>);
impl Drop for PluginUpgradeGuard {
    fn drop(&mut self) {
        if let Ok(mut admission) = self.0.lock() {
            admission.frozen = false;
        }
    }
}

struct PluginManagerInner {
    management: Mutex<()>,
    catalog_installs: Mutex<BTreeMap<String, CatalogInstallControl>>,
    admission: Arc<Mutex<PluginAdmission>>,
    distribution: PluginDistributionService,
    app_data_dir: PathBuf,
    documents_dir: PathBuf,
    installed_root: PathBuf,
    environment: Arc<dyn HostEnvironment>,
    runtime: tokio::runtime::Handle,
    next_service_request_id: AtomicU64,
    next_scan_retry_id: AtomicU64,
    next_instance_id: AtomicU64,
    activation: Arc<activation::ActivationCoordinator>,
    lifecycle: Arc<lifecycle::LifecycleStore>,
    auto_start_requested: AtomicBool,
    desktop_host: bool,
    shutting_down: AtomicBool,
    state: Mutex<PluginManagerState>,
}

struct CatalogInstallControl {
    plugin_id: String,
    cancel: tokio::sync::watch::Sender<bool>,
    committing: bool,
}

struct CatalogInstallGuard {
    inner: Arc<PluginManagerInner>,
    id: String,
}
impl Drop for CatalogInstallGuard {
    fn drop(&mut self) {
        if let Ok(mut jobs) = self.inner.catalog_installs.lock() {
            jobs.remove(&self.id);
        }
    }
}

struct PluginManagerState {
    preferences: Preferences,
    config_error: Option<String>,
    plugins: BTreeMap<String, PluginRecord>,
}

struct PluginRecord {
    snapshot: PluginRuntimeState,
    runtime_dependency_warnings: Vec<String>,
    directory: PathBuf,
    contract: Value,
    contract_sha256: String,
    process: Option<Arc<PluginProcess>>,
    revision: u64,
    scan_retry: Option<PluginScanRetry>,
}

struct PluginScanRetry {
    attempts: u8,
    started_at: Instant,
    worker_id: u64,
    worker_running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preferences {
    schema_version: u32,
    #[serde(default)]
    enabled_plugins: BTreeMap<String, bool>,
    #[serde(default)]
    active_versions: BTreeMap<String, String>,
    #[serde(default)]
    installation_sources: BTreeMap<String, PluginInstallSource>,
    /// Preserve malformed identity data for diagnosis without disabling existing plugins.
    #[serde(default = "empty_publisher_identities")]
    publisher_identities: Value,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 4,
            enabled_plugins: BTreeMap::new(),
            active_versions: BTreeMap::new(),
            installation_sources: BTreeMap::new(),
            publisher_identities: empty_publisher_identities(),
        }
    }
}

#[derive(Deserialize)]
struct LegacyPreferences {
    #[serde(default)]
    disabled_plugins: Vec<String>,
}

impl PluginManager {
    pub(crate) fn load(
        context: &AppContext,
        environment: Arc<dyn HostEnvironment>,
        runtime: tokio::runtime::Handle,
        desktop_host: bool,
    ) -> Result<Self, PluginError> {
        let app_data_dir = context.app_data_dir.clone();
        let documents_dir = context.documents_dir.clone();
        let installed_root = app_data_dir.join("installed-plugins");
        fs::create_dir_all(&installed_root).map_err(|error| {
            error!(phase = "create_install_root", reason = %error, "插件管理器初始化失败");
            plugin_error(
                "INTERNAL",
                &format!("Cannot create plugin installation directory: {error}"),
            )
        })?;
        for (plugin_id, error) in crate::plugin_install_recovery::recover_pending(
            &installed_root,
            &app_data_dir.join(MANAGER_CONFIG_FILE),
        )
        .map_err(|message| plugin_error("INSTALL_RECOVERY_REQUIRED", &message))?
        {
            warn!(%plugin_id, reason = %error, "Interrupted plugin installation was retained for explicit recovery");
        }
        let preferences = load_preferences(&app_data_dir)
            .map_err(|message| plugin_error("INVALID_CONFIG", &message))?;
        let config_error: Option<String> = None;
        if let Some(config_error) = &config_error {
            warn!(phase = "load_preferences", reason = %config_error, "插件设置加载异常，将使用恢复后的设置");
        }
        info!(
            core_version = env!("CARGO_PKG_VERSION"),
            configured_plugin_count = preferences.active_versions.len(),
            "插件管理器初始化"
        );
        let manager = Self {
            inner: Arc::new(PluginManagerInner {
                management: Mutex::new(()),
                catalog_installs: Mutex::new(BTreeMap::new()),
                admission: Arc::new(Mutex::new(PluginAdmission::default())),
                distribution: PluginDistributionService::load(&app_data_dir),
                lifecycle: Arc::new(lifecycle::LifecycleStore::load(&app_data_dir)),
                activation: Arc::new(activation::ActivationCoordinator::default()),
                app_data_dir,
                documents_dir,
                installed_root,
                environment,
                runtime,
                next_service_request_id: AtomicU64::new(1),
                next_scan_retry_id: AtomicU64::new(1),
                next_instance_id: AtomicU64::new(1),
                auto_start_requested: AtomicBool::new(false),
                desktop_host,
                shutting_down: AtomicBool::new(false),
                state: Mutex::new(PluginManagerState {
                    preferences,
                    config_error,
                    plugins: BTreeMap::new(),
                }),
            }),
        };
        manager.refresh()?;
        Ok(manager)
    }

    /// Only roots with an explicit restoration need (or legacy policy) start with desktop Core.
    pub fn start_enabled(&self) {
        if !self.inner.desktop_host {
            return;
        }
        self.inner
            .auto_start_requested
            .store(true, Ordering::Release);
        let roots: Vec<_> = self
            .snapshots()
            .into_iter()
            .filter(|s| self.inner.lifecycle.should_restore(s))
            .collect();
        for id in dependency_start_order(&roots).0 {
            self.schedule_restore(&id, "background_restore");
        }
    }

    /// Explicitly stop children even when a delayed retry or request still holds a manager clone.
    pub(crate) fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::Release);
        self.inner.activation.close();
        let processes = match self.inner.state.lock() {
            Ok(mut state) => state
                .plugins
                .values_mut()
                .filter_map(|record| {
                    record.revision = record.revision.wrapping_add(1);
                    record.snapshot.runtime = RuntimeState::Stopped;
                    if let Some(retry) = record.scan_retry.as_mut() {
                        retry.worker_running = false;
                    }
                    if let Some(diagnostic) = record.snapshot.scan_diagnostic.as_mut() {
                        diagnostic.retrying = false;
                    }
                    record.process.take()
                })
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        for process in processes {
            if let Err(error) = process.stop() {
                error!(code = %error.code, reason = %error.message, "停止插件宿主失败");
            }
        }
    }

    fn start_target(&self, plugin_id: &str) -> Result<(), PluginError> {
        self.inner.start_one(plugin_id)
    }

    fn schedule_restore(&self, id: &str, reason: &str) {
        let manager = self.clone();
        let id = id.to_owned();
        let reason = reason.to_owned();
        self.inner.runtime.spawn_blocking(move || {
            if let Err(error) = manager.ensure_active(&id, &reason, &crate::host_services::HostInvocation::default(), false) {
                warn!(plugin_id = %id, code = %error.code, reason = %error.message, "插件后台恢复启动失败");
            }
        });
    }

    #[cfg(debug_assertions)]
    fn inject_backend_exit_for_test(
        &self,
        plugin_id: &str,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let process = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?
            .plugins
            .get(plugin_id)
            .and_then(|record| record.process.clone())
            .ok_or_else(|| plugin_error("NOT_RUNNING", "Plugin backend is not running."))?;
        process.terminate_for_test()?;

        let deadline = Instant::now() + Duration::from_secs(2);
        while process.is_alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        let snapshots = self.snapshots();
        if !snapshots.iter().any(|snapshot| {
            snapshot.manifest.id == plugin_id
                && snapshot.runtime == RuntimeState::Failed
                && snapshot
                    .last_error
                    .as_ref()
                    .is_some_and(|failure| failure.code == "PLUGIN_CRASHED")
        }) {
            return Err(plugin_error(
                "PLUGIN_CRASH_NOT_DETECTED",
                "The plugin exit was not reflected as PLUGIN_CRASHED.",
            ));
        }
        Ok(snapshots)
    }

    fn refresh(&self) -> Result<(), PluginError> {
        self.refresh_with_install(None)
    }

    fn refresh_with_install(
        &self,
        verified_install: Option<&plugin_package::InspectedPlugin>,
    ) -> Result<(), PluginError> {
        let _work = self.inner.begin_work()?;
        let _notification = PluginChangeNotification(self.inner.environment.clone());
        let scan_id = NEXT_PLUGIN_SCAN_ID.fetch_add(1, Ordering::Relaxed);
        let active_versions = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?
            .preferences
            .active_versions
            .clone();
        let scan_started = Instant::now();
        info!(
            scan_id,
            active_plugin_count = active_versions.len(),
            phase = "installed_root",
            "开始扫描已安装插件"
        );
        let scanned =
            plugin_package::scan_installed_root(&self.inner.installed_root, &active_versions);
        let scanned_count = scanned.len();
        let mut valid_count = 0_usize;
        let mut invalid_count = 0_usize;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let old = std::mem::take(&mut state.plugins);
        let mut next = BTreeMap::new();
        let mut preferences_changed = false;
        for item in scanned {
            // The owner of a live transaction already verified these exact bytes.
            // Ordinary scans must still isolate an unfinished journal after a crash.
            let item = match (item, verified_install) {
                (Err(error), Some(plugin))
                    if error.code == "INSTALL_RECOVERY_REQUIRED"
                        && error.plugin_id == plugin.manifest.id =>
                {
                    Ok(plugin.clone())
                }
                (item, _) => item,
            };
            match item {
                Ok(plugin) => {
                    valid_count += 1;
                    let id = plugin.manifest.id.clone();
                    let version = plugin.manifest.version.clone();
                    info!(
                        scan_id,
                        plugin_id = %id,
                        version = %version,
                        compatible = plugin.compatible,
                        phase = "package_validation",
                        "插件包扫描成功"
                    );
                    let previous_record = old.get(&id);
                    let (record, changed) = scanned_plugin_record(
                        &self.inner.app_data_dir,
                        &mut state,
                        plugin,
                        previous_record,
                    );
                    preferences_changed |= changed;
                    next.insert(id, record);
                }
                Err(scan_error) => {
                    invalid_count += 1;
                    let id = safe_display_id(&scan_error.plugin_id, next.len());
                    let message =
                        sanitize_scan_message(&self.inner.app_data_dir, &scan_error.message);
                    let diagnostic_retryable =
                        scan_error.retryable && valid_plugin_id(&scan_error.plugin_id);
                    let can_retry = diagnostic_retryable;
                    let installation = if can_retry {
                        InstallationState::Checking
                    } else {
                        InstallationState::Invalid
                    };
                    let diagnostic_code = if can_retry {
                        scan_error.code
                    } else if scan_error.retryable {
                        "PLUGIN_SCAN_RETRY_UNAVAILABLE"
                    } else {
                        scan_error.code
                    };
                    let mut snapshot = invalid_snapshot(&id, &message);
                    snapshot.installation = installation;
                    snapshot.last_error = Some(failure(diagnostic_code, &message));
                    snapshot.scan_diagnostic = Some(PluginScanDiagnostic {
                        core_version: env!("CARGO_PKG_VERSION").to_owned(),
                        code: diagnostic_code.to_owned(),
                        phase: scan_error.phase.to_owned(),
                        message: message.clone(),
                        retry_attempt: u8::from(can_retry),
                        retry_limit: PLUGIN_SCAN_RETRY_LIMIT,
                        retrying: can_retry,
                        retryable: diagnostic_retryable,
                    });
                    warn!(
                        scan_id,
                        plugin_id = %id,
                        code = diagnostic_code,
                        phase = scan_error.phase,
                        retryable = scan_error.retryable,
                        attempt = u8::from(can_retry),
                        error = %message,
                        "插件包扫描失败"
                    );
                    next.insert(
                        id,
                        PluginRecord {
                            snapshot,
                            runtime_dependency_warnings: Vec::new(),
                            directory: PathBuf::new(),
                            contract: Value::Null,
                            contract_sha256: String::new(),
                            process: None,
                            revision: 0,
                            scan_retry: can_retry.then(|| PluginScanRetry {
                                attempts: 1,
                                started_at: Instant::now(),
                                worker_id: 0,
                                worker_running: false,
                            }),
                        },
                    );
                }
            }
        }
        state.plugins = next;
        update_dependency_issues(&mut state.plugins);
        if preferences_changed {
            if let Err(error) = persist_preferences(&self.inner.app_data_dir, &state.preferences) {
                error!(phase = "persist_preferences", reason = %error, "保存插件设置失败");
                return Err(plugin_error(
                    "INTERNAL",
                    &format!("Cannot save plugin state: {error}"),
                ));
            }
            state.config_error = None;
        }
        let retry_ids = state
            .plugins
            .iter()
            .filter(|(_, record)| record.scan_retry.is_some())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let retry_count = retry_ids.len();
        drop(state);
        info!(
            scan_id,
            scanned = scanned_count,
            valid = valid_count,
            invalid = invalid_count,
            retrying = retry_count,
            elapsed_ms = scan_started.elapsed().as_millis() as u64,
            phase = "installed_root",
            "已安装插件扫描完成"
        );
        for id in retry_ids {
            self.schedule_plugin_scan_retry(&id, false);
        }
        Ok(())
    }

    pub(crate) fn snapshots(&self) -> Vec<PluginRuntimeState> {
        let mut snapshots: Vec<PluginRuntimeState> = self
            .inner
            .state
            .lock()
            .map(|state| {
                state
                    .plugins
                    .values()
                    .map(|record| record.snapshot.clone())
                    .collect()
            })
            .unwrap_or_default();
        for snapshot in &mut snapshots {
            snapshot.lifecycle = Some(self.inner.lifecycle.view(&snapshot.manifest));
        }
        snapshots
    }

    pub(crate) fn process_exited(
        &self,
        plugin_id: &str,
        process: &Arc<PluginProcess>,
        error: PluginError,
    ) {
        let retired = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let Some(record) = state.plugins.get_mut(plugin_id) else {
                return;
            };
            if record
                .process
                .as_ref()
                .is_none_or(|active| !Arc::ptr_eq(active, process))
            {
                return;
            }
            if record.snapshot.runtime == RuntimeState::Starting {
                // The activation owner records the handshake result.
            } else if matches!(
                record.snapshot.runtime,
                RuntimeState::Stopping | RuntimeState::Stopped
            ) {
                record.snapshot.runtime = RuntimeState::Stopped;
            } else {
                record.snapshot.runtime = RuntimeState::Failed;
                record.snapshot.last_error = Some(failure(&error.code, &error.message));
            }
            let retired = record.process.take();
            update_dependency_issues(&mut state.plugins);
            retired
        };
        drop(retired);
        self.emit_plugin_states_changed();
    }

    fn schedule_plugin_scan_retry(&self, plugin_id: &str, immediate: bool) {
        let worker_id = self
            .inner
            .next_scan_retry_id
            .fetch_add(1, Ordering::Relaxed);
        let should_spawn = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let Some(record) = state.plugins.get_mut(plugin_id) else {
                return;
            };
            let Some(retry) = record.scan_retry.as_mut() else {
                return;
            };
            if retry.worker_running {
                return;
            }
            retry.worker_running = true;
            retry.worker_id = worker_id;
            true
        };
        if !should_spawn {
            return;
        }
        info!(
            plugin_id = %plugin_id,
            worker_id,
            first_attempt = immediate,
            attempt_limit = PLUGIN_SCAN_RETRY_LIMIT,
            retry_window_seconds = PLUGIN_SCAN_RETRY_WINDOW.as_secs(),
            "已调度插件包扫描重试"
        );

        let manager = self.clone();
        let worker_plugin_id = plugin_id.to_owned();
        let spawn = thread::Builder::new()
            .name(format!("plugin-{worker_plugin_id}-scan-retry"))
            .spawn(move || {
                manager.run_plugin_scan_retry_worker(&worker_plugin_id, immediate, worker_id)
            });
        if let Err(error) = spawn {
            self.finish_scan_retry_worker_error(plugin_id, worker_id, &error.to_string());
        }
    }

    fn run_plugin_scan_retry_worker(&self, plugin_id: &str, immediate: bool, worker_id: u64) {
        let mut immediate_attempt = immediate;
        loop {
            let delay = match self.inner.state.lock() {
                Ok(state) => {
                    let Some(record) = state.plugins.get(plugin_id) else {
                        return;
                    };
                    let Some(retry) = record.scan_retry.as_ref() else {
                        return;
                    };
                    if !retry.worker_running || retry.worker_id != worker_id {
                        return;
                    }
                    let elapsed = retry.started_at.elapsed();
                    if retry.attempts >= PLUGIN_SCAN_RETRY_LIMIT
                        || elapsed >= PLUGIN_SCAN_RETRY_WINDOW
                    {
                        None
                    } else if immediate_attempt || retry.attempts == 0 {
                        Some(Duration::ZERO)
                    } else {
                        Some(Duration::from_secs(
                            PLUGIN_SCAN_RETRY_DELAYS_SECS[(retry.attempts - 1) as usize],
                        ))
                    }
                }
                Err(_) => return,
            };
            let Some(delay) = delay else {
                let Ok(_work) = self.inner.begin_work() else {
                    self.pause_scan_retry_worker(plugin_id, worker_id);
                    return;
                };
                self.finish_scan_retry_exhausted(plugin_id, worker_id);
                return;
            };

            if !delay.is_zero() {
                thread::sleep(delay);
            }

            let Ok(_work) = self.inner.begin_work() else {
                self.pause_scan_retry_worker(plugin_id, worker_id);
                return;
            };

            let attempt_context = match self.inner.state.lock() {
                Ok(mut state) => {
                    let active_version = state.preferences.active_versions.get(plugin_id).cloned();
                    let Some(record) = state.plugins.get_mut(plugin_id) else {
                        return;
                    };
                    let Some(retry) = record.scan_retry.as_mut() else {
                        return;
                    };
                    if !retry.worker_running || retry.worker_id != worker_id {
                        return;
                    }
                    if retry.started_at.elapsed() >= PLUGIN_SCAN_RETRY_WINDOW
                        || retry.attempts >= PLUGIN_SCAN_RETRY_LIMIT
                    {
                        None
                    } else {
                        retry.attempts = retry.attempts.saturating_add(1);
                        let attempt = retry.attempts;
                        if let Some(diagnostic) = record.snapshot.scan_diagnostic.as_mut() {
                            diagnostic.retry_attempt = attempt;
                            diagnostic.retrying = true;
                        }
                        Some((
                            attempt,
                            active_version,
                            retry.started_at.elapsed().as_millis() as u64,
                        ))
                    }
                }
                Err(_) => return,
            };
            let Some((attempt, active_version, retry_elapsed_ms)) = attempt_context else {
                self.finish_scan_retry_exhausted(plugin_id, worker_id);
                return;
            };
            immediate_attempt = false;
            self.emit_plugin_states_changed();

            info!(
                plugin_id = %plugin_id,
                worker_id,
                attempt,
                attempt_limit = PLUGIN_SCAN_RETRY_LIMIT,
                retry_elapsed_ms,
                phase = "package_validation",
                "开始插件包扫描重试"
            );
            let scan_started = Instant::now();
            let result = plugin_package::scan_installed_plugin(
                &self.inner.installed_root,
                plugin_id,
                active_version.as_deref(),
            );
            let scan_elapsed_ms = scan_started.elapsed().as_millis() as u64;
            match result {
                Ok(plugin) => {
                    self.accept_scanned_plugin(plugin, attempt, worker_id, scan_elapsed_ms);
                    return;
                }
                Err(scan_error) => {
                    let keep_retrying = self.record_scan_retry_failure(
                        scan_error,
                        attempt,
                        worker_id,
                        scan_elapsed_ms,
                    );
                    if !keep_retrying {
                        return;
                    }
                }
            }
        }
    }

    fn pause_scan_retry_worker(&self, plugin_id: &str, worker_id: u64) {
        if let Ok(mut state) = self.inner.state.lock()
            && let Some(record) = state.plugins.get_mut(plugin_id)
            && record
                .scan_retry
                .as_ref()
                .is_some_and(|retry| retry.worker_id == worker_id)
        {
            if let Some(retry) = record.scan_retry.as_mut() {
                retry.worker_running = false;
            }
            if let Some(diagnostic) = record.snapshot.scan_diagnostic.as_mut() {
                diagnostic.retrying = false;
            }
        }
        self.emit_plugin_states_changed();
    }

    fn record_scan_retry_failure(
        &self,
        scan_error: plugin_package::ScanFailure,
        attempt: u8,
        worker_id: u64,
        scan_elapsed_ms: u64,
    ) -> bool {
        let plugin_id = safe_display_id(&scan_error.plugin_id, 0);
        let message = sanitize_scan_message(&self.inner.app_data_dir, &scan_error.message);
        let mut keep_retrying = false;
        let mut applied = false;
        if let Ok(mut state) = self.inner.state.lock()
            && let Some(record) = state.plugins.get_mut(&plugin_id)
            && record
                .scan_retry
                .as_ref()
                .is_some_and(|retry| retry.worker_running && retry.worker_id == worker_id)
        {
            applied = true;
            let exhausted = record.scan_retry.as_ref().is_some_and(|retry| {
                retry.attempts >= PLUGIN_SCAN_RETRY_LIMIT
                    || retry.started_at.elapsed() >= PLUGIN_SCAN_RETRY_WINDOW
            });
            let attempts = record
                .scan_retry
                .as_ref()
                .map(|retry| retry.attempts)
                .unwrap_or(attempt);
            keep_retrying = scan_error.retryable && !exhausted;
            let code = if exhausted {
                "PLUGIN_SCAN_RETRY_EXHAUSTED"
            } else {
                scan_error.code
            };
            let detail = if exhausted {
                if attempts >= PLUGIN_SCAN_RETRY_LIMIT {
                    format!(
                        "Automatic verification stopped after {attempts} attempts. Last error: {message}"
                    )
                } else {
                    format!(
                        "Automatic verification stopped after 60 seconds and {attempts} attempts. Last error: {message}"
                    )
                }
            } else {
                message.clone()
            };
            let core_version = record
                .snapshot
                .scan_diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.core_version.clone())
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
            record.snapshot.installation = if scan_error.retryable {
                InstallationState::Checking
            } else {
                InstallationState::Invalid
            };
            record.snapshot.enabled = false;
            record.snapshot.last_error = Some(failure(code, &detail));
            record.snapshot.scan_diagnostic = Some(PluginScanDiagnostic {
                core_version,
                code: code.to_owned(),
                phase: scan_error.phase.to_owned(),
                message: detail.clone(),
                retry_attempt: attempt,
                retry_limit: PLUGIN_SCAN_RETRY_LIMIT,
                retrying: keep_retrying,
                retryable: scan_error.retryable,
            });
            if !keep_retrying && let Some(retry) = record.scan_retry.as_mut() {
                retry.worker_running = false;
            }
        }
        if !applied {
            return false;
        }
        warn!(
            plugin_id = %plugin_id,
            code = scan_error.code,
            phase = scan_error.phase,
            retryable = scan_error.retryable,
            attempt,
            worker_id,
            elapsed_ms = scan_elapsed_ms,
            error = %message,
            "插件包扫描重试失败"
        );
        self.emit_plugin_states_changed();
        keep_retrying
    }

    fn finish_scan_retry_exhausted(&self, plugin_id: &str, worker_id: u64) {
        let mut applied = false;
        let mut attempts = 0;
        if let Ok(mut state) = self.inner.state.lock()
            && let Some(record) = state.plugins.get_mut(plugin_id)
            && record
                .scan_retry
                .as_ref()
                .is_some_and(|retry| retry.worker_id == worker_id)
        {
            applied = true;
            attempts = record
                .scan_retry
                .as_ref()
                .map(|retry| retry.attempts)
                .unwrap_or_default();
            if let Some(retry) = record.scan_retry.as_mut() {
                retry.worker_running = false;
            }
            record.snapshot.installation = InstallationState::Checking;
            record.snapshot.enabled = false;
            if let Some(diagnostic) = record.snapshot.scan_diagnostic.as_mut() {
                diagnostic.code = "PLUGIN_SCAN_RETRY_EXHAUSTED".to_owned();
                diagnostic.message = if attempts >= PLUGIN_SCAN_RETRY_LIMIT {
                    format!(
                        "Automatic verification stopped after {attempts} attempts. Last error: {}",
                        diagnostic.message
                    )
                } else {
                    format!(
                        "Automatic verification stopped after 60 seconds and {attempts} attempts. Last error: {}",
                        diagnostic.message
                    )
                };
                diagnostic.retry_attempt = attempts;
                diagnostic.retrying = false;
                diagnostic.retryable = true;
                record.snapshot.last_error =
                    Some(failure("PLUGIN_SCAN_RETRY_EXHAUSTED", &diagnostic.message));
            }
        }
        if !applied {
            return;
        }
        warn!(
            plugin_id = %plugin_id,
            attempts,
            attempt_limit = PLUGIN_SCAN_RETRY_LIMIT,
            worker_id,
            retry_window_seconds = PLUGIN_SCAN_RETRY_WINDOW.as_secs(),
            "插件包自动扫描重试已达到上限"
        );
        self.emit_plugin_states_changed();
    }

    fn finish_scan_retry_worker_error(&self, plugin_id: &str, worker_id: u64, error: &str) {
        let mut applied = false;
        if let Ok(mut state) = self.inner.state.lock()
            && let Some(record) = state.plugins.get_mut(plugin_id)
            && record
                .scan_retry
                .as_ref()
                .is_some_and(|retry| retry.worker_id == worker_id)
        {
            applied = true;
            if let Some(retry) = record.scan_retry.as_mut() {
                retry.worker_running = false;
            }
            record.snapshot.installation = InstallationState::Checking;
            if let Some(diagnostic) = record.snapshot.scan_diagnostic.as_mut() {
                diagnostic.code = "PLUGIN_SCAN_RETRY_WORKER_FAILED".to_owned();
                diagnostic.message = format!("Could not start plugin scan retry worker: {error}");
                diagnostic.retrying = false;
                diagnostic.retryable = true;
                record.snapshot.last_error = Some(failure(
                    "PLUGIN_SCAN_RETRY_WORKER_FAILED",
                    &diagnostic.message,
                ));
            }
        }
        if !applied {
            return;
        }
        warn!(plugin_id = %plugin_id, error = %error, "插件扫描重试线程启动失败");
        self.emit_plugin_states_changed();
    }

    fn accept_scanned_plugin(
        &self,
        plugin: plugin_package::InspectedPlugin,
        attempt: u8,
        worker_id: u64,
        scan_elapsed_ms: u64,
    ) {
        let plugin_id = plugin.manifest.id.clone();
        let mut should_start = false;
        if let Ok(mut state) = self.inner.state.lock() {
            if !state
                .plugins
                .get(&plugin_id)
                .and_then(|record| record.scan_retry.as_ref())
                .is_some_and(|retry| retry.worker_running && retry.worker_id == worker_id)
            {
                return;
            }
            let previous_record = state.plugins.remove(&plugin_id);
            let (record, preferences_changed) = scanned_plugin_record(
                &self.inner.app_data_dir,
                &mut state,
                plugin,
                previous_record.as_ref(),
            );
            should_start = self.inner.lifecycle.should_restore(&record.snapshot);
            let version = record.snapshot.manifest.version.clone();
            state.plugins.insert(plugin_id.clone(), record);
            update_dependency_issues(&mut state.plugins);
            if preferences_changed
                && let Err(error) =
                    persist_preferences(&self.inner.app_data_dir, &state.preferences)
            {
                warn!(plugin_id = %plugin_id, error = %error, "插件扫描恢复后无法保存插件设置");
            }
            info!(
                plugin_id = %plugin_id,
                version = %version,
                attempt,
                worker_id,
                elapsed_ms = scan_elapsed_ms,
                "插件包扫描恢复成功"
            );
        }
        self.emit_plugin_states_changed();
        if should_start && self.inner.auto_start_requested.load(Ordering::Acquire) {
            self.schedule_restore(&plugin_id, "scan_restore");
        }
    }

    fn retry_plugin_scan(&self, plugin_id: &str) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let _management = self.inner.management.try_lock().map_err(|_| {
            plugin_error(
                "PLUGIN_BUSY",
                "Another plugin management change is in progress.",
            )
        })?;
        let _work = self.inner.begin_work()?;
        let _notification = PluginChangeNotification(self.inner.environment.clone());
        if !valid_plugin_id(plugin_id) {
            return Err(plugin_error("INVALID_REQUEST", "Plugin ID is invalid."));
        }
        if self
            .inner
            .installed_root
            .join(plugin_id)
            .join(".install-recovery.json")
            .exists()
        {
            crate::plugin_install_recovery::InstallRecovery::recover_one(
                &self.inner.installed_root,
                &self.inner.app_data_dir.join(MANAGER_CONFIG_FILE),
                plugin_id,
            )
            .map_err(|message| plugin_error("INSTALL_RECOVERY_REQUIRED", &message))?;
            let preferences = load_preferences(&self.inner.app_data_dir)
                .map_err(|message| plugin_error("INVALID_CONFIG", &message))?;
            self.inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?
                .preferences = preferences;
            self.refresh()?;
            if self.snapshots().iter().any(|state| {
                state.manifest.id == plugin_id
                    && self.inner.desktop_host
                    && self.inner.lifecycle.should_restore(state)
            }) {
                self.start_target(plugin_id)?;
            }
            return Ok(self.snapshots());
        }
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let record = state
                .plugins
                .get_mut(plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            if record
                .scan_retry
                .as_ref()
                .is_some_and(|retry| retry.worker_running)
            {
                return Err(plugin_error(
                    "PLUGIN_SCAN_IN_PROGRESS",
                    "Plugin package verification is already running.",
                ));
            }
            if record.snapshot.scan_diagnostic.is_none() {
                return Err(plugin_error(
                    "INVALID_REQUEST",
                    "This plugin has no package scan failure to retry.",
                ));
            }
            if record
                .snapshot
                .scan_diagnostic
                .as_ref()
                .is_some_and(|diagnostic| diagnostic.code == "PLUGIN_SCAN_RETRY_UNAVAILABLE")
            {
                return Err(plugin_error(
                    "INVALID_REQUEST",
                    "The installed plugin root could not be associated with a plugin ID.",
                ));
            }
            record.snapshot.installation = InstallationState::Checking;
            record.snapshot.enabled = false;
            if let Some(diagnostic) = record.snapshot.scan_diagnostic.as_mut() {
                diagnostic.code = "PLUGIN_SCAN_RETRYING".to_owned();
                diagnostic.message = "Manual plugin package verification started.".to_owned();
                diagnostic.retry_attempt = 0;
                diagnostic.retrying = true;
                diagnostic.retryable = true;
            }
            record.snapshot.last_error = Some(failure(
                "PLUGIN_SCAN_RETRYING",
                "Manual plugin package verification started.",
            ));
            record.scan_retry = Some(PluginScanRetry {
                attempts: 0,
                started_at: Instant::now(),
                worker_id: 0,
                worker_running: false,
            });
        }
        info!(plugin_id = %plugin_id, source = "manual", "用户请求重新检查插件包");
        self.schedule_plugin_scan_retry(plugin_id, true);
        Ok(self.snapshots())
    }

    fn emit_plugin_states_changed(&self) {
        self.inner.environment.emit("plugins:changed", Value::Null);
    }

    /// Installs a local package directory passed by a development launch script.
    /// This entry point is unavailable in release builds and uses the same package
    /// validation and atomic replacement path as the developer directory picker.
    #[cfg(debug_assertions)]
    pub fn install_debug_directory(
        &self,
        source: PathBuf,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        if !source.is_dir() {
            warn!(
                source_kind = "local",
                source_type = "development_directory",
                phase = "resolve_source",
                "开发插件目录不存在"
            );
            return Err(plugin_error(
                "INVALID_REQUEST",
                "The configured development plugin directory does not exist.",
            ));
        }
        let manifest = match plugin_package::manifest_for_install_source(&source) {
            Ok(manifest) => manifest,
            Err(message) => {
                warn!(
                    source_kind = "local",
                    source_type = "development_directory",
                    phase = "manifest_inspection",
                    error = %message,
                    "开发插件目录清单读取失败"
                );
                return Err(plugin_error("INVALID_REQUEST", &message));
            }
        };
        self.install(source, true, true, Some(&manifest))?;
        self.set_enabled(&manifest.id, true)
    }

    fn install_cli_package(
        &self,
        source: PathBuf,
        overwrite: bool,
        approve_source_change: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        if source.is_dir() && !cfg!(debug_assertions) {
            warn!(
                source_kind = "local",
                source_type = "development_directory",
                phase = "resolve_source",
                "发布版 CLI 拒绝安装开发目录"
            );
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Release CLI installs require a validated .wplug archive.",
            ));
        }
        let source_type = if source.is_dir() {
            "development_directory"
        } else {
            "archive"
        };
        let manifest = match plugin_package::manifest_for_install_source(&source) {
            Ok(manifest) => manifest,
            Err(message) => {
                warn!(
                    source_kind = "local",
                    source_type,
                    phase = "manifest_inspection",
                    error = %message,
                    "本地插件包清单读取失败"
                );
                return Err(plugin_error("INVALID_REQUEST", &message));
            }
        };
        let origin = source.canonicalize().map_err(|error| {
            warn!(
                plugin_id = %manifest.id,
                version = %manifest.version,
                source_kind = "local",
                source_type,
                phase = "resolve_source",
                reason = %error,
                "无法解析本地插件包路径"
            );
            plugin_error(
                "INVALID_REQUEST",
                &format!("Cannot resolve local package: {error}"),
            )
        })?;
        let install_path = plugin_package::install_path(&self.inner.installed_root, &manifest);
        if install_path.exists() && !overwrite {
            warn!(
                plugin_id = %manifest.id,
                version = %manifest.version,
                phase = "preflight",
                "插件包已安装且未允许覆盖"
            );
            return Err(plugin_error(
                "ALREADY_INSTALLED",
                "This plugin version is already installed. Pass --overwrite to replace it.",
            ));
        }
        self.install_transaction(
            source,
            overwrite,
            Some(&manifest),
            None,
            PluginInstallSource {
                kind: "local".to_owned(),
                origin: origin.to_string_lossy().into_owned(),
                author: None,
            },
            approve_source_change,
        )
    }

    fn install_catalog_package(
        &self,
        source: PathBuf,
        entry: &PluginCatalogEntry,
        approved_source_change: bool,
        mode: CatalogInstallMode,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let _management = self.inner.management.try_lock().map_err(|_| {
            plugin_error(
                "PLUGIN_BUSY",
                "Another plugin management change is in progress.",
            )
        })?;
        crate::plugin_distribution::validate_install_mode(
            mode,
            &entry.version,
            self.snapshots()
                .iter()
                .find(|state| state.manifest.id == entry.id)
                .map(|state| state.manifest.version.as_str()),
        )?;
        let manifest = match plugin_package::manifest_for_install_source(&source) {
            Ok(manifest) => manifest,
            Err(message) => {
                warn!(
                    source_kind = "catalog",
                    source_type = "archive",
                    phase = "manifest_inspection",
                    error = %message,
                    "目录插件包清单读取失败"
                );
                return Err(plugin_error("INVALID_REQUEST", &message));
            }
        };
        let manifest_matches = manifest.id == entry.id
            && manifest.name == entry.name
            && manifest.description.as_deref().unwrap_or_default() == entry.description
            && manifest.author.as_deref() == Some(entry.author.as_str())
            && manifest.version == entry.version
            && manifest.dependencies == entry.dependencies
            && manifest.host_compatibility.min_core_version
                == entry.host_compatibility.min_core_version
            && manifest.host_compatibility.max_core_version_exclusive
                == entry.host_compatibility.max_core_version_exclusive
            && manifest.host_compatibility.protocol.min_version
                == entry.host_compatibility.protocol.min_version
            && manifest.host_compatibility.protocol.max_version_exclusive
                == entry.host_compatibility.protocol.max_version_exclusive
            && manifest.ui.as_ref().map(|ui| {
                (
                    &ui.bridge_compatibility.min_version,
                    &ui.bridge_compatibility.max_version_exclusive,
                )
            }) == entry.ui_bridge_compatibility.as_ref().map(|compatibility| {
                (
                    &compatibility.min_version,
                    &compatibility.max_version_exclusive,
                )
            })
            && manifest.platform.os == entry.platform.os
            && manifest.platform.architecture == entry.platform.architecture
            && manifest.platform.abi == entry.platform.abi;
        if !manifest_matches {
            warn!(
                plugin_id = %entry.id,
                version = %entry.version,
                phase = "catalog_review",
                "下载的插件包元数据与已审核目录不匹配"
            );
            return Err(plugin_error(
                "CATALOG_MISMATCH",
                "The downloaded package metadata does not match the reviewed catalog entry.",
            ));
        }
        if !plugin_package::manifest_is_compatible(&manifest) {
            warn!(
                plugin_id = %manifest.id,
                version = %manifest.version,
                phase = "compatibility_check",
                "目录插件包与当前 Core 不兼容"
            );
            return Err(plugin_error(
                "INCOMPATIBLE_CORE",
                "This plugin version is incompatible with the current Core.",
            ));
        }
        self.install_transaction_locked(
            source,
            true,
            Some(&manifest),
            Some(entry.signing_public_key.as_str()),
            PluginInstallSource {
                kind: "catalog".to_owned(),
                origin: entry.source_url.clone(),
                author: Some(entry.author.clone()),
            },
            approved_source_change,
        )
    }

    #[cfg(debug_assertions)]
    fn install(
        &self,
        source: PathBuf,
        overwrite: bool,
        approved_source_change: bool,
        reviewed_manifest: Option<&PluginManifest>,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let origin = source.canonicalize().map_err(|error| {
            warn!(
                source_kind = "local",
                phase = "resolve_source",
                reason = %error,
                "无法解析本地插件包路径"
            );
            plugin_error(
                "INVALID_REQUEST",
                &format!("Cannot resolve local package: {error}"),
            )
        })?;
        self.install_transaction(
            source,
            overwrite,
            reviewed_manifest,
            None,
            PluginInstallSource {
                kind: "local".to_owned(),
                origin: origin.to_string_lossy().into_owned(),
                author: None,
            },
            approved_source_change,
        )
    }

    fn install_transaction(
        &self,
        source: PathBuf,
        overwrite: bool,
        reviewed_manifest: Option<&PluginManifest>,
        expected_signing_key: Option<&str>,
        install_source: PluginInstallSource,
        approved_source_change: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let _management = self.inner.management.try_lock().map_err(|_| {
            plugin_error(
                "PLUGIN_BUSY",
                "Another plugin management change is in progress; retry after it completes.",
            )
        })?;
        self.install_transaction_locked(
            source,
            overwrite,
            reviewed_manifest,
            expected_signing_key,
            install_source,
            approved_source_change,
        )
    }

    fn install_transaction_locked(
        &self,
        source: PathBuf,
        overwrite: bool,
        reviewed_manifest: Option<&PluginManifest>,
        expected_signing_key: Option<&str>,
        install_source: PluginInstallSource,
        approved_source_change: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let _work = self.inner.begin_work()?;
        let _notification = PluginChangeNotification(self.inner.environment.clone());
        let install_id = NEXT_PLUGIN_INSTALL_ID.fetch_add(1, Ordering::Relaxed);
        let source_kind = install_source.kind.clone();
        let source_type = if source.is_dir() {
            "development_directory"
        } else {
            "archive"
        };
        let install_started = Instant::now();
        let source_manifest = match plugin_package::manifest_for_install_source(&source) {
            Ok(manifest) => manifest,
            Err(message) => {
                warn!(
                    install_id,
                    source_kind = %source_kind,
                    source_type,
                    phase = "manifest_inspection",
                    error = %message,
                    "插件包安装失败"
                );
                return Err(plugin_error("INVALID_REQUEST", &message));
            }
        };
        info!(
            install_id,
            plugin_id = %source_manifest.id,
            version = %source_manifest.version,
            source_kind = %source_kind,
            source_type,
            overwrite,
            phase = "preflight",
            "开始安装插件包"
        );
        if let Some(reviewed) = reviewed_manifest
            && let Err(message) =
                plugin_package::ensure_reviewed_manifest(reviewed, &source_manifest)
        {
            warn!(
                install_id,
                plugin_id = %source_manifest.id,
                version = %source_manifest.version,
                phase = "manifest_review",
                error = %message,
                "插件包安装失败"
            );
            return Err(plugin_error("INVALID_REQUEST", &message));
        }
        let replacing_same_version = overwrite
            && plugin_package::install_path(&self.inner.installed_root, &source_manifest).exists();
        let incoming_key = plugin_package::signature_for_install_source(&source)
            .map_err(|message| package_signature_error(&message))?;
        if let Some(expected) = expected_signing_key
            && incoming_key.as_deref() != Some(expected)
        {
            return Err(plugin_error(
                if incoming_key.is_none() {
                    "PACKAGE_SIGNATURE_REQUIRED"
                } else {
                    "PUBLISHER_IDENTITY_CHANGED"
                },
                "The online package must be signed by its registered publisher.",
            ));
        }
        let (previous_version, previous_source, previous_identity, previous_enabled) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            if state
                .plugins
                .get(&source_manifest.id)
                .is_some_and(|record| record.snapshot.runtime == RuntimeState::Starting)
            {
                return Err(plugin_error(
                    "PLUGIN_BUSY",
                    "Wait for the plugin to finish starting before replacing its package.",
                ));
            }
            let preferences = &state.preferences;
            if let Some(expected) = expected_signing_key
                && publisher_key(preferences, &source_manifest.id)?
                    .as_deref()
                    .is_some_and(|key| key != expected)
            {
                return Err(plugin_error(
                    "PUBLISHER_IDENTITY_CONFLICT",
                    "The catalog publisher differs from this plugin's recorded identity. The existing installation was retained.",
                ));
            }
            (
                preferences
                    .active_versions
                    .get(&source_manifest.id)
                    .cloned(),
                preferences
                    .installation_sources
                    .get(&source_manifest.id)
                    .cloned(),
                preferences
                    .publisher_identities
                    .get(&source_manifest.id)
                    .cloned(),
                preferences
                    .enabled_plugins
                    .get(&source_manifest.id)
                    .copied(),
            )
        };
        let _source_changed = match check_install_source(
            previous_version.as_deref(),
            previous_source.as_ref(),
            &install_source,
            approved_source_change,
        ) {
            Ok(changed) => changed,
            Err(error) => {
                warn!(
                    install_id,
                    plugin_id = %source_manifest.id,
                    version = %source_manifest.version,
                    code = %error.code,
                    phase = "source_review",
                    reason = %error.message,
                    "插件包安装未通过来源检查"
                );
                return Err(error);
            }
        };
        let replacing_active_version = replacing_same_version
            && previous_version.as_deref() == Some(source_manifest.version.as_str());
        if replacing_active_version && !plugin_package::manifest_is_compatible(&source_manifest) {
            warn!(
                install_id,
                plugin_id = %source_manifest.id,
                version = %source_manifest.version,
                phase = "compatibility_check",
                "拒绝用不兼容版本替换当前插件"
            );
            return Err(plugin_error(
                "INCOMPATIBLE_CORE",
                "The selected package cannot replace the active plugin because it is incompatible with this Core.",
            ));
        }
        let _target_pause = self.inner.lifecycle.pause(&source_manifest.id, false)?;
        let was_running = if previous_version.is_some() {
            self.stop_for_reinstall(&source_manifest.id)?
        } else {
            false
        };
        let _lifecycle_install = self.inner.lifecycle.begin_install(&source_manifest.id);
        let inspected_result = if source.is_dir() {
            plugin_package::install_development_directory(
                install_id,
                &source,
                &self.inner.installed_root,
                overwrite,
                &source_manifest,
                Some(&self.inner.app_data_dir.join(MANAGER_CONFIG_FILE)),
            )
        } else {
            plugin_package::install_archive(
                install_id,
                &source,
                &self.inner.installed_root,
                overwrite,
                &source_manifest,
                Some(&self.inner.app_data_dir.join(MANAGER_CONFIG_FILE)),
            )
        };
        let (inspected, mut package_commit) = match inspected_result {
            Ok(inspected) => inspected,
            Err(message) => {
                warn!(
                    install_id,
                    plugin_id = %source_manifest.id,
                    version = %source_manifest.version,
                    source_kind = %source_kind,
                    source_type,
                    overwrite,
                    phase = "extract_validate_commit",
                    elapsed_ms = install_started.elapsed().as_millis() as u64,
                    error = %message,
                    "插件包安装失败"
                );
                if was_running && let Err(restore_error) = self.inner.start_one(&source_manifest.id)
                {
                    error!(
                        install_id,
                        plugin_id = %source_manifest.id,
                        code = %restore_error.code,
                        reason = %restore_error.message,
                        "安装失败后恢复原插件启动失败"
                    );
                }
                return Err(plugin_error("INVALID_REQUEST", &message));
            }
        };
        let plugin_id = inspected.manifest.id.clone();
        let version = inspected.manifest.version.clone();
        if let Some(expected) = expected_signing_key
            && inspected.signature_public_key.as_deref() != Some(expected)
        {
            package_commit
                .rollback()
                .map_err(|message| plugin_error("INSTALL_RECOVERY_REQUIRED", &message))?;
            return Err(plugin_error(
                "PUBLISHER_IDENTITY_CHANGED",
                "Package changed after signature review; the publisher identity was not recorded.",
            ));
        }
        info!(
            install_id,
            plugin_id = %plugin_id,
            version = %version,
            compatible = inspected.compatible,
            phase = "package_validation",
            "插件包解包与校验完成"
        );
        if !inspected.compatible && previous_version.is_some() {
            warn!(
                install_id,
                plugin_id = %plugin_id,
                version = %version,
                phase = "compatibility_check",
                "插件包已安装但与当前 Core 不兼容，未切换活动版本"
            );
            return Err(plugin_error(
                "INCOMPATIBLE_CORE",
                "The incompatible package was rejected; the previous installation was retained.",
            ));
        }
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let mut next = state.preferences.clone();
            if let Some(expected) = expected_signing_key {
                if publisher_key(&next, &plugin_id)?
                    .as_deref()
                    .is_some_and(|key| key != expected)
                {
                    return Err(plugin_error(
                        "PUBLISHER_IDENTITY_CONFLICT",
                        "Publisher identity changed during installation.",
                    ));
                }
                next.publisher_identities
                    .as_object_mut()
                    .ok_or_else(|| {
                        plugin_error(
                            "PUBLISHER_IDENTITY_UNAVAILABLE",
                            "Original publisher records need repair.",
                        )
                    })?
                    .insert(plugin_id.clone(), json!(expected));
            }
            next.active_versions
                .insert(plugin_id.clone(), version.clone());
            next.installation_sources
                .insert(plugin_id.clone(), install_source);
            next.enabled_plugins
                .entry(plugin_id.clone())
                .or_insert(true);
            if let Err(error) = persist_preferences(&self.inner.app_data_dir, &next) {
                error!(
                    install_id,
                    plugin_id = %plugin_id,
                    version = %version,
                    phase = "persist_preferences",
                    reason = %error,
                    "插件包已安装但插件设置保存失败"
                );
                package_commit
                    .rollback()
                    .map_err(|message| plugin_error("INSTALL_RECOVERY_REQUIRED", &message))?;
                drop(state);
                if was_running && let Err(restore_error) = self.inner.start_one(&plugin_id) {
                    warn!(plugin_id = %plugin_id, reason = %restore_error.message, "Previous package was restored but needs a manual restart");
                }
                return Err(plugin_error(
                    "PUBLISHER_IDENTITY_PERSIST_FAILED",
                    &format!("Installation and identity were not committed: {error}"),
                ));
            }
            state.preferences = next;
            state.config_error = None;
        }
        if let Err(error) = self.refresh_with_install(Some(&inspected)) {
            error!(
                install_id,
                plugin_id = %plugin_id,
                version = %version,
                code = %error.code,
                reason = %error.message,
                phase = "post_install_refresh",
                "插件包安装后刷新插件列表失败"
            );
            package_commit
                .rollback()
                .map_err(|message| plugin_error("INSTALL_RECOVERY_REQUIRED", &message))?;
            self.restore_install_preferences(
                &plugin_id,
                previous_version.clone(),
                previous_source.clone(),
                previous_identity.clone(),
                previous_enabled,
            )?;
            return Err(error);
        }
        let should_start = self.snapshots().iter().any(|snapshot| {
            snapshot.manifest.id == plugin_id
                && snapshot.installation == InstallationState::Installed
                && snapshot.enabled
                && (was_running
                    || (self.inner.desktop_host && self.inner.lifecycle.should_restore(snapshot)))
        });
        if expected_signing_key.is_some()
            && let Ok(jobs) = self.inner.catalog_installs.lock()
        {
            for (id, _) in jobs
                .iter()
                .filter(|(_, job)| job.plugin_id == plugin_id && job.committing)
            {
                self.inner.environment.emit("plugins:catalog-install-progress", json!({"operationId":id,"pluginId":plugin_id,"phase":if should_start {"starting"} else {"finishing"},"downloadedBytes":0,"totalBytes":0,"cancellable":false}));
            }
        }
        if should_start
            && let Err(error) = self.inner.start_one(&plugin_id)
            && let Some(previous_version) = previous_version.clone()
        {
            let rollback = (|| {
                package_commit
                    .rollback()
                    .map_err(|message| plugin_error("INSTALL_RECOVERY_REQUIRED", &message))?;
                self.restore_install_preferences(
                    &plugin_id,
                    Some(previous_version),
                    previous_source.clone(),
                    previous_identity.clone(),
                    previous_enabled,
                )?;
                self.refresh()?;
                self.restore_previous_runtime(&plugin_id, was_running)
            })();
            if let Err(rollback_error) = rollback {
                error!(
                    install_id,
                    plugin_id = %plugin_id,
                    version = %version,
                    start_error = %error.message,
                    rollback_error = %rollback_error.message,
                    phase = "rollback",
                    "新插件启动失败且回滚失败"
                );
                return Err(plugin_error(
                    "PLUGIN_START_FAILED",
                    &format!(
                        "New plugin version failed to start ({}); rollback also failed ({}).",
                        error.message, rollback_error.message
                    ),
                ));
            }
            // Source archives remain available; activation restored the previous bytes.
            warn!(
                install_id,
                plugin_id = %plugin_id,
                version = %version,
                reason = %error.message,
                phase = "rollback",
                "新插件启动失败，已恢复旧版本"
            );
            return Err(error);
        }
        if let Err(message) = package_commit.finish() {
            // Never acknowledge an installation whose durable completion marker failed.
            self.stop_for_reinstall(&plugin_id)?;
            package_commit
                .rollback()
                .map_err(|message| plugin_error("INSTALL_RECOVERY_REQUIRED", &message))?;
            self.restore_install_preferences(
                &plugin_id,
                previous_version.clone(),
                previous_source,
                previous_identity,
                previous_enabled,
            )?;
            self.refresh()?;
            if previous_version.is_some() && previous_enabled.unwrap_or(true) {
                self.restore_previous_runtime(&plugin_id, was_running)?;
            }
            return Err(plugin_error("INSTALL_RECOVERY_REQUIRED", &message));
        }
        self.inner.lifecycle.package_changed(&plugin_id);
        info!(
            install_id,
            plugin_id = %plugin_id,
            version = %version,
            source_kind = %source_kind,
            overwrite,
            compatible = inspected.compatible,
            start_requested = should_start,
            elapsed_ms = install_started.elapsed().as_millis() as u64,
            phase = "complete",
            "插件包安装完成"
        );
        Ok(self.snapshots())
    }

    fn restore_install_preferences(
        &self,
        plugin_id: &str,
        version: Option<String>,
        source: Option<PluginInstallSource>,
        identity: Option<serde_json::Value>,
        enabled: Option<bool>,
    ) -> Result<(), PluginError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let mut next = state.preferences.clone();
        restore_map_entry(&mut next.active_versions, plugin_id, version);
        restore_map_entry(&mut next.installation_sources, plugin_id, source);
        restore_map_entry(&mut next.enabled_plugins, plugin_id, enabled);
        // Preserve malformed records verbatim during a local development install.
        // Online preflight already requires the identity collection to be an object.
        if let Some(identities) = next.publisher_identities.as_object_mut() {
            if let Some(identity) = identity {
                identities.insert(plugin_id.to_owned(), identity);
            } else {
                identities.remove(plugin_id);
            }
        }
        persist_preferences(&self.inner.app_data_dir, &next).map_err(|error| {
            plugin_error(
                "INSTALL_RECOVERY_REQUIRED",
                &format!("Could not persist installation rollback: {error}"),
            )
        })?;
        state.preferences = next;
        Ok(())
    }

    fn stop_for_reinstall(&self, plugin_id: &str) -> Result<bool, PluginError> {
        let _work = self.inner.begin_work()?;
        let _notification = PluginChangeNotification(self.inner.environment.clone());
        if !self.inner.lifecycle.blockers(plugin_id).is_empty() {
            return Err(plugin_error(
                "PLUGIN_BUSY",
                "Finish the plugin's active background work before replacing it.",
            ));
        }
        let process = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let record = state
                .plugins
                .get_mut(plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            if record.snapshot.runtime == RuntimeState::Stopping
                || record
                    .process
                    .as_ref()
                    .is_some_and(|p| p.active_request_count() > 0)
            {
                return Err(plugin_error(
                    "PLUGIN_BUSY",
                    "Wait for the plugin to finish starting or stopping before replacing it.",
                ));
            }
            let was_running = record.snapshot.runtime == RuntimeState::Running
                && record
                    .process
                    .as_ref()
                    .is_some_and(|process| process.is_alive());
            record.revision = record.revision.wrapping_add(1);
            record.snapshot.runtime = if record.process.is_some() {
                RuntimeState::Stopping
            } else {
                RuntimeState::Stopped
            };
            record.snapshot.last_error = None;
            (record.process.take(), was_running)
        };
        let (process, was_running) = process;
        self.inner.activation.invalidate(plugin_id);
        self.emit_plugin_states_changed();
        if let Some(process) = process {
            if let Err(error) = process.stop() {
                if let Ok(mut state) = self.inner.state.lock()
                    && let Some(record) = state.plugins.get_mut(plugin_id)
                {
                    record.process = Some(process);
                    record.snapshot.runtime = RuntimeState::Failed;
                    record.snapshot.last_error = Some(failure(&error.code, &error.message));
                }
                error!(
                    plugin_id = %plugin_id,
                    code = %error.code,
                    reason = %error.message,
                    phase = "stop_for_reinstall",
                    "重新安装前停止插件失败"
                );
                return Err(error);
            }
            if let Ok(mut state) = self.inner.state.lock()
                && let Some(record) = state.plugins.get_mut(plugin_id)
            {
                record.snapshot.runtime = RuntimeState::Stopped;
                record.snapshot.last_error = None;
            }
            drop(process);
        }
        Ok(was_running)
    }

    fn set_enabled(
        &self,
        plugin_id: &str,
        enabled: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let _management = self.inner.management.try_lock().map_err(|_| {
            plugin_error(
                "PLUGIN_BUSY",
                "Another plugin management change is in progress; retry after it completes.",
            )
        })?;
        let _work = self.inner.begin_work()?;
        let _notification = PluginChangeNotification(self.inner.environment.clone());
        let process = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let record = state
                .plugins
                .get(plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            if record.snapshot.runtime == RuntimeState::Stopping {
                return Err(plugin_error(
                    "PLUGIN_BUSY",
                    "Wait for the plugin to finish starting or stopping before changing its state.",
                ));
            }
            if record.snapshot.installation == InstallationState::Incompatible {
                return Err(plugin_error(
                    "INCOMPATIBLE_CORE",
                    "Plugin is incompatible with this Core.",
                ));
            }
            if record.snapshot.installation != InstallationState::Installed {
                return Err(plugin_error(
                    "INVALID_REQUEST",
                    "Invalid plugin packages cannot be enabled.",
                ));
            }
            let mut next_preferences = state.preferences.clone();
            next_preferences
                .enabled_plugins
                .insert(plugin_id.to_owned(), enabled);
            if let Err(error) = persist_preferences(&self.inner.app_data_dir, &next_preferences) {
                error!(
                    plugin_id = %plugin_id,
                    enabled,
                    phase = "persist_preferences",
                    reason = %error,
                    "保存插件启用状态失败"
                );
                return Err(plugin_error(
                    "INTERNAL",
                    &format!("Cannot save plugin state: {error}"),
                ));
            }
            info!(plugin_id = %plugin_id, enabled, "更新插件启用状态");
            state.preferences = next_preferences;
            state.config_error = None;
            let record = state
                .plugins
                .get_mut(plugin_id)
                .expect("record checked above");
            record.snapshot.enabled = enabled;
            let process = if !enabled {
                record.revision = record.revision.wrapping_add(1);
                record.snapshot.runtime = RuntimeState::Stopping;
                record.process.take()
            } else {
                None
            };
            update_dependency_issues(&mut state.plugins);
            process
        };
        if !enabled {
            self.inner.activation.invalidate(plugin_id);
        }
        self.emit_plugin_states_changed();
        if let Some(process) = process {
            if let Err(error) = process.stop() {
                if let Ok(mut state) = self.inner.state.lock()
                    && let Some(record) = state.plugins.get_mut(plugin_id)
                {
                    record.process = Some(process);
                    record.snapshot.runtime = RuntimeState::Failed;
                    record.snapshot.last_error = Some(failure(&error.code, &error.message));
                }
                error!(
                    plugin_id = %plugin_id,
                    code = %error.code,
                    reason = %error.message,
                    phase = "stop_backend",
                    "修改启用状态时停止插件失败"
                );
                return Err(error);
            }
            if let Ok(mut state) = self.inner.state.lock()
                && let Some(record) = state.plugins.get_mut(plugin_id)
            {
                record.snapshot.runtime = RuntimeState::Stopped;
                record.snapshot.last_error = None;
            }
            drop(process);
        } else if !enabled
            && let Ok(mut state) = self.inner.state.lock()
            && let Some(record) = state.plugins.get_mut(plugin_id)
        {
            record.snapshot.runtime = RuntimeState::Stopped;
            record.snapshot.last_error = None;
        }
        if enabled
            && self.inner.desktop_host
            && self.snapshots().iter().any(|snapshot| {
                snapshot.manifest.id == plugin_id && self.inner.lifecycle.should_restore(snapshot)
            })
        {
            self.start_target(plugin_id)?;
        }
        Ok(self.snapshots())
    }

    fn uninstall(
        &self,
        plugin_id: &str,
        remove_plugin_data: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let _management = self.inner.management.try_lock().map_err(|_| {
            plugin_error(
                "PLUGIN_BUSY",
                "Another plugin management change is in progress; retry after it completes.",
            )
        })?;
        let _work = self.inner.begin_work()?;
        let _notification = PluginChangeNotification(self.inner.environment.clone());
        let _target_pause = self.inner.lifecycle.pause(plugin_id, true)?;
        let uninstall_started = Instant::now();
        info!(plugin_id = %plugin_id, remove_plugin_data, phase = "uninstall", "开始卸载插件");
        let process = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            if !state.plugins.contains_key(plugin_id) {
                return Err(not_installed(plugin_id));
            }
            if state.plugins[plugin_id].snapshot.runtime == RuntimeState::Stopping {
                return Err(plugin_error(
                    "PLUGIN_BUSY",
                    "Wait for the plugin to finish starting or stopping before removing it.",
                ));
            }
            let previous_preferences = state.preferences.clone();
            state
                .preferences
                .enabled_plugins
                .insert(plugin_id.to_owned(), false);
            if let Err(error) = persist_preferences(&self.inner.app_data_dir, &state.preferences) {
                state.preferences = previous_preferences;
                error!(
                    plugin_id = %plugin_id,
                    phase = "prepare_uninstall",
                    reason = %error,
                    "无法安全准备插件卸载"
                );
                return Err(plugin_error(
                    "INTERNAL",
                    &format!("Cannot prepare plugin removal safely: {error}"),
                ));
            }
            let record = state
                .plugins
                .get_mut(plugin_id)
                .expect("record checked above");
            record.revision = record.revision.wrapping_add(1);
            record.snapshot.enabled = false;
            record.snapshot.runtime = RuntimeState::Stopping;
            let process = record.process.take();
            update_dependency_issues(&mut state.plugins);
            process
        };

        self.inner.activation.invalidate(plugin_id);
        self.emit_plugin_states_changed();
        if let Some(process) = process {
            if let Err(error) = process.stop() {
                if let Ok(mut state) = self.inner.state.lock()
                    && let Some(record) = state.plugins.get_mut(plugin_id)
                {
                    record.process = Some(process);
                    record.snapshot.runtime = RuntimeState::Failed;
                    record.snapshot.last_error = Some(failure(&error.code, &error.message));
                }
                error!(
                    plugin_id = %plugin_id,
                    code = %error.code,
                    reason = %error.message,
                    phase = "stop_backend",
                    "卸载前停止插件失败"
                );
                return Err(plugin_error(
                    &error.code,
                    &format!(
                        "Uninstall was cancelled because the plugin process could not be stopped. Its package and local data were left untouched. {}",
                        error.message
                    ),
                ));
            }
            drop(process);
        }

        // Remove the package first. If it fails, keep the plugin's private data untouched.
        let install_directory = self.inner.installed_root.join(plugin_id);
        let package_removal = (|| {
            if install_directory.exists() {
                reject_symlink(&install_directory)?;
                fs::remove_dir_all(&install_directory)
                    .map_err(|error| plugin_package_removal_error(&install_directory, error))
            } else {
                Ok(())
            }
        })();
        if let Err(error) = package_removal {
            if let Ok(mut state) = self.inner.state.lock()
                && let Some(record) = state.plugins.get_mut(plugin_id)
            {
                record.snapshot.runtime = RuntimeState::Stopped;
                record.snapshot.last_error = Some(failure(&error.code, &error.message));
            }
            error!(
                plugin_id = %plugin_id,
                code = %error.code,
                reason = %error.message,
                phase = "remove_package",
                "卸载插件包失败"
            );
            return Err(error);
        }

        // Once the package is gone, finalize its removal even if an optional data cleanup fails.
        let data_removal = if remove_plugin_data {
            let data_directory = self.inner.app_data_dir.join("plugin-data").join(plugin_id);
            let secret_directory = self
                .inner
                .app_data_dir
                .join("plugin-secrets")
                .join(plugin_id);
            apply_plugin_data_removal(&data_directory, true)
                .and_then(|()| apply_plugin_data_removal(&secret_directory, true))
                .and_then(|()| self.inner.lifecycle.remove(plugin_id))
        } else {
            Ok(())
        };

        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        state.plugins.remove(plugin_id);
        update_dependency_issues(&mut state.plugins);
        let removal_preferences = state.preferences.clone();
        clear_plugin_preferences(&mut state.preferences, plugin_id);
        if remove_plugin_data
            && data_removal.is_ok()
            && let Some(identities) = state.preferences.publisher_identities.as_object_mut()
        {
            identities.remove(plugin_id);
        }
        if let Err(error) = persist_preferences(&self.inner.app_data_dir, &state.preferences) {
            // Keep the persisted disabled intent if final preference cleanup fails.
            state.preferences = removal_preferences;
            state.config_error = Some(format!("Plugin removal preferences need cleanup: {error}"));
            warn!(plugin_id = %plugin_id, reason = %error, "插件已卸载，但管理配置的最终清理未完成");
        } else {
            state.config_error = None;
        }
        // `snapshots()` takes the same mutex; release this guard first to avoid
        // deadlocking after the package has already been removed.
        drop(state);
        if let Err(error) = data_removal {
            error!(
                plugin_id = %plugin_id,
                code = %error.code,
                reason = %error.message,
                phase = "remove_private_data",
                "插件包已卸载，但清理插件数据失败"
            );
            return Err(error);
        }
        info!(
            plugin_id = %plugin_id,
            remove_plugin_data,
            elapsed_ms = uninstall_started.elapsed().as_millis() as u64,
            phase = "uninstall",
            "插件卸载完成"
        );
        Ok(self.snapshots())
    }

    #[cfg(test)]
    pub(crate) fn call(
        &self,
        plugin_id: &str,
        request_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginError> {
        self.call_with_context(plugin_id, request_id, method, params, None)
    }

    #[cfg(test)]
    pub(crate) fn call_with_context(
        &self,
        plugin_id: &str,
        request_id: &str,
        method: &str,
        params: Value,
        remote: Option<Arc<crate::remote_interaction::RemoteContext>>,
    ) -> Result<Value, PluginError> {
        self.call_with_timeout_context(plugin_id, request_id, method, params, None, remote)
    }

    pub(crate) fn call_with_timeout_context(
        &self,
        plugin_id: &str,
        request_id: &str,
        method: &str,
        params: Value,
        timeout_override: Option<u64>,
        remote: Option<Arc<crate::remote_interaction::RemoteContext>>,
    ) -> Result<Value, PluginError> {
        self.call_with_parent_context(
            plugin_id,
            request_id,
            method,
            params,
            timeout_override,
            remote,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn call_with_parent_context(
        &self,
        plugin_id: &str,
        request_id: &str,
        method: &str,
        params: Value,
        timeout_override: Option<u64>,
        remote: Option<Arc<crate::remote_interaction::RemoteContext>>,
        parent: Option<&crate::host_services::HostInvocation>,
    ) -> Result<Value, PluginError> {
        let admitted = Instant::now();
        let _work = self.inner.begin_work()?;
        let _target_call = self.inner.lifecycle.enter_call(plugin_id)?;
        validate_timeout_ms(timeout_override)?;
        if let Some(parent) = parent {
            parent.check()?;
        }
        let contract = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let record = state
                .plugins
                .get(plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            if record.snapshot.installation == InstallationState::Incompatible {
                return Err(plugin_error(
                    "INCOMPATIBLE_CORE",
                    "Plugin is incompatible with this Core.",
                ));
            }
            if record.snapshot.installation != InstallationState::Installed {
                return Err(not_installed(plugin_id));
            }
            if !record.snapshot.enabled {
                return Err(plugin_error("DISABLED", "Plugin is disabled."));
            }
            if remote.is_some()
                && record.snapshot.manifest.backend.supports_service_context != Some(true)
            {
                return Err(plugin_error(
                    "REMOTE_UNSUPPORTED",
                    "Plugin does not support remote invocation contexts.",
                ));
            }
            record.contract.clone()
        };
        let validated = validate_method_call(&contract, method, &params, remote.is_some())?;
        let budget = remaining_timeout(
            earliest_timeout(
                optional_timeout_ms(validated.get("timeoutMs"))?,
                timeout_override,
            ),
            admitted,
        )?;
        let mut wait_context = parent.cloned().unwrap_or_default().for_wait(budget)?;
        wait_context.remote = remote.clone();
        let _wait = self
            .inner
            .activation
            .register(plugin_id, request_id, wait_context.clone())?;
        let process = self.ensure_active(plugin_id, "call", &wait_context, false)?;
        trace!(
            plugin_id,
            request_id,
            phase = "call_activated",
            "插件调用已取得实例"
        );
        wait_context.check()?;
        let actual_contract = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let record = state
                .plugins
                .get(plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            if !record.snapshot.enabled {
                return Err(plugin_error("DISABLED", "Plugin is disabled."));
            }
            if record
                .process
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(current, &process))
            {
                return Err(plugin_error(
                    "PLUGIN_BUSY",
                    "Plugin package changed while preparing the call.",
                ));
            }
            record.contract.clone()
        };
        let method_contract =
            validate_method_call(&actual_contract, method, &params, remote.is_some())?;
        let current_budget = remaining_timeout(
            earliest_timeout(
                optional_timeout_ms(method_contract.get("timeoutMs"))?,
                timeout_override,
            ),
            admitted,
        )?;
        wait_context = wait_context.with_timeout_ms(current_budget)?;
        wait_context.check()?;
        let timeout_ms = wait_context.remaining().map(duration_to_timeout);
        let result = process.call_with_parent(
            request_id,
            method,
            params,
            timeout_ms,
            remote,
            Some(&wait_context),
        )?;
        trace!(
            plugin_id,
            request_id,
            phase = "call_result",
            "插件方法已返回结果"
        );
        crate::plugin_schema::validate(&result, &method_contract["result"], &actual_contract)
            .map_err(|message| plugin_error("INVALID_RESPONSE", &message))?;

        Ok(result)
    }

    #[cfg(test)]
    pub(crate) fn invoke_plugin(
        &self,
        consumer_id: &str,
        target_id: &str,
        method: &str,
        params: Value,
        timeout_override: Option<u64>,
        remote: Option<Arc<crate::remote_interaction::RemoteContext>>,
    ) -> Result<Value, PluginError> {
        let mut invocation = crate::host_services::HostInvocation::default();
        invocation.remote = remote;
        self.invoke_plugin_with_invocation(
            consumer_id,
            target_id,
            method,
            params,
            timeout_override,
            &invocation,
        )
    }

    pub(crate) fn invoke_plugin_with_invocation(
        &self,
        consumer_id: &str,
        target_id: &str,
        method: &str,
        params: Value,
        timeout_override: Option<u64>,
        invocation: &crate::host_services::HostInvocation,
    ) -> Result<Value, PluginError> {
        let admitted = Instant::now();
        let remote = invocation.remote.clone();
        let _work = self.inner.begin_work()?;
        if !valid_plugin_id(target_id) || !valid_service_method(method) {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Target plugin or method is invalid.",
            ));
        }
        if remote
            .as_ref()
            .is_some_and(|context| context.invocation_depth() >= 16)
        {
            return Err(plugin_error(
                "CALL_DEPTH_EXCEEDED",
                "Plugin call depth exceeded.",
            ));
        }
        validate_timeout_ms(timeout_override)?;
        let mut _warning_notification = None;
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let consumer = state
                .plugins
                .get_mut(consumer_id)
                .ok_or_else(|| not_installed(consumer_id))?;
            if consumer.snapshot.runtime != RuntimeState::Running {
                return Err(plugin_error(
                    "NOT_RUNNING",
                    "Calling plugin is not running.",
                ));
            }
            if remote.is_some()
                && !consumer
                    .snapshot
                    .manifest
                    .dependencies
                    .iter()
                    .any(|dependency| {
                        dependency.plugin_id == target_id
                            && dependency.methods.iter().any(|declared| declared == method)
                    })
            {
                return Err(plugin_error(
                    "REMOTE_DEPENDENCY_NOT_DECLARED",
                    "Remote call is outside the shared tool's declared dependencies.",
                ));
            }
            if remote.is_none() {
                let dependency = consumer
                    .snapshot
                    .manifest
                    .dependencies
                    .iter()
                    .find(|dependency| dependency.plugin_id == target_id);
                let warning = match dependency {
                    None => Some(format!(
                        "Runtime call '{target_id}.{method}' has no declared plugin dependency."
                    )),
                    Some(dependency)
                        if !dependency.methods.iter().any(|declared| declared == method) =>
                    {
                        Some(format!(
                            "Runtime call '{target_id}.{method}' has no declared method expectation."
                        ))
                    }
                    Some(_) => None,
                };
                if let Some(warning) = warning
                    && !consumer.runtime_dependency_warnings.contains(&warning)
                {
                    if consumer.runtime_dependency_warnings.len() >= 32 {
                        consumer.runtime_dependency_warnings.remove(0);
                    }
                    consumer.runtime_dependency_warnings.push(warning.clone());
                    warn!(plugin_id = %consumer_id, target_plugin = %target_id, method = %method,
                        diagnostic = %warning, "插件依赖声明缺口；继续执行实际调用");
                    update_dependency_issues(&mut state.plugins);
                    _warning_notification =
                        Some(PluginChangeNotification(self.inner.environment.clone()));
                }
            }
        }
        let request_id = format!(
            "plugin-call-{}",
            self.inner
                .next_service_request_id
                .fetch_add(1, Ordering::Relaxed)
        );
        self.call_with_parent_context(
            target_id,
            &request_id,
            method,
            params,
            remaining_timeout(timeout_override, admitted)?,
            remote.map(|context| context.for_plugin(target_id)),
            Some(invocation),
        )
    }

    pub(crate) fn distribution_settings(&self) -> Result<DistributionSettingsView, PluginError> {
        // Observing a known expiry can delete its snapshot and persist the state.
        let _work = self.admit_work()?;
        let before = self.inner.distribution.visibility_key();
        let view = self.inner.distribution.settings();
        if before != self.inner.distribution.visibility_key() {
            self.distribution_changed();
        }
        Ok(view)
    }

    pub(crate) fn configure_distribution(
        &self,
        request: DistributionConfigureRequest,
    ) -> Result<DistributionSettingsView, PluginError> {
        let _work = self.admit_work()?;
        let result = self.inner.distribution.configure(request)?;
        self.distribution_changed();
        Ok(result)
    }

    /// Only CLI / IPC worker threads use this bridge; Tauri commands await directly.
    pub(crate) fn block_on_distribution<F: std::future::Future>(&self, future: F) -> F::Output {
        self.inner.runtime.block_on(future)
    }

    pub(crate) async fn catalog_snapshot(&self) -> Result<PluginCatalogSnapshot, PluginError> {
        let _work = self.admit_work()?;
        let before = self.inner.distribution.visibility_key();
        let result = self.inner.distribution.catalog(&self.snapshots()).await;
        // CLI reads can discover revocation or reduced scope while the desktop
        // already shows a catalog. Notify only semantic changes, never read times.
        if before != self.inner.distribution.visibility_key() {
            self.distribution_changed();
        }
        result
    }

    pub(crate) async fn install_catalog_request(
        &self,
        request: CatalogInstallRequest,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        // Retain admission through download and the blocking install transaction. If an
        // awaiting caller disconnects, the transaction still owns its upgrade blocker.
        let work = self.admit_work()?;
        let operation_id = request.operation_id.clone().unwrap_or_else(|| {
            format!(
                "catalog-{}",
                NEXT_PLUGIN_INSTALL_ID.fetch_add(1, Ordering::Relaxed)
            )
        });
        if operation_id.is_empty()
            || operation_id.len() > 128
            || !operation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Invalid installation operation ID.",
            ));
        }
        let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
        {
            let mut jobs = self
                .inner
                .catalog_installs
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Installation jobs are unavailable."))?;
            if jobs.contains_key(&operation_id) {
                return Err(plugin_error(
                    "INVALID_REQUEST",
                    "Installation operation ID is already in use.",
                ));
            }
            jobs.insert(
                operation_id.clone(),
                CatalogInstallControl {
                    plugin_id: request.plugin_id.clone(),
                    cancel,
                    committing: false,
                },
            );
        }
        let job = CatalogInstallGuard {
            inner: self.inner.clone(),
            id: operation_id.clone(),
        };
        let last_progress = Mutex::new(Instant::now() - Duration::from_secs(1));
        let progress = |phase: &str, downloaded: u64, total: u64| {
            if let Ok(mut last) = last_progress.lock() {
                if phase == "downloading"
                    && downloaded > 0
                    && downloaded < total
                    && last.elapsed() < Duration::from_millis(100)
                {
                    return;
                }
                *last = Instant::now();
            }
            self.inner.environment.emit("plugins:catalog-install-progress", json!({"operationId":operation_id,"pluginId":request.plugin_id,"phase":phase,"downloadedBytes":downloaded,"totalBytes":total,"cancellable":true}));
        };
        progress("checking", 0, 0);
        let approved_source_change = request.approved_source_change;
        let snapshots = self.snapshots();
        let prepared_result = tokio::select! {
            result = tokio::time::timeout(
            Duration::from_secs(150),
            self.inner
                .distribution
                .prepare_install_with_progress(&request, &snapshots, &progress),
            ) => result,
            _ = cancelled.changed() => return Err(plugin_error("CANCELLED", "Plugin download was cancelled before installation.")),
        };
        let prepared = match prepared_result {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => {
                self.distribution_changed();
                return Err(error);
            }
            Err(_) => {
                return Err(plugin_error(
                    "DISTRIBUTION_UNAVAILABLE",
                    "Plugin download timed out before installation started. Retry the request.",
                ));
            }
        };
        {
            let mut jobs = self
                .inner
                .catalog_installs
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Installation jobs are unavailable."))?;
            let control = jobs
                .get_mut(&operation_id)
                .ok_or_else(|| plugin_error("CANCELLED", "Installation was cancelled."))?;
            if *control.cancel.borrow() {
                return Err(plugin_error(
                    "CANCELLED",
                    "Plugin download was cancelled before installation.",
                ));
            }
            control.committing = true;
        }
        self.inner.environment.emit("plugins:catalog-install-progress", json!({"operationId":operation_id,"pluginId":request.plugin_id,"phase":"installing","downloadedBytes":prepared.bytes.len(),"totalBytes":prepared.entry.size_bytes,"cancellable":false}));
        let mode = request.mode;
        let manager = self.clone();
        tokio::task::spawn_blocking(move || {
            let _work = work;
            let _job = job;
            let download_dir = manager.inner.app_data_dir.join("plugin-downloads");
            fs::create_dir_all(&download_dir).map_err(|_| {
                plugin_error("LOCAL_IO", "Cannot prepare the plugin download directory.")
            })?;
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let source = download_dir.join(format!(
                "{}-{}-{}-{nonce}.wplug",
                prepared.entry.id,
                prepared.entry.version,
                std::process::id(),
            ));
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&source)
                .map_err(|_| plugin_error("LOCAL_IO", "Cannot save the downloaded plugin."))?;
            let result = (|| {
                file.write_all(&prepared.bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| plugin_error("LOCAL_IO", "Cannot save the downloaded plugin."))?;
                drop(file);
                manager.install_catalog_package(
                    source.clone(),
                    &prepared.entry,
                    approved_source_change,
                    mode,
                )
            })();
            let _ = fs::remove_file(&source);
            manager.distribution_changed();
            result
        })
        .await
        .map_err(|_| {
            plugin_error(
                "INTERNAL",
                "Plugin installation did not return a result. Check the installed plugin state.",
            )
        })?
    }

    fn cancel_catalog_install(&self, operation_id: &str) -> Result<bool, PluginError> {
        let jobs = self
            .inner
            .catalog_installs
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Installation jobs are unavailable."))?;
        let Some(control) = jobs.get(operation_id) else {
            return Ok(false);
        };
        if control.committing {
            return Ok(false);
        }
        let _ = control.cancel.send(true);
        Ok(true)
    }

    fn distribution_changed(&self) {
        self.inner
            .environment
            .emit("plugins:distribution-changed", Value::Null);
    }

    pub(crate) fn admit_work(&self) -> Result<PluginWorkGuard, PluginError> {
        self.inner.begin_work()
    }

    pub(crate) fn begin_upgrade(&self) -> Result<PluginUpgradeGuard, PluginError> {
        let mut admission = self
            .inner
            .admission
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin admission state is unavailable."))?;
        if admission.frozen {
            return Err(plugin_error(
                "UPGRADE_IN_PROGRESS",
                "Core upgrade is already in progress.",
            ));
        }
        if admission.active > 0 {
            return Err(plugin_error(
                "UPGRADE_BUSY",
                "Finish active plugin calls, startup and background work before upgrading Core.",
            ));
        }
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        if state.plugins.values().any(|record| {
            matches!(
                record.snapshot.runtime,
                RuntimeState::Starting | RuntimeState::Stopping
            ) || record
                .process
                .as_ref()
                .is_some_and(|process| process.active_request_count() > 0)
        }) {
            return Err(plugin_error(
                "UPGRADE_BUSY",
                "Finish active plugin work before upgrading Core.",
            ));
        }
        admission.frozen = true;
        Ok(PluginUpgradeGuard(self.inner.admission.clone()))
    }

    pub(crate) fn upgrade_blockers(&self) -> Vec<String> {
        let Ok(state) = self.inner.state.lock() else {
            return vec!["Plugin state is unavailable.".into()];
        };
        let plugins: Vec<_> = state
            .plugins
            .values()
            .map(|record| {
                let active = record
                    .process
                    .as_ref()
                    .map_or(0, |process| process.active_request_count());
                (
                    record.snapshot.manifest.id.clone(),
                    record.snapshot.runtime,
                    active,
                )
            })
            .collect();
        drop(state);
        let activating = self.inner.activation.pending_ids();
        plugins.into_iter().filter_map(|(id, runtime, active)| {
            let activities = self.inner.lifecycle.blockers(&id);
            if matches!(runtime, RuntimeState::Starting | RuntimeState::Stopping)
                || active > 0 || !activities.is_empty() || activating.contains(&id) {
                Some(format!("{id} ({runtime:?}, {active} active requests, {} background activities{}{})",
                    activities.len(), if activities.is_empty() { "" } else { ": " }, activities.join(", ")))
            } else { None }
        }).collect()
    }

    pub(crate) fn cancel(&self, plugin_id: &str, request_id: &str) -> Result<(), PluginError> {
        if self.inner.activation.cancel(plugin_id, request_id) {
            return Ok(());
        }
        let process = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            state
                .plugins
                .get(plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?
                .process
                .clone()
                .ok_or_else(|| plugin_error("NOT_RUNNING", "Plugin backend is not running."))?
        };
        process.cancel(request_id)
    }

    pub(crate) fn plugin_ui_launch_info(
        &self,
        plugin_id: &str,
    ) -> Result<PluginUiLaunchInfo, PluginError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let record = state
            .plugins
            .get(plugin_id)
            .ok_or_else(|| not_installed(plugin_id))?;
        if record.snapshot.installation != InstallationState::Installed || !record.snapshot.enabled
        {
            return Err(plugin_error(
                "DISABLED",
                "Plugin UI is unavailable while the plugin is disabled.",
            ));
        }
        let ui = record.snapshot.manifest.ui.as_ref().ok_or_else(|| {
            plugin_error(
                "NO_PLUGIN_UI",
                "This service plugin does not provide a user interface.",
            )
        })?;
        // Offer only versions both implemented by this Core and declared by this plugin.
        let bridge_versions = plugin_package::supported_versions_in_range(
            UI_BRIDGE_SUPPORTED_VERSIONS,
            &ui.bridge_compatibility.min_version,
            &ui.bridge_compatibility.max_version_exclusive,
        );
        if bridge_versions.is_empty() {
            return Err(plugin_error(
                "INCOMPATIBLE_CORE",
                "Plugin UI does not support any bridge version provided by this Core.",
            ));
        }
        let origin = if cfg!(windows) {
            "http://plugin-asset.localhost"
        } else {
            "plugin-asset://localhost"
        };
        Ok(PluginUiLaunchInfo {
            url: format!("{origin}/{plugin_id}/{}", ui.entry),
            bridge_versions,
        })
    }

    fn plugin_ui_url(&self, plugin_id: &str) -> Result<String, PluginError> {
        self.plugin_ui_launch_info(plugin_id)
            .map(|launch| launch.url)
    }

    fn read_plugin_asset(&self, path: &str, method: &str) -> (u16, String, Vec<u8>) {
        if method != "GET" {
            return (
                405,
                "text/plain; charset=utf-8".to_owned(),
                b"method not allowed".to_vec(),
            );
        }
        if path.contains('%') || path.contains('\\') || path.contains(':') {
            return (
                400,
                "text/plain; charset=utf-8".to_owned(),
                b"invalid plugin asset path".to_vec(),
            );
        }
        let Some((plugin_id, relative)) = path.trim_start_matches('/').split_once('/') else {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        };
        if !valid_plugin_id(plugin_id)
            || !relative.starts_with("ui/")
            || relative
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return (
                400,
                "text/plain; charset=utf-8".to_owned(),
                b"invalid plugin asset path".to_vec(),
            );
        }
        let state = match self.inner.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return (
                    500,
                    "text/plain; charset=utf-8".to_owned(),
                    b"plugin state unavailable".to_vec(),
                );
            }
        };
        let Some(record) = state.plugins.get(plugin_id) else {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        };
        let is_icon_asset = relative.starts_with("ui/icons/");
        let package_available = matches!(
            record.snapshot.installation,
            InstallationState::Installed | InstallationState::Incompatible
        );
        if !package_available || (!is_icon_asset && !record.snapshot.enabled) {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        }
        let Ok(root) = record.directory.canonicalize() else {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        };
        let path = record.directory.join(relative);
        let Ok(canonical) = path.canonicalize() else {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        };
        if !canonical.starts_with(&root) || !canonical.is_file() {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        }
        if is_icon_asset
            && !matches!(
                canonical
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("svg" | "png" | "webp")
            )
        {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin icon not found".to_vec(),
            );
        }
        let Ok(metadata) = fs::metadata(&canonical) else {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        };
        let max_bytes = if is_icon_asset {
            512 * 1024
        } else {
            128 * 1024 * 1024
        };
        if metadata.len() > max_bytes {
            return (
                413,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset too large".to_vec(),
            );
        }
        let Ok(bytes) = fs::read(&canonical) else {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        };
        (200, content_type(&canonical), bytes)
    }
}

impl PluginManagerInner {
    fn begin_work(&self) -> Result<PluginWorkGuard, PluginError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(plugin_error(
                "SERVICE_UNAVAILABLE",
                "Core is stopping its plugin host.",
            ));
        }
        let mut admission = self
            .admission
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin admission state is unavailable."))?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(plugin_error(
                "SERVICE_UNAVAILABLE",
                "Core is stopping its plugin host.",
            ));
        }
        if admission.frozen {
            return Err(plugin_error(
                "UPGRADE_IN_PROGRESS",
                "Core upgrade is in progress.",
            ));
        }
        admission.active += 1;
        Ok(PluginWorkGuard(self.admission.clone()))
    }

    fn start_one(self: &Arc<Self>, plugin_id: &str) -> Result<(), PluginError> {
        self.activation
            .activate(
                self,
                plugin_id,
                "explicit",
                &crate::host_services::HostInvocation::default(),
                true,
            )
            .map(|_| ())
    }

    fn spawn_one(
        self: &Arc<Self>,
        plugin_id: &str,
        expected_revision: u64,
    ) -> Result<(), PluginError> {
        let _work = self.begin_work()?;
        let _notification = PluginChangeNotification(self.environment.clone());
        let start_started = Instant::now();
        let (snapshot, directory, contract, contract_sha256, previous_process, start_revision) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            if self.shutting_down.load(Ordering::Acquire) {
                return Err(plugin_error(
                    "SERVICE_UNAVAILABLE",
                    "Core is stopping its plugin host.",
                ));
            }
            let record = state
                .plugins
                .get_mut(plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            if record.revision != expected_revision {
                return Err(plugin_error(
                    "CANCELLED",
                    "Plugin changed before activation.",
                ));
            }
            if record.snapshot.runtime == RuntimeState::Stopping {
                return Err(plugin_error(
                    "PLUGIN_BUSY",
                    "Plugin is stopping and cannot be started yet.",
                ));
            }
            if record.snapshot.runtime == RuntimeState::Starting {
                return Err(plugin_error("PLUGIN_BUSY", "Plugin is already starting."));
            }
            if !record.snapshot.enabled
                || record.snapshot.installation != InstallationState::Installed
            {
                return Err(plugin_error(
                    "DISABLED",
                    "Plugin is disabled or incompatible.",
                ));
            }
            if record
                .process
                .as_ref()
                .is_some_and(|process| process.is_alive())
            {
                return Ok(());
            }
            record.snapshot.runtime = RuntimeState::Starting;
            record.snapshot.last_error = None;
            record.revision = record.revision.wrapping_add(1);
            let start_revision = record.revision;
            let previous_process = record.process.take();
            (
                record.snapshot.clone(),
                record.directory.clone(),
                record.contract.clone(),
                record.contract_sha256.clone(),
                previous_process,
                start_revision,
            )
        };
        self.environment.emit("plugins:changed", Value::Null);
        info!(
            plugin_id = %plugin_id,
            version = %snapshot.manifest.version,
            phase = "backend_start",
            "开始启动插件后端"
        );
        if let Some(process) = previous_process {
            if let Err(error) = process.stop() {
                if let Ok(mut state) = self.state.lock()
                    && let Some(record) = state.plugins.get_mut(plugin_id)
                {
                    record.process = Some(process);
                    record.snapshot.runtime = RuntimeState::Failed;
                    record.snapshot.last_error = Some(failure(&error.code, &error.message));
                }
                error!(
                    plugin_id = %plugin_id,
                    code = %error.code,
                    reason = %error.message,
                    phase = "stop_previous_process",
                    "重启前停止旧插件进程失败"
                );
                return Err(error);
            }
            drop(process);
        }
        let instance_id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let process = (|| {
            let executable = directory.join(&snapshot.manifest.backend.entry);
            let canonical_directory = directory.canonicalize().map_err(|error| {
                plugin_error(
                    "PLUGIN_START_FAILED",
                    &format!("Cannot resolve plugin directory: {error}"),
                )
            })?;
            let canonical_executable = executable.canonicalize().map_err(|error| {
                plugin_error(
                    "PLUGIN_START_FAILED",
                    &format!("Plugin backend is missing: {error}"),
                )
            })?;
            if !canonical_executable.starts_with(&canonical_directory)
                || !canonical_executable.is_file()
            {
                return Err(plugin_error(
                    "PLUGIN_START_FAILED",
                    "Plugin backend path escapes its package.",
                ));
            }
            PluginProcess::start(
                RuntimeServices::new(
                    self.environment.clone(),
                    self.runtime.clone(),
                    WeakPluginManager {
                        inner: Arc::downgrade(self),
                    },
                )
                .with_instance(instance_id, start_revision),
                &snapshot,
                &canonical_executable,
                contract,
                self.app_data_dir.join("plugin-data").join(plugin_id),
                self.documents_dir.clone(),
                &contract_sha256,
            )
        })();
        let mut state = self
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let Some(record) = state.plugins.get_mut(plugin_id) else {
            drop(state);
            if let Ok(process) = process {
                let _ = process.stop();
            }
            return Err(not_installed(plugin_id));
        };
        if self.shutting_down.load(Ordering::Acquire)
            || record.revision != start_revision
            || record.snapshot.runtime != RuntimeState::Starting
            || !record.snapshot.enabled
            || record.directory != directory
            || record.contract_sha256 != contract_sha256
        {
            drop(state);
            if let Ok(process) = process {
                let _ = process.stop();
            }
            warn!(
                plugin_id = %plugin_id,
                version = %snapshot.manifest.version,
                phase = "state_validation",
                "插件启动期间状态已变化，已丢弃过期启动结果"
            );
            return Err(plugin_error(
                "PLUGIN_BUSY",
                "Plugin state changed while the backend was starting; the stale process was stopped.",
            ));
        }
        let result = match process {
            Ok(process) if process.is_alive() => {
                self.lifecycle.connect_instance(plugin_id, instance_id);
                record.snapshot.runtime = RuntimeState::Running;
                record.snapshot.last_error = None;
                record.process = Some(process);
                Ok(())
            }
            Ok(_) => {
                let error = plugin_error(
                    "PLUGIN_CRASHED",
                    "Plugin exited immediately after its handshake.",
                );
                record.snapshot.runtime = RuntimeState::Failed;
                record.snapshot.last_error = Some(failure(&error.code, &error.message));
                Err(error)
            }
            Err(error) => {
                record.snapshot.runtime = RuntimeState::Failed;
                record.snapshot.last_error = Some(failure(&error.code, &error.message));
                Err(error)
            }
        };
        drop(state);
        match &result {
            Ok(()) => info!(
                plugin_id = %plugin_id,
                version = %snapshot.manifest.version,
                elapsed_ms = start_started.elapsed().as_millis() as u64,
                phase = "backend_start",
                "插件后端启动成功"
            ),
            Err(error) => warn!(
                plugin_id = %plugin_id,
                version = %snapshot.manifest.version,
                code = %error.code,
                reason = %error.message,
                elapsed_ms = start_started.elapsed().as_millis() as u64,
                phase = "backend_start",
                "插件后端启动失败"
            ),
        }
        result
    }
}

fn ensure_method_access(contract: &Value, remote: bool) -> Result<(), PluginError> {
    if remote && contract.get("localOnly").and_then(Value::as_bool) == Some(true) {
        return Err(plugin_error(
            "LOCAL_ONLY",
            "This plugin method is only available on the Core host.",
        ));
    }
    Ok(())
}

fn validate_method_call(
    contract: &Value,
    method: &str,
    params: &Value,
    remote: bool,
) -> Result<Value, PluginError> {
    let method_contract = contract
        .get("methods")
        .and_then(Value::as_object)
        .and_then(|methods| methods.get(method))
        .ok_or_else(|| {
            plugin_error(
                "METHOD_NOT_FOUND",
                "Method is not declared by the plugin contract.",
            )
        })?;
    ensure_method_access(method_contract, remote)?;
    crate::plugin_schema::validate(params, &method_contract["params"], contract)
        .map_err(|message| plugin_error("INVALID_INPUT", &message))?;
    Ok(method_contract.clone())
}

fn duration_to_timeout(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis().saturating_add(u128::from(
        !duration.subsec_nanos().is_multiple_of(1_000_000),
    )))
    .unwrap_or(u64::MAX)
    .max(1)
}

fn validate_timeout_ms(timeout: Option<u64>) -> Result<(), PluginError> {
    if timeout.is_some_and(|milliseconds| {
        milliseconds == 0
            || Instant::now()
                .checked_add(Duration::from_millis(milliseconds))
                .is_none()
    }) {
        return Err(plugin_error(
            "INVALID_INPUT",
            "Call timeout is not representable.",
        ));
    }
    Ok(())
}

fn optional_timeout_ms(value: Option<&Value>) -> Result<Option<u64>, PluginError> {
    let timeout = match value {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or_else(|| {
            plugin_error(
                "INVALID_INPUT",
                "Call timeout must be null or a positive integer.",
            )
        })?),
    };
    validate_timeout_ms(timeout)?;
    Ok(timeout)
}

fn earliest_timeout(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

fn remaining_timeout(timeout: Option<u64>, admitted: Instant) -> Result<Option<u64>, PluginError> {
    timeout
        .map(|milliseconds| {
            let deadline = admitted
                .checked_add(Duration::from_millis(milliseconds))
                .ok_or_else(|| {
                    plugin_error("INVALID_INPUT", "Call timeout is not representable.")
                })?;
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or_else(|| {
                    plugin_error("TIMEOUT", "Call time budget expired before dispatch.")
                })?;
            u64::try_from(remaining.as_nanos().div_ceil(1_000_000)).map_err(|_| {
                plugin_error("INVALID_INPUT", "Remaining timeout is not representable.")
            })
        })
        .transpose()
}

/// Lists packages discovered from installed-plugins/<id>/<version>.
#[tauri::command]
pub fn plugins_list(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window)?;
    Ok(manager.snapshots())
}

#[tauri::command]
pub fn plugins_retry_scan(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window)?;
    manager.retry_plugin_scan(&plugin_id)
}

/// Reads the public plugin registry and each plugin's signed update manifest.
#[tauri::command]
pub fn plugins_distribution_settings(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
) -> Result<DistributionSettingsView, PluginError> {
    ensure_main(&window)?;
    manager.distribution_settings()
}

#[tauri::command]
pub fn plugins_distribution_configure(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    request: DistributionConfigureRequest,
) -> Result<DistributionSettingsView, PluginError> {
    ensure_main(&window)?;
    manager.configure_distribution(request)
}

#[tauri::command]
pub async fn plugins_catalog_list(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
) -> Result<PluginCatalogSnapshot, PluginError> {
    ensure_main(&window)?;
    manager.catalog_snapshot().await
}

#[tauri::command]
pub async fn plugins_catalog_install(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    request: CatalogInstallRequest,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window)?;
    manager.install_catalog_request(request).await
}

#[tauri::command]
pub fn plugins_catalog_cancel(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    operation_id: String,
) -> Result<bool, PluginError> {
    ensure_main(&window)?;
    manager.cancel_catalog_install(&operation_id)
}

#[tauri::command]
pub async fn plugins_set_enabled(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
    enabled: bool,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.set_enabled(&plugin_id, enabled))
        .await
        .map_err(|error| {
            plugin_error("INTERNAL", &format!("Plugin state update failed: {error}"))
        })?
}

#[tauri::command]
pub async fn plugins_remove(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
    remove_plugin_data: bool,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.uninstall(&plugin_id, remove_plugin_data))
        .await
        .map_err(|error| plugin_error("INTERNAL", &format!("Plugin uninstall failed: {error}")))?
}

#[tauri::command]
pub async fn plugins_ui_url(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
    request_id: String,
    retry: Option<bool>,
) -> Result<PluginUiLaunchInfo, PluginError> {
    ensure_main(&window)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.prepare_ui(
            &plugin_id,
            &request_id,
            retry.unwrap_or(false),
            &crate::host_services::HostInvocation::default(),
        )
    })
    .await
    .map_err(|error| plugin_error("INTERNAL", &error.to_string()))?
}

#[tauri::command]
pub async fn plugins_retry_runtime(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.retry_runtime(&plugin_id))
        .await
        .map_err(|error| plugin_error("INTERNAL", &error.to_string()))?
}

#[tauri::command]
pub async fn plugin_call(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
    request_id: String,
    method: String,
    params: Value,
    timeout_ms: Option<u64>,
) -> Result<Value, PluginError> {
    ensure_main(&window)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.call_with_timeout_context(
            &plugin_id,
            &request_id,
            &method,
            params,
            timeout_ms,
            None,
        )
    })
    .await
    .map_err(|error| plugin_error("INTERNAL", &format!("Plugin request failed: {error}")))?
}

#[tauri::command]
pub fn plugin_cancel(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
    request_id: String,
) -> Result<(), PluginError> {
    ensure_main(&window)?;
    manager.cancel(&plugin_id, &request_id)
}

fn load_preferences(app_data_dir: &Path) -> Result<Preferences, String> {
    let current = app_data_dir.join(MANAGER_CONFIG_FILE);
    if current.exists() {
        let bytes =
            fs::read(&current).map_err(|error| format!("Cannot read plugin config: {error}"))?;
        let mut value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Plugin config is invalid: {error}"))?;
        let version = value
            .get("schemaVersion")
            .and_then(Value::as_u64)
            .ok_or("Plugin config has no schemaVersion.")?;
        if !(1..=4).contains(&version) {
            return Err("Unsupported plugin manager config version.".into());
        }
        if version < 4 {
            let object = value
                .as_object_mut()
                .ok_or("Plugin config is not an object.")?;
            object.remove("grantedCapabilities");
            object.insert("schemaVersion".into(), json!(4));
        }
        let preferences: Preferences = serde_json::from_value(value)
            .map_err(|error| format!("Plugin config is invalid: {error}"))?;
        if version < 4 {
            backup_control_config(&current, &bytes)?;
            persist_preferences(app_data_dir, &preferences)?;
        }
        return Ok(preferences);
    }
    let legacy = app_data_dir.join(LEGACY_CONFIG_FILE);
    if legacy.exists() {
        let bytes = fs::read(&legacy)
            .map_err(|error| format!("Cannot read legacy plugin config: {error}"))?;
        let old: LegacyPreferences = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Legacy plugin config is invalid: {error}"))?;
        let mut preferences = Preferences::default();
        for id in old.disabled_plugins {
            preferences.enabled_plugins.insert(id, false);
        }
        backup_control_config(&legacy, &bytes)?;
        persist_preferences(app_data_dir, &preferences)?;
        return Ok(preferences);
    }
    Ok(Preferences::default())
}

fn backup_control_config(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let backup = path.with_file_name(format!(
        "{}.pre-v4-{}.bak",
        path.file_name().unwrap_or_default().to_string_lossy(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(backup)
        .map_err(|error| format!("Cannot back up plugin config: {error}"))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Cannot preserve original plugin config: {error}"))
}

fn persist_preferences(app_data_dir: &Path, preferences: &Preferences) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(preferences).map_err(|error| error.to_string())?;
    let temporary = app_data_dir.join(format!(".{MANAGER_CONFIG_FILE}.tmp"));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("Cannot create temporary plugin config: {error}"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Cannot write temporary plugin config: {error}"))?;
    drop(file);
    atomic_replace(&temporary, &app_data_dir.join(MANAGER_CONFIG_FILE))
        .map_err(|error| format!("Cannot replace plugin config: {error}"))
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

fn scanned_plugin_record(
    app_data_dir: &Path,
    state: &mut PluginManagerState,
    plugin: plugin_package::InspectedPlugin,
    previous_record: Option<&PluginRecord>,
) -> (PluginRecord, bool) {
    let id = plugin.manifest.id.clone();
    let config_is_valid = state.config_error.is_none();
    let mut preferences_changed = false;
    let enabled = if let Some(enabled) = state.preferences.enabled_plugins.get(&id) {
        *enabled && config_is_valid
    } else {
        preferences_changed = true;
        config_is_valid
    };
    state
        .preferences
        .enabled_plugins
        .entry(id.clone())
        .or_insert(enabled);
    state
        .preferences
        .active_versions
        .insert(id.clone(), plugin.manifest.version.clone());

    let installation = if plugin.compatible {
        InstallationState::Installed
    } else {
        InstallationState::Incompatible
    };
    let mut snapshot = PluginRuntimeState {
        manifest: plugin.manifest.clone(),
        lifecycle: None,
        signature: wonderland_plugin_protocol::PluginSignatureState {
            state: if plugin.signature_public_key.is_some() {
                "unconfirmed"
            } else {
                "unsigned"
            }
            .into(),
            public_key: plugin.signature_public_key.clone(),
            expected_public_key: None,
        },
        installation,
        scan_diagnostic: None,
        enabled: enabled && installation == InstallationState::Installed,
        runtime: RuntimeState::Stopped,
        last_error: if installation == InstallationState::Incompatible {
            Some(failure(
                "INCOMPATIBLE_CORE",
                "Plugin does not support this Core version, protocol, platform, or architecture.",
            ))
        } else {
            None
        },
        installation_source: state.preferences.installation_sources.get(&id).cloned(),
        dependency_issues: Vec::new(),
        plugin_data_directory: Some(
            app_data_dir
                .join("plugin-data")
                .join(&id)
                .to_string_lossy()
                .into_owned(),
        ),
    };
    match publisher_key(&state.preferences, &id) {
        Ok(expected) => {
            snapshot.signature.expected_public_key = expected.clone();
            if snapshot.signature.public_key.is_some() && snapshot.signature.public_key == expected
            {
                snapshot.signature.state = "confirmed".into();
            }
        }
        Err(_) => snapshot.signature.state = "identity_unavailable".into(),
    }
    let revision = previous_record
        .map(|record| record.revision.wrapping_add(1))
        .unwrap_or_default();
    let process = previous_record.and_then(|record| {
        (record.directory == plugin.directory
            && record
                .process
                .as_ref()
                .is_some_and(|process| process.is_alive()))
        .then(|| record.process.clone())
        .flatten()
    });
    if process.is_some() {
        snapshot.runtime = RuntimeState::Running;
    }
    (
        PluginRecord {
            snapshot,
            runtime_dependency_warnings: previous_record
                .filter(|record| {
                    record.directory == plugin.directory
                        && record.snapshot.manifest.dependencies == plugin.manifest.dependencies
                })
                .map(|record| record.runtime_dependency_warnings.clone())
                .unwrap_or_default(),
            directory: plugin.directory,
            contract: plugin.contract,
            contract_sha256: plugin.contract_sha256,
            process,
            revision,
            scan_retry: None,
        },
        preferences_changed,
    )
}

fn sanitize_scan_message(app_data_dir: &Path, message: &str) -> String {
    let app_data_dir = app_data_dir.to_string_lossy();
    message.replace(app_data_dir.as_ref(), "<app-data>")
}

fn invalid_snapshot(id: &str, message: &str) -> PluginRuntimeState {
    let id = if valid_plugin_id(id) {
        id.to_owned()
    } else {
        format!(
            "invalid_{}",
            id.chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
                .take(48)
                .collect::<String>()
        )
    };
    PluginRuntimeState {
        manifest: PluginManifest {
            manifest_version: 2,
            id: id.clone(),
            name: format!("无效插件包 ({id})"),
            version: "0.0.0".to_owned(),
            description: None,
            author: None,
            homepage: None,
            license: None,
            icon: None,
            host_compatibility: HostCompatibility {
                min_core_version: "0.0.0".to_owned(),
                max_core_version_exclusive: "999.0.0".to_owned(),
                protocol: ProtocolCompatibility {
                    min_version: "1.0.0".to_owned(),
                    max_version_exclusive: "2.0.0".to_owned(),
                },
            },
            platform: PluginPlatform {
                os: "windows".to_owned(),
                architecture: "x86_64".to_owned(),
                abi: "msvc".to_owned(),
            },
            ui: Some(PluginUi {
                entry: "ui/index.html".to_owned(),
                bridge_compatibility: ProtocolCompatibility {
                    min_version: "1.0.0".to_owned(),
                    max_version_exclusive: "2.0.0".to_owned(),
                },
                integrations: Vec::new(),
                contributions: vec![PluginUiContribution {
                    id: "activity".to_owned(),
                    kind: PluginUiContributionKind::Activity,
                    title: format!("无效插件包 ({id})"),
                    icon: None,
                    default_order: 0,
                    location: None,
                    commands: Vec::new(),
                }],
            }),
            backend: PluginBackend {
                entry: "backend/invalid.exe".to_owned(),
                transport: "stdio-ndjson-v1".to_owned(),
                supports_service_context: None,
                activation: None,
            },
            contract: "contract.json".to_owned(),
            remote_access: None,
            dependencies: Vec::new(),
        },
        installation: InstallationState::Invalid,
        signature: wonderland_plugin_protocol::PluginSignatureState {
            state: "invalid".into(),
            ..Default::default()
        },
        scan_diagnostic: None,
        enabled: false,
        runtime: RuntimeState::Stopped,
        last_error: Some(failure("INVALID_PACKAGE", message)),
        installation_source: None,
        dependency_issues: Vec::new(),
        plugin_data_directory: None,
        lifecycle: None,
    }
}

fn valid_service_method(value: &str) -> bool {
    value.len() <= 128
        && !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_lowercase()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

fn dependency_start_order(snapshots: &[PluginRuntimeState]) -> (Vec<String>, BTreeSet<String>) {
    let nodes: BTreeMap<_, _> = snapshots
        .iter()
        .filter(|snapshot| {
            snapshot.enabled && snapshot.installation == InstallationState::Installed
        })
        .map(|snapshot| {
            (
                snapshot.manifest.id.clone(),
                &snapshot.manifest.dependencies,
            )
        })
        .collect();
    let mut pending: BTreeMap<String, BTreeSet<String>> = nodes
        .iter()
        .map(|(id, dependencies)| {
            (
                id.clone(),
                dependencies
                    .iter()
                    .filter(|dependency| nodes.contains_key(&dependency.plugin_id))
                    .map(|dependency| dependency.plugin_id.clone())
                    .collect(),
            )
        })
        .collect();
    let mut order = Vec::new();
    loop {
        let ready = pending
            .iter()
            .find(|(_, dependencies)| dependencies.is_empty())
            .map(|(id, _)| id.clone());
        let Some(id) = ready else {
            break;
        };
        pending.remove(&id);
        for dependencies in pending.values_mut() {
            dependencies.remove(&id);
        }
        order.push(id);
    }
    let unresolved: BTreeSet<_> = pending.keys().cloned().collect();
    order.extend(unresolved.iter().cloned());
    (order, unresolved)
}

fn update_dependency_issues(plugins: &mut BTreeMap<String, PluginRecord>) {
    let (_, cycles) = dependency_start_order(
        &plugins
            .values()
            .map(|record| record.snapshot.clone())
            .collect::<Vec<_>>(),
    );
    let issues = plugins
        .iter()
        .map(|(id, record)| {
            let mut issues = record.runtime_dependency_warnings.clone();
            if cycles.contains(id) {
                issues.push(
                    "Dependency startup order contains a cycle; independent startup is used."
                        .into(),
                );
            }
            for dependency in &record.snapshot.manifest.dependencies {
                let label = if dependency.optional {
                    "Optional"
                } else {
                    "Required"
                };
                match plugins.get(&dependency.plugin_id) {
                    None => issues.push(format!(
                        "{label} plugin '{}' is missing.",
                        dependency.plugin_id
                    )),
                    Some(target) => {
                        if !target.snapshot.enabled
                            || target.snapshot.installation != InstallationState::Installed
                        {
                            issues.push(format!(
                                "{label} plugin '{}' is unavailable.",
                                dependency.plugin_id
                            ));
                        }
                        if !Version::parse(&target.snapshot.manifest.version)
                            .ok()
                            .zip(VersionReq::parse(&dependency.version_req).ok())
                            .is_some_and(|(version, range)| range.matches(&version))
                        {
                            issues.push(format!(
                                "Plugin '{}' version does not match {}.",
                                dependency.plugin_id, dependency.version_req
                            ));
                        }
                        for method in &dependency.methods {
                            if target
                                .contract
                                .get("methods")
                                .and_then(Value::as_object)
                                .is_none_or(|methods| !methods.contains_key(method))
                            {
                                issues.push(format!(
                                    "Plugin '{}' has no method '{}'.",
                                    dependency.plugin_id, method
                                ));
                            }
                        }
                    }
                }
            }
            (id.clone(), issues)
        })
        .collect::<BTreeMap<_, _>>();
    for (id, record) in plugins.iter_mut() {
        record.snapshot.dependency_issues = issues.get(id).cloned().unwrap_or_default();
    }
}

fn failure(code: &str, message: &str) -> PluginFailure {
    PluginFailure {
        code: code.to_owned(),
        message: message.to_owned(),
        occurred_at: Utc::now().to_rfc3339(),
    }
}

fn safe_display_id(value: &str, index: usize) -> String {
    if valid_plugin_id(value) {
        value.to_owned()
    } else {
        format!("invalid_{index}")
    }
}

fn valid_plugin_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

fn content_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
    .to_owned()
}

pub fn plugin_asset_response(
    app: &AppHandle,
    path: &str,
    method: &str,
) -> tauri::http::Response<Vec<u8>> {
    let manager = app.try_state::<PluginManager>();
    plugin_asset_response_for_manager(manager.as_deref(), path, method)
}

fn plugin_asset_response_for_manager(
    manager: Option<&PluginManager>,
    path: &str,
    method: &str,
) -> tauri::http::Response<Vec<u8>> {
    let (status, mime, body) = match manager {
        Some(manager) => manager.read_plugin_asset(path, method),
        None => (
            503,
            "text/plain; charset=utf-8".to_owned(),
            b"plugin manager unavailable".to_vec(),
        ),
    };
    let status = tauri::http::StatusCode::from_u16(status)
        .unwrap_or(tauri::http::StatusCode::INTERNAL_SERVER_ERROR);
    let plugin_id = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    tauri::http::Response::builder()
        .status(status)
        .header(tauri::http::header::CONTENT_TYPE, mime)
        .header(tauri::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(tauri::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(tauri::http::header::CACHE_CONTROL, "no-store")
        .header(
            "Content-Security-Policy",
            crate::ui_policy::plugin_csp(plugin_id, None),
        )
        .body(body)
        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
}

fn reject_symlink(path: &Path) -> Result<(), PluginError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| plugin_error("INTERNAL", &error.to_string()))?;
    if is_reparse_metadata(&metadata) || !metadata.is_dir() {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Refusing to use a non-directory plugin path or a path that is a symbolic link, junction, or reparse point.",
        ));
    }
    Ok(())
}

fn not_installed(plugin_id: &str) -> PluginError {
    plugin_error(
        "NOT_INSTALLED",
        &format!("Plugin '{plugin_id}' is not installed."),
    )
}

fn plugin_error(code: &str, message: &str) -> PluginError {
    PluginError {
        code: code.to_owned(),
        message: message.to_owned(),
        details: None,
    }
}

fn ensure_main(window: &WebviewWindow) -> Result<(), PluginError> {
    crate::commands::ensure_main_window(window).map_err(|error| PluginError {
        code: "UNAUTHORIZED".to_owned(),
        message: error.to_string(),
        details: None,
    })
}

fn install_source_changed(
    previous: Option<&PluginInstallSource>,
    next: &PluginInstallSource,
) -> bool {
    previous.is_none_or(|previous| {
        previous.kind != next.kind || !previous.origin.eq_ignore_ascii_case(&next.origin)
    })
}

fn check_install_source(
    previous_version: Option<&str>,
    previous_source: Option<&PluginInstallSource>,
    next: &PluginInstallSource,
    approved_source_change: bool,
) -> Result<bool, PluginError> {
    let changed = previous_version.is_some() && install_source_changed(previous_source, next);
    if changed && !approved_source_change {
        return Err(plugin_error(
            "SOURCE_CHANGE_APPROVAL_REQUIRED",
            "The installed plugin has a different or unrecorded source. Review and confirm the new source before replacing it.",
        ));
    }
    Ok(changed)
}

fn plugin_package_removal_error(path: &Path, error: std::io::Error) -> PluginError {
    let message = format!("Cannot remove plugin files at {}: {error}", path.display());
    #[cfg(windows)]
    if error.raw_os_error() == Some(5) {
        return plugin_error(
            "INTERNAL",
            &format!(
                "{message} The plugin may still be in use by an external MCP client. Close the AI client that launched this plugin's MCP server, then retry uninstalling. The plugin remains installed but disabled, and its local data has not been removed."
            ),
        );
    }
    plugin_error("INTERNAL", &message)
}

fn empty_publisher_identities() -> Value {
    json!({})
}

fn publisher_key(preferences: &Preferences, id: &str) -> Result<Option<String>, PluginError> {
    let records = preferences
        .publisher_identities
        .as_object()
        .ok_or_else(|| {
            plugin_error(
                "PUBLISHER_IDENTITY_UNAVAILABLE",
                "Original publisher records are damaged; they were retained.",
            )
        })?;
    let Some(value) = records.get(id) else {
        return Ok(None);
    };
    let key = value
        .as_str()
        .filter(|key| {
            key.len() == 64
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| {
            plugin_error(
                "PUBLISHER_IDENTITY_UNAVAILABLE",
                "The original publisher identity is damaged; it was retained.",
            )
        })?;
    Ok(Some(key.to_owned()))
}

fn package_signature_error(message: &str) -> PluginError {
    let code = match message.split(':').next().unwrap_or_default() {
        "INVALID_PACKAGE_SIGNATURE" => "INVALID_PACKAGE_SIGNATURE",
        "UNSUPPORTED_PACKAGE_SIGNATURE" => "UNSUPPORTED_PACKAGE_SIGNATURE",
        _ => "INVALID_REQUEST",
    };
    plugin_error(code, message)
}

fn clear_plugin_preferences(preferences: &mut Preferences, plugin_id: &str) {
    preferences.enabled_plugins.remove(plugin_id);
    preferences.active_versions.remove(plugin_id);
    preferences.installation_sources.remove(plugin_id);
}

fn apply_plugin_data_removal(path: &Path, remove_data: bool) -> Result<(), PluginError> {
    if !remove_data {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(plugin_error(
                "INTERNAL",
                &format!("Cannot inspect plugin data at {}: {error}", path.display()),
            ));
        }
    };
    if is_reparse_metadata(&metadata) {
        return Err(plugin_error(
            "UNSAFE_PATH",
            &format!(
                "Refusing to remove plugin data through a symbolic link, junction, or reparse point: {}",
                path.display()
            ),
        ));
    }
    if !metadata.is_dir() {
        return Err(plugin_error(
            "UNSAFE_PATH",
            &format!("Plugin data path is not a directory: {}", path.display()),
        ));
    }
    reject_reparse_points_below(path)?;
    fs::remove_dir_all(path).map_err(|error| {
        plugin_error(
            "INTERNAL",
            &format!("Cannot remove plugin data at {}: {error}", path.display()),
        )
    })
}

fn reject_reparse_points_below(root: &Path) -> Result<(), PluginError> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| {
            plugin_error(
                "INTERNAL",
                &format!(
                    "Cannot inspect plugin data at {}: {error}",
                    directory.display()
                ),
            )
        })? {
            let entry = entry.map_err(|error| plugin_error("INTERNAL", &error.to_string()))?;
            let child = entry.path();
            let metadata = fs::symlink_metadata(&child)
                .map_err(|error| plugin_error("INTERNAL", &error.to_string()))?;
            if is_reparse_metadata(&metadata) {
                return Err(plugin_error(
                    "UNSAFE_PATH",
                    &format!(
                        "Refusing to remove plugin data containing a symbolic link, junction, or reparse point: {}",
                        child.display()
                    ),
                ));
            }
            if metadata.is_dir() {
                pending.push(child);
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_metadata(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_metadata(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn restore_map_entry<T>(map: &mut BTreeMap<String, T>, key: &str, previous: Option<T>) {
    if let Some(value) = previous {
        map.insert(key.to_owned(), value);
    } else {
        map.remove(key);
    }
}
