//! The single-origin plugin distribution application service shared by desktop and CLI.
//! Cached catalogs are introductions only: installation always rechecks the live source.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::Utc;
use ring::rand::{SecureRandom, SystemRandom};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;
use wonderland_kernel::logging::Secret;
use wonderland_net::{DistributionClient, DistributionHttpError, DistributionSource};
use wonderland_plugin_protocol::{
    HostCompatibility, PROTOCOL_SUPPORTED_VERSIONS, PluginDependency, PluginError, PluginPlatform,
    PluginRuntimeState, ProtocolCompatibility, UI_BRIDGE_SUPPORTED_VERSIONS,
};

const SETTINGS_FILE: &str = "plugin-distribution.json";
const DEFAULT_SOURCE_URL: &str = "https://plugins-stable.yuef.me";
// Preserve old protocol snapshots as evidence; never interpret their entries as
// authenticated package indexes. Explicit source/code clearing removes old views too.
const PUBLIC_CACHE_FILE: &str = "plugin-distribution-public-v2.json";
const RESTRICTED_CACHE_FILE: &str = "plugin-distribution-restricted-v2.json";
const MAX_SETTINGS_BYTES: usize = 32 * 1024;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const CATALOG_BUDGET: Duration = Duration::from_secs(30);
const INSTALL_BUDGET: Duration = Duration::from_secs(150);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DistributionSettingsView {
    pub(crate) source_url: Option<String>,
    pub(crate) configured: bool,
    pub(crate) credential_configured: bool,
    pub(crate) credential_available: bool,
    pub(crate) credential_state: String,
    pub(crate) expires_at: Option<String>,
    pub(crate) last_checked_at: Option<String>,
    pub(crate) warning: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DistributionConfigureRequest {
    pub(crate) source_url: Option<String>,
    pub(crate) access_code: Option<String>,
    #[serde(default)]
    pub(crate) clear_access_code: bool,
}

impl fmt::Debug for DistributionConfigureRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DistributionConfigureRequest")
            .field("source_url", &self.source_url)
            .field(
                "access_code",
                &self.access_code.as_ref().map(|_| "<redacted>"),
            )
            .field("clear_access_code", &self.clear_access_code)
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginCatalogRegistration {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) author: String,
    pub(crate) visibility: String,
    pub(crate) update_manifest_url: String,
    pub(crate) signing_public_key: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginCatalogEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) author: String,
    pub(crate) source_url: String,
    pub(crate) signing_public_key: String,
    pub(crate) version: String,
    pub(crate) release_notes_url: Option<String>,
    pub(crate) download_url: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) host_compatibility: HostCompatibility,
    pub(crate) ui_bridge_compatibility: Option<ProtocolCompatibility>,
    pub(crate) platform: PluginPlatform,
    #[serde(default)]
    pub(crate) dependencies: Vec<PluginDependency>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginCatalogItem {
    pub(crate) registration: PluginCatalogRegistration,
    pub(crate) entry: Option<PluginCatalogEntry>,
    pub(crate) compatible: bool,
    pub(crate) installed_version: Option<String>,
    pub(crate) installable: bool,
    #[serde(default)]
    pub(crate) action: String,
    pub(crate) stale: bool,
    pub(crate) error: Option<PluginError>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginCatalogSnapshot {
    pub(crate) source_url: Option<String>,
    pub(crate) generated_at: Option<String>,
    pub(crate) fetched_at: Option<String>,
    pub(crate) stale: bool,
    pub(crate) status: String,
    pub(crate) credential_state: String,
    pub(crate) retry_after_seconds: Option<u64>,
    pub(crate) message: Option<String>,
    pub(crate) plugins: Vec<PluginCatalogItem>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogInstallMode {
    #[default]
    Update,
    Repair,
    Rollback,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CatalogInstallRequest {
    pub(crate) plugin_id: String,
    pub(crate) version: String,
    pub(crate) expected_sha256: String,
    pub(crate) approved_source_change: bool,
    #[serde(default)]
    pub(crate) mode: CatalogInstallMode,
    #[serde(default)]
    pub(crate) operation_id: Option<String>,
    #[serde(default)]
    pub(crate) expected_source_url: Option<String>,
}

pub(crate) struct PreparedCatalogPackage {
    pub(crate) entry: PluginCatalogEntry,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsFile {
    schema_version: u32,
    source_url: Option<String>,
    encrypted_access_code: Option<String>,
    credential_identity: Option<String>,
    #[serde(default)]
    credential_state: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    last_checked_at: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CacheFile {
    schema_version: u32,
    source_url: String,
    credential_identity: Option<String>,
    generated_at: String,
    fetched_at: String,
    plugins: Vec<PluginCatalogItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogIndex {
    schema_version: u32,
    generated_at: String,
    plugins: Vec<PluginCatalogRegistration>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccessStatus {
    schema_version: u32,
    state: String,
    expires_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdatePayload {
    schema_version: u32,
    id: String,
    version: String,
    release_notes_url: Option<String>,
    download_url: String,
    sha256: String,
    size_bytes: u64,
    host_compatibility: HostCompatibility,
    ui_bridge_compatibility: Option<ProtocolCompatibility>,
    platform: PluginPlatform,
    #[serde(default)]
    dependencies: Vec<PluginDependency>,
}

struct RuntimeState {
    source: Option<DistributionSource>,
    code: Option<Arc<Secret<String>>>,
    encrypted_code: Option<String>,
    credential_identity: Option<String>,
    credential_state: String,
    expires_at: Option<String>,
    last_checked_at: Option<String>,
    warning: Option<String>,
    config_error: bool,
    generation: u64,
    request_sequence: u64,
    public_cache: Option<CacheFile>,
    restricted_cache: Option<CacheFile>,
    recent: Option<(u64, Instant, PluginCatalogSnapshot)>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            source: None,
            code: None,
            encrypted_code: None,
            credential_identity: None,
            credential_state: "none".into(),
            expires_at: None,
            last_checked_at: None,
            warning: None,
            config_error: false,
            generation: 1,
            request_sequence: 0,
            public_cache: None,
            restricted_cache: None,
            recent: None,
        }
    }
}

#[derive(Clone)]
struct RequestContext {
    source: DistributionSource,
    code: Option<Arc<Secret<String>>>,
    identity: Option<String>,
    generation: u64,
    sequence: u64,
}

impl RequestContext {
    fn code(&self) -> Option<&str> {
        self.code.as_ref().map(|code| code.expose().as_str())
    }
}

pub(crate) struct PluginDistributionService {
    app_data_dir: PathBuf,
    state: Mutex<RuntimeState>,
    catalog_gate: tokio::sync::Mutex<()>,
}

impl PluginDistributionService {
    pub(crate) fn load(app_data_dir: &Path) -> Self {
        #[cfg(debug_assertions)]
        let test_origin = std::env::var("WONDERLAND_DISTRIBUTION_TEST_ORIGIN").ok();
        #[cfg(not(debug_assertions))]
        let test_origin: Option<String> = None;
        Self::load_with_origin(app_data_dir, test_origin.as_deref())
    }

    fn load_with_origin(app_data_dir: &Path, test_origin: Option<&str>) -> Self {
        #[cfg(not(debug_assertions))]
        let _ = test_origin;
        let mut state = RuntimeState::default();
        let path = app_data_dir.join(SETTINGS_FILE);
        match read_bounded(&path, MAX_SETTINGS_BYTES) {
            Ok(Some(bytes)) => {
                if let Err(error) = load_settings_bytes(&mut state, &bytes) {
                    state.config_error = true;
                    state.credential_state = "unavailable".into();
                    state.warning = Some(error.message);
                }
            }
            Ok(None) => {
                state.source = Some(
                    DistributionSource::parse(DEFAULT_SOURCE_URL)
                        .expect("the official distribution origin must be valid"),
                );
            }
            Err(_) => {
                state.config_error = true;
                state.credential_state = "unavailable".into();
                state.warning = Some(message("CREDENTIAL_STORE_UNAVAILABLE").into());
            }
        }
        #[cfg(debug_assertions)]
        if let Some(origin) = test_origin
            && let Ok(source) = DistributionSource::debug_loopback(origin)
        {
            if state.source.as_ref() != Some(&source) {
                state.public_cache = None;
                state.restricted_cache = None;
                // A persisted test source may only be reactivated by the explicit injection.
                if let Ok(Some(bytes)) = read_bounded(&path, MAX_SETTINGS_BYTES)
                    && let Ok(file) = serde_json::from_slice::<SettingsFile>(&bytes)
                    && file.schema_version == 1
                    && file.source_url.as_deref() == Some(source.as_str())
                {
                    state.config_error = false;
                    state.warning = None;
                    load_credential(
                        &mut state,
                        file.encrypted_access_code,
                        file.credential_identity,
                    );
                    restore_access_state(
                        &mut state,
                        file.credential_state,
                        file.expires_at,
                        file.last_checked_at,
                    );
                } else {
                    state.code = None;
                    state.encrypted_code = None;
                    state.credential_identity = None;
                    state.credential_state = "none".into();
                }
            }
            state.source = Some(source);
        }
        if let Some(source) = &state.source {
            state.public_cache =
                load_cache(&app_data_dir.join(PUBLIC_CACHE_FILE), source, None, false);
            if state.code.is_some()
                && !matches!(
                    state.credential_state.as_str(),
                    "invalid" | "expired" | "revoked"
                )
            {
                state.restricted_cache = load_cache(
                    &app_data_dir.join(RESTRICTED_CACHE_FILE),
                    source,
                    state.credential_identity.as_deref(),
                    true,
                );
            }
        }
        Self {
            app_data_dir: app_data_dir.to_owned(),
            state: Mutex::new(state),
            catalog_gate: tokio::sync::Mutex::new(()),
        }
    }

    pub(crate) fn settings(&self) -> DistributionSettingsView {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.expire_known_code(&mut state);
        settings_view(&state)
    }

    pub(crate) fn visibility_key(&self) -> (Option<String>, Option<String>, String, Vec<String>) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut ids: Vec<_> = state
            .restricted_cache
            .as_ref()
            .into_iter()
            .flat_map(|cache| &cache.plugins)
            .map(|item| item.registration.id.clone())
            .collect();
        ids.sort();
        (
            state.source.as_ref().map(|source| source.as_str().into()),
            state.credential_identity.clone(),
            state.credential_state.clone(),
            ids,
        )
    }

    pub(crate) fn configure(
        &self,
        request: DistributionConfigureRequest,
    ) -> Result<DistributionSettingsView, PluginError> {
        if request.clear_access_code && request.access_code.is_some() {
            return Err(failure("INVALID_REQUEST"));
        }
        let parsed_source = request
            .source_url
            .as_deref()
            .map(|raw| {
                if raw.trim().is_empty() {
                    Ok(None)
                } else {
                    DistributionSource::parse(raw)
                        .map(Some)
                        .map_err(http_failure)
                }
            })
            .transpose()?;
        if let Some(code) = &request.access_code {
            validate_code(code)?;
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
        let source = parsed_source.unwrap_or_else(|| state.source.clone());
        let source_changed = state.source != source;
        let clear = source_changed || request.clear_access_code;
        let mut encrypted_code = if clear {
            None
        } else {
            state.encrypted_code.clone()
        };
        let mut identity = if clear {
            None
        } else {
            state.credential_identity.clone()
        };
        let mut code = if clear { None } else { state.code.clone() };
        if let Some(value) = request.access_code {
            if source.is_none() {
                return Err(failure("DISTRIBUTION_NOT_CONFIGURED"));
            }
            encrypted_code = Some(
                base64::engine::general_purpose::STANDARD.encode(
                    wonderland_secret::seal(value.as_bytes())
                        .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?,
                ),
            );
            identity = Some(random_id()?);
            code = Some(Arc::new(Secret::new(value)));
        }
        let code_changed = identity != state.credential_identity;
        let reset = source_changed || code_changed || request.clear_access_code;
        let persisted = SettingsFile {
            schema_version: 1,
            source_url: source.as_ref().map(|source| source.as_str().into()),
            encrypted_access_code: encrypted_code.clone(),
            credential_identity: identity.clone(),
            credential_state: Some(
                if reset {
                    if code.is_some() { "unchecked" } else { "none" }
                } else {
                    &state.credential_state
                }
                .into(),
            ),
            expires_at: if reset {
                None
            } else {
                state.expires_at.clone()
            },
            last_checked_at: if reset {
                None
            } else {
                state.last_checked_at.clone()
            },
        };
        let bytes = serde_json::to_vec_pretty(&persisted)
            .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
        let path = self.app_data_dir.join(SETTINGS_FILE);
        if state.config_error {
            preserve_damaged_file(&path)?;
        }
        atomic_write(&path, &bytes).map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
        state.source = source;
        state.encrypted_code = encrypted_code;
        state.credential_identity = identity;
        state.code = code;
        state.config_error = false;
        state.warning = None;
        if source_changed || code_changed || request.clear_access_code {
            state.generation = state.generation.wrapping_add(1);
            state.request_sequence = state.request_sequence.wrapping_add(1);
            state.recent = None;
            state.restricted_cache = None;
            let _ = fs::remove_file(self.app_data_dir.join(RESTRICTED_CACHE_FILE));
            let _ = fs::remove_file(
                self.app_data_dir
                    .join("plugin-distribution-restricted-v1.json"),
            );
            state.credential_state = if state.code.is_some() {
                "unchecked"
            } else {
                "none"
            }
            .into();
            state.expires_at = None;
            state.last_checked_at = None;
            if source_changed {
                state.public_cache = None;
                let _ = fs::remove_file(self.app_data_dir.join(PUBLIC_CACHE_FILE));
                let _ =
                    fs::remove_file(self.app_data_dir.join("plugin-distribution-public-v1.json"));
            }
        }
        Ok(settings_view(&state))
    }

    pub(crate) async fn catalog(
        &self,
        installed: &[PluginRuntimeState],
    ) -> Result<PluginCatalogSnapshot, PluginError> {
        match tokio::time::timeout(CATALOG_BUDGET, self.catalog_inner(installed)).await {
            Ok(result) => result,
            Err(_) => Ok(self.fallback_snapshot(
                installed,
                "offline",
                Some(failure("DISTRIBUTION_UNAVAILABLE")),
            )),
        }
    }

    async fn catalog_inner(
        &self,
        installed: &[PluginRuntimeState],
    ) -> Result<PluginCatalogSnapshot, PluginError> {
        let requested_at = Instant::now();
        let _gate = self.catalog_gate.lock().await;
        let mut context = match self.begin_request() {
            Ok(context) => context,
            Err(error) => {
                return Ok(self.fallback_snapshot(
                    installed,
                    if error.code == "DISTRIBUTION_NOT_CONFIGURED" {
                        "unconfigured"
                    } else {
                        "unavailable"
                    },
                    Some(error),
                ));
            }
        };
        {
            let state = self
                .state
                .lock()
                .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
            if let Some((generation, at, snapshot)) = &state.recent
                && *generation == context.generation
                && *at >= requested_at
            {
                let mut snapshot = snapshot.clone();
                apply_installed(&mut snapshot, installed);
                return Ok(snapshot);
            }
        }
        let client = DistributionClient::new(context.source.clone()).map_err(http_failure)?;
        if let Err(error) = self.authenticate(&client, &mut context).await {
            return Ok(self.fallback_for_error(installed, error));
        }
        let bytes = match client.catalog(context.code()).await {
            Ok(bytes) => bytes,
            Err(error) if credential_error(error.code) => {
                context = self.invalidate_and_renew(&context, error.code, None, true)?;
                match client.catalog(None).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        return Ok(self.fallback_for_error(installed, http_failure(error)));
                    }
                }
            }
            Err(error) => return Ok(self.fallback_for_error(installed, http_failure(error))),
        };
        let index = match parse_index(&bytes, &context.source, context.code.is_some()) {
            Ok(index) => index,
            Err(error) => return Ok(self.fallback_for_error(installed, error)),
        };
        // A valid complete index already establishes the current visible scope. Remove
        // lost entries now, even if individual manifests subsequently fail or time out.
        self.accept_live_scope(&mut context, &index);
        self.ensure_current(&context)?;
        let mut pending = index.plugins.into_iter();
        let mut tasks = JoinSet::new();
        for registration in pending.by_ref().take(8) {
            spawn_update(
                &mut tasks,
                client.clone(),
                context.code.clone(),
                registration,
            );
        }
        let mut items = Vec::new();
        let mut item_bytes = 0usize;
        let mut first_error = None;
        while let Some(result) = tasks.join_next().await {
            let (registration, result) = result.map_err(|_| failure("DISTRIBUTION_UNAVAILABLE"))?;
            let (entry, error) = match result {
                Ok(bytes) => match parse_update_index(&bytes, &registration, &context.source) {
                    Ok(entry) => (Some(entry), None),
                    Err(error) => (None, Some(error)),
                },
                Err(error) if credential_error(error.code) => {
                    let _ = self.invalidate(&context, error.code, None);
                    return Ok(self.fallback_for_error(installed, http_failure(error)));
                }
                Err(error)
                    if error.code == "NOT_AVAILABLE" && registration.visibility == "restricted" =>
                {
                    // The fresh complete index is also replaced below, but this denial must
                    // invalidate any older in-flight catalog immediately.
                    self.ensure_current(&context)?;
                    context = self.invalidate_and_renew(
                        &context,
                        error.code,
                        Some(&registration.id),
                        false,
                    )?;
                    if let Some(registration) = pending.next() {
                        spawn_update(
                            &mut tasks,
                            client.clone(),
                            context.code.clone(),
                            registration,
                        );
                    }
                    continue;
                }
                Err(error) => (None, Some(http_failure(error))),
            };
            if let Some(error) = &error {
                first_error.get_or_insert_with(|| error.clone());
            }
            let item = PluginCatalogItem {
                registration,
                entry,
                compatible: false,
                installed_version: None,
                installable: false,
                action: "unavailable".into(),
                stale: error.is_some(),
                error,
            };
            item_bytes = item_bytes.saturating_add(
                serde_json::to_vec(&item)
                    .map_err(|_| failure("INVALID_CATALOG"))?
                    .len(),
            );
            if item_bytes > MAX_SNAPSHOT_BYTES - 4096 {
                return Ok(self.fallback_for_error(installed, failure("RESOURCE_LIMIT")));
            }
            items.push(item);
            if let Some(registration) = pending.next() {
                spawn_update(
                    &mut tasks,
                    client.clone(),
                    context.code.clone(),
                    registration,
                );
            }
        }
        items.sort_by(|left, right| left.registration.id.cmp(&right.registration.id));
        self.commit_catalog(&context, index.generated_at, items, installed, first_error)
    }

    #[cfg(test)]
    pub(crate) async fn prepare_install(
        &self,
        request: &CatalogInstallRequest,
        installed: &[PluginRuntimeState],
    ) -> Result<PreparedCatalogPackage, PluginError> {
        self.prepare_install_with_progress(request, installed, &|_, _, _| {})
            .await
    }

    pub(crate) async fn prepare_install_with_progress(
        &self,
        request: &CatalogInstallRequest,
        installed: &[PluginRuntimeState],
        progress: &(dyn Fn(&str, u64, u64) + Send + Sync),
    ) -> Result<PreparedCatalogPackage, PluginError> {
        if !valid_id(&request.plugin_id)
            || !valid_version(&request.version)
            || !lower_hex(&request.expected_sha256, 64)
        {
            return Err(failure("INVALID_REQUEST"));
        }
        tokio::time::timeout(
            INSTALL_BUDGET,
            self.prepare_install_inner(request, installed, progress),
        )
        .await
        .map_err(|_| failure("DISTRIBUTION_UNAVAILABLE"))?
    }

    async fn prepare_install_inner(
        &self,
        request: &CatalogInstallRequest,
        installed: &[PluginRuntimeState],
        progress: &(dyn Fn(&str, u64, u64) + Send + Sync),
    ) -> Result<PreparedCatalogPackage, PluginError> {
        let mut context = self.begin_request()?;
        if request
            .expected_source_url
            .as_deref()
            .is_some_and(|origin| origin != context.source.as_str())
        {
            return Err(failure("CATALOG_CHANGED"));
        }
        let client = DistributionClient::new(context.source.clone()).map_err(http_failure)?;
        self.authenticate(&client, &mut context).await?;
        let bytes = match client.catalog(context.code()).await {
            Ok(bytes) => bytes,
            Err(error) if credential_error(error.code) => {
                context = self.invalidate_and_renew(&context, error.code, None, true)?;
                client.catalog(None).await.map_err(http_failure)?
            }
            Err(error) => return Err(http_failure(error)),
        };
        let index = parse_index(&bytes, &context.source, context.code.is_some())?;
        self.accept_live_scope(&mut context, &index);
        let registration = index
            .plugins
            .iter()
            .find(|registration| registration.id == request.plugin_id)
            .ok_or_else(|| {
                let _ = self.invalidate(&context, "NOT_AVAILABLE", Some(&request.plugin_id));
                failure("NOT_AVAILABLE")
            })?;
        let update = self.request_result(
            &context,
            client.update(&registration.id, context.code()).await,
            Some(&registration.id),
        )?;
        let entry = parse_update_index(&update, registration, &context.source)?;
        if entry.version != request.version || entry.sha256 != request.expected_sha256 {
            return Err(failure("CATALOG_CHANGED"));
        }
        if !catalog_entry_is_compatible(&entry) {
            return Err(failure("INCOMPATIBLE_CORE"));
        }
        validate_install_mode(
            request.mode,
            &entry.version,
            installed
                .iter()
                .find(|state| state.manifest.id == entry.id)
                .map(|state| state.manifest.version.as_str()),
        )?;
        self.ensure_current(&context)?;
        progress("downloading", 0, entry.size_bytes);
        let bytes = self.request_result(
            &context,
            client
                .package_with_progress(&entry.id, &entry.version, context.code(), &|bytes, _| {
                    progress("downloading", bytes, entry.size_bytes)
                })
                .await,
            Some(&entry.id),
        )?;
        progress("verifying", bytes.len() as u64, entry.size_bytes);
        if bytes.len() as u64 != entry.size_bytes
            || hex::encode(Sha256::digest(&bytes)) != entry.sha256
        {
            return Err(failure("PACKAGE_CHECKSUM_MISMATCH"));
        }
        // A completed download is intentionally permitted to finish verification/installation
        // even when the user changes the code while its already-authorized stream is running.
        Ok(PreparedCatalogPackage { entry, bytes })
    }

    fn begin_request(&self) -> Result<RequestContext, PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
        self.expire_known_code(&mut state);
        if state.config_error {
            return Err(failure("CREDENTIAL_STORE_UNAVAILABLE"));
        }
        let source = state
            .source
            .clone()
            .ok_or_else(|| failure("DISTRIBUTION_NOT_CONFIGURED"))?;
        state.request_sequence = state.request_sequence.wrapping_add(1);
        // Keep checking the saved code on new requests: administrators may extend its
        // expiry or restore its scope. Previously known failures never restore cached data.
        Ok(RequestContext {
            source,
            code: state.code.clone(),
            identity: state.credential_identity.clone(),
            generation: state.generation,
            sequence: state.request_sequence,
        })
    }

    async fn authenticate(
        &self,
        client: &DistributionClient,
        context: &mut RequestContext,
    ) -> Result<(), PluginError> {
        let bytes = match client.access(context.code()).await {
            Ok(bytes) => bytes,
            Err(error) if credential_error(error.code) => {
                *context = self.invalidate_and_renew(context, error.code, None, true)?;
                client.access(None).await.map_err(http_failure)?
            }
            Err(error) => return Err(http_failure(error)),
        };
        let access: AccessStatus =
            serde_json::from_slice(&bytes).map_err(|_| failure("INVALID_CATALOG"))?;
        if access.schema_version != 1
            || access.state
                != if context.code.is_some() {
                    "valid"
                } else {
                    "none"
                }
            || access
                .expires_at
                .as_deref()
                .is_some_and(|at| !valid_timestamp(at))
        {
            return Err(failure("INVALID_CATALOG"));
        }
        if let Some(expires) = &access.expires_at
            && chrono::DateTime::parse_from_rfc3339(expires)
                .is_ok_and(|expires| expires <= Utc::now())
        {
            *context = self.invalidate_and_renew(context, "ACCESS_CODE_EXPIRED", None, true)?;
            return Ok(());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
        if state.generation == context.generation {
            if context.code.is_some() {
                state.credential_state = "valid".into();
                state.expires_at = access.expires_at;
            } else if state.encrypted_code.is_none() {
                state.credential_state = "none".into();
            }
            state.last_checked_at = Some(Utc::now().to_rfc3339());
            if !self.persist_access_state(&state) {
                state.warning = Some("访问状态无法保存；本次同步结果仅在当前进程内有效。".into());
            }
        }
        Ok(())
    }

    fn ensure_current(&self, context: &RequestContext) -> Result<(), PluginError> {
        let state = self
            .state
            .lock()
            .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
        if state.generation != context.generation || state.source.as_ref() != Some(&context.source)
        {
            Err(failure("CATALOG_CHANGED"))
        } else {
            Ok(())
        }
    }

    fn renew_same_identity(
        &self,
        previous: &RequestContext,
        anonymous: bool,
    ) -> Result<RequestContext, PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
        if state.source.as_ref() != Some(&previous.source)
            || state.credential_identity != previous.identity
            || state.generation != previous.generation
            || state.request_sequence != previous.sequence
        {
            return Err(failure("CATALOG_CHANGED"));
        }
        state.request_sequence = state.request_sequence.wrapping_add(1);
        Ok(RequestContext {
            source: previous.source.clone(),
            code: if anonymous { None } else { state.code.clone() },
            identity: previous.identity.clone(),
            generation: state.generation,
            sequence: state.request_sequence,
        })
    }

    fn invalidate_and_renew(
        &self,
        previous: &RequestContext,
        code: &str,
        plugin_id: Option<&str>,
        anonymous: bool,
    ) -> Result<RequestContext, PluginError> {
        let generation = self
            .invalidate(previous, code, plugin_id)
            .ok_or_else(|| failure("CATALOG_CHANGED"))?;
        let mut invalidated = previous.clone();
        invalidated.generation = generation;
        self.renew_same_identity(&invalidated, anonymous)
    }

    fn request_result(
        &self,
        context: &RequestContext,
        result: Result<Vec<u8>, DistributionHttpError>,
        plugin_id: Option<&str>,
    ) -> Result<Vec<u8>, PluginError> {
        result.map_err(|error| {
            if credential_error(error.code) || error.code == "NOT_AVAILABLE" {
                let _ = self.invalidate(context, error.code, plugin_id);
            }
            http_failure(error)
        })
    }

    fn invalidate(
        &self,
        context: &RequestContext,
        code: &str,
        plugin_id: Option<&str>,
    ) -> Option<u64> {
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        if state.generation != context.generation {
            return None;
        }
        state.generation = state.generation.wrapping_add(1);
        state.recent = None;
        if credential_error(code) {
            state.credential_state = match code {
                "ACCESS_CODE_EXPIRED" => "expired",
                "ACCESS_CODE_REVOKED" => "revoked",
                _ => "invalid",
            }
            .into();
            state.last_checked_at = Some(Utc::now().to_rfc3339());
            state.restricted_cache = None;
            let saved = self.persist_access_state(&state);
            let removed = self.write_restricted_cache(&state);
            if !saved && !removed {
                state.warning = Some("访问码失效状态和受限目录清理均无法保存；重启后可能重新显示旧介绍，请修复配置存储权限。".into());
            }
        } else if let Some(id) = plugin_id
            && let Some(cache) = &mut state.restricted_cache
        {
            cache.plugins.retain(|item| item.registration.id != id);
        }
        if !self.write_restricted_cache(&state) && !credential_error(code) {
            state.warning =
                Some("受限目录清理无法保存；重启后可能重新显示旧介绍，请修复配置存储权限。".into());
        }
        Some(state.generation)
    }

    fn accept_live_scope(&self, context: &mut RequestContext, index: &CatalogIndex) {
        let allowed: BTreeSet<_> = index
            .plugins
            .iter()
            .filter(|item| item.visibility == "restricted")
            .map(|item| item.id.as_str())
            .collect();
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.generation != context.generation {
            return;
        }
        let changed = state.restricted_cache.as_mut().is_some_and(|cache| {
            let old_len = cache.plugins.len();
            cache
                .plugins
                .retain(|item| allowed.contains(item.registration.id.as_str()));
            old_len != cache.plugins.len()
        });
        if changed {
            state.generation = state.generation.wrapping_add(1);
            context.generation = state.generation;
            state.recent = None;
            if !self.write_restricted_cache(&state) {
                state.warning = Some(
                    "受限目录范围清理无法保存；重启后可能重新显示旧介绍，请修复配置存储权限。"
                        .into(),
                );
            }
        }
    }

    fn expire_known_code(&self, state: &mut RuntimeState) {
        if state.credential_state == "valid"
            && state.expires_at.as_deref().is_some_and(|at| {
                chrono::DateTime::parse_from_rfc3339(at).is_ok_and(|at| at <= Utc::now())
            })
        {
            state.credential_state = "expired".into();
            state.generation = state.generation.wrapping_add(1);
            state.recent = None;
            state.restricted_cache = None;
            let removed = self.write_restricted_cache(state);
            let saved = self.persist_access_state(state);
            if !saved && !removed {
                state.warning = Some("访问码过期状态和受限目录清理均无法保存；重启后可能重新显示旧介绍，请修复配置存储权限。".into());
            }
        }
    }

    fn commit_catalog(
        &self,
        context: &RequestContext,
        generated_at: String,
        items: Vec<PluginCatalogItem>,
        installed: &[PluginRuntimeState],
        problem: Option<PluginError>,
    ) -> Result<PluginCatalogSnapshot, PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
        if state.generation != context.generation || state.request_sequence != context.sequence {
            return Ok(snapshot_from_state(
                &state,
                installed,
                "unavailable",
                Some(failure("CATALOG_CHANGED")),
            ));
        }
        let fetched_at = Utc::now().to_rfc3339();
        let public = CacheFile {
            schema_version: 2,
            source_url: context.source.as_str().into(),
            credential_identity: None,
            generated_at: generated_at.clone(),
            fetched_at: fetched_at.clone(),
            plugins: items
                .iter()
                .filter(|item| item.registration.visibility == "public")
                .cloned()
                .collect(),
        };
        let restricted = CacheFile {
            schema_version: 2,
            source_url: context.source.as_str().into(),
            credential_identity: context.identity.clone(),
            generated_at: generated_at.clone(),
            fetched_at: fetched_at.clone(),
            plugins: items
                .iter()
                .filter(|item| item.registration.visibility == "restricted")
                .cloned()
                .collect(),
        };
        let old_ids: BTreeSet<_> = state
            .restricted_cache
            .as_ref()
            .into_iter()
            .flat_map(|cache| &cache.plugins)
            .map(|item| item.registration.id.clone())
            .collect();
        let new_ids: BTreeSet<_> = restricted
            .plugins
            .iter()
            .map(|item| item.registration.id.clone())
            .collect();
        if old_ids != new_ids {
            state.generation = state.generation.wrapping_add(1);
        }
        state.public_cache = Some(public);
        state.restricted_cache = if context.code.is_some() {
            Some(restricted)
        } else {
            None
        };
        if let Some(cache) = &state.public_cache {
            let _ = persist_cache(&self.app_data_dir.join(PUBLIC_CACHE_FILE), cache);
        }
        if !self.write_restricted_cache(&state) {
            state.warning = Some("受限目录快照无法保存；当前同步结果仍可展示。".into());
        }
        let mut snapshot = PluginCatalogSnapshot {
            source_url: Some(context.source.as_str().into()),
            generated_at: Some(generated_at),
            fetched_at: Some(fetched_at),
            stale: problem.is_some(),
            status: problem
                .as_ref()
                .map_or("ready", |error| status_for_error(&error.code))
                .into(),
            credential_state: state.credential_state.clone(),
            retry_after_seconds: problem.as_ref().and_then(retry_seconds),
            message: problem
                .as_ref()
                .map(|error| error.message.clone())
                .or_else(|| credential_message(&state)),
            plugins: items,
        };
        apply_installed(&mut snapshot, installed);
        if problem.is_some() {
            for item in &mut snapshot.plugins {
                item.installable = false;
            }
        }
        state.recent = Some((state.generation, Instant::now(), snapshot.clone()));
        Ok(snapshot)
    }

    fn write_restricted_cache(&self, state: &RuntimeState) -> bool {
        let path = self.app_data_dir.join(RESTRICTED_CACHE_FILE);
        if let Some(cache) = &state.restricted_cache
            && !cache.plugins.is_empty()
        {
            persist_cache(&path, cache).is_ok()
        } else {
            let _ = fs::remove_file(
                self.app_data_dir
                    .join("plugin-distribution-restricted-v1.json"),
            );
            match fs::remove_file(path) {
                Ok(()) => true,
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            }
        }
    }

    fn persist_access_state(&self, state: &RuntimeState) -> bool {
        if state.config_error {
            return false;
        }
        let file = SettingsFile {
            schema_version: 1,
            source_url: state.source.as_ref().map(|source| source.as_str().into()),
            encrypted_access_code: state.encrypted_code.clone(),
            credential_identity: state.credential_identity.clone(),
            credential_state: Some(state.credential_state.clone()),
            expires_at: state.expires_at.clone(),
            last_checked_at: state.last_checked_at.clone(),
        };
        serde_json::to_vec_pretty(&file)
            .is_ok_and(|bytes| atomic_write(&self.app_data_dir.join(SETTINGS_FILE), &bytes).is_ok())
    }

    fn fallback_for_error(
        &self,
        installed: &[PluginRuntimeState],
        error: PluginError,
    ) -> PluginCatalogSnapshot {
        let status = status_for_error(&error.code);
        self.fallback_snapshot(installed, status, Some(error))
    }

    fn fallback_snapshot(
        &self,
        installed: &[PluginRuntimeState],
        status: &str,
        error: Option<PluginError>,
    ) -> PluginCatalogSnapshot {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.expire_known_code(&mut state);
        snapshot_from_state(&state, installed, status, error)
    }
}

fn settings_view(state: &RuntimeState) -> DistributionSettingsView {
    DistributionSettingsView {
        source_url: state.source.as_ref().map(|source| source.as_str().into()),
        configured: state.source.is_some() && !state.config_error,
        credential_configured: state.encrypted_code.is_some(),
        credential_available: !state.config_error
            && (state.encrypted_code.is_none() || state.code.is_some()),
        credential_state: state.credential_state.clone(),
        expires_at: state.expires_at.clone(),
        last_checked_at: state.last_checked_at.clone(),
        warning: state.warning.clone(),
    }
}

fn load_settings_bytes(state: &mut RuntimeState, bytes: &[u8]) -> Result<(), PluginError> {
    let file: SettingsFile =
        serde_json::from_slice(bytes).map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
    if file.schema_version != 1
        || file.encrypted_access_code.is_some() != file.credential_identity.is_some()
        || file
            .credential_identity
            .as_deref()
            .is_some_and(|id| !lower_hex(id, 32))
    {
        return Err(failure("CREDENTIAL_STORE_UNAVAILABLE"));
    }
    state.source = file
        .source_url
        .as_deref()
        .map(DistributionSource::parse)
        .transpose()
        .map_err(http_failure)?;
    if state.source.is_none() && file.encrypted_access_code.is_some() {
        return Err(failure("CREDENTIAL_STORE_UNAVAILABLE"));
    }
    load_credential(state, file.encrypted_access_code, file.credential_identity);
    restore_access_state(
        state,
        file.credential_state,
        file.expires_at,
        file.last_checked_at,
    );
    Ok(())
}

fn restore_access_state(
    state: &mut RuntimeState,
    saved: Option<String>,
    expires: Option<String>,
    checked: Option<String>,
) {
    if state.code.is_some()
        && saved.as_deref().is_some_and(|value| {
            matches!(
                value,
                "unchecked" | "valid" | "invalid" | "expired" | "revoked"
            )
        })
        && expires.as_deref().is_none_or(valid_timestamp)
        && checked.as_deref().is_none_or(valid_timestamp)
    {
        state.credential_state = saved.unwrap_or_else(|| "unchecked".into());
        state.expires_at = expires;
        state.last_checked_at = checked;
    }
}

fn load_credential(state: &mut RuntimeState, encrypted: Option<String>, identity: Option<String>) {
    state.encrypted_code = encrypted;
    state.credential_identity = identity;
    state.code = state.encrypted_code.as_ref().and_then(|value| {
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .ok()
            .and_then(|value| wonderland_secret::unseal(&value).ok())
            .and_then(|value| String::from_utf8(value).ok())
            .filter(|value| validate_code(value).is_ok())
            .map(|value| Arc::new(Secret::new(value)))
    });
    state.credential_state = if state.encrypted_code.is_none() {
        "none"
    } else if state.code.is_some() {
        "unchecked"
    } else {
        "unavailable"
    }
    .into();
    if state.encrypted_code.is_some() && state.code.is_none() {
        state.warning = Some(message("CREDENTIAL_STORE_UNAVAILABLE").into());
    }
}

fn spawn_update(
    tasks: &mut JoinSet<(
        PluginCatalogRegistration,
        Result<Vec<u8>, DistributionHttpError>,
    )>,
    client: DistributionClient,
    code: Option<Arc<Secret<String>>>,
    registration: PluginCatalogRegistration,
) {
    tasks.spawn(async move {
        let bytes = client
            .update(
                &registration.id,
                code.as_ref().map(|code| code.expose().as_str()),
            )
            .await;
        (registration, bytes)
    });
}

fn snapshot_from_state(
    state: &RuntimeState,
    installed: &[PluginRuntimeState],
    status: &str,
    error: Option<PluginError>,
) -> PluginCatalogSnapshot {
    let caches = state
        .public_cache
        .iter()
        .chain(state.restricted_cache.iter());
    let mut plugins = Vec::new();
    let mut generated_at = None;
    let mut fetched_at = None;
    let mut item_bytes = 0usize;
    for cache in caches {
        generated_at = Some(cache.generated_at.clone());
        fetched_at = Some(cache.fetched_at.clone());
        for item in &cache.plugins {
            let size = serde_json::to_vec(item).map_or(MAX_SNAPSHOT_BYTES, |bytes| bytes.len());
            if item_bytes.saturating_add(size) > MAX_SNAPSHOT_BYTES - 4096 {
                break;
            }
            item_bytes += size;
            plugins.push(item.clone());
        }
    }
    plugins.sort_by(|left, right| left.registration.id.cmp(&right.registration.id));
    plugins.dedup_by(|left, right| left.registration.id == right.registration.id);
    let mut snapshot = PluginCatalogSnapshot {
        source_url: state.source.as_ref().map(|source| source.as_str().into()),
        generated_at,
        fetched_at,
        stale: true,
        status: status.into(),
        credential_state: state.credential_state.clone(),
        retry_after_seconds: error.as_ref().and_then(retry_seconds),
        message: error
            .map(|error| error.message)
            .or_else(|| credential_message(state)),
        plugins,
    };
    apply_installed(&mut snapshot, installed);
    for item in &mut snapshot.plugins {
        item.stale = true;
        item.installable = false;
    }
    snapshot
}

fn apply_installed(snapshot: &mut PluginCatalogSnapshot, installed: &[PluginRuntimeState]) {
    let versions: BTreeMap<_, _> = installed
        .iter()
        .map(|state| (state.manifest.id.as_str(), state.manifest.version.as_str()))
        .collect();
    for item in &mut snapshot.plugins {
        item.installed_version = versions
            .get(item.registration.id.as_str())
            .map(|version| (*version).into());
        item.compatible = item.entry.as_ref().is_some_and(catalog_entry_is_compatible);
        item.action = item
            .entry
            .as_ref()
            .map(|entry| match item.installed_version.as_deref() {
                None => "install",
                Some(current) => match Version::parse(current)
                    .ok()
                    .zip(Version::parse(&entry.version).ok())
                    .map(|(a, b)| b.cmp(&a))
                {
                    Some(std::cmp::Ordering::Greater) => "update",
                    Some(std::cmp::Ordering::Less) => "rollback",
                    _ => "repair",
                },
            })
            .unwrap_or("unavailable")
            .into();
        item.installable =
            !snapshot.stale && !item.stale && item.compatible && item.entry.is_some();
    }
}

pub(crate) fn validate_install_mode(
    mode: CatalogInstallMode,
    version: &str,
    current: Option<&str>,
) -> Result<(), PluginError> {
    let next = Version::parse(version).map_err(|_| failure("INVALID_REQUEST"))?;
    let comparison = current
        .map(Version::parse)
        .transpose()
        .map_err(|_| failure("INVALID_REQUEST"))?
        .map(|current| next.cmp(&current));
    let allowed = match mode {
        CatalogInstallMode::Update => comparison.is_none_or(|value| value.is_gt()),
        CatalogInstallMode::Repair => comparison.is_some_and(|value| value.is_eq()),
        CatalogInstallMode::Rollback => comparison.is_some_and(|value| value.is_lt()),
    };
    if allowed {
        Ok(())
    } else {
        Err(failure(if mode == CatalogInstallMode::Update {
            "CATALOG_VERSION_NOT_NEWER"
        } else {
            "CATALOG_INSTALL_MODE_MISMATCH"
        }))
    }
}

fn parse_index(
    bytes: &[u8],
    source: &DistributionSource,
    authenticated: bool,
) -> Result<CatalogIndex, PluginError> {
    if bytes.len() > wonderland_net::distribution::CATALOG_MAX_BYTES {
        return Err(failure("INVALID_CATALOG"));
    }
    let index: CatalogIndex =
        serde_json::from_slice(bytes).map_err(|_| failure("INVALID_CATALOG"))?;
    if index.schema_version != 1
        || index.plugins.len() > 500
        || !valid_timestamp(&index.generated_at)
    {
        return Err(failure("INVALID_CATALOG"));
    }
    let mut ids = BTreeSet::new();
    for registration in &index.plugins {
        validate_registration(registration, source)?;
        if !ids.insert(&registration.id)
            || (!authenticated && registration.visibility == "restricted")
        {
            return Err(failure("INVALID_CATALOG"));
        }
    }
    Ok(index)
}

fn validate_registration(
    registration: &PluginCatalogRegistration,
    source: &DistributionSource,
) -> Result<(), PluginError> {
    if !valid_id(&registration.id)
        || registration.name.trim().is_empty()
        || registration.name.chars().count() > 120
        || registration.description.chars().count() > 2000
        || registration.author.trim().is_empty()
        || registration.author.chars().count() > 120
        || !matches!(registration.visibility.as_str(), "public" | "restricted")
        || registration.update_manifest_url != source.update_url(&registration.id)
        || !lower_hex(&registration.signing_public_key, 64)
    {
        return Err(failure("INVALID_CATALOG"));
    }
    Ok(())
}

fn parse_update_index(
    bytes: &[u8],
    registration: &PluginCatalogRegistration,
    source: &DistributionSource,
) -> Result<PluginCatalogEntry, PluginError> {
    if bytes.len() > wonderland_net::distribution::UPDATE_MAX_BYTES {
        return Err(failure("INVALID_UPDATE_MANIFEST"));
    }
    let payload: UpdatePayload =
        serde_json::from_slice(bytes).map_err(|_| failure("INVALID_UPDATE_MANIFEST"))?;
    if payload.schema_version != 2 || payload.id != registration.id {
        return Err(failure("INVALID_UPDATE_MANIFEST"));
    }
    let entry = PluginCatalogEntry {
        id: registration.id.clone(),
        name: registration.name.clone(),
        description: registration.description.clone(),
        author: registration.author.clone(),
        source_url: source.as_str().into(),
        signing_public_key: registration.signing_public_key.clone(),
        version: payload.version,
        release_notes_url: payload.release_notes_url,
        download_url: payload.download_url,
        sha256: payload.sha256,
        size_bytes: payload.size_bytes,
        host_compatibility: payload.host_compatibility,
        ui_bridge_compatibility: payload.ui_bridge_compatibility,
        platform: payload.platform,
        dependencies: payload.dependencies,
    };
    validate_entry(&entry, source)?;
    Ok(entry)
}

fn validate_entry(
    entry: &PluginCatalogEntry,
    source: &DistributionSource,
) -> Result<(), PluginError> {
    let dependencies: BTreeSet<_> = entry
        .dependencies
        .iter()
        .map(|dependency| &dependency.plugin_id)
        .collect();
    if !valid_id(&entry.id)
        || !valid_version(&entry.version)
        || entry.source_url != source.as_str()
        || entry.download_url != source.package_url(&entry.id, &entry.version)
        || entry
            .release_notes_url
            .as_deref()
            .is_some_and(|url| url.len() > 2048 || !source.accepts_url(url))
        || !lower_hex(&entry.signing_public_key, 64)
        || !lower_hex(&entry.sha256, 64)
        || entry.size_bytes == 0
        || entry.size_bytes > wonderland_net::distribution::PACKAGE_MAX_BYTES as u64
        || !valid_range(
            &entry.host_compatibility.min_core_version,
            &entry.host_compatibility.max_core_version_exclusive,
        )
        || !valid_range(
            &entry.host_compatibility.protocol.min_version,
            &entry.host_compatibility.protocol.max_version_exclusive,
        )
        || entry
            .ui_bridge_compatibility
            .as_ref()
            .is_some_and(|range| !valid_range(&range.min_version, &range.max_version_exclusive))
        || entry.platform.os != "windows"
        || !matches!(entry.platform.architecture.as_str(), "x86_64" | "aarch64")
        || entry.platform.abi != "msvc"
        || entry.dependencies.len() > 32
        || dependencies.len() != entry.dependencies.len()
        || entry.dependencies.iter().any(|dependency| {
            !valid_id(&dependency.plugin_id)
                || VersionReq::parse(&dependency.version_req).is_err()
                || dependency.methods.len() > 128
                || dependency
                    .methods
                    .iter()
                    .any(|method| !valid_method(method))
                || dependency.methods.iter().collect::<BTreeSet<_>>().len()
                    != dependency.methods.len()
        })
    {
        return Err(failure("INVALID_UPDATE_MANIFEST"));
    }
    Ok(())
}

pub(crate) fn catalog_entry_is_compatible(entry: &PluginCatalogEntry) -> bool {
    let Ok(core) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        return false;
    };
    let Ok(min) = Version::parse(&entry.host_compatibility.min_core_version) else {
        return false;
    };
    let Ok(max) = Version::parse(&entry.host_compatibility.max_core_version_exclusive) else {
        return false;
    };
    let supported = |versions: &[&str], range: &ProtocolCompatibility| {
        !crate::plugin_package::supported_versions_in_range(
            versions,
            &range.min_version,
            &range.max_version_exclusive,
        )
        .is_empty()
    };
    core >= min
        && core < max
        && entry.platform.os == "windows"
        && entry.platform.abi == "msvc"
        && ((cfg!(target_arch = "x86_64") && entry.platform.architecture == "x86_64")
            || (cfg!(target_arch = "aarch64") && entry.platform.architecture == "aarch64"))
        && supported(
            PROTOCOL_SUPPORTED_VERSIONS,
            &entry.host_compatibility.protocol,
        )
        && entry
            .ui_bridge_compatibility
            .as_ref()
            .is_none_or(|range| supported(UI_BRIDGE_SUPPORTED_VERSIONS, range))
}

fn load_cache(
    path: &Path,
    source: &DistributionSource,
    identity: Option<&str>,
    restricted: bool,
) -> Option<CacheFile> {
    let (bytes, legacy) = match read_bounded(path, MAX_CACHE_BYTES).ok()? {
        Some(bytes) => (bytes, false),
        None => {
            let name = if restricted {
                "plugin-distribution-restricted-v1.json"
            } else {
                "plugin-distribution-public-v1.json"
            };
            (
                read_bounded(&path.parent()?.join(name), MAX_CACHE_BYTES).ok()??,
                true,
            )
        }
    };
    let cache: CacheFile = if legacy {
        // Old introductions may still be shown offline, but are never an install
        // authority. Every installation separately fetches a fresh schema-2 index.
        // Keep the original bytes on disk until a source/code clear invalidates them.
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        if value.get("schemaVersion")?.as_u64()? != 1 {
            return None;
        }
        value["schemaVersion"] = serde_json::json!(2);
        for item in value.get_mut("plugins")?.as_array_mut()? {
            let key = item.get("registration")?.get("signingPublicKey")?.clone();
            if let Some(entry) = item
                .get_mut("entry")
                .and_then(|entry| entry.as_object_mut())
            {
                entry.insert("signingPublicKey".into(), key);
            }
        }
        serde_json::from_value(value).ok()?
    } else {
        serde_json::from_slice(&bytes).ok()?
    };
    if cache.schema_version != 2
        || cache.source_url != source.as_str()
        || cache.credential_identity.as_deref() != identity
        || !valid_timestamp(&cache.generated_at)
        || !valid_timestamp(&cache.fetched_at)
        || cache.plugins.len() > 500
        || cache.plugins.iter().any(|item| {
            (item.registration.visibility == "restricted") != restricted
                || validate_registration(&item.registration, source).is_err()
                || item.entry.as_ref().is_some_and(|entry| {
                    entry.id != item.registration.id || validate_entry(entry, source).is_err()
                })
        })
    {
        return None;
    }
    Some(cache)
}

fn persist_cache(path: &Path, cache: &CacheFile) -> Result<(), PluginError> {
    let bytes = serde_json::to_vec(cache).map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
    if bytes.len() > MAX_CACHE_BYTES {
        return Err(failure("RESOURCE_LIMIT"));
    }
    atomic_write(path, &bytes).map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))
}

