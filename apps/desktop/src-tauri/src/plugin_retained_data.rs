use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetainedPluginData {
    pub(super) id: String,
    data_directory: String,
    has_data: bool,
    has_secrets: bool,
    has_publisher_identity: bool,
    has_lifecycle: bool,
}

impl PluginManager {
    pub(crate) fn retained_plugin_data(&self) -> Result<Vec<RetainedPluginData>, PluginError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let mut ids = BTreeSet::new();
        ids.extend(
            self.inner
                .lifecycle
                .retained_ids()
                .into_iter()
                .filter(|id| valid_plugin_id(id)),
        );
        for directory in ["plugin-data", "plugin-secrets"] {
            let root = self.inner.app_data_dir.join(directory);
            if !root.exists() {
                continue;
            }
            reject_symlink(&root)?;
            for entry in
                fs::read_dir(&root).map_err(|error| plugin_error("LOCAL_IO", &error.to_string()))?
            {
                let entry = entry.map_err(|error| plugin_error("LOCAL_IO", &error.to_string()))?;
                let id = entry.file_name().to_string_lossy().into_owned();
                if valid_plugin_id(&id) {
                    ids.insert(id);
                }
            }
        }
        if let Some(identities) = state.preferences.publisher_identities.as_object() {
            ids.extend(identities.keys().filter(|id| valid_plugin_id(id)).cloned());
        }
        Ok(ids
            .into_iter()
            .filter(|id| !state.plugins.contains_key(id))
            .map(|id| {
                let data = self.inner.app_data_dir.join("plugin-data").join(&id);
                RetainedPluginData {
                    has_lifecycle: self.inner.lifecycle.retained_ids().contains(&id),
                    has_data: data.exists(),
                    has_secrets: self
                        .inner
                        .app_data_dir
                        .join("plugin-secrets")
                        .join(&id)
                        .exists(),
                    has_publisher_identity: state
                        .preferences
                        .publisher_identities
                        .get(&id)
                        .is_some(),
                    data_directory: data.to_string_lossy().into_owned(),
                    id,
                }
            })
            .collect())
    }

    pub(crate) fn cleanup_retained_plugin_data(
        &self,
        plugin_id: &str,
    ) -> Result<Vec<RetainedPluginData>, PluginError> {
        if !valid_plugin_id(plugin_id) {
            return Err(plugin_error("INVALID_REQUEST", "Plugin ID is invalid."));
        }
        let _management = self.inner.management.try_lock().map_err(|_| {
            plugin_error(
                "PLUGIN_BUSY",
                "Another plugin management change is in progress.",
            )
        })?;
        let _work = self.inner.begin_work()?;
        let _notification = PluginChangeNotification(self.inner.environment.clone());
        {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            if state.plugins.contains_key(plugin_id) {
                return Err(plugin_error(
                    "PLUGIN_STILL_INSTALLED",
                    "Uninstall the plugin before clearing its retained data.",
                ));
            }
            if !state.preferences.publisher_identities.is_object() {
                return Err(plugin_error(
                    "PUBLISHER_IDENTITY_UNAVAILABLE",
                    "Publisher records need repair; retained data was not removed.",
                ));
            }
        }
        // Validate both roots before deleting either partition. Never follow a junction.
        for directory in ["plugin-data", "plugin-secrets"] {
            let root = self.inner.app_data_dir.join(directory);
            if root.exists() {
                reject_symlink(&root)?;
            }
            let path = root.join(plugin_id);
            if path.exists() {
                reject_symlink(&path)?;
            }
        }
        for directory in ["plugin-data", "plugin-secrets"] {
            apply_plugin_data_removal(
                &self.inner.app_data_dir.join(directory).join(plugin_id),
                true,
            )?;
        }
        self.inner.lifecycle.remove(plugin_id)?;
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let mut next = state.preferences.clone();
            clear_plugin_preferences(&mut next, plugin_id);
            next.publisher_identities
                .as_object_mut()
                .unwrap()
                .remove(plugin_id);
            persist_preferences(&self.inner.app_data_dir, &next)
                .map_err(|message| plugin_error("LOCAL_IO", &message))?;
            state.preferences = next;
        }
        self.retained_plugin_data()
    }
}

#[tauri::command]
pub(crate) fn plugins_retained_data(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
) -> Result<Vec<RetainedPluginData>, PluginError> {
    ensure_main(&window)?;
    manager.retained_plugin_data()
}

#[tauri::command]
pub(crate) async fn plugins_cleanup_data(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    plugin_id: String,
) -> Result<Vec<RetainedPluginData>, PluginError> {
    ensure_main(&window)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.cleanup_retained_plugin_data(&plugin_id))
        .await
        .map_err(|error| plugin_error("INTERNAL", &error.to_string()))?
}
