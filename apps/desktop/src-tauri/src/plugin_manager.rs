//! Generic plugin host commands and lifecycle state.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
#[cfg(debug_assertions)]
use std::time::{Duration, Instant};

use chrono::Utc;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use wonderland_kernel::AppContext;
use wonderland_kernel::logging::warn;
use wonderland_plugin_protocol::{
    HostCompatibility, InstallationState, PluginBackend, PluginError, PluginFailure,
    PluginInstallSource, PluginManifest, PluginPlatform, PluginRuntimeState, PluginService,
    PluginServiceRequirement, PluginServiceResolution, PluginUi, PluginUiCommand,
    PluginUiCommandEffect, PluginUiContribution, PluginUiContributionKind, ProtocolCompatibility,
    RuntimeState,
};

use crate::{plugin_package, plugin_runtime::PluginProcess};

const MANAGER_CONFIG_FILE: &str = "plugin-manager.json";
const LEGACY_CONFIG_FILE: &str = "plugins.json";
const CATALOG_CACHE_FILE: &str = "plugin-catalog-v1.json";
const CATALOG_MAX_BYTES: usize = 2 * 1024 * 1024;
const CATALOG_MAX_ITEMS: usize = 500;
const PLUGIN_PACKAGE_MAX_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginCatalogIndex {
    schema_version: u32,
    generated_at: String,
    plugins: Vec<PluginCatalogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginCatalogEntry {
    id: String,
    name: String,
    description: String,
    author: String,
    repository_url: String,
    version: String,
    release_notes_url: Option<String>,
    download_url: String,
    sha256: String,
    size_bytes: u64,
    host_compatibility: HostCompatibility,
    ui_bridge_compatibility: Option<ProtocolCompatibility>,
    platform: PluginPlatform,
    capabilities: Vec<String>,
    #[serde(default)]
    provides: Vec<PluginService>,
    #[serde(default)]
    requires: Vec<PluginServiceRequirement>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCatalogSnapshot {
    generated_at: String,
    stale: bool,
    plugins: Vec<PluginCatalogItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginCatalogItem {
    entry: PluginCatalogEntry,
    compatible: bool,
    installed_version: Option<String>,
    installable: bool,
}

#[derive(Clone)]
pub struct PluginManager {
    inner: Arc<PluginManagerInner>,
}

struct PluginManagerInner {
    app_data_dir: PathBuf,
    documents_dir: PathBuf,
    installed_root: PathBuf,
    app_handle: AppHandle,
    next_service_request_id: AtomicU64,
    state: Mutex<PluginManagerState>,
}

struct PluginManagerState {
    preferences: Preferences,
    config_error: Option<String>,
    plugins: BTreeMap<String, PluginRecord>,
}

struct PluginRecord {
    snapshot: PluginRuntimeState,
    directory: PathBuf,
    contract: Value,
    contract_sha256: String,
    process: Option<Arc<PluginProcess>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preferences {
    schema_version: u32,
    #[serde(default)]
    enabled_plugins: BTreeMap<String, bool>,
    #[serde(default)]
    granted_capabilities: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    active_versions: BTreeMap<String, String>,
    #[serde(default)]
    installation_sources: BTreeMap<String, PluginInstallSource>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 3,
            enabled_plugins: BTreeMap::new(),
            granted_capabilities: BTreeMap::new(),
            active_versions: BTreeMap::new(),
            installation_sources: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize)]
struct LegacyPreferences {
    #[serde(default)]
    disabled_plugins: Vec<String>,
}

impl PluginManager {
    pub fn load(context: &AppContext, app_handle: AppHandle) -> Result<Self, PluginError> {
        let app_data_dir = context.app_data_dir.clone();
        let documents_dir = context.documents_dir.clone();
        let installed_root = app_data_dir.join("installed-plugins");
        fs::create_dir_all(&installed_root).map_err(|error| {
            plugin_error(
                "INTERNAL",
                &format!("Cannot create plugin installation directory: {error}"),
            )
        })?;
        let (preferences, config_error) = load_preferences(&app_data_dir);
        let manager = Self {
            inner: Arc::new(PluginManagerInner {
                app_data_dir,
                documents_dir,
                installed_root,
                app_handle,
                next_service_request_id: AtomicU64::new(1),
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

    /// Starts enabled, compatible plugins independently so one slow or broken plugin does not
    /// block the desktop window from opening.
    pub fn start_enabled(&self) {
        let ids = self
            .snapshots()
            .into_iter()
            .filter(|state| {
                state.enabled
                    && state.installation == InstallationState::Installed
                    && state.service_dependency_issues.is_empty()
            })
            .map(|state| state.manifest.id)
            .collect::<Vec<_>>();
        for plugin_id in ids {
            let manager = self.clone();
            let _ = thread::Builder::new()
                .name(format!("plugin-{plugin_id}-start"))
                .spawn(move || {
                    if let Err(error) = manager.inner.start_one(&plugin_id) {
                        warn!(plugin_id = %plugin_id, code = %error.code, "插件启动失败");
                    }
                });
        }
    }

    fn ensure_cli_plugin_started(&self, plugin_id: &str) -> Result<(), PluginError> {
        self.inner.start_one(plugin_id)
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
        let active_versions = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?
            .preferences
            .active_versions
            .clone();
        let scanned =
            plugin_package::scan_installed_root(&self.inner.installed_root, &active_versions);
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let old = std::mem::take(&mut state.plugins);
        let mut next = BTreeMap::new();
        let mut preferences_changed = false;
        let activate_legacy_defaults = state.preferences.schema_version < 2;
        if state.preferences.schema_version < 3 {
            let old_grants = std::mem::take(&mut state.preferences.granted_capabilities);
            state.preferences.granted_capabilities = migrate_capability_grants(old_grants);
            state.preferences.schema_version = 3;
            preferences_changed = true;
        }

        for item in scanned {
            match item {
                Ok(plugin) => {
                    let id = plugin.manifest.id.clone();
                    let version = plugin.manifest.version.clone();
                    let enabled = if let Some(enabled) = state.preferences.enabled_plugins.get(&id)
                    {
                        *enabled && state.config_error.is_none()
                    } else {
                        preferences_changed = true;
                        state.config_error.is_none()
                    };
                    state
                        .preferences
                        .enabled_plugins
                        .entry(id.clone())
                        .or_insert(enabled);
                    state
                        .preferences
                        .active_versions
                        .insert(id.clone(), version);
                    let requested = plugin.manifest.capabilities.iter().collect::<BTreeSet<_>>();
                    let grant_key = capability_grant_key(&id);
                    let (granted, grant_added) =
                        match state.preferences.granted_capabilities.entry(grant_key) {
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                (entry.insert(plugin.manifest.capabilities.clone()), true)
                            }
                            std::collections::btree_map::Entry::Occupied(entry) => {
                                let granted = entry.into_mut();
                                if activate_legacy_defaults && granted.is_empty() {
                                    *granted = plugin.manifest.capabilities.clone();
                                    (granted, true)
                                } else {
                                    (granted, false)
                                }
                            }
                        };
                    preferences_changed |= grant_added;
                    let original_len = granted.len();
                    granted.retain(|capability| requested.contains(capability));
                    preferences_changed |= original_len != granted.len();

                    let installation = if plugin.compatible {
                        InstallationState::Installed
                    } else {
                        InstallationState::Incompatible
                    };
                    let mut snapshot = PluginRuntimeState {
                        manifest: plugin.manifest.clone(),
                        installation,
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
                        granted_capabilities: granted.clone(),
                        installation_source: state
                            .preferences
                            .installation_sources
                            .get(&id)
                            .cloned(),
                        service_dependency_issues: Vec::new(),
                        plugin_data_directory: Some(
                            self.inner
                                .app_data_dir
                                .join("plugin-data")
                                .join(&id)
                                .to_string_lossy()
                                .into_owned(),
                        ),
                    };
                    let process = old.get(&id).and_then(|record| {
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
                    next.insert(
                        id,
                        PluginRecord {
                            snapshot,
                            directory: plugin.directory,
                            contract: plugin.contract,
                            contract_sha256: plugin.contract_sha256,
                            process,
                        },
                    );
                }
                Err((id, message)) => {
                    let id = safe_display_id(&id, next.len());
                    let snapshot = invalid_snapshot(&id, &message);
                    next.insert(
                        id,
                        PluginRecord {
                            snapshot,
                            directory: PathBuf::new(),
                            contract: Value::Null,
                            contract_sha256: String::new(),
                            process: None,
                        },
                    );
                }
            }
        }
        state.plugins = next;
        update_service_dependency_issues(&mut state.plugins);
        if preferences_changed {
            persist_preferences(&self.inner.app_data_dir, &state.preferences).map_err(|error| {
                plugin_error("INTERNAL", &format!("Cannot save plugin state: {error}"))
            })?;
            state.config_error = None;
        }
        Ok(())
    }

    fn snapshots(&self) -> Vec<PluginRuntimeState> {
        let Ok(mut state) = self.inner.state.lock() else {
            return Vec::new();
        };
        for record in state.plugins.values_mut() {
            if record.snapshot.runtime == RuntimeState::Running
                && !record
                    .process
                    .as_ref()
                    .is_some_and(|process| process.is_alive())
            {
                record.snapshot.runtime = RuntimeState::Failed;
                record.snapshot.last_error = Some(failure(
                    "PLUGIN_CRASHED",
                    "Plugin process exited unexpectedly.",
                ));
                record.process = None;
            }
        }
        state
            .plugins
            .values()
            .map(|record| record.snapshot.clone())
            .collect()
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
            return Err(plugin_error(
                "INVALID_REQUEST",
                "The configured development plugin directory does not exist.",
            ));
        }
        let manifest = plugin_package::manifest_for_install_source(&source)
            .map_err(|message| plugin_error("INVALID_REQUEST", &message))?;
        self.install(source, true, true)?;
        self.set_enabled(&manifest.id, true)
    }

    fn install_cli_package(
        &self,
        source: PathBuf,
        approved_capabilities: Vec<String>,
        overwrite: bool,
        approve_source_change: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        if source.is_dir() && !cfg!(debug_assertions) {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Release CLI installs require a validated .wplug archive.",
            ));
        }
        let manifest = plugin_package::manifest_for_install_source(&source)
            .map_err(|message| plugin_error("INVALID_REQUEST", &message))?;
        let approved = approved_capabilities.iter().collect::<BTreeSet<_>>();
        if approved.len() != approved_capabilities.len() {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Capability grants must not contain duplicates.",
            ));
        }
        let requested = manifest.capabilities.iter().collect::<BTreeSet<_>>();
        if approved
            .iter()
            .any(|capability| !requested.contains(capability))
        {
            return Err(plugin_error(
                "UNAUTHORIZED",
                "A requested CLI grant is not declared by this plugin.",
            ));
        }
        let origin = source.canonicalize().map_err(|error| {
            plugin_error(
                "INVALID_REQUEST",
                &format!("Cannot resolve local package: {error}"),
            )
        })?;
        let install_path = plugin_package::install_path(&self.inner.installed_root, &manifest);
        if install_path.exists() && !overwrite {
            return Err(plugin_error(
                "ALREADY_INSTALLED",
                "This plugin version is already installed. Pass --overwrite to replace it.",
            ));
        }
        self.install_with_capabilities(
            source,
            overwrite,
            Some(&approved_capabilities),
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
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let manifest = plugin_package::manifest_for_install_source(&source)
            .map_err(|message| plugin_error("INVALID_REQUEST", &message))?;
        let manifest_capabilities = manifest
            .capabilities
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let catalog_capabilities = entry.capabilities.iter().cloned().collect::<BTreeSet<_>>();
        let manifest_matches = manifest.id == entry.id
            && manifest.name == entry.name
            && manifest.version == entry.version
            && manifest_capabilities == catalog_capabilities
            && manifest.provides == entry.provides
            && manifest.requires == entry.requires
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
            return Err(plugin_error(
                "CATALOG_MISMATCH",
                "The downloaded package metadata does not match the reviewed catalog entry.",
            ));
        }
        if !plugin_package::manifest_is_compatible(&manifest) {
            return Err(plugin_error(
                "INCOMPATIBLE_CORE",
                "This plugin version is incompatible with the current Core.",
            ));
        }
        self.install_with_capabilities(
            source,
            true,
            Some(&entry.capabilities),
            PluginInstallSource {
                kind: "catalog".to_owned(),
                origin: entry.repository_url.clone(),
                author: Some(entry.author.clone()),
            },
            approved_source_change,
        )
    }

    fn install(
        &self,
        source: PathBuf,
        overwrite: bool,
        approved_source_change: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let origin = source.canonicalize().map_err(|error| {
            plugin_error(
                "INVALID_REQUEST",
                &format!("Cannot resolve local package: {error}"),
            )
        })?;
        self.install_with_capabilities(
            source,
            overwrite,
            None,
            PluginInstallSource {
                kind: "local".to_owned(),
                origin: origin.to_string_lossy().into_owned(),
                author: None,
            },
            approved_source_change,
        )
    }

    fn install_with_capabilities(
        &self,
        source: PathBuf,
        overwrite: bool,
        approved_capabilities: Option<&[String]>,
        install_source: PluginInstallSource,
        approved_source_change: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let source_manifest = plugin_package::manifest_for_install_source(&source)
            .map_err(|message| plugin_error("INVALID_REQUEST", &message))?;
        let replacing_same_version = overwrite
            && plugin_package::install_path(&self.inner.installed_root, &source_manifest).exists();
        let (previous_version, previous_source, previous_grants) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let preferences = &state.preferences;
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
                    .granted_capabilities
                    .get(&capability_grant_key(&source_manifest.id))
                    .cloned(),
            )
        };
        let source_changed = check_install_source(
            previous_version.as_deref(),
            previous_source.as_ref(),
            &install_source,
            approved_source_change,
        )?;
        let replacing_active_version = replacing_same_version
            && previous_version.as_deref() == Some(source_manifest.version.as_str());
        if replacing_active_version && !plugin_package::manifest_is_compatible(&source_manifest) {
            return Err(plugin_error(
                "INCOMPATIBLE_CORE",
                "The selected package cannot replace the active plugin because it is incompatible with this Core.",
            ));
        }
        let was_running = if replacing_active_version {
            self.stop_for_reinstall(&source_manifest.id)?
        } else {
            false
        };
        let inspected_result = if source.is_dir() {
            plugin_package::install_development_directory(
                &source,
                &self.inner.installed_root,
                overwrite,
            )
        } else {
            plugin_package::install_archive(&source, &self.inner.installed_root, overwrite)
        };
        let inspected = match inspected_result {
            Ok(inspected) => inspected,
            Err(message) => {
                if was_running {
                    let _ = self.inner.start_one(&source_manifest.id);
                }
                return Err(plugin_error("INVALID_REQUEST", &message));
            }
        };
        let plugin_id = inspected.manifest.id.clone();
        let version = inspected.manifest.version.clone();
        let grant_key = capability_grant_key(&plugin_id);
        if !inspected.compatible && previous_version.is_some() {
            return Err(plugin_error(
                "INCOMPATIBLE_CORE",
                "Plugin package was installed but not activated because it is incompatible with this Core.",
            ));
        }
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            state
                .preferences
                .active_versions
                .insert(plugin_id.clone(), version);
            state
                .preferences
                .installation_sources
                .insert(plugin_id.clone(), install_source);
            state
                .preferences
                .enabled_plugins
                .entry(plugin_id.clone())
                .or_insert(true);
            state.preferences.granted_capabilities.insert(
                grant_key,
                select_install_grants(
                    previous_grants.as_deref(),
                    &inspected.manifest.capabilities,
                    approved_capabilities,
                    source_changed || previous_version.is_none(),
                ),
            );
            persist_preferences(&self.inner.app_data_dir, &state.preferences).map_err(|error| {
                plugin_error(
                    "INTERNAL",
                    &format!("Plugin installed but preferences could not be saved: {error}"),
                )
            })?;
            state.config_error = None;
        }
        self.refresh()?;
        let should_start = self.snapshots().iter().any(|snapshot| {
            snapshot.manifest.id == plugin_id
                && snapshot.installation == InstallationState::Installed
                && snapshot.enabled
                && snapshot.service_dependency_issues.is_empty()
        });
        if should_start
            && let Err(error) = self.inner.start_one(&plugin_id)
            && let Some(previous_version) = previous_version
        {
            let rollback = (|| {
                let mut state = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
                state
                    .preferences
                    .active_versions
                    .insert(plugin_id.clone(), previous_version);
                restore_map_entry(
                    &mut state.preferences.installation_sources,
                    &plugin_id,
                    previous_source,
                );
                restore_map_entry(
                    &mut state.preferences.granted_capabilities,
                    &plugin_id,
                    previous_grants,
                );
                persist_preferences(&self.inner.app_data_dir, &state.preferences).map_err(
                    |save_error| {
                        plugin_error(
                            "INTERNAL",
                            &format!("Could not persist plugin rollback: {save_error}"),
                        )
                    },
                )?;
                drop(state);
                self.refresh()?;
                self.inner.start_one(&plugin_id)
            })();
            if let Err(rollback_error) = rollback {
                return Err(plugin_error(
                    "PLUGIN_START_FAILED",
                    &format!(
                        "New plugin version failed to start ({}); rollback also failed ({}).",
                        error.message, rollback_error.message
                    ),
                ));
            }
            // Keep the installed package available for inspection and recovery.
            return Err(error);
        }
        Ok(self.snapshots())
    }

    fn stop_for_reinstall(&self, plugin_id: &str) -> Result<bool, PluginError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let record = state
            .plugins
            .get_mut(plugin_id)
            .ok_or_else(|| not_installed(plugin_id))?;
        let was_running = record.process.is_some();
        if was_running {
            record.snapshot.runtime = RuntimeState::Stopping;
            record.process = None;
            record.snapshot.runtime = RuntimeState::Stopped;
        }
        Ok(was_running)
    }

    fn set_enabled(
        &self,
        plugin_id: &str,
        enabled: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        {
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
                    "Plugin is stopping and cannot change state yet.",
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
            state
                .preferences
                .enabled_plugins
                .insert(plugin_id.to_owned(), enabled);
            persist_preferences(&self.inner.app_data_dir, &state.preferences).map_err(|error| {
                plugin_error("INTERNAL", &format!("Cannot save plugin state: {error}"))
            })?;
            state.config_error = None;
            let record = state
                .plugins
                .get_mut(plugin_id)
                .expect("record checked above");
            record.snapshot.enabled = enabled;
            if !enabled {
                record.snapshot.runtime = RuntimeState::Stopping;
                record.process = None;
                record.snapshot.runtime = RuntimeState::Stopped;
                record.snapshot.last_error = None;
            }
            update_service_dependency_issues(&mut state.plugins);
        }
        if enabled {
            let can_start = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?
                .plugins
                .get(plugin_id)
                .is_some_and(|record| record.snapshot.service_dependency_issues.is_empty());
            if can_start {
                self.inner.start_one(plugin_id)?;
            }
        }
        Ok(self.snapshots())
    }

    fn set_capabilities(
        &self,
        plugin_id: &str,
        capabilities: Vec<String>,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let unique = capabilities.iter().collect::<BTreeSet<_>>();
        if unique.len() != capabilities.len() {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Duplicate capability grant.",
            ));
        }
        {
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
                    "Plugin is stopping and cannot change capabilities yet.",
                ));
            }
            let grant_key = capability_grant_key(plugin_id);
            let requested = record
                .snapshot
                .manifest
                .capabilities
                .iter()
                .collect::<BTreeSet<_>>();
            if capabilities
                .iter()
                .any(|capability| !requested.contains(capability))
            {
                return Err(plugin_error(
                    "UNAUTHORIZED",
                    "A capability was not requested by this plugin.",
                ));
            }
            state
                .preferences
                .granted_capabilities
                .insert(grant_key, capabilities.clone());
            persist_preferences(&self.inner.app_data_dir, &state.preferences).map_err(|error| {
                plugin_error(
                    "INTERNAL",
                    &format!("Cannot save capability grants: {error}"),
                )
            })?;
            state.config_error = None;
            let record = state
                .plugins
                .get_mut(plugin_id)
                .expect("record checked above");
            record.snapshot.granted_capabilities = capabilities;
            if record.process.take().is_some() {
                record.snapshot.runtime = RuntimeState::Stopping;
                record.snapshot.runtime = RuntimeState::Stopped;
            }
        }
        let enabled = self.snapshots().iter().any(|snapshot| {
            snapshot.manifest.id == plugin_id
                && snapshot.enabled
                && snapshot.service_dependency_issues.is_empty()
        });
        if enabled {
            self.inner.start_one(plugin_id)?;
        }
        Ok(self.snapshots())
    }

    fn remove(
        &self,
        plugin_id: &str,
        remove_plugin_data: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let process = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            if !state.plugins.contains_key(plugin_id) {
                return Err(not_installed(plugin_id));
            }
            if matches!(
                state.plugins[plugin_id].snapshot.runtime,
                RuntimeState::Starting | RuntimeState::Stopping
            ) {
                return Err(plugin_error(
                    "PLUGIN_BUSY",
                    "Wait for the plugin to finish starting or stopping before removing it.",
                ));
            }
            let previous_preferences = state.preferences.clone();
            clear_plugin_preferences(&mut state.preferences, plugin_id);
            state
                .preferences
                .enabled_plugins
                .insert(plugin_id.to_owned(), false);
            state
                .preferences
                .granted_capabilities
                .insert(capability_grant_key(plugin_id), Vec::new());
            if let Err(error) = persist_preferences(&self.inner.app_data_dir, &state.preferences) {
                state.preferences = previous_preferences;
                return Err(plugin_error(
                    "INTERNAL",
                    &format!("Cannot prepare plugin removal safely: {error}"),
                ));
            }
            let record = state
                .plugins
                .get_mut(plugin_id)
                .expect("record checked above");
            record.snapshot.enabled = false;
            record.snapshot.granted_capabilities.clear();
            record.snapshot.runtime = RuntimeState::Stopping;
            let process = record.process.take();
            update_service_dependency_issues(&mut state.plugins);
            process
        };

        if let Some(process) = process
            && let Err(error) = process.stop()
        {
            if let Ok(mut state) = self.inner.state.lock()
                && let Some(record) = state.plugins.get_mut(plugin_id)
            {
                record.snapshot.runtime = RuntimeState::Failed;
                record.snapshot.last_error = Some(failure(&error.code, &error.message));
            }
            return Err(error);
        }

        if let Ok(mut state) = self.inner.state.lock()
            && let Some(record) = state.plugins.get_mut(plugin_id)
        {
            record.snapshot.runtime = RuntimeState::Stopped;
            record.snapshot.last_error = None;
        }

        if remove_plugin_data {
            let data_directory = self.inner.app_data_dir.join("plugin-data").join(plugin_id);
            apply_plugin_data_removal(&data_directory, true)?;
        }

        let install_directory = self.inner.installed_root.join(plugin_id);
        if install_directory.exists() {
            reject_symlink(&install_directory)?;
            fs::remove_dir_all(&install_directory).map_err(|error| {
                plugin_error(
                    "INTERNAL",
                    &format!(
                        "Cannot remove plugin files at {}: {error}",
                        install_directory.display()
                    ),
                )
            })?;
        }

        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        state.plugins.remove(plugin_id);
        update_service_dependency_issues(&mut state.plugins);
        let removal_preferences = state.preferences.clone();
        clear_plugin_preferences(&mut state.preferences, plugin_id);
        if let Err(error) = persist_preferences(&self.inner.app_data_dir, &state.preferences) {
            // The already-persisted removal intent has no grants and cannot silently authorize a
            // later package with the same ID. Keep it if final preference cleanup fails.
            state.preferences = removal_preferences;
            state.config_error = Some(format!("Plugin removal preferences need cleanup: {error}"));
            warn!(plugin_id = %plugin_id, reason = %error, "插件已移除，但授权记录的最终清理未完成");
        } else {
            state.config_error = None;
        }
        Ok(self.snapshots())
    }

    fn call(
        &self,
        plugin_id: &str,
        request_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginError> {
        let (process, contract, enabled) = {
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
                return Err(plugin_error("NOT_INSTALLED", "Plugin package is invalid."));
            }
            if !record.snapshot.service_dependency_issues.is_empty() {
                return Err(plugin_error(
                    "SERVICE_DEPENDENCY_MISSING",
                    &record.snapshot.service_dependency_issues.join(" "),
                ));
            }
            (
                record.process.clone(),
                record.contract.clone(),
                record.snapshot.enabled,
            )
        };
        if !enabled {
            return Err(plugin_error("DISABLED", "Plugin is disabled."));
        }
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
        let params_schema = method_contract
            .get("params")
            .ok_or_else(|| plugin_error("INTERNAL", "Plugin contract is invalid."))?;
        if let Err(error) = crate::plugin_schema::validate(&params, params_schema, &contract) {
            return Err(plugin_error("INVALID_INPUT", &error));
        }
        let timeout_ms = method_contract
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .ok_or_else(|| plugin_error("INTERNAL", "Plugin method timeout is invalid."))?;
        let process = process
            .filter(|process| process.is_alive())
            .ok_or_else(|| plugin_error("NOT_RUNNING", "Plugin backend is not running."))?;
        let result = process.call(request_id, method, params, timeout_ms)?;
        let result_schema = method_contract
            .get("result")
            .ok_or_else(|| plugin_error("INTERNAL", "Plugin contract is invalid."))?;
        if let Err(error) = crate::plugin_schema::validate(&result, result_schema, &contract) {
            return Err(plugin_error(
                "INVALID_RESPONSE",
                &format!("Plugin result failed contract validation: {error}"),
            ));
        }
        Ok(result)
    }

    pub(crate) fn resolve_service(
        &self,
        consumer_id: &str,
        service_id: &str,
    ) -> Result<PluginServiceResolution, PluginError> {
        self.service_target(consumer_id, service_id)
            .map(|(resolution, _, _)| resolution)
    }

    pub(crate) fn invoke_service(
        &self,
        consumer_id: &str,
        service_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginError> {
        if !valid_service_id(service_id) || !valid_service_method(method) {
            return Err(plugin_error(
                "INVALID_INPUT",
                "Service ID or method is invalid.",
            ));
        }
        let (resolution, process, contract) = self.service_target(consumer_id, service_id)?;
        if !resolution.methods.iter().any(|allowed| allowed == method) {
            return Err(plugin_error(
                "UNAUTHORIZED",
                "The requested method is not declared by the consumer plugin.",
            ));
        }
        let method_contract = contract
            .get("methods")
            .and_then(Value::as_object)
            .and_then(|methods| methods.get(method))
            .ok_or_else(|| {
                plugin_error(
                    "SERVICE_METHOD_NOT_FOUND",
                    "The provider no longer declares this service method.",
                )
            })?;
        let params_schema = method_contract
            .get("params")
            .ok_or_else(|| plugin_error("INVALID_RESPONSE", "Provider contract is invalid."))?;
        if let Err(error) = crate::plugin_schema::validate(&params, params_schema, &contract) {
            return Err(plugin_error("INVALID_INPUT", &error));
        }
        let timeout_ms = method_contract
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .filter(|timeout| (1..=30_000).contains(timeout))
            .ok_or_else(|| {
                plugin_error(
                    "INVALID_RESPONSE",
                    "Provider service timeout is outside the supported range.",
                )
            })?;
        let request_id = format!(
            "service-{}",
            self.inner
                .next_service_request_id
                .fetch_add(1, Ordering::Relaxed)
        );
        let result = process
            .call(&request_id, method, params, timeout_ms)
            .map_err(service_provider_error)?;
        let result_schema = method_contract
            .get("result")
            .ok_or_else(|| plugin_error("INVALID_RESPONSE", "Provider contract is invalid."))?;
        if let Err(error) = crate::plugin_schema::validate(&result, result_schema, &contract) {
            return Err(plugin_error(
                "INVALID_RESPONSE",
                &format!("Provider result failed contract validation: {error}"),
            ));
        }
        Ok(result)
    }

    fn service_target(
        &self,
        consumer_id: &str,
        service_id: &str,
    ) -> Result<(PluginServiceResolution, Arc<PluginProcess>, Value), PluginError> {
        if !valid_service_id(service_id) {
            return Err(plugin_error("INVALID_INPUT", "Service ID is invalid."));
        }
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let consumer = state
            .plugins
            .get(consumer_id)
            .ok_or_else(|| not_installed(consumer_id))?;
        if consumer.snapshot.installation != InstallationState::Installed {
            return Err(plugin_error(
                "INCOMPATIBLE_CORE",
                "Consumer plugin is not compatible with this Core.",
            ));
        }
        if !consumer.snapshot.enabled {
            return Err(plugin_error("DISABLED", "Consumer plugin is disabled."));
        }
        if consumer.snapshot.runtime != RuntimeState::Running
            || !consumer
                .process
                .as_ref()
                .is_some_and(|process| process.is_alive())
        {
            return Err(plugin_error(
                "NOT_RUNNING",
                "Consumer plugin is not running.",
            ));
        }
        if !consumer
            .snapshot
            .granted_capabilities
            .iter()
            .any(|capability| capability == "services.call")
        {
            return Err(plugin_error(
                "UNAUTHORIZED",
                "Consumer plugin has not been granted services.call.",
            ));
        }
        let requirement = consumer
            .snapshot
            .manifest
            .requires
            .iter()
            .find(|requirement| requirement.id == service_id)
            .ok_or_else(|| {
                plugin_error(
                    "SERVICE_NOT_DECLARED",
                    "Consumer manifest does not declare this service requirement.",
                )
            })?;

        let candidates = state
            .plugins
            .iter()
            .filter(|(plugin_id, _)| plugin_id.as_str() != consumer_id)
            .flat_map(|(plugin_id, record)| {
                record
                    .snapshot
                    .manifest
                    .provides
                    .iter()
                    .filter(move |service| service.id == service_id)
                    .map(move |service| {
                        (
                            plugin_id.as_str(),
                            service,
                            record.snapshot.installation == InstallationState::Installed,
                            record.snapshot.enabled,
                        )
                    })
            })
            .collect::<Vec<_>>();
        let provider_id = choose_service_provider(consumer_id, requirement, &candidates)?;
        let provider = state.plugins.get(provider_id).ok_or_else(|| {
            plugin_error("SERVICE_UNAVAILABLE", "Service provider is unavailable.")
        })?;
        if provider.snapshot.installation != InstallationState::Installed
            || !provider.snapshot.enabled
            || provider.snapshot.runtime != RuntimeState::Running
            || !provider.snapshot.service_dependency_issues.is_empty()
        {
            return Err(plugin_error(
                "SERVICE_UNAVAILABLE",
                "The selected service provider is not available.",
            ));
        }
        let process = provider
            .process
            .as_ref()
            .filter(|process| process.is_alive())
            .cloned()
            .ok_or_else(|| {
                plugin_error(
                    "SERVICE_UNAVAILABLE",
                    "The selected service provider is not running.",
                )
            })?;
        Ok((
            PluginServiceResolution {
                service_id: service_id.to_owned(),
                provider_id: provider_id.to_owned(),
                version: provider
                    .snapshot
                    .manifest
                    .provides
                    .iter()
                    .find(|service| service.id == service_id)
                    .map(|service| service.version.clone())
                    .ok_or_else(|| {
                        plugin_error(
                            "SERVICE_UNAVAILABLE",
                            "Service provider changed during resolution.",
                        )
                    })?,
                methods: requirement.methods.clone(),
            },
            process,
            provider.contract.clone(),
        ))
    }

    fn cancel(&self, plugin_id: &str, request_id: &str) -> Result<(), PluginError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let record = state
            .plugins
            .get(plugin_id)
            .ok_or_else(|| not_installed(plugin_id))?;
        record
            .process
            .as_ref()
            .ok_or_else(|| plugin_error("NOT_RUNNING", "Plugin backend is not running."))?
            .cancel(request_id)
    }

    fn plugin_ui_url(&self, plugin_id: &str) -> Result<String, PluginError> {
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
        if record.snapshot.runtime != RuntimeState::Running {
            return Err(plugin_error(
                "NOT_RUNNING",
                "Plugin backend is not running.",
            ));
        }
        if !record.snapshot.service_dependency_issues.is_empty() {
            return Err(plugin_error(
                "SERVICE_DEPENDENCY_MISSING",
                &record.snapshot.service_dependency_issues.join(" "),
            ));
        }
        let ui = record.snapshot.manifest.ui.as_ref().ok_or_else(|| {
            plugin_error(
                "NO_PLUGIN_UI",
                "This service plugin does not provide a user interface.",
            )
        })?;
        let origin = if cfg!(windows) {
            "http://plugin-asset.localhost"
        } else {
            "plugin-asset://localhost"
        };
        Ok(format!("{origin}/{plugin_id}/{}", ui.entry))
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
        if record.snapshot.installation != InstallationState::Installed || !record.snapshot.enabled
        {
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
        let Ok(metadata) = fs::metadata(&canonical) else {
            return (
                404,
                "text/plain; charset=utf-8".to_owned(),
                b"plugin asset not found".to_vec(),
            );
        };
        if metadata.len() > 128 * 1024 * 1024 {
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
    fn start_one(&self, plugin_id: &str) -> Result<(), PluginError> {
        let (snapshot, directory, contract, contract_sha256) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let record = state
                .plugins
                .get_mut(plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            if record.snapshot.runtime == RuntimeState::Stopping {
                return Err(plugin_error(
                    "PLUGIN_BUSY",
                    "Plugin is stopping and cannot be started yet.",
                ));
            }
            if !record.snapshot.enabled
                || record.snapshot.installation != InstallationState::Installed
            {
                return Err(plugin_error(
                    "DISABLED",
                    "Plugin is disabled or incompatible.",
                ));
            }
            if !record.snapshot.service_dependency_issues.is_empty() {
                return Err(plugin_error(
                    "SERVICE_DEPENDENCY_MISSING",
                    &record.snapshot.service_dependency_issues.join(" "),
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
            (
                record.snapshot.clone(),
                record.directory.clone(),
                record.contract.clone(),
                record.contract_sha256.clone(),
            )
        };
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
                self.app_handle.clone(),
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
        let record = state
            .plugins
            .get_mut(plugin_id)
            .ok_or_else(|| not_installed(plugin_id))?;
        match process {
            Ok(process) => {
                record.snapshot.runtime = RuntimeState::Running;
                record.snapshot.last_error = None;
                record.process = Some(process);
                Ok(())
            }
            Err(error) => {
                record.snapshot.runtime = RuntimeState::Failed;
                record.snapshot.last_error = Some(failure(&error.code, &error.message));
                Err(error)
            }
        }
    }
}

/// Implements the plugin-management commands exposed by the Core CLI.
/// Mutating commands require an explicit `--yes`; install grants are supplied one at a time.
pub fn run_cli_command(
    app: &AppHandle,
    manager: &PluginManager,
    args: &[OsString],
    desktop_running: bool,
) -> Result<Value, PluginError> {
    let words = args
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if words.as_slice() == ["core", "status"] {
        let plugins = manager.snapshots();
        let profile_id = hex::encode(Sha256::digest(
            manager
                .inner
                .app_data_dir
                .to_string_lossy()
                .to_lowercase()
                .as_bytes(),
        ));
        return Ok(core_status_payload(
            &app.package_info().version.to_string(),
            desktop_running,
            &plugins,
            &profile_id[..16],
        ));
    }
    if words.first().is_some_and(|word| word == "logs") {
        return run_cli_logs(manager, &words[1..]);
    }
    if words.first().is_some_and(|word| word == "ui")
        && words.get(1).is_some_and(|word| word == "command")
    {
        return run_cli_ui_command(manager, &words[2..]);
    }
    if words.first().is_some_and(|word| word == "plugins")
        && words.get(1).is_some_and(|word| word == "permissions")
    {
        return run_cli_permissions(manager, &words[2..]);
    }
    if words.len() < 2 || words[0] != "plugins" {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Expected a Core, logs, or plugins command. Run `wla --help` for usage.",
        ));
    }

    match words[1].as_str() {
        "list" if words.len() == 2 => Ok(json!({"plugins": manager.snapshots()})),
        "ui-check" if (3..=4).contains(&words.len()) => {
            let plugin_id = &words[2];
            let snapshot = manager
                .snapshots()
                .into_iter()
                .find(|snapshot| snapshot.manifest.id == *plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            let ui = snapshot.manifest.ui.clone().ok_or_else(|| {
                plugin_error(
                    "NO_PLUGIN_UI",
                    "This plugin does not provide a user interface.",
                )
            })?;
            manager.ensure_cli_plugin_started(plugin_id)?;
            let snapshot = manager
                .snapshots()
                .into_iter()
                .find(|snapshot| snapshot.manifest.id == *plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            let ui_url = manager.plugin_ui_url(plugin_id)?;
            let asset = words.get(3).map(String::as_str).unwrap_or(&ui.entry);
            let response_path = format!("/{plugin_id}/{asset}");
            let response = plugin_asset_response(app, &response_path, "GET");
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(tauri::http::header::CONTENT_TYPE)
                .and_then(|header| header.to_str().ok())
                .unwrap_or_default();
            let csp = response
                .headers()
                .get("Content-Security-Policy")
                .and_then(|header| header.to_str().ok())
                .unwrap_or_default();
            let no_sniff = response
                .headers()
                .get(tauri::http::header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|header| header.to_str().ok())
                == Some("nosniff");
            let body = String::from_utf8_lossy(response.body());
            let is_html = content_type.starts_with("text/html")
                && body.to_ascii_lowercase().contains("<html");
            let csp_is_restricted = csp.contains("default-src 'none'")
                && csp.contains("connect-src 'none'")
                && csp.contains("object-src 'none'");
            let ok = status == 200 && is_html && no_sniff && csp_is_restricted;
            let report = json!({
                "ok": ok,
                "pluginId": plugin_id,
                "runtime": snapshot.runtime,
                "uiUrl": ui_url,
                "asset": asset,
                "status": status,
                "contentType": content_type,
                "contentSecurityPolicy": csp,
                "noSniff": no_sniff,
                "htmlDocument": is_html,
                "bytes": response.body().len(),
                "bridgeCompatibility": ui.bridge_compatibility,
                "contributions": ui.contributions,
            });
            if ok {
                Ok(report)
            } else {
                Err(PluginError {
                    code: "UI_RESOURCE_CHECK_FAILED".to_owned(),
                    message: format!("Plugin UI asset check failed for '{plugin_id}/{asset}'."),
                    details: Some(report),
                })
            }
        }
        "install" if words.len() >= 3 => {
            let mut confirmed = false;
            let mut overwrite = false;
            let mut approve_source_change = false;
            let mut grants = Vec::new();
            let mut index = 3;
            while index < args.len() {
                let flag = args[index].to_string_lossy();
                match flag.as_ref() {
                    "--yes" => confirmed = true,
                    "--overwrite" => overwrite = true,
                    "--approve-source-change" => approve_source_change = true,
                    "--grant" => {
                        index += 1;
                        let grant = args.get(index).ok_or_else(|| {
                            plugin_error("INVALID_REQUEST", "--grant requires a capability ID.")
                        })?;
                        grants.push(grant.to_string_lossy().into_owned());
                    }
                    value if value.starts_with("--grant=") => {
                        grants.push(value[8..].to_owned());
                    }
                    _ => {
                        return Err(plugin_error(
                            "INVALID_REQUEST",
                            &format!("Unknown install option '{flag}'."),
                        ));
                    }
                }
                index += 1;
            }
            if !confirmed {
                return Err(plugin_error(
                    "CONFIRMATION_REQUIRED",
                    "Plugin installation runs local code. Review its source and pass --yes to confirm.",
                ));
            }
            let states = manager.install_cli_package(
                PathBuf::from(&args[2]),
                grants,
                overwrite,
                approve_source_change,
            )?;
            Ok(json!({ "plugins": states }))
        }
        "enable" | "disable" if words.len() >= 3 => {
            require_cli_confirmation(&args[3..])?;
            ensure_only_cli_flag(&args[3..], "--yes")?;
            let states = manager.set_enabled(&words[2], words[1] == "enable")?;
            Ok(json!({ "plugins": states }))
        }
        #[cfg(debug_assertions)]
        "test-backend-exit" if words.len() == 4 => {
            require_cli_confirmation(&args[3..])?;
            ensure_only_cli_flag(&args[3..], "--yes")?;
            for snapshot in manager.snapshots().into_iter().filter(|snapshot| {
                snapshot.enabled && snapshot.installation == InstallationState::Installed
            }) {
                manager.ensure_cli_plugin_started(&snapshot.manifest.id)?;
            }
            manager.ensure_cli_plugin_started(&words[2])?;
            let states = manager.inject_backend_exit_for_test(&words[2])?;
            Ok(json!({ "plugins": states }))
        }
        "remove" if words.len() >= 3 => {
            require_cli_confirmation(&args[3..])?;
            let remove_data = args[3..].iter().any(|argument| argument == "--remove-data");
            for argument in &args[3..] {
                if argument != "--yes" && argument != "--remove-data" {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        &format!("Unknown remove option '{}'.", argument.to_string_lossy()),
                    ));
                }
            }
            let states = manager.remove(&words[2], remove_data)?;
            Ok(json!({ "ok": true, "plugins": states }))
        }
        _ => Err(plugin_error(
            "INVALID_REQUEST",
            "Unknown or malformed plugins command. Run `wla --help` for usage.",
        )),
    }
}

fn core_status_payload(
    version: &str,
    desktop_running: bool,
    plugins: &[PluginRuntimeState],
    profile_id: &str,
) -> Value {
    json!({
        "version": version,
        "desktopRunning": desktop_running,
        "pluginCount": plugins.len(),
        "enabledPluginCount": plugins.iter().filter(|plugin| plugin.enabled).count(),
        "runningPluginCount": plugins.iter().filter(|plugin| plugin.runtime == RuntimeState::Running).count(),
        "profileId": profile_id,
    })
}

#[cfg(test)]
mod core_status_tests {
    use super::*;

    #[test]
    fn status_reflects_whether_it_was_requested_from_the_desktop_runtime() {
        let plugins = [];
        let standalone = core_status_payload("0.1.0", false, &plugins, "profile");
        let desktop = core_status_payload("0.1.0", true, &plugins, "profile");

        assert_eq!(standalone["desktopRunning"], false);
        assert_eq!(desktop["desktopRunning"], true);
        assert_eq!(desktop["version"], "0.1.0");
        assert_eq!(desktop["pluginCount"], 0);
    }
}

fn run_cli_ui_command(manager: &PluginManager, args: &[String]) -> Result<Value, PluginError> {
    if args.len() != 2 || args[0] != "list" {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Expected `ui command list <plugin>/<contribution>`. UI command execution requires an open desktop Core.",
        ));
    }
    let (plugin_id, contribution_id) = parse_contribution_selector(&args[1])?;
    let plugin = manager
        .snapshots()
        .into_iter()
        .find(|plugin| plugin.manifest.id == plugin_id)
        .ok_or_else(|| not_installed(&plugin_id))?;
    let contribution = plugin
        .manifest
        .ui
        .as_ref()
        .and_then(|ui| {
            ui.contributions
                .iter()
                .find(|item| item.id == contribution_id)
        })
        .ok_or_else(|| {
            plugin_error(
                "CONTRIBUTION_NOT_FOUND",
                "Plugin contribution is not registered.",
            )
        })?;
    Ok(json!({
        "pluginId": plugin_id,
        "contributionId": contribution_id,
        "commands": contribution.commands,
    }))
}

pub fn validate_cli_ui_command(
    manager: &PluginManager,
    args: &[String],
) -> Result<(String, PluginUiCommand, Value), PluginError> {
    if args.len() < 5 || args[0] != "ui" || args[1] != "command" || args[2] != "run" {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Expected `ui command run <plugin>/<contribution> <command-id> --input-json <json> [--yes]`.",
        ));
    }
    let (plugin_id, contribution_id) = parse_contribution_selector(&args[3])?;
    let command_id = &args[4];
    let mut input_json = None;
    let mut confirmed = false;
    let mut index = 5;
    while index < args.len() {
        match args[index].as_str() {
            "--yes" => {
                if confirmed {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        "--yes may be specified once.",
                    ));
                }
                confirmed = true;
            }
            "--input-json" => {
                if input_json.is_some() {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        "--input-json may be specified once.",
                    ));
                }
                index += 1;
                input_json = Some(args.get(index).ok_or_else(|| {
                    plugin_error("INVALID_REQUEST", "--input-json requires a JSON value.")
                })?);
            }
            option => {
                return Err(plugin_error(
                    "INVALID_REQUEST",
                    &format!("Unknown UI command option '{option}'."),
                ));
            }
        }
        index += 1;
    }
    let input_json =
        input_json.ok_or_else(|| plugin_error("INVALID_REQUEST", "--input-json is required."))?;
    let input: Value = serde_json::from_str(input_json)
        .map_err(|_| plugin_error("INVALID_REQUEST", "--input-json must contain valid JSON."))?;
    let plugin = manager
        .snapshots()
        .into_iter()
        .find(|plugin| plugin.manifest.id == plugin_id)
        .ok_or_else(|| not_installed(&plugin_id))?;
    let command = plugin
        .manifest
        .ui
        .as_ref()
        .and_then(|ui| {
            ui.contributions
                .iter()
                .find(|item| item.id == contribution_id)
        })
        .and_then(|contribution| {
            contribution
                .commands
                .iter()
                .find(|command| command.id == *command_id)
        })
        .cloned()
        .ok_or_else(|| {
            plugin_error(
                "UI_COMMAND_NOT_FOUND",
                "The command is not declared by this contribution.",
            )
        })?;
    if matches!(command.effect, PluginUiCommandEffect::Mutating) && !confirmed {
        return Err(plugin_error(
            "CONFIRMATION_REQUIRED",
            "This plugin UI command changes user content. Pass --yes to confirm.",
        ));
    }
    crate::plugin_schema::validate(&input, &command.input_schema, &command.input_schema).map_err(
        |error| PluginError {
            code: "SCHEMA_VALIDATION_FAILED".to_owned(),
            message: "The input does not match the plugin command schema.".to_owned(),
            details: Some(json!({ "reason": error })),
        },
    )?;
    Ok((format!("{plugin_id}/{contribution_id}"), command, input))
}