fn read_bounded(path: &Path, max: usize) -> Result<Option<Vec<u8>>, std::io::Error> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > max as u64 {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    let bytes = fs::read(path)?;
    if bytes.len() > max {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    Ok(Some(bytes))
}

fn random_id() -> Result<String, PluginError> {
    let mut bytes = [0; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
    Ok(hex::encode(bytes))
}

fn preserve_damaged_file(path: &Path) -> Result<(), PluginError> {
    let Some(bytes) = read_bounded(path, MAX_SETTINGS_BYTES)
        .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?
    else {
        return Ok(());
    };
    let backup = path.with_extension(format!("json.preserved-{}", random_id()?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup)
        .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("CREDENTIAL_STORE_UNAVAILABLE"))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    fs::create_dir_all(parent)?;
    let nonce = random_id().map_err(|_| std::io::Error::from(std::io::ErrorKind::Other))?;
    let temp = path.with_extension(format!("{nonce}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let source = wide(source);
    let destination = wide(destination);
    // Both pointers refer to NUL-terminated paths that live through this call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    fs::rename(source, destination)
}

fn validate_code(code: &str) -> Result<(), PluginError> {
    if code.is_empty() || code.len() > 4096 || !code.bytes().all(|byte| byte.is_ascii_graphic()) {
        Err(failure("INVALID_REQUEST"))
    } else {
        Ok(())
    }
}
fn valid_timestamp(value: &str) -> bool {
    value.len() <= 64 && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}
fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}
fn valid_method(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_lowercase()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}
fn valid_version(value: &str) -> bool {
    value.len() <= 128 && Version::parse(value).is_ok_and(|version| version.build.is_empty())
}
fn valid_range(min: &str, max: &str) -> bool {
    min.len() <= 128
        && max.len() <= 128
        && Version::parse(min)
            .ok()
            .zip(Version::parse(max).ok())
            .is_some_and(|(min, max)| min < max)
}
fn credential_error(code: &str) -> bool {
    matches!(
        code,
        "ACCESS_CODE_INVALID" | "ACCESS_CODE_EXPIRED" | "ACCESS_CODE_REVOKED"
    )
}
fn credential_message(state: &RuntimeState) -> Option<String> {
    match state.credential_state.as_str() {
        "invalid" => Some(message("ACCESS_CODE_INVALID").into()),
        "expired" => Some(message("ACCESS_CODE_EXPIRED").into()),
        "revoked" => Some(message("ACCESS_CODE_REVOKED").into()),
        "unavailable" => Some(message("CREDENTIAL_STORE_UNAVAILABLE").into()),
        _ => state.warning.clone(),
    }
}
fn retry_seconds(error: &PluginError) -> Option<u64> {
    error.details.as_ref()?.get("retryAfterSeconds")?.as_u64()
}
fn status_for_error(code: &str) -> &'static str {
    match code {
        "DISTRIBUTION_UNAVAILABLE" => "offline",
        "RATE_LIMITED" => "rate_limited",
        "DISTRIBUTION_NOT_CONFIGURED" => "unconfigured",
        "ACCESS_CODE_INVALID"
        | "ACCESS_CODE_EXPIRED"
        | "ACCESS_CODE_REVOKED"
        | "NOT_AVAILABLE"
        | "CREDENTIAL_STORE_UNAVAILABLE"
        | "CATALOG_CHANGED" => "unavailable",
        _ => "invalid_response",
    }
}
fn http_failure(error: DistributionHttpError) -> PluginError {
    let mut failure = failure(error.code);
    let mut details = serde_json::Map::new();
    if let Some(seconds) = error.retry_after_seconds {
        details.insert("retryAfterSeconds".into(), seconds.into());
    }
    if let Some(status) = error.status {
        details.insert("httpStatus".into(), status.into());
    }
    if let Some(transport) = error.transport {
        failure.message = format!(
            "{}（{}）",
            failure.message,
            transport.kind.diagnostic_message()
        );
        details.insert(
            "transport".into(),
            serde_json::to_value(transport).expect("transport diagnostics are serializable"),
        );
    }
    if !details.is_empty() {
        failure.details = Some(details.into());
    }
    failure
}
fn failure(code: &str) -> PluginError {
    PluginError {
        code: code.into(),
        message: message(code).into(),
        details: None,
    }
}
fn message(code: &str) -> &'static str {
    match code {
        "DISTRIBUTION_NOT_CONFIGURED" => "尚未配置插件分发来源。",
        "INVALID_DISTRIBUTION_SOURCE" => "插件分发来源必须是有效的 HTTPS origin。",
        "CREDENTIAL_STORE_UNAVAILABLE" => "插件分发配置或访问码无法读取或安全保存；原文件已保留。",
        "ACCESS_CODE_INVALID" => "访问码无效；公开插件仍可获取。",
        "ACCESS_CODE_EXPIRED" => "访问码已过期；公开插件仍可获取。",
        "ACCESS_CODE_REVOKED" => "访问码已撤销；公开插件仍可获取。",
        "NOT_AVAILABLE" => "此插件当前不可获取。",
        "RATE_LIMITED" => "请求过于频繁，请稍后重试。",
        "DISTRIBUTION_UNAVAILABLE" => "插件分发服务暂时不可用；目录快照仅供查看，权限待确认。",
        "CATALOG_CHANGED" => "分发配置、资格或版本已变化，请刷新目录后重试。",
        "CATALOG_VERSION_NOT_NEWER" => "当前目录没有更高版本。",
        "INCOMPATIBLE_CORE" => "此插件版本与当前 Core 不兼容。",
        "INVALID_CATALOG" => "插件目录或访问状态格式无效。",
        "INVALID_UPDATE_MANIFEST" => "插件更新索引无效或需要新版分发协议。",
        "PACKAGE_CHECKSUM_MISMATCH" => "下载包的尺寸或摘要与已审核清单不符。",
        "RESOURCE_LIMIT" => "插件分发内容超出资源上限。",
        "INVALID_REQUEST" => "插件分发请求格式无效。",
        _ => "插件分发响应无效。",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const CODE: &str = "test-distribution-code-only";
    const PACKAGE: &[u8] = b"test package, not a real plugin archive";

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "wonderland-distribution-test-{}",
                random_id().unwrap()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            assert!(self.0.parent() == Some(std::env::temp_dir().as_path()));
            assert!(
                self.0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("wonderland-distribution-test-")
            );
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct MockReply {
        status: u16,
        body: Vec<u8>,
    }
    impl MockReply {
        fn json(value: serde_json::Value) -> Self {
            Self {
                status: 200,
                body: serde_json::to_vec(&value).unwrap(),
            }
        }
        fn status(status: u16) -> Self {
            Self {
                status,
                body: Vec::new(),
            }
        }
    }
    type Handler = dyn Fn(&str, Option<&str>, &str) -> MockReply + Send + Sync;
    type RecordedRequests = Arc<Mutex<Vec<(String, Option<String>)>>>;
    struct MockServer {
        source: DistributionSource,
        stop: Arc<AtomicBool>,
        task: Option<std::thread::JoinHandle<()>>,
        requests: RecordedRequests,
    }
    impl MockServer {
        fn start(
            handler: impl Fn(&str, Option<&str>, &str) -> MockReply + Send + Sync + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let source = DistributionSource::debug_loopback(&format!(
                "http://{}",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let origin = source.as_str().to_owned();
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            let handler: Arc<Handler> = Arc::new(handler);
            let task = std::thread::spawn(move || {
                let mut workers = Vec::new();
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let handler = handler.clone();
                            let origin = origin.clone();
                            let requests = thread_requests.clone();
                            workers.push(std::thread::spawn(move || {
                                stream.set_nonblocking(false).unwrap();
                                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                                let mut bytes = Vec::new();
                                let mut chunk = [0; 1024];
                                while !bytes.windows(4).any(|part| part == b"\r\n\r\n") && bytes.len() < 8192 {
                                    let Ok(count) = stream.read(&mut chunk) else { return; };
                                    if count == 0 { return; }
                                    bytes.extend_from_slice(&chunk[..count]);
                                }
                                let header = String::from_utf8(bytes).unwrap();
                                let path = header.lines().next().unwrap().split(' ').nth(1).unwrap();
                                let code = header.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("authorization")).map(|(_, value)| value.trim().strip_prefix("Bearer ").unwrap().to_owned()));
                                requests.lock().unwrap().push((path.to_owned(), code.clone()));
                                let reply = handler(path, code.as_deref(), &origin);
                                let response = format!("HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.status, reply.body.len());
                                let _ = stream.write_all(response.as_bytes()).and_then(|()| stream.write_all(&reply.body));
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("mock accept failed: {error}"),
                    }
                }
                for worker in workers {
                    worker.join().unwrap();
                }
            });
            Self {
                source,
                stop,
                task: Some(task),
                requests,
            }
        }
    }
    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.task.take().unwrap().join().unwrap();
        }
    }

    fn signing_key() -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(&[17; 32]).unwrap()
    }
    fn registration(origin: &str, id: &str, restricted: bool) -> serde_json::Value {
        serde_json::json!({"id":id,"name":id,"description":"test introduction","author":"test","visibility":if restricted { "restricted" } else { "public" },"updateManifestUrl":format!("{origin}/v1/plugins/{id}/update"),"signingPublicKey":hex::encode(signing_key().public_key().as_ref())})
    }
    fn catalog_index(origin: &str, private: bool) -> serde_json::Value {
        let mut plugins = vec![registration(origin, "public_plugin", false)];
        if private {
            plugins.push(registration(origin, "private_plugin", true));
        }
        serde_json::json!({"schemaVersion":1,"generatedAt":"2026-10-13T00:00:00Z","plugins":plugins})
    }
    fn update_index(origin: &str, id: &str, package: &[u8]) -> serde_json::Value {
        serde_json::json!({"schemaVersion":2,"id":id,"version":"1.2.3","releaseNotesUrl":null,"downloadUrl":format!("{origin}/v1/plugins/{id}/versions/1.2.3/package"),"sha256":hex::encode(Sha256::digest(package)),"sizeBytes":package.len(),"hostCompatibility":{"minCoreVersion":"0.0.0","maxCoreVersionExclusive":"999.0.0","protocol":{"minVersion":"0.0.0","maxVersionExclusive":"999.0.0"}},"uiBridgeCompatibility":null,"platform":{"os":"windows","architecture":std::env::consts::ARCH,"abi":"msvc"},"dependencies":[]})
    }

    fn successful_reply(path: &str, code: Option<&str>, origin: &str, private: bool) -> MockReply {
        match path {
            "/v1/access" => MockReply::json(
                serde_json::json!({"schemaVersion":1,"state":if code.is_some() { "valid" } else { "none" },"expiresAt":null}),
            ),
            "/v1/catalog" => MockReply::json(catalog_index(origin, private && code.is_some())),
            "/v1/plugins/public_plugin/update" => {
                MockReply::json(update_index(origin, "public_plugin", PACKAGE))
            }
            "/v1/plugins/private_plugin/update" => {
                MockReply::json(update_index(origin, "private_plugin", PACKAGE))
            }
            "/v1/plugins/public_plugin/versions/1.2.3/package"
            | "/v1/plugins/private_plugin/versions/1.2.3/package" => MockReply {
                status: 200,
                body: PACKAGE.to_vec(),
            },
            _ => MockReply::status(404),
        }
    }
    fn mock_service(
        root: &TestDirectory,
        source: &DistributionSource,
        code: bool,
    ) -> PluginDistributionService {
        let service = PluginDistributionService::load(&root.0);
        service.state.lock().unwrap().source = Some(source.clone());
        if code {
            service
                .configure(DistributionConfigureRequest {
                    access_code: Some(CODE.into()),
                    ..Default::default()
                })
                .unwrap();
        }
        service
    }
    fn install_request() -> CatalogInstallRequest {
        CatalogInstallRequest {
            plugin_id: "public_plugin".into(),
            version: "1.2.3".into(),
            expected_sha256: hex::encode(Sha256::digest(PACKAGE)),
            approved_source_change: false,
            mode: CatalogInstallMode::Update,
            operation_id: None,
            expected_source_url: None,
        }
    }
    fn ids(snapshot: &PluginCatalogSnapshot) -> Vec<&str> {
        snapshot
            .plugins
            .iter()
            .map(|item| item.registration.id.as_str())
            .collect()
    }

    #[test]
    fn new_data_directory_uses_official_source_without_persisting_an_override() {
        let root = TestDirectory::new();
        let service = PluginDistributionService::load_with_origin(&root.0, None);
        let settings = service.settings();
        assert!(settings.configured);
        assert_eq!(settings.source_url.as_deref(), Some(DEFAULT_SOURCE_URL));
        assert!(!settings.credential_configured);
        assert!(!root.0.join(SETTINGS_FILE).exists());
    }

    #[test]
    fn repair_and_rollback_require_the_exact_reviewed_version_direction() {
        assert!(validate_install_mode(CatalogInstallMode::Update, "1.2.3", None).is_ok());
        assert!(validate_install_mode(CatalogInstallMode::Update, "1.2.3", Some("1.2.2")).is_ok());
        assert!(validate_install_mode(CatalogInstallMode::Update, "1.2.3", Some("1.2.3")).is_err());
        assert!(validate_install_mode(CatalogInstallMode::Repair, "1.2.3", Some("1.2.3")).is_ok());
        assert!(validate_install_mode(CatalogInstallMode::Repair, "1.2.3", Some("1.2.2")).is_err());
        assert!(
            validate_install_mode(CatalogInstallMode::Rollback, "1.2.3", Some("1.3.0")).is_ok()
        );
        assert!(
            validate_install_mode(CatalogInstallMode::Rollback, "1.2.3", Some("1.2.3")).is_err()
        );
        assert!(validate_install_mode(CatalogInstallMode::Rollback, "1.2.3", None).is_err());
    }

    #[tokio::test]
    async fn package_progress_is_monotonic_and_reviewed_source_cannot_be_replaced() {
        let server =
            MockServer::start(|path, code, origin| successful_reply(path, code, origin, true));
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, false);
        let events = Mutex::new(Vec::new());
        let callback = |phase: &str, bytes: u64, total: u64| {
            events
                .lock()
                .unwrap()
                .push((phase.to_owned(), bytes, total))
        };
        let prepared = service
            .prepare_install_with_progress(&install_request(), &[], &callback)
            .await
            .unwrap();
        assert_eq!(prepared.bytes, PACKAGE);
        let downloads = events
            .lock()
            .unwrap()
            .iter()
            .filter(|(phase, _, _)| phase == "downloading")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(downloads.first().unwrap().1, 0);
        assert_eq!(downloads.last().unwrap().1, PACKAGE.len() as u64);
        assert!(downloads.windows(2).all(|items| items[0].1 <= items[1].1));
        let requests = server.requests.lock().unwrap().len();
        let mut request = install_request();
        request.expected_source_url = Some("https://changed.example.invalid".into());
        assert_eq!(
            service
                .prepare_install(&request, &[])
                .await
                .err()
                .unwrap()
                .code,
            "CATALOG_CHANGED"
        );
        assert_eq!(server.requests.lock().unwrap().len(), requests);
    }

    #[test]
    fn explicit_source_clear_survives_restart_without_restoring_the_default() {
        let root = TestDirectory::new();
        let service = PluginDistributionService::load_with_origin(&root.0, None);
        service
            .configure(DistributionConfigureRequest {
                source_url: Some(String::new()),
                ..Default::default()
            })
            .unwrap();
        let reloaded = PluginDistributionService::load_with_origin(&root.0, None);
        let settings = reloaded.settings();
        assert!(!settings.configured);
        assert_eq!(settings.source_url, None);
        assert_eq!(
            reloaded.begin_request().err().unwrap().code,
            "DISTRIBUTION_NOT_CONFIGURED"
        );
    }

    #[tokio::test]
    async fn legacy_snapshot_remains_display_only_and_retains_original_bytes() {
        let offline = Arc::new(AtomicBool::new(false));
        let handler_offline = offline.clone();
        let server = MockServer::start(move |path, code, origin| {
            if handler_offline.load(Ordering::Relaxed) {
                MockReply::status(503)
            } else {
                successful_reply(path, code, origin, false)
            }
        });
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, false);
        service.catalog(&[]).await.unwrap();
        let mut legacy =
            serde_json::to_value(service.state.lock().unwrap().public_cache.clone().unwrap())
                .unwrap();
        legacy["schemaVersion"] = serde_json::json!(1);
        for item in legacy["plugins"].as_array_mut().unwrap() {
            item["entry"]
                .as_object_mut()
                .unwrap()
                .remove("signingPublicKey");
        }
        let bytes = serde_json::to_vec(&legacy).unwrap();
        let original = root.0.join("plugin-distribution-public-v1.json");
        fs::write(&original, &bytes).unwrap();
        fs::remove_file(root.0.join(PUBLIC_CACHE_FILE)).unwrap();
        let loaded =
            load_cache(&root.0.join(PUBLIC_CACHE_FILE), &server.source, None, false).unwrap();
        assert_eq!(
            loaded.plugins[0].entry.as_ref().unwrap().signing_public_key,
            loaded.plugins[0].registration.signing_public_key
        );
        {
            let mut state = service.state.lock().unwrap();
            state.public_cache = Some(loaded);
            state.recent = None;
        }
        offline.store(true, Ordering::Relaxed);
        let view = service.catalog(&[]).await.unwrap();
        assert!(view.stale);
        assert!(!view.plugins[0].installable);
        assert!(
            service
                .prepare_install(&install_request(), &[])
                .await
                .is_err()
        );
        assert_eq!(fs::read(original).unwrap(), bytes);
    }

    #[tokio::test]
    async fn damaged_settings_leave_distribution_unavailable_and_preserve_bytes() {
        let root = TestDirectory::new();
        let damaged = b"{corrupt distribution settings";
        fs::write(root.0.join(SETTINGS_FILE), damaged).unwrap();
        let service = PluginDistributionService::load_with_origin(&root.0, None);
        assert_eq!(service.settings().source_url, None);
        assert_eq!(service.catalog(&[]).await.unwrap().status, "unavailable");
        assert_eq!(fs::read(root.0.join(SETTINGS_FILE)).unwrap(), damaged);
        service
            .configure(DistributionConfigureRequest {
                source_url: Some("https://distribution.test".into()),
                ..Default::default()
            })
            .unwrap();
        let backups: Vec<_> = fs::read_dir(&root.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("preserved-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), damaged);
    }

    #[test]
    fn unsupported_settings_never_fall_back_to_the_official_source() {
        for (schema_version, source) in [
            (2, "https://configured.test"),
            (1, "http://unsupported.test"),
        ] {
            let root = TestDirectory::new();
            let bytes = serde_json::to_vec(&serde_json::json!({
                "schemaVersion": schema_version,
                "sourceUrl": source,
                "encryptedAccessCode": null,
                "credentialIdentity": null
            }))
            .unwrap();
            fs::write(root.0.join(SETTINGS_FILE), &bytes).unwrap();
            let service = PluginDistributionService::load_with_origin(&root.0, None);
            assert!(!service.settings().configured);
            assert_eq!(service.settings().source_url, None);
            assert_eq!(
                service.begin_request().err().unwrap().code,
                "CREDENTIAL_STORE_UNAVAILABLE"
            );
            assert_eq!(fs::read(root.0.join(SETTINGS_FILE)).unwrap(), bytes);
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    fn debug_test_origin_does_not_inherit_official_source_credentials() {
        let root = TestDirectory::new();
        let service = PluginDistributionService::load_with_origin(&root.0, None);
        service
            .configure(DistributionConfigureRequest {
                access_code: Some(CODE.into()),
                ..Default::default()
            })
            .unwrap();
        let saved = fs::read(root.0.join(SETTINGS_FILE)).unwrap();
        let injected =
            PluginDistributionService::load_with_origin(&root.0, Some("http://127.0.0.1:12345"));
        let settings = injected.settings();
        assert_eq!(
            settings.source_url.as_deref(),
            Some("http://127.0.0.1:12345")
        );
        assert!(!settings.credential_configured);
        assert_eq!(settings.credential_state, "none");
        assert!(injected.begin_request().unwrap().code().is_none());
        assert_eq!(fs::read(root.0.join(SETTINGS_FILE)).unwrap(), saved);
        let restored = PluginDistributionService::load_with_origin(&root.0, None);
        assert_eq!(
            restored.settings().source_url.as_deref(),
            Some(DEFAULT_SOURCE_URL)
        );
        assert_eq!(restored.begin_request().unwrap().code(), Some(CODE));
    }

    #[test]
    fn dpapi_configuration_is_redacted_and_source_changes_keep_business_data() {
        let root = TestDirectory::new();
        let service = PluginDistributionService::load(&root.0);
        let request = DistributionConfigureRequest {
            source_url: Some("https://first.test".into()),
            access_code: Some(CODE.into()),
            clear_access_code: false,
        };
        assert!(!format!("{request:?}").contains(CODE));
        assert_eq!(
            service.configure(request).unwrap().credential_state,
            "unchecked"
        );
        let persisted = fs::read_to_string(root.0.join(SETTINGS_FILE)).unwrap();
        assert!(!persisted.contains(CODE));
        let reloaded = PluginDistributionService::load(&root.0);
        assert!(reloaded.settings().credential_available);
        assert_eq!(reloaded.begin_request().unwrap().code(), Some(CODE));
        fs::write(root.0.join("plugin-owned-business-data.json"), b"keep me").unwrap();
        fs::write(root.0.join(RESTRICTED_CACHE_FILE), b"old introduction").unwrap();
        let view = service
            .configure(DistributionConfigureRequest {
                source_url: Some("https://second.test".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(!view.credential_configured);
        assert!(!root.0.join(RESTRICTED_CACHE_FILE).exists());
        let reloaded = PluginDistributionService::load_with_origin(&root.0, None);
        assert_eq!(
            reloaded.settings().source_url.as_deref(),
            Some("https://second.test")
        );
        assert!(!reloaded.settings().credential_configured);
        assert_eq!(
            fs::read(root.0.join("plugin-owned-business-data.json")).unwrap(),
            b"keep me"
        );
    }

    #[test]
    fn failed_atomic_configuration_preserves_previous_runtime_and_disk_target() {
        let root = TestDirectory::new();
        let service = PluginDistributionService::load(&root.0);
        service
            .configure(DistributionConfigureRequest {
                source_url: Some("https://first.test".into()),
                access_code: Some(CODE.into()),
                clear_access_code: false,
            })
            .unwrap();
        let previous = service.visibility_key();
        fs::remove_file(root.0.join(SETTINGS_FILE)).unwrap();
        fs::create_dir(root.0.join(SETTINGS_FILE)).unwrap();
        let error = service
            .configure(DistributionConfigureRequest {
                source_url: Some("https://second.test".into()),
                ..Default::default()
            })
            .err()
            .unwrap();
        assert_eq!(error.code, "CREDENTIAL_STORE_UNAVAILABLE");
        assert_eq!(service.visibility_key(), previous);
        assert!(root.0.join(SETTINGS_FILE).is_dir());
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn renewal_cannot_pair_new_source_or_code_with_an_old_client() {
        let root = TestDirectory::new();
        let service = PluginDistributionService::load(&root.0);
        service
            .configure(DistributionConfigureRequest {
                source_url: Some("https://first.test".into()),
                access_code: Some(CODE.into()),
                clear_access_code: false,
            })
            .unwrap();
        let context = service.begin_request().unwrap();
        let anonymous = service
            .invalidate_and_renew(&context, "ACCESS_CODE_REVOKED", None, true)
            .unwrap();
        assert!(anonymous.code().is_none());
        assert_eq!(anonymous.source, context.source);
        service
            .configure(DistributionConfigureRequest {
                access_code: Some("test-replacement-code".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            service
                .renew_same_identity(&context, false)
                .err()
                .unwrap()
                .code,
            "CATALOG_CHANGED"
        );
        let context = service.begin_request().unwrap();
        let _ = service.invalidate(&context, "NOT_AVAILABLE", Some("private_plugin"));
        service
            .configure(DistributionConfigureRequest {
                source_url: Some("https://second.test".into()),
                access_code: Some("test-third-code".into()),
                clear_access_code: false,
            })
            .unwrap();
        assert_eq!(
            service
                .renew_same_identity(&context, true)
                .err()
                .unwrap()
                .code,
            "CATALOG_CHANGED"
        );
    }

    #[test]
    fn renewal_rejects_intervening_same_code_revocation_and_newer_requests() {
        let root = TestDirectory::new();
        let service = PluginDistributionService::load(&root.0);
        service
            .configure(DistributionConfigureRequest {
                source_url: Some("https://first.test".into()),
                access_code: Some(CODE.into()),
                clear_access_code: false,
            })
            .unwrap();
        let mut previous = service.begin_request().unwrap();
        previous.generation = service
            .invalidate(&previous, "NOT_AVAILABLE", Some("private_plugin"))
            .unwrap();
        let newer = service.begin_request().unwrap();
        assert_eq!(
            service
                .renew_same_identity(&previous, false)
                .err()
                .unwrap()
                .code,
            "CATALOG_CHANGED"
        );
        let _ = service.invalidate(&newer, "ACCESS_CODE_REVOKED", None);
        assert_eq!(
            service
                .renew_same_identity(&previous, false)
                .err()
                .unwrap()
                .code,
            "CATALOG_CHANGED"
        );
        assert_eq!(service.settings().credential_state, "revoked");
    }

    #[tokio::test]
    async fn successful_catalog_and_package_use_fresh_signed_metadata_and_verify_hash() {
        let server =
            MockServer::start(|path, code, origin| successful_reply(path, code, origin, true));
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, true);
        let snapshot = service.catalog(&[]).await.unwrap();
        assert_eq!(snapshot.status, "ready");
        assert_eq!(ids(&snapshot), ["private_plugin", "public_plugin"]);
        assert!(snapshot.plugins.iter().all(|item| item.installable));
        assert!(root.0.join(RESTRICTED_CACHE_FILE).exists());
        let prepared = service
            .prepare_install(&install_request(), &[])
            .await
            .unwrap();
        assert_eq!(prepared.bytes, PACKAGE);
        assert_eq!(prepared.entry.source_url, server.source.as_str());
        let mut request = install_request();
        request.expected_sha256 = "0".repeat(64);
        assert_eq!(
            service
                .prepare_install(&request, &[])
                .await
                .err()
                .unwrap()
                .code,
            "CATALOG_CHANGED"
        );
        assert_eq!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path.ends_with("/package"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn offline_snapshot_is_display_only_and_revocation_removes_restricted_entries() {
        let phase = Arc::new(AtomicUsize::new(0));
        let server_phase = phase.clone();
        let server = MockServer::start(move |path, code, origin| {
            match server_phase.load(Ordering::Relaxed) {
                1 => MockReply::status(503),
                2 if code.is_some() => MockReply {
                    status: 401,
                    body: br#"{"error":{"code":"ACCESS_CODE_REVOKED","message":"ignored"}}"#
                        .to_vec(),
                },
                _ => successful_reply(path, code, origin, true),
            }
        });
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, true);
        assert_eq!(service.catalog(&[]).await.unwrap().status, "ready");
        phase.store(1, Ordering::Relaxed);
        let snapshot = service.catalog(&[]).await.unwrap();
        assert_eq!(snapshot.status, "offline");
        assert!(snapshot.fetched_at.is_some());
        assert_eq!(ids(&snapshot), ["private_plugin", "public_plugin"]);
        assert!(
            snapshot
                .plugins
                .iter()
                .all(|item| item.stale && !item.installable)
        );
        assert_eq!(
            service
                .prepare_install(&install_request(), &[])
                .await
                .err()
                .unwrap()
                .code,
            "DISTRIBUTION_UNAVAILABLE"
        );
        phase.store(2, Ordering::Relaxed);
        let snapshot = service.catalog(&[]).await.unwrap();
        assert_eq!(snapshot.credential_state, "revoked");
        assert_eq!(ids(&snapshot), ["public_plugin"]);
        assert!(!root.0.join(RESTRICTED_CACHE_FILE).exists());
        let saved: SettingsFile =
            serde_json::from_slice(&fs::read(root.0.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(saved.credential_state.as_deref(), Some("revoked"));
        let requests = server.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|(path, code)| path == "/v1/catalog" && code.is_none())
        );
    }

    #[tokio::test]
    async fn authoritative_scope_shrink_survives_following_manifest_failure_and_old_response() {
        let narrow = Arc::new(AtomicBool::new(false));
        let server_narrow = narrow.clone();
        let server = MockServer::start(move |path, code, origin| {
            if server_narrow.load(Ordering::Relaxed) && path.ends_with("/update") {
                MockReply::status(503)
            } else {
                successful_reply(path, code, origin, !server_narrow.load(Ordering::Relaxed))
            }
        });
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, true);
        service.catalog(&[]).await.unwrap();
        let old_context = service.begin_request().unwrap();
        let old_items = service
            .state
            .lock()
            .unwrap()
            .restricted_cache
            .as_ref()
            .unwrap()
            .plugins
            .clone();
        narrow.store(true, Ordering::Relaxed);
        let snapshot = service.catalog(&[]).await.unwrap();
        assert_eq!(ids(&snapshot), ["public_plugin"]);
        assert!(snapshot.stale);
        let delayed = service
            .commit_catalog(
                &old_context,
                "2026-10-13T00:00:00Z".into(),
                old_items,
                &[],
                None,
            )
            .unwrap();
        assert_eq!(ids(&delayed), ["public_plugin"]);
        assert!(
            service
                .state
                .lock()
                .unwrap()
                .restricted_cache
                .as_ref()
                .is_none_or(|cache| cache.plugins.is_empty())
        );
        assert!(!root.0.join(RESTRICTED_CACHE_FILE).exists());
    }

    #[tokio::test]
    async fn individual_restricted_404_clears_cache_and_never_becomes_a_download_link() {
        let unavailable = Arc::new(AtomicBool::new(false));
        let server_unavailable = unavailable.clone();
        let server = MockServer::start(move |path, code, origin| {
            if server_unavailable.load(Ordering::Relaxed)
                && path == "/v1/plugins/private_plugin/update"
            {
                MockReply::status(404)
            } else {
                successful_reply(path, code, origin, true)
            }
        });
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, true);
        service.catalog(&[]).await.unwrap();
        unavailable.store(true, Ordering::Relaxed);
        assert_eq!(ids(&service.catalog(&[]).await.unwrap()), ["public_plugin"]);
        assert!(!root.0.join(RESTRICTED_CACHE_FILE).exists());
    }

    #[tokio::test]
    async fn legacy_update_index_and_package_hash_are_rejected() {
        let package_bad = Arc::new(AtomicBool::new(false));
        let server_bad = package_bad.clone();
        let server = MockServer::start(move |path, code, origin| {
            if path.ends_with("/package") && server_bad.load(Ordering::Relaxed) {
                MockReply {
                    status: 200,
                    body: b"tampered bytes".to_vec(),
                }
            } else {
                successful_reply(path, code, origin, false)
            }
        });
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, false);
        let registration: PluginCatalogRegistration =
            serde_json::from_value(registration(server.source.as_str(), "public_plugin", false))
                .unwrap();
        let mut envelope = update_index(server.source.as_str(), "public_plugin", PACKAGE);
        envelope["schemaVersion"] = serde_json::json!(1);
        assert_eq!(
            parse_update_index(
                &serde_json::to_vec(&envelope).unwrap(),
                &registration,
                &server.source
            )
            .err()
            .unwrap()
            .code,
            "INVALID_UPDATE_MANIFEST"
        );
        package_bad.store(true, Ordering::Relaxed);
        let result = service
            .prepare_install(&install_request(), &[])
            .await
            .err()
            .unwrap();
        assert_eq!(
            result.code,
            "PACKAGE_CHECKSUM_MISMATCH",
            "mock requests: {:?}",
            server.requests.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn queued_catalog_calls_share_one_bounded_sync() {
        let server = MockServer::start(|path, code, origin| {
            if path == "/v1/access" {
                std::thread::sleep(Duration::from_millis(30));
            }
            successful_reply(path, code, origin, true)
        });
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, true);
        let (first, second, third) = tokio::join!(
            service.catalog(&[]),
            service.catalog(&[]),
            service.catalog(&[])
        );
        assert_eq!(first.unwrap().status, "ready");
        assert_eq!(second.unwrap().status, "ready");
        assert_eq!(third.unwrap().status, "ready");
        assert_eq!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path == "/v1/catalog")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn failed_revocation_persistence_warns_and_never_restores_runtime_scope() {
        let server =
            MockServer::start(|path, code, origin| successful_reply(path, code, origin, true));
        let root = TestDirectory::new();
        let service = mock_service(&root, &server.source, true);
        service.catalog(&[]).await.unwrap();
        let context = service.begin_request().unwrap();
        for file in [SETTINGS_FILE, RESTRICTED_CACHE_FILE] {
            fs::remove_file(root.0.join(file)).unwrap();
            fs::create_dir(root.0.join(file)).unwrap();
        }
        let _ = service.invalidate(&context, "ACCESS_CODE_REVOKED", None);
        assert!(service.state.lock().unwrap().restricted_cache.is_none());
        assert!(service.settings().warning.unwrap().contains("重启后可能"));
        assert_eq!(
            ids(&service.fallback_snapshot(&[], "offline", None)),
            ["public_plugin"]
        );
    }
}
