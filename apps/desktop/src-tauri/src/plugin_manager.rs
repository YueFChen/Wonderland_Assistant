//! Generic plugin host commands and lifecycle state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use chrono::Utc;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use wonderland_kernel::AppContext;
use wonderland_kernel::logging::warn;
use wonderland_plugin_protocol::{
    HostCompatibility, InstallationState, PluginBackend, PluginError, PluginFailure,
    PluginManifest, PluginPlatform, PluginRuntimeState, PluginUi, PluginUiContribution,
    PluginUiContributionKind, ProtocolCompatibility, RuntimeState,
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
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 3,
            enabled_plugins: BTreeMap::new(),
            granted_capabilities: BTreeMap::new(),
            active_versions: BTreeMap::new(),
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
                        service_dependency_issues: Vec::new(),
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
        self.install(source, true)?;
        self.set_enabled(&manifest.id, true)
    }

    fn install_catalog_package(
        &self,
        source: PathBuf,
        entry: &PluginCatalogEntry,
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
        self.install_with_capabilities(source, true, Some(&entry.capabilities))
    }

    fn install(
        &self,
        source: PathBuf,
        overwrite: bool,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        self.install_with_capabilities(source, overwrite, None)
    }

    fn install_with_capabilities(
        &self,
        source: PathBuf,
        overwrite: bool,
        approved_capabilities: Option<&[String]>,
    ) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let source_manifest = plugin_package::manifest_for_install_source(&source)
            .map_err(|message| plugin_error("INVALID_REQUEST", &message))?;
        let replacing_same_version = overwrite
            && plugin_package::install_path(&self.inner.installed_root, &source_manifest).exists();
        let previous_version = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?
            .preferences
            .active_versions
            .get(&source_manifest.id)
            .cloned();
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
                .enabled_plugins
                .entry(plugin_id.clone())
                .or_insert(true);
            if let Some(approved) = approved_capabilities {
                state
                    .preferences
                    .granted_capabilities
                    .insert(grant_key, approved.to_vec());
            } else {
                state
                    .preferences
                    .granted_capabilities
                    .entry(grant_key)
                    .or_insert_with(|| inspected.manifest.capabilities.clone());
            }
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
            record.snapshot.runtime = RuntimeState::Stopping;
            record.process = None;
            state.plugins.remove(plugin_id);
            update_service_dependency_issues(&mut state.plugins);
            let install_directory = self.inner.installed_root.join(plugin_id);
            if install_directory.exists() {
                reject_symlink(&install_directory)?;
                fs::remove_dir_all(&install_directory).map_err(|error| {
                    plugin_error("INTERNAL", &format!("Cannot remove plugin files: {error}"))
                })?;
            }
            state.preferences.enabled_plugins.remove(plugin_id);
            let grant_prefix = format!("{plugin_id}@");
            state
                .preferences
                .granted_capabilities
                .retain(|key, _| !key.starts_with(&grant_prefix));
            state.preferences.active_versions.remove(plugin_id);
            persist_preferences(&self.inner.app_data_dir, &state.preferences).map_err(|error| {
                plugin_error(
                    "INTERNAL",
                    &format!("Plugin removed but preferences could not be saved: {error}"),
                )
            })?;
            state.config_error = None;
        }
        if remove_plugin_data {
            let data_directory = self.inner.app_data_dir.join("plugin-data").join(plugin_id);
            if data_directory.exists() {
                reject_symlink(&data_directory)?;
                fs::remove_dir_all(&data_directory).map_err(|error| {
                    plugin_error("INTERNAL", &format!("Cannot remove plugin data: {error}"))
                })?;
            }
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
        if !plugin_ui_isolation_test_enabled() {
            return Err(plugin_error(
                "PLUGIN_UI_NOT_VERIFIED",
                "Dynamic plugin UI remains disabled until the WebView isolation prototype is verified.",
            ));
        }
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
#[tauri::command]
pub async fn plugins_catalog_install(
    window: WebviewWindow,
    context: State<'_, AppContext>,
    manager: State<'_, PluginManager>,
    plugin_id: String,
    version: String,
    expected_sha256: String,
    approved_capabilities: Vec<String>,
) -> Result<Vec<PluginRuntimeState>, PluginError> {
    ensure_main(&window).map_err(|error| plugin_error("INVALID_REQUEST", &error.message))?;
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
        let result = manager.install_catalog_package(source.clone(), &entry);
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
        let overwrite = if same_version_path.exists() {
            app.dialog()
                .message(format!(
                    "插件「{}」v{} 已安装。要用所选版本覆盖吗？",
                    manifest.name, manifest.version
                ))
                .title("确认覆盖插件")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "覆盖".to_owned(),
                    "取消".to_owned(),
                ))
                .blocking_show()
        } else {
            false
        };
        if same_version_path.exists() && !overwrite {
            return Ok(manager.snapshots());
        }
        manager.install(source, overwrite)
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
        service_dependency_issues: Vec::new(),
    }
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
                .filter(|(provider_id, service_id, version, enabled)| {
                    provider_id != plugin_id
                        && service_id == &requirement.id
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

fn plugin_ui_isolation_test_enabled() -> bool {
    cfg!(debug_assertions)
        && std::env::var("WONDERLAND_PLUGIN_UI_ISOLATION_TEST").as_deref() == Ok("1")
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
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Refusing to remove a non-directory plugin path.",
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