fn parse_contribution_selector(selector: &str) -> Result<(String, String), PluginError> {
    let (plugin_id, contribution_id) = selector
        .split_once('/')
        .ok_or_else(|| plugin_error("INVALID_REQUEST", "Use <plugin-id>/<contribution-id>."))?;
    if plugin_id.is_empty() || contribution_id.is_empty() || contribution_id.contains('/') {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Use <plugin-id>/<contribution-id>.",
        ));
    }
    Ok((plugin_id.to_owned(), contribution_id.to_owned()))
}

fn run_cli_permissions(manager: &PluginManager, args: &[String]) -> Result<Value, PluginError> {
    let Some(action) = args.first().map(String::as_str) else {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Expected `plugins permissions list|set <plugin-id>`. ",
        ));
    };
    let Some(plugin_id) = args.get(1) else {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "A plugin ID is required for permission management.",
        ));
    };
    let snapshot = || {
        manager
            .snapshots()
            .into_iter()
            .find(|plugin| plugin.manifest.id == *plugin_id)
            .ok_or_else(|| not_installed(plugin_id))
    };
    match action {
        "list" if args.len() == 2 => {
            let plugin = snapshot()?;
            Ok(json!({
                "pluginId": plugin_id,
                "requested": plugin.manifest.capabilities,
                "granted": plugin.granted_capabilities,
            }))
        }
        "set" => {
            let mut confirmed = false;
            let mut grants = Vec::new();
            let mut index = 2;
            while index < args.len() {
                match args[index].as_str() {
                    "--yes" => confirmed = true,
                    "--grant" => {
                        index += 1;
                        let grant = args.get(index).ok_or_else(|| {
                            plugin_error("INVALID_REQUEST", "--grant requires a capability ID.")
                        })?;
                        grants.push(grant.clone());
                    }
                    option if option.starts_with("--grant=") => {
                        grants.push(option[8..].to_owned());
                    }
                    option => {
                        return Err(plugin_error(
                            "INVALID_REQUEST",
                            &format!("Unknown permission option '{option}'."),
                        ));
                    }
                }
                index += 1;
            }
            if !confirmed {
                return Err(plugin_error(
                    "CONFIRMATION_REQUIRED",
                    "Changing plugin permissions requires --yes.",
                ));
            }
            let states = manager.set_capabilities(plugin_id, grants)?;
            Ok(json!({ "plugins": states }))
        }
        _ => Err(plugin_error(
            "INVALID_REQUEST",
            "Expected `plugins permissions list <plugin-id>` or `set <plugin-id> --grant <capability>... --yes`.",
        )),
    }
}

