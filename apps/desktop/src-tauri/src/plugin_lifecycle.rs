//! Core stores only restore intent and instance-owned work, never plugin business tasks.
use super::*;
use std::sync::Condvar;
use wonderland_plugin_protocol::{
    BackgroundRestoreState, PluginActivationPolicy, PluginLifecycleState,
};

const FILE_NAME: &str = "plugin-lifecycle.json";

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreRecord {
    background_restore: BackgroundRestoreState,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreFile {
    schema_version: u32,
    plugins: BTreeMap<String, RestoreRecord>,
}

struct Activity {
    _work: PluginWorkGuard,
    label: String,
}

#[derive(Default)]
struct LifecycleState {
    records: BTreeMap<String, RestoreRecord>,
    diagnostic: Option<String>,
    read_error: bool,
    corrupt_bytes: Option<Vec<u8>>,
    installing: BTreeSet<String>,
    activities: BTreeMap<(String, u64), BTreeMap<String, Activity>>,
    reasons: BTreeMap<String, String>,
    next_activity: u64,
    instances: BTreeMap<String, u64>,
    paused: BTreeSet<String>,
    calls: BTreeMap<String, usize>,
    package_revisions: BTreeMap<String, u32>,
}

pub(super) struct LifecycleStore {
    root: PathBuf,
    state: Mutex<LifecycleState>,
    changed: Condvar,
}

impl LifecycleStore {
    pub(super) fn load(root: &Path) -> Self {
        let mut state = LifecycleState::default();
        match fs::read(root.join(FILE_NAME)) {
            Ok(bytes) => match serde_json::from_slice::<RestoreFile>(&bytes) {
                Ok(file) if file.schema_version == 1 => state.records = file.plugins,
                _ => {
                    state.corrupt_bytes = Some(bytes);
                    state.diagnostic = Some("Lifecycle records need reconciliation; original bytes will be backed up before repair.".into());
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                state.read_error = true;
                state.diagnostic = Some(format!("Cannot read lifecycle records: {error}"))
            }
        }
        Self {
            root: root.to_owned(),
            state: Mutex::new(state),
            changed: Condvar::new(),
        }
    }

    pub(super) fn restore(&self, id: &str) -> BackgroundRestoreState {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.records.get(id).cloned())
            .map_or(BackgroundRestoreState::Unknown, |r| r.background_restore)
    }

    pub(super) fn view(&self, manifest: &PluginManifest) -> PluginLifecycleState {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        PluginLifecycleState {
            policy: manifest
                .backend
                .activation
                .unwrap_or(PluginActivationPolicy::OnStartup),
            policy_source: if manifest.backend.activation.is_some() {
                "manifest"
            } else {
                "legacy"
            }
            .into(),
            background_restore: state
                .records
                .get(&manifest.id)
                .map_or(BackgroundRestoreState::Unknown, |r| r.background_restore),
            active_activities: state
                .activities
                .iter()
                .filter(|((id, _), _)| id == &manifest.id)
                .map(|(_, activities)| activities.len() as u32)
                .sum(),
            last_activation_reason: state.reasons.get(&manifest.id).cloned(),
            diagnostic: state.diagnostic.clone(),
            package_revision: state
                .package_revisions
                .get(&manifest.id)
                .copied()
                .unwrap_or(0),
        }
    }

    pub(super) fn should_restore(&self, snapshot: &PluginRuntimeState) -> bool {
        if !snapshot.enabled || snapshot.installation != InstallationState::Installed {
            return false;
        }
        let restore = self.restore(&snapshot.manifest.id);
        restore == BackgroundRestoreState::Required
            || match snapshot.manifest.backend.activation {
                Some(PluginActivationPolicy::OnDemand) => false,
                Some(PluginActivationPolicy::BackgroundRestore) => {
                    restore != BackgroundRestoreState::NotRequired
                }
                Some(PluginActivationPolicy::OnStartup) | None => true,
            }
    }

    fn persist(
        &self,
        state: &mut LifecycleState,
        records: BTreeMap<String, RestoreRecord>,
    ) -> Result<(), PluginError> {
        fs::create_dir_all(&self.root).map_err(io_error)?;
        // Never replace unreadable controls with a synthetic empty file.
        if state.read_error {
            return Err(plugin_error(
                "LIFECYCLE_STORAGE",
                state.diagnostic.as_deref().unwrap(),
            ));
        }
        if let Some(bytes) = &state.corrupt_bytes {
            let backup = self.root.join(format!(
                "plugin-lifecycle.corrupt-{}-{}.json",
                Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                NEXT_PLUGIN_INSTALL_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(backup)
                .map_err(io_error)?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(io_error)?;
        }
        let value = RestoreFile {
            schema_version: 1,
            plugins: records.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|e| plugin_error("LIFECYCLE_STORAGE", &e.to_string()))?;
        let temporary = self.root.join(format!(
            ".plugin-lifecycle-{}-{}-{}.tmp",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            NEXT_PLUGIN_INSTALL_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let write = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(io_error)?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(io_error)?;
            atomic_replace(&temporary, &self.root.join(FILE_NAME)).map_err(io_error)
        })();
        if write.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        if let Err(error) = &write {
            state.diagnostic = Some(format!(
                "Cannot persist lifecycle records: {}",
                error.message
            ));
        }
        write?;
        state.records = records;
        state.corrupt_bytes = None;
        state.diagnostic = None;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn set_restore(&self, id: &str, required: bool) -> Result<(), PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Lifecycle store is unavailable."))?;
        let value = if required {
            BackgroundRestoreState::Required
        } else {
            BackgroundRestoreState::NotRequired
        };
        if state.diagnostic.is_none()
            && state
                .records
                .get(id)
                .is_some_and(|r| r.background_restore == value)
        {
            return Ok(());
        }
        let mut next = state.records.clone();
        next.insert(
            id.into(),
            RestoreRecord {
                background_restore: value,
            },
        );
        self.persist(&mut state, next)
    }

    pub(super) fn remove(&self, id: &str) -> Result<(), PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Lifecycle store is unavailable."))?;
        if !state.records.contains_key(id) && state.diagnostic.is_none() {
            return Ok(());
        }
        let mut next = state.records.clone();
        next.remove(id);
        self.persist(&mut state, next)
    }

    pub(super) fn retained_ids(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| s.records.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub(super) fn reason(&self, id: &str, reason: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.reasons.insert(id.into(), reason.into());
        }
    }

    pub(super) fn package_changed(&self, id: &str) {
        if let Ok(mut state) = self.state.lock() {
            let revision = state.package_revisions.entry(id.into()).or_default();
            *revision = revision.wrapping_add(1);
        }
    }

    pub(super) fn begin_install(self: &Arc<Self>, id: &str) -> LifecycleInstallGuard {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .installing
            .insert(id.into());
        LifecycleInstallGuard {
            store: self.clone(),
            id: id.into(),
        }
    }

    pub(super) fn begin_activity(
        &self,
        id: &str,
        instance: u64,
        label: String,
        work: PluginWorkGuard,
    ) -> Result<String, PluginError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.instances.get(id) != Some(&instance) {
            return Err(plugin_error("CANCELLED", "Plugin instance was replaced."));
        }
        if state.paused.contains(id) {
            return Err(plugin_error(
                "PLUGIN_BUSY",
                "Plugin is being replaced or removed.",
            ));
        }
        state.next_activity = state.next_activity.wrapping_add(1);
        let activity_id = format!("activity-{}", state.next_activity);
        state
            .activities
            .entry((id.into(), instance))
            .or_default()
            .insert(activity_id.clone(), Activity { _work: work, label });
        Ok(activity_id)
    }

    pub(super) fn end_activity(&self, id: &str, instance: u64, activity: &str) {
        let retired = self.state.lock().ok().and_then(|mut s| {
            s.activities
                .get_mut(&(id.into(), instance))
                .and_then(|a| a.remove(activity))
        });
        drop(retired);
    }

    pub(super) fn clear_instance(&self, id: &str, instance: u64) {
        let retired = self.state.lock().ok().and_then(|mut s| {
            if s.instances.get(id) == Some(&instance) {
                s.instances.remove(id);
            }
            s.activities.remove(&(id.into(), instance))
        });
        drop(retired);
        self.changed.notify_all();
    }

    pub(super) fn connect_instance(&self, id: &str, instance: u64) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .instances
            .insert(id.into(), instance);
        self.changed.notify_all();
    }

    pub(super) fn wait_instance(
        &self,
        id: &str,
        instance: u64,
        invocation: &crate::host_services::HostInvocation,
    ) -> Result<(), PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Lifecycle state is unavailable."))?;
        loop {
            invocation.check()?;
            match state.instances.get(id) {
                Some(current) if *current != instance => {
                    return Err(plugin_error("CANCELLED", "Plugin instance was replaced."));
                }
                Some(_) if !state.installing.contains(id) => return Ok(()),
                _ => {
                    state = self
                        .changed
                        .wait_timeout(state, Duration::from_millis(20))
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            }
        }
    }

    pub(super) fn report(
        &self,
        id: &str,
        instance: u64,
        required: bool,
    ) -> Result<(), PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Lifecycle state is unavailable."))?;
        if state.instances.get(id) != Some(&instance) {
            return Err(plugin_error("CANCELLED", "Plugin instance was replaced."));
        }
        let value = if required {
            BackgroundRestoreState::Required
        } else {
            BackgroundRestoreState::NotRequired
        };
        if state.diagnostic.is_none()
            && state
                .records
                .get(id)
                .is_some_and(|r| r.background_restore == value)
        {
            return Ok(());
        }
        let mut next = state.records.clone();
        next.insert(
            id.into(),
            RestoreRecord {
                background_restore: value,
            },
        );
        self.persist(&mut state, next)
    }

    pub(super) fn blockers(&self, id: &str) -> Vec<String> {
        self.state
            .lock()
            .map(|s| {
                s.activities
                    .iter()
                    .filter(|((plugin, _), _)| plugin == id)
                    .flat_map(|(_, a)| a.values().map(|a| a.label.clone()))
                    .collect()
            })
            .unwrap_or_else(|_| vec!["Lifecycle state unavailable".into()])
    }

    pub(super) fn enter_call(self: &Arc<Self>, id: &str) -> Result<TargetCallGuard, PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Lifecycle state is unavailable."))?;
        if state.paused.contains(id) {
            return Err(plugin_error(
                "PLUGIN_BUSY",
                "Plugin is being replaced or removed.",
            ));
        }
        *state.calls.entry(id.into()).or_default() += 1;
        Ok(TargetCallGuard {
            store: self.clone(),
            id: id.into(),
        })
    }

    pub(super) fn pause(
        self: &Arc<Self>,
        id: &str,
        force: bool,
    ) -> Result<TargetPauseGuard, PluginError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Lifecycle state is unavailable."))?;
        if !force
            && (state.calls.get(id).copied().unwrap_or(0) > 0
                || state
                    .activities
                    .iter()
                    .any(|((plugin, _), tasks)| plugin == id && !tasks.is_empty()))
        {
            return Err(plugin_error(
                "PLUGIN_BUSY",
                "Finish the plugin's current calls and background work before replacing it.",
            ));
        }
        if !state.paused.insert(id.into()) {
            return Err(plugin_error(
                "PLUGIN_BUSY",
                "Plugin management is in progress.",
            ));
        }
        Ok(TargetPauseGuard {
            store: self.clone(),
            id: id.into(),
        })
    }
}