fn run_cli_logs(manager: &PluginManager, args: &[String]) -> Result<Value, PluginError> {
    let log_dir = manager.inner.app_data_dir.join("logs");
    match args.first().map(String::as_str) {
        Some("dir") if args.len() == 1 => Ok(json!({
            "directory": log_dir,
            "exists": log_dir.is_dir(),
        })),
        Some("list") if args.len() == 1 => {
            let mut files = Vec::new();
            if let Ok(entries) = fs::read_dir(&log_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("wonderland-assistant.log."))
                        && let Ok(metadata) = entry.metadata()
                    {
                        files.push(json!({
                            "name": path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                            "sizeBytes": metadata.len(),
                            "modified": metadata.modified().ok().and_then(|time| {
                                time.duration_since(std::time::UNIX_EPOCH).ok().map(|value| value.as_secs())
                            }),
                        }));
                    }
                }
            }
            files.sort_by(|left, right| right["name"].as_str().cmp(&left["name"].as_str()));
            Ok(json!({ "directory": log_dir, "files": files }))
        }
        Some("tail") => {
            let mut line_count = 100_usize;
            let mut index = 1;
            while index < args.len() {
                if args[index] == "--lines" {
                    index += 1;
                    let value = args.get(index).ok_or_else(|| {
                        plugin_error("INVALID_REQUEST", "--lines requires a count.")
                    })?;
                    line_count = value.parse::<usize>().map_err(|_| {
                        plugin_error(
                            "INVALID_REQUEST",
                            "--lines must be an integer from 1 to 500.",
                        )
                    })?;
                    if !(1..=500).contains(&line_count) {
                        return Err(plugin_error(
                            "INVALID_REQUEST",
                            "--lines must be an integer from 1 to 500.",
                        ));
                    }
                } else {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        &format!("Unknown logs tail option '{}'.", args[index]),
                    ));
                }
                index += 1;
            }
            let mut candidates = fs::read_dir(&log_dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("wonderland-assistant.log."))
                })
                .collect::<Vec<_>>();
            candidates.sort();
            let Some(path) = candidates.pop() else {
                return Ok(json!({ "path": null, "lines": [] }));
            };
            let content = fs::read_to_string(&path).map_err(|error| {
                plugin_error("LOG_READ_FAILED", &format!("Cannot read log file: {error}"))
            })?;
            let lines = content
                .lines()
                .rev()
                .take(line_count)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>();
            Ok(json!({ "path": path, "lines": lines }))
        }
        _ => Err(plugin_error(
            "INVALID_REQUEST",
            "Expected `logs dir`, `logs list`, or `logs tail [--lines <1-500>]`.",
        )),
    }
}

fn require_cli_confirmation(args: &[OsString]) -> Result<(), PluginError> {
    if args.iter().any(|argument| argument == "--yes") {
        Ok(())
    } else {
        Err(plugin_error(
            "CONFIRMATION_REQUIRED",
            "This command changes plugin state. Pass --yes to confirm.",
        ))
    }
}

fn ensure_only_cli_flag(args: &[OsString], allowed: &str) -> Result<(), PluginError> {
    if let Some(unknown) = args
        .iter()
        .find(|argument| argument.to_string_lossy() != allowed)
    {
        return Err(plugin_error(
            "INVALID_REQUEST",
            &format!("Unknown command option '{}'.", unknown.to_string_lossy()),
        ));
    }
    Ok(())
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

/// Reads the reviewed, public plugin catalog. A validated last-known copy is returned offline.
#[tauri::command]
pub async fn plugins_catalog_list(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    manager: State<'_, PluginManager>,
) -> Result<PluginCatalogSnapshot, PluginError> {
    ensure_main(&window).map_err(|error| plugin_error("INVALID_REQUEST", &error.message))?;
    let app_data_dir = context.app_data_dir.clone();
    let client = wonderland_net::PluginCatalogClient::new()
        .map_err(|error| plugin_error("CATALOG_UNAVAILABLE", &error.to_string()))?;
    let (index, stale) = match client.get_index().await {
        Ok(bytes) => {
            let index = parse_catalog_index(&bytes)?;
            let _ = fs::write(app_data_dir.join(CATALOG_CACHE_FILE), &bytes);
            (index, false)
        }
        Err(network_error) => {
            let cached = fs::read(app_data_dir.join(CATALOG_CACHE_FILE))
                .map_err(|_| plugin_error("CATALOG_UNAVAILABLE", &network_error.to_string()))?;
            (parse_catalog_index(&cached)?, true)
        }
    };
    let installed_versions = manager
        .snapshots()
        .into_iter()
        .map(|state| (state.manifest.id, state.manifest.version))
        .collect::<BTreeMap<_, _>>();
    let plugins = index
        .plugins
        .into_iter()
        .map(|entry| {
            let compatible = catalog_entry_is_compatible(&entry);
            let installed_version = installed_versions.get(&entry.id).cloned();
            let installable = compatible
                && installed_version.as_deref().is_none_or(|installed| {
                    let current = Version::parse(&entry.version);
                    let installed = Version::parse(installed);
                    current
                        .ok()
                        .zip(installed.ok())
                        .is_some_and(|(current, installed)| current > installed)
                });
            PluginCatalogItem {
                entry,
                compatible,
                installed_version,
                installable,
            }
        })
        .collect();
    Ok(PluginCatalogSnapshot {
        generated_at: index.generated_at,
        stale,
        plugins,
    })
}

/// Downloads one reviewed catalog version, verifies its digest and manifest, then installs it
/// through the same package validation and atomic replacement path as a local .wplug file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogInstallRequest {
    plugin_id: String,
    version: String,
    expected_sha256: String,
    approved_capabilities: Vec<String>,
    approved_source_change: bool,
}