pub(super) struct TargetCallGuard {
    store: Arc<LifecycleStore>,
    id: String,
}
impl Drop for TargetCallGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.store.state.lock()
            && let Some(count) = state.calls.get_mut(&self.id)
        {
            *count = count.saturating_sub(1);
        }
    }
}

pub(super) struct TargetPauseGuard {
    store: Arc<LifecycleStore>,
    id: String,
}
impl Drop for TargetPauseGuard {
    fn drop(&mut self) {
        self.store
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .paused
            .remove(&self.id);
    }
}

pub(super) struct LifecycleInstallGuard {
    store: Arc<LifecycleStore>,
    id: String,
}
impl Drop for LifecycleInstallGuard {
    fn drop(&mut self) {
        self.store
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .installing
            .remove(&self.id);
        self.store.changed.notify_all();
    }
}

fn io_error(error: std::io::Error) -> PluginError {
    plugin_error("LIFECYCLE_STORAGE", &error.to_string())
}

impl PluginManager {
    pub(crate) fn lifecycle_call(
        &self,
        id: &str,
        instance: u64,
        method: &str,
        params: &Value,
        invocation: &crate::host_services::HostInvocation,
    ) -> Result<Value, PluginError> {
        use wonderland_plugin_protocol::{
            ActivityBeginParams, ActivityHandle, BackgroundRestoreParams,
        };
        if method == "core.lifecycle.activity.end" {
            let handle: ActivityHandle = serde_json::from_value(params.clone())
                .map_err(|e| plugin_error("INVALID_INPUT", &e.to_string()))?;
            self.inner
                .lifecycle
                .end_activity(id, instance, &handle.activity_id);
            self.emit_plugin_states_changed();
            return Ok(json!({"ok":true}));
        }
        let work = self.inner.begin_work()?;
        self.inner
            .lifecycle
            .wait_instance(id, instance, invocation)?;
        match method {
            "core.lifecycle.set_background_restore" => {
                let report: BackgroundRestoreParams = serde_json::from_value(params.clone())
                    .map_err(|e| plugin_error("INVALID_INPUT", &e.to_string()))?;
                self.inner.lifecycle.report(id, instance, report.required)?;
                self.emit_plugin_states_changed();
                Ok(json!({"required":report.required}))
            }
            "core.lifecycle.activity.begin" => {
                let input: ActivityBeginParams = serde_json::from_value(params.clone())
                    .map_err(|e| plugin_error("INVALID_INPUT", &e.to_string()))?;
                let label = input.label.unwrap_or_else(|| "Background work".into());
                if label.len() > 256 {
                    return Err(plugin_error(
                        "INVALID_INPUT",
                        "Activity label exceeds 256 bytes.",
                    ));
                }
                let activity_id = self
                    .inner
                    .lifecycle
                    .begin_activity(id, instance, label, work)?;
                self.emit_plugin_states_changed();
                Ok(json!({"activityId":activity_id}))
            }
            _ => Err(plugin_error(
                "METHOD_NOT_FOUND",
                "Lifecycle method is not available.",
            )),
        }
    }

    pub(crate) fn lifecycle_instance_closed(&self, id: &str, instance: u64) {
        self.inner.lifecycle.clear_instance(id, instance);
    }
}