#[tauri::command]
pub async fn plugins_catalog_install(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    manager: State<'_, PluginManager>,
    request: CatalogInstallRequest,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window).map_err(|error| plugin_error("INVALID_REQUEST", &error.message))?;
    let CatalogInstallRequest {
        plugin_id,
        version,
        expected_sha256,
        approved_capabilities,
        approved_source_change,
    } = request;
    if !valid_plugin_id(&plugin_id) {
        return Err(plugin_error("INVALID_REQUEST", "Plugin ID is invalid."));
    }
    let app_data_dir = context.app_data_dir.clone();
    let client = wonderland_net::PluginCatalogClient::new()
        .map_err(|error| plugin_error("CATALOG_UNAVAILABLE", &error.to_string()))?;
    let catalog_bytes = client
        .get_index()
        .await
        .map_err(|error| plugin_error("CATALOG_UNAVAILABLE", &error.to_string()))?;
    let index = parse_catalog_index(&catalog_bytes)?;
    let entry = index
        .plugins
        .into_iter()
        .find(|entry| entry.id == plugin_id && entry.version == version)
        .ok_or_else(|| {
            plugin_error(
                "CATALOG_VERSION_MISSING",
                "This plugin version is no longer listed in the catalog. Refresh and try again.",
            )
        })?;
    if !catalog_entry_is_compatible(&entry) {
        return Err(plugin_error(
            "INCOMPATIBLE_CORE",
            "This plugin version is incompatible with the current Windows build.",
        ));
    }
    if entry.sha256 != expected_sha256 {
        return Err(plugin_error(
            "CATALOG_CHANGED",
            "The package changed after the catalog was displayed. Refresh the catalog and review it again.",
        ));
    }
    let available_version = Version::parse(&entry.version)
        .map_err(|_| plugin_error("INVALID_CATALOG", "Plugin version is invalid."))?;
    if let Some(installed) = manager
        .snapshots()
        .into_iter()
        .find(|state| state.manifest.id == entry.id)
        .and_then(|state| Version::parse(&state.manifest.version).ok())
        && available_version <= installed
    {
        return Err(plugin_error(
            "CATALOG_VERSION_NOT_NEWER",
            "The catalog does not contain a newer plugin version.",
        ));
    }
    let approved = approved_capabilities
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let requested = entry.capabilities.iter().cloned().collect::<BTreeSet<_>>();
    if approved.len() != approved_capabilities.len() || approved != requested {
        return Err(plugin_error(
            "CAPABILITY_APPROVAL_REQUIRED",
            "Confirm every capability requested by the catalog entry before installing.",
        ));
    }

    let package_bytes = client
        .download_package(&entry.download_url)
        .await
        .map_err(|error| plugin_error("DOWNLOAD_FAILED", &error.to_string()))?;
    let digest = hex::encode(Sha256::digest(&package_bytes));
    if package_bytes.len() as u64 != entry.size_bytes || digest != entry.sha256 {
        return Err(plugin_error(
            "PACKAGE_CHECKSUM_MISMATCH",
            "The downloaded package does not match the reviewed catalog checksum.",
        ));
    }

    let download_dir = app_data_dir.join("plugin-downloads");
    fs::create_dir_all(&download_dir)
        .map_err(|error| plugin_error("LOCAL_IO", &format!("Cannot prepare download: {error}")))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let source = download_dir.join(format!(
        "{}-{}-{}-{}.wplug",
        entry.id,
        entry.version,
        std::process::id(),
        nonce
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&source)
        .map_err(|error| plugin_error("LOCAL_IO", &format!("Cannot save download: {error}")))?;
    file.write_all(&package_bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| plugin_error("LOCAL_IO", &format!("Cannot save download: {error}")))?;
    drop(file);

    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result =
            manager.install_catalog_package(source.clone(), &entry, approved_source_change);
        let _ = fs::remove_file(source);
        result
    })
    .await
    .map_err(|error| plugin_error("INTERNAL", &format!("Plugin installation failed: {error}")))?
}

fn parse_catalog_index(bytes: &[u8]) -> Result<PluginCatalogIndex, PluginError> {
    if bytes.len() > CATALOG_MAX_BYTES {
        return Err(plugin_error(
            "INVALID_CATALOG",
            "Plugin catalog is too large.",
        ));
    }
    let index: PluginCatalogIndex = serde_json::from_slice(bytes)
        .map_err(|_| plugin_error("INVALID_CATALOG", "Plugin catalog format is invalid."))?;
    if index.schema_version != 1
        || index.plugins.len() > CATALOG_MAX_ITEMS
        || index.generated_at.len() > 64
        || chrono::DateTime::parse_from_rfc3339(&index.generated_at).is_err()
    {
        return Err(plugin_error(
            "INVALID_CATALOG",
            "Plugin catalog metadata is invalid.",
        ));
    }
    let mut ids = BTreeSet::new();
    for entry in &index.plugins {
        validate_catalog_entry(entry)?;
        if !ids.insert(entry.id.as_str()) {
            return Err(plugin_error(
                "INVALID_CATALOG",
                "Plugin catalog contains duplicate IDs.",
            ));
        }
    }
    Ok(index)
}

fn validate_catalog_entry(entry: &PluginCatalogEntry) -> Result<(), PluginError> {
    let version = Version::parse(&entry.version).ok();
    let min_core = Version::parse(&entry.host_compatibility.min_core_version).ok();
    let max_core = Version::parse(&entry.host_compatibility.max_core_version_exclusive).ok();
    let min_protocol = Version::parse(&entry.host_compatibility.protocol.min_version).ok();
    let max_protocol =
        Version::parse(&entry.host_compatibility.protocol.max_version_exclusive).ok();
    let release_prefix = format!("{}/releases/download/", entry.repository_url);
    let release_tail = entry.download_url.strip_prefix(&release_prefix);
    let release_path_valid = release_tail.is_some_and(|tail| {
        let mut pieces = tail.split('/');
        let tag = pieces.next().unwrap_or_default();
        let file = pieces.next().unwrap_or_default();
        pieces.next().is_none()
            && !tag.is_empty()
            && (tag == entry.version || tag == format!("v{}", entry.version))
            && tag
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            && file
                == format!(
                    "{}-{}-windows-{}.wplug",
                    entry.id, entry.version, entry.platform.architecture
                )
    });
    let capabilities: BTreeSet<_> = entry.capabilities.iter().collect();
    let provided_ids: BTreeSet<_> = entry
        .provides
        .iter()
        .map(|service| service.id.as_str())
        .collect();
    let required_ids: BTreeSet<_> = entry
        .requires
        .iter()
        .map(|service| service.id.as_str())
        .collect();
    let services_valid = entry.provides.len() <= 32
        && entry.requires.len() <= 32
        && provided_ids.len() == entry.provides.len()
        && required_ids.len() == entry.requires.len()
        && entry.provides.iter().all(|service| {
            valid_service_id(&service.id)
                && Version::parse(&service.version).is_ok()
                && !service.methods.is_empty()
                && service.methods.len() <= 128
                && service
                    .methods
                    .iter()
                    .all(|method| valid_service_method(method))
                && service.methods.iter().collect::<BTreeSet<_>>().len() == service.methods.len()
        })
        && entry.requires.iter().all(|requirement| {
            let min = Version::parse(&requirement.min_version);
            let max = Version::parse(&requirement.max_version_exclusive);
            valid_service_id(&requirement.id)
                && min.is_ok()
                && max.is_ok()
                && min.ok().zip(max.ok()).is_some_and(|(min, max)| min < max)
                && !requirement.methods.is_empty()
                && requirement.methods.len() <= 128
                && requirement
                    .methods
                    .iter()
                    .all(|method| valid_service_method(method))
                && requirement.methods.iter().collect::<BTreeSet<_>>().len()
                    == requirement.methods.len()
        });
    let valid_release_notes = entry.release_notes_url.as_ref().is_none_or(|url| {
        let prefix = format!("{}/releases/tag/", entry.repository_url);
        url.strip_prefix(&prefix).is_some_and(|tag| {
            !tag.is_empty()
                && tag
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        })
    });
    if !valid_plugin_id(&entry.id)
        || entry.name.trim().is_empty()
        || entry.name.chars().count() > 120
        || entry.description.chars().count() > 2000
        || entry.author.trim().is_empty()
        || entry.author.chars().count() > 120
        || !valid_github_repository_url(&entry.repository_url)
        || version.is_none()
        || version
            .as_ref()
            .is_some_and(|version| !version.build.is_empty())
        || min_core.is_none()
        || max_core.is_none()
        || min_core
            .as_ref()
            .zip(max_core.as_ref())
            .is_some_and(|(min, max)| min >= max)
        || min_protocol.is_none()
        || max_protocol.is_none()
        || min_protocol
            .as_ref()
            .zip(max_protocol.as_ref())
            .is_some_and(|(min, max)| min >= max)
        || entry
            .ui_bridge_compatibility
            .as_ref()
            .is_some_and(|compatibility| {
                let min = Version::parse(&compatibility.min_version);
                let max = Version::parse(&compatibility.max_version_exclusive);
                min.is_err()
                    || max.is_err()
                    || min.ok().zip(max.ok()).is_some_and(|(min, max)| min >= max)
            })
        || entry.platform.os != "windows"
        || !["x86_64", "aarch64"].contains(&entry.platform.architecture.as_str())
        || entry.platform.abi != "msvc"
        || !release_path_valid
        || !wonderland_net::is_valid_plugin_release_url(&entry.download_url)
        || entry.sha256.len() != 64
        || !entry
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || entry.size_bytes == 0
        || entry.size_bytes > PLUGIN_PACKAGE_MAX_BYTES
        || capabilities.len() != entry.capabilities.len()
        || !services_valid
        || entry
            .capabilities
            .iter()
            .any(|capability| capability.trim().is_empty() || capability.chars().count() > 128)
        || !valid_release_notes
    {
        return Err(plugin_error(
            "INVALID_CATALOG",
            "A plugin catalog entry contains invalid metadata.",
        ));
    }
    Ok(())
}

fn valid_github_repository_url(raw: &str) -> bool {
    let Some(path) = raw.strip_prefix("https://github.com/") else {
        return false;
    };
    let parts = path.split('/').collect::<Vec<_>>();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        })
}

fn catalog_entry_is_compatible(entry: &PluginCatalogEntry) -> bool {
    let Ok(core) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        return false;
    };
    let Ok(protocol) = Version::parse(wonderland_plugin_protocol::PROTOCOL_VERSION) else {
        return false;
    };
    let Ok(min_core) = Version::parse(&entry.host_compatibility.min_core_version) else {
        return false;
    };
    let Ok(max_core) = Version::parse(&entry.host_compatibility.max_core_version_exclusive) else {
        return false;
    };
    let Ok(min_protocol) = Version::parse(&entry.host_compatibility.protocol.min_version) else {
        return false;
    };
    let Ok(max_protocol) = Version::parse(&entry.host_compatibility.protocol.max_version_exclusive)
    else {
        return false;
    };
    let ui_compatible = entry
        .ui_bridge_compatibility
        .as_ref()
        .is_none_or(|compatibility| {
            let (Ok(current), Ok(min), Ok(max)) = (
                Version::parse(wonderland_plugin_protocol::UI_BRIDGE_VERSION),
                Version::parse(&compatibility.min_version),
                Version::parse(&compatibility.max_version_exclusive),
            ) else {
                return false;
            };
            current >= min && current < max
        });
    entry.platform.os == "windows"
        && (cfg!(target_arch = "x86_64") && entry.platform.architecture == "x86_64"
            || cfg!(target_arch = "aarch64") && entry.platform.architecture == "aarch64")
        && entry.platform.abi == "msvc"
        && core >= min_core
        && core < max_core
        && protocol >= min_protocol
        && protocol < max_protocol
        && ui_compatible
}

/// Opens a native picker; release builds accept .wplug archives, while debug builds also accept
/// an unpacked developer package directory.
#[tauri::command]
pub async fn plugins_install(
    window: WebviewWindow,
    app: AppHandle,
    manager: State<'_, PluginManager>,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window)?;
    let picker_app = app.clone();
    let source = tauri::async_runtime::spawn_blocking(move || {
        let dialog = picker_app.dialog().file();
        if cfg!(debug_assertions) {
            dialog.set_title("选择插件开发目录").blocking_pick_folder()
        } else {
            dialog
                .add_filter("Wonderland Plugin", &["wplug"])
                .blocking_pick_file()
        }
    })
    .await
    .map_err(|error| plugin_error("INTERNAL", &format!("Plugin picker failed: {error}")))?;
    let Some(source) = source else {
        return Ok(manager.snapshots());
    };
    let source = source.into_path().map_err(|error| {
        plugin_error(
            "INVALID_REQUEST",
            &format!("Selected path is invalid: {error}"),
        )
    })?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let manifest = plugin_package::manifest_for_install_source(&source)
            .map_err(|message| plugin_error("INVALID_REQUEST", &message))?;
        let same_version_path =
            plugin_package::install_path(&manager.inner.installed_root, &manifest);
        let existing = manager
            .snapshots()
            .into_iter()
            .find(|state| state.manifest.id == manifest.id);
        let previous_source = existing
            .as_ref()
            .and_then(|state| state.installation_source.as_ref())
            .map(|source| {
                let kind = if source.kind == "catalog" {
                    "在线目录"
                } else {
                    "本地安装"
                };
                format!("{kind}：{}", source.origin)
            })
            .unwrap_or_else(|| {
                if existing.is_some() {
                    "来源未记录（旧版本安装）".to_owned()
                } else {
                    "尚未安装".to_owned()
                }
            });
        let capabilities = if manifest.capabilities.is_empty() {
            "无".to_owned()
        } else {
            manifest.capabilities.join("、")
        };
        let confirmed = app
            .dialog()
            .message(format!(
                "安装「{}」v{}？\n插件 ID：{}\n本地包：{}\n现有来源：{}\n请求的宿主能力：{}\n\n同 ID 替换会保留已有插件数据；新插件后端以当前用户身份运行，可能读取这些数据和本机其他可访问文件。{}",
                manifest.name,
                manifest.version,
                manifest.id,
                source.display(),
                previous_source,
                capabilities,
                if same_version_path.exists() {
                    "这将覆盖已安装的相同版本。"
                } else {
                    ""
                }
            ))
            .title("确认安装本地插件")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "安装".to_owned(),
                "取消".to_owned(),
            ))
            .blocking_show();
        if !confirmed {
            return Ok(manager.snapshots());
        }
        manager.install(source, same_version_path.exists(), true)
    })
    .await
    .map_err(|error| plugin_error("INTERNAL", &format!("Plugin installation failed: {error}")))?
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
pub async fn plugins_set_capabilities(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
    capabilities: Vec<String>,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.set_capabilities(&plugin_id, capabilities))
        .await
        .map_err(|error| plugin_error("INTERNAL", &format!("Capability update failed: {error}")))?
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
    tauri::async_runtime::spawn_blocking(move || manager.remove(&plugin_id, remove_plugin_data))
        .await
        .map_err(|error| plugin_error("INTERNAL", &format!("Plugin removal failed: {error}")))?
}

#[tauri::command]
pub fn plugins_ui_url(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
) -> Result<String, PluginError> {
    ensure_main(&window)?;
    manager.plugin_ui_url(&plugin_id)
}

#[tauri::command]
pub async fn plugin_call(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
    request_id: String,
    method: String,
    params: Value,
) -> Result<Value, PluginError> {
    ensure_main(&window)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.call(&plugin_id, &request_id, &method, params)
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

fn load_preferences(app_data_dir: &Path) -> (Preferences, Option<String>) {
    let current = app_data_dir.join(MANAGER_CONFIG_FILE);
    if current.exists() {
        return match fs::read(&current)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<Preferences>(&bytes).map_err(|error| error.to_string())
            }) {
            Ok(preferences) if (1..=3).contains(&preferences.schema_version) => (preferences, None),
            Ok(_) => backup_invalid_config(&current, "Unsupported plugin manager config version."),
            Err(error) => backup_invalid_config(
                &current,
                &format!("Plugin manager config is invalid: {error}"),
            ),
        };
    }
    let legacy = app_data_dir.join(LEGACY_CONFIG_FILE);
    if legacy.exists() {
        let migrated = fs::read(&legacy)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<LegacyPreferences>(&bytes)
                    .map_err(|error| error.to_string())
            });
        match migrated {
            Ok(legacy_preferences) => {
                let mut preferences = Preferences::default();
                for id in legacy_preferences.disabled_plugins {
                    preferences.enabled_plugins.insert(id, false);
                }
                if persist_preferences(app_data_dir, &preferences).is_ok() {
                    let backup = app_data_dir.join("plugins.json.pre-v1.bak");
                    if !backup.exists() {
                        let _ = fs::rename(&legacy, backup);
                    }
                    return (preferences, None);
                }
                return (
                    preferences,
                    Some("Legacy plugin preferences could not be migrated.".to_owned()),
                );
            }
            Err(error) => {
                return backup_invalid_config(
                    &legacy,
                    &format!("Legacy plugin config is invalid: {error}"),
                );
            }
        }
    }
    (Preferences::default(), None)
}

fn backup_invalid_config(path: &Path, message: &str) -> (Preferences, Option<String>) {
    let suffix = Utc::now().timestamp_millis();
    let backup = path.with_file_name(format!(
        "{}.invalid-{suffix}.bak",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = fs::rename(path, backup);
    (Preferences::default(), Some(message.to_owned()))
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
            },
            contract: "contract.json".to_owned(),
            capabilities: Vec::new(),
            provides: Vec::new(),
            requires: Vec::new(),
        },
        installation: InstallationState::Invalid,
        enabled: false,
        runtime: RuntimeState::Stopped,
        last_error: Some(failure("INVALID_PACKAGE", message)),
        granted_capabilities: Vec::new(),
        installation_source: None,
        service_dependency_issues: Vec::new(),
        plugin_data_directory: None,
    }
}

fn choose_service_provider<'a>(
    consumer_id: &str,
    requirement: &PluginServiceRequirement,
    candidates: &'a [(&'a str, &'a PluginService, bool, bool)],
) -> Result<&'a str, PluginError> {
    let candidates = candidates
        .iter()
        .filter(|(plugin_id, _, _, _)| *plugin_id != consumer_id)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(plugin_error(
            "SERVICE_NOT_FOUND",
            "No installed plugin provides this service.",
        ));
    }

    let min = Version::parse(&requirement.min_version)
        .map_err(|_| plugin_error("INTERNAL", "Service requirement version is invalid."))?;
    let max = Version::parse(&requirement.max_version_exclusive)
        .map_err(|_| plugin_error("INTERNAL", "Service requirement version is invalid."))?;
    let version_matches = candidates
        .into_iter()
        .filter(|(_, service, _, _)| {
            Version::parse(&service.version).is_ok_and(|version| version >= min && version < max)
        })
        .collect::<Vec<_>>();
    if version_matches.is_empty() {
        return Err(plugin_error(
            "SERVICE_VERSION_MISMATCH",
            "Installed service providers do not match the requested version range.",
        ));
    }

    let method_matches = version_matches
        .into_iter()
        .filter(|(_, service, _, _)| {
            requirement
                .methods
                .iter()
                .all(|method| service.methods.contains(method))
        })
        .collect::<Vec<_>>();
    if method_matches.is_empty() {
        return Err(plugin_error(
            "SERVICE_METHOD_NOT_FOUND",
            "No version-compatible provider declares every required service method.",
        ));
    }

    let active = method_matches
        .into_iter()
        .filter(|(_, _, installed, enabled)| *installed && *enabled)
        .collect::<Vec<_>>();
    match active.as_slice() {
        [] => Err(plugin_error(
            "SERVICE_UNAVAILABLE",
            "Matching service providers are installed but disabled or incompatible.",
        )),
        [(plugin_id, _, _, _)] => Ok(plugin_id),
        _ => Err(plugin_error(
            "AMBIGUOUS_PROVIDER",
            "Multiple enabled plugins provide a compatible service; disable all but one provider.",
        )),
    }
}

fn service_provider_error(error: PluginError) -> PluginError {
    match error.code.as_str() {
        "TIMEOUT" => plugin_error(
            "SERVICE_TIMEOUT",
            "The selected service provider did not answer before the call timed out.",
        ),
        "NOT_INSTALLED"
        | "DISABLED"
        | "NOT_RUNNING"
        | "INCOMPATIBLE_CORE"
        | "PLUGIN_START_FAILED"
        | "PLUGIN_STOPPED"
        | "PLUGIN_CRASHED"
        | "SERVICE_DEPENDENCY_MISSING" => plugin_error(
            "SERVICE_UNAVAILABLE",
            "The selected service provider stopped or became unavailable during the call.",
        ),
        "METHOD_NOT_FOUND" => plugin_error(
            "SERVICE_METHOD_NOT_FOUND",
            "The provider no longer declares this service method.",
        ),
        _ => error,
    }
}

fn valid_service_id(value: &str) -> bool {
    value.len() <= 128
        && value.split('.').count() >= 2
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_lowercase()
                && part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'
                        || byte == b'-'
                })
        })
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

fn update_service_dependency_issues(plugins: &mut BTreeMap<String, PluginRecord>) {
    let providers = plugins
        .iter()
        .filter(|(_, record)| record.snapshot.installation == InstallationState::Installed)
        .flat_map(|(plugin_id, record)| {
            record
                .snapshot
                .manifest
                .provides
                .iter()
                .map(move |service| {
                    (
                        plugin_id.clone(),
                        service.id.clone(),
                        service.version.clone(),
                        service.methods.clone(),
                        record.snapshot.enabled,
                    )
                })
        })
        .collect::<Vec<_>>();

    for (plugin_id, record) in plugins.iter_mut() {
        let mut issues = Vec::new();
        for requirement in record
            .snapshot
            .manifest
            .requires
            .iter()
            .filter(|requirement| !requirement.optional)
        {
            let matching = providers
                .iter()
                .filter(|(provider_id, service_id, version, methods, enabled)| {
                    provider_id != plugin_id
                        && service_id == &requirement.id
                        && requirement
                            .methods
                            .iter()
                            .all(|method| methods.contains(method))
                        && *enabled
                        && Version::parse(version).is_ok_and(|version| {
                            Version::parse(&requirement.min_version).is_ok_and(|min| version >= min)
                                && Version::parse(&requirement.max_version_exclusive)
                                    .is_ok_and(|max| version < max)
                        })
                })
                .collect::<Vec<_>>();
            if matching.len() != 1 {
                let reason = if matching.is_empty() {
                    "no enabled installed provider declares a compatible version"
                } else {
                    "multiple enabled providers match; provider selection is not configured"
                };
                issues.push(format!(
                    "Required service '{}' ({} <= version < {}) is unavailable: {reason}.",
                    requirement.id, requirement.min_version, requirement.max_version_exclusive
                ));
            }
        }
        record.snapshot.service_dependency_issues = issues;
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
    let (status, mime, body) = match app.try_state::<PluginManager>() {
        Some(manager) => manager.read_plugin_asset(path, method),
        None => (
            503,
            "text/plain; charset=utf-8".to_owned(),
            b"plugin manager unavailable".to_vec(),
        ),
    };
    let status = tauri::http::StatusCode::from_u16(status)
        .unwrap_or(tauri::http::StatusCode::INTERNAL_SERVER_ERROR);
    tauri::http::Response::builder()
        .status(status)
        .header(tauri::http::header::CONTENT_TYPE, mime)
        .header(tauri::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(tauri::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(tauri::http::header::CACHE_CONTROL, "no-store")
        .header(
            // Allow remote images while denying remote scripts and connections.
            "Content-Security-Policy",
            "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'",
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

fn capability_grant_key(plugin_id: &str) -> String {
    plugin_id.to_owned()
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

fn select_install_grants(
    previous: Option<&[String]>,
    requested: &[String],
    approved: Option<&[String]>,
    reset: bool,
) -> Vec<String> {
    if let Some(approved) = approved {
        return approved.to_vec();
    }
    if reset {
        return requested.to_vec();
    }
    let requested = requested.iter().collect::<BTreeSet<_>>();
    previous
        .unwrap_or_default()
        .iter()
        .filter(|capability| requested.contains(capability))
        .cloned()
        .collect()
}

fn clear_plugin_preferences(preferences: &mut Preferences, plugin_id: &str) {
    preferences.enabled_plugins.remove(plugin_id);
    preferences.granted_capabilities.remove(plugin_id);
    let legacy_prefix = format!("{plugin_id}@");
    preferences
        .granted_capabilities
        .retain(|key, _| !key.starts_with(&legacy_prefix));
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

fn migrate_capability_grants(
    grants: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    let mut merged: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (key, capabilities) in grants {
        let plugin_id = key
            .split_once('@')
            .map(|(plugin_id, _)| plugin_id)
            .filter(|plugin_id| !plugin_id.is_empty())
            .unwrap_or(&key)
            .to_owned();
        let capabilities = capabilities.into_iter().collect::<BTreeSet<_>>();
        match merged.entry(plugin_id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(capabilities);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let intersection = entry.get().intersection(&capabilities).cloned().collect();
                *entry.get_mut() = intersection;
            }
        }
    }
    merged
        .into_iter()
        .map(|(plugin_id, capabilities)| (plugin_id, capabilities.into_iter().collect()))
        .collect()
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn uninstall_clears_current_and_legacy_grants_without_touching_other_plugins() {
        let mut preferences = Preferences::default();
        preferences
            .granted_capabilities
            .insert("editor".into(), vec!["files.pick".into()]);
        preferences
            .granted_capabilities
            .insert("editor@1.0.0".into(), vec!["files.export".into()]);
        preferences
            .granted_capabilities
            .insert("editor_plus".into(), vec!["files.pick".into()]);
        preferences
            .installation_sources
            .insert("editor".into(), local_source("C:/plugins/editor.wplug"));
        preferences
            .active_versions
            .insert("editor".into(), "1.0.0".into());

        clear_plugin_preferences(&mut preferences, "editor");

        assert!(!preferences.granted_capabilities.contains_key("editor"));
        assert!(
            !preferences
                .granted_capabilities
                .contains_key("editor@1.0.0")
        );
        assert!(preferences.granted_capabilities.contains_key("editor_plus"));
        assert!(!preferences.installation_sources.contains_key("editor"));
        assert!(!preferences.active_versions.contains_key("editor"));
    }

    #[test]
    fn source_switch_requires_new_review_and_does_not_reuse_grants() {
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

        let previous = vec!["files.pick".into()];
        let requested = vec!["files.pick".into(), "network.public".into()];
        assert_eq!(
            select_install_grants(Some(&previous), &requested, None, false),
            previous
        );
        assert_eq!(
            select_install_grants(Some(&previous), &requested, None, true),
            requested
        );
    }

    #[test]
    fn existing_preferences_without_source_remain_readable() {
        let preferences: Preferences = serde_json::from_str(
            r#"{"schemaVersion":3,"enabledPlugins":{"editor":true},"grantedCapabilities":{"editor":["files.pick"]},"activeVersions":{"editor":"1.0.0"}}"#,
        )
        .unwrap();
        assert!(preferences.installation_sources.is_empty());
        assert_eq!(preferences.granted_capabilities["editor"], ["files.pick"]);
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
}

#[cfg(test)]
mod service_resolution_tests {
    use super::*;

    fn requirement(min_version: &str, max_version_exclusive: &str) -> PluginServiceRequirement {
        PluginServiceRequirement {
            id: "wonderland.comments.archive".into(),
            min_version: min_version.into(),
            max_version_exclusive: max_version_exclusive.into(),
            optional: true,
            methods: vec!["service_archives".into(), "archive_page".into()],
        }
    }

    fn service(version: &str, methods: &[&str]) -> PluginService {
        PluginService {
            id: "wonderland.comments.archive".into(),
            version: version.into(),
            methods: methods.iter().map(|method| (*method).to_owned()).collect(),
        }
    }

    #[test]
    fn resolves_one_compatible_enabled_provider_and_excludes_self() {
        let own_service = service("1.0.0", &["service_archives", "archive_page"]);
        let provider = service("1.2.0", &["service_archives", "archive_page"]);
        let providers = [
            ("knowledge_library", &own_service, true, true),
            ("comment_collector", &provider, true, true),
        ];

        let selected = choose_service_provider(
            "knowledge_library",
            &requirement("1.0.0", "2.0.0"),
            &providers,
        )
        .unwrap();

        assert_eq!(selected, "comment_collector");
    }

    #[test]
    fn service_resolution_reports_missing_version_and_method_errors() {
        let no_providers = [];
        assert_eq!(
            choose_service_provider("consumer", &requirement("1.0.0", "2.0.0"), &no_providers,)
                .unwrap_err()
                .code,
            "SERVICE_NOT_FOUND"
        );

        // An upgrade to the next major version no longer satisfies the consumer's pinned range.
        let wrong_version = service("2.0.0", &["service_archives", "archive_page"]);
        assert_eq!(
            choose_service_provider(
                "consumer",
                &requirement("1.0.0", "2.0.0"),
                &[("provider", &wrong_version, true, true)],
            )
            .unwrap_err()
            .code,
            "SERVICE_VERSION_MISMATCH"
        );

        let missing_method = service("1.0.0", &["service_archives"]);
        assert_eq!(
            choose_service_provider(
                "consumer",
                &requirement("1.0.0", "2.0.0"),
                &[("provider", &missing_method, true, true)],
            )
            .unwrap_err()
            .code,
            "SERVICE_METHOD_NOT_FOUND"
        );
    }

    #[test]
    fn service_resolution_rejects_ambiguous_or_disabled_providers() {
        let first = service("1.0.0", &["service_archives", "archive_page"]);
        let second = service("1.1.0", &["service_archives", "archive_page"]);
        assert_eq!(
            choose_service_provider(
                "consumer",
                &requirement("1.0.0", "2.0.0"),
                &[
                    ("provider_a", &first, true, true),
                    ("provider_b", &second, true, true),
                ],
            )
            .unwrap_err()
            .code,
            "AMBIGUOUS_PROVIDER"
        );

        assert_eq!(
            choose_service_provider(
                "consumer",
                &requirement("1.0.0", "2.0.0"),
                &[("provider", &first, true, false)],
            )
            .unwrap_err()
            .code,
            "SERVICE_UNAVAILABLE"
        );
    }

    #[test]
    fn provider_call_failures_use_stable_service_codes() {
        assert_eq!(
            service_provider_error(plugin_error("TIMEOUT", "late")).code,
            "SERVICE_TIMEOUT"
        );
        for original in ["PLUGIN_CRASHED", "NOT_RUNNING", "DISABLED"] {
            assert_eq!(
                service_provider_error(plugin_error(original, "stopped")).code,
                "SERVICE_UNAVAILABLE"
            );
        }
    }
}
