//! Desktop input adapters and reviewed, independently committed local installs.
use super::*;
use base64::Engine;
use std::io::Read;

#[derive(Default)]
pub(crate) struct LocalPluginInstaller {
    previews: Mutex<BTreeMap<String, ReviewedBatch>>,
    uploads: Mutex<BTreeMap<String, ImportUpload>>,
    running: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
}

struct ImportFile {
    path: PathBuf,
    label: String,
}
impl Drop for ImportFile {
    fn drop(&mut self) {
        // Only remove the single file created by this importer, never a user directory.
        if !self.path.is_dir() {
            let _ = fs::remove_file(&self.path);
            return;
        }
        if let (Some(parent), Ok(target)) = (self.path.parent(), self.path.canonicalize())
            && let Ok(root) = parent.canonicalize()
            && parent
                .file_name()
                .is_some_and(|name| name == "plugin-imports")
            && target.starts_with(&root)
            && target != root
            && self.path.extension().is_some_and(|ext| ext == "review")
        {
            let _ = fs::remove_dir_all(target);
        }
    }
}
struct ImportUpload {
    file: Option<std::fs::File>,
    source: ImportFile,
    size: u64,
    received: u64,
}
struct ReviewedItem {
    view: InstallPreviewItem,
    path: PathBuf,
    original: PathBuf,
    fingerprint: String,
    previous: Option<PluginRuntimeState>,
}
struct ReviewedBatch {
    cancel: Arc<AtomicBool>,
    items: Vec<ReviewedItem>,
    _imports: Vec<ImportFile>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallPreviewItem {
    key: String,
    source: String,
    manifest: Option<PluginManifest>,
    signature_public_key: Option<String>,
    existing: Option<PluginRuntimeState>,
    error: Option<PluginError>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallPreview {
    batch_id: String,
    items: Vec<InstallPreviewItem>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallItemResult {
    key: String,
    status: String,
    runtime: Option<RuntimeState>,
    error: Option<PluginError>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallBatchResult {
    items: Vec<InstallItemResult>,
    states: Vec<PluginRuntimeState>,
}
fn next_id() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        NEXT_PLUGIN_INSTALL_ID.fetch_add(1, Ordering::Relaxed)
    )
}
fn io_error(error: impl std::fmt::Display) -> PluginError {
    plugin_error(
        "INVALID_REQUEST",
        &format!("Cannot read local package: {error}"),
    )
}
fn package_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wplug"))
}

/// Expand only the selected directory itself, without following nested directories.
fn expand_sources(sources: Vec<PathBuf>, development: bool) -> Vec<(PathBuf, Option<PluginError>)> {
    let mut output = BTreeMap::new();
    for source in sources {
        if source.is_dir() && !development {
            match fs::read_dir(&source) {
                Ok(entries) => {
                    let mut found = false;
                    for entry in entries {
                        match entry {
                            Ok(entry) if package_file(&entry.path()) && entry.path().is_file() => {
                                found = true;
                                let path = entry.path().canonicalize().unwrap_or(entry.path());
                                output.entry(path).or_insert(None);
                            }
                            Ok(_) => {}
                            Err(error) => {
                                output.insert(source.clone(), Some(io_error(error)));
                            }
                        }
                    }
                    if !found && !output.contains_key(&source) {
                        output.insert(
                            source.clone(),
                            Some(plugin_error(
                                "NO_PLUGIN_PACKAGES",
                                "This directory has no .wplug files at its first level.",
                            )),
                        );
                    }
                }
                Err(error) => {
                    output.insert(source, Some(io_error(error)));
                }
            }
        } else if development || package_file(&source) {
            let source = source.canonicalize().unwrap_or(source);
            output.entry(source).or_insert(None);
        } else {
            output.insert(
                source,
                Some(plugin_error(
                    "INVALID_REQUEST",
                    "Select a .wplug package or a directory containing packages.",
                )),
            );
        }
    }
    output.into_iter().collect()
}

fn fingerprint(source: &Path) -> Result<String, PluginError> {
    fn visit(path: &Path, root: &Path, digest: &mut Sha256) -> Result<(), PluginError> {
        let metadata = fs::symlink_metadata(path).map_err(io_error)?;
        if metadata.is_symlink() {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Package sources cannot contain symbolic links.",
            ));
        }
        if metadata.is_dir() {
            let mut paths = fs::read_dir(path)
                .map_err(io_error)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(io_error)?;
            paths.sort();
            for path in paths {
                visit(&path, root, digest)?;
            }
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
            digest.update((relative.len() as u64).to_le_bytes());
            digest.update(relative.as_bytes());
            digest.update(metadata.len().to_le_bytes());
            let mut file = std::fs::File::open(path).map_err(io_error)?;
            let mut bytes = [0u8; 64 * 1024];
            loop {
                let count = file.read(&mut bytes).map_err(io_error)?;
                if count == 0 {
                    break;
                }
                digest.update(&bytes[..count]);
            }
        } else {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Package sources must be regular files.",
            ));
        }
        Ok(())
    }
    let mut digest = Sha256::new();
    visit(source, source, &mut digest)?;
    Ok(hex::encode(digest.finalize()))
}

fn prepare(
    manager: &PluginManager,
    sources: Vec<PathBuf>,
    development: bool,
    mut imports: Vec<ImportFile>,
) -> InstallPreviewAndBatch {
    let states = manager.snapshots();
    let imported_paths: BTreeSet<_> = imports
        .iter()
        .filter_map(|file| file.path.canonicalize().ok())
        .collect();
    let mut imported_digests = BTreeSet::new();
    let mut items = Vec::new();
    for (index, (path, initial_error)) in
        expand_sources(sources, development).into_iter().enumerate()
    {
        let source = imports
            .iter()
            .find(|file| file.path.canonicalize().ok().as_ref() == Some(&path))
            .map(|file| file.label.clone())
            .unwrap_or_else(|| path.display().to_string());
        let mut item = ReviewedItem {
            view: InstallPreviewItem {
                key: index.to_string(),
                source,
                manifest: None,
                signature_public_key: None,
                existing: None,
                error: initial_error,
            },
            original: path.clone(),
            path,
            fingerprint: String::new(),
            previous: None,
        };
        let mut inspect = || -> Result<(PluginManifest, Option<String>, String), PluginError> {
            let before = fingerprint(&item.path)?;
            // Verify and later install our own immutable snapshot, never reopen an author build.
            let directory = manager.inner.app_data_dir.join("plugin-imports");
            fs::create_dir_all(&directory).map_err(io_error)?;
            let target = directory.join(format!("{}.review", next_id()));
            let owned = ImportFile {
                path: target.clone(),
                label: item.view.source.clone(),
            };
            fn copy_source(source: &Path, target: &Path) -> Result<(), PluginError> {
                let metadata = fs::symlink_metadata(source).map_err(io_error)?;
                if metadata.is_symlink() {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        "Package sources cannot contain symbolic links.",
                    ));
                }
                if metadata.is_dir() {
                    fs::create_dir(target).map_err(io_error)?;
                    for entry in fs::read_dir(source).map_err(io_error)? {
                        let entry = entry.map_err(io_error)?;
                        copy_source(&entry.path(), &target.join(entry.file_name()))?;
                    }
                } else if metadata.is_file() {
                    let mut input = std::fs::File::open(source).map_err(io_error)?;
                    let mut output = OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(target)
                        .map_err(io_error)?;
                    std::io::copy(&mut input, &mut output).map_err(io_error)?;
                } else {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        "Package sources must be regular files.",
                    ));
                }
                Ok(())
            }
            imports.push(owned);
            copy_source(&item.path, &target)?;
            if before != fingerprint(&target)? || before != fingerprint(&item.path)? {
                return Err(plugin_error(
                    "SOURCE_CHANGED",
                    "Package changed during inspection; select it again.",
                ));
            }
            item.path = target;
            let manifest = plugin_package::manifest_for_install_source(&item.path)
                .map_err(|message| package_signature_error(&message))?;
            let signature = plugin_package::signature_for_install_source(&item.path)
                .map_err(|message| package_signature_error(&message))?;
            if !plugin_package::manifest_is_compatible(&manifest) {
                return Err(plugin_error(
                    "INCOMPATIBLE_CORE",
                    "This package is incompatible with the current Core.",
                ));
            }
            if before != fingerprint(&item.path)? {
                return Err(plugin_error(
                    "SOURCE_CHANGED",
                    "Package changed during inspection; select it again.",
                ));
            }
            Ok((manifest, signature, before))
        };
        if item.view.error.is_none() {
            match inspect() {
                Ok((manifest, signature, digest)) => {
                    // Browser drops hide host paths and create separate staging files.
                    // An identical imported archive is still one reviewed package.
                    if imported_paths.contains(&item.original)
                        && !imported_digests.insert(digest.clone())
                    {
                        continue;
                    }
                    item.previous = states
                        .iter()
                        .find(|state| state.manifest.id == manifest.id)
                        .cloned();
                    item.view.existing = item.previous.clone();
                    item.view.manifest = Some(manifest);
                    item.view.signature_public_key = signature;
                    item.fingerprint = digest;
                }
                Err(error) => item.view.error = Some(error),
            }
        }
        items.push(item);
    }
    let view = InstallPreview {
        batch_id: next_id(),
        items: items.iter().map(|item| item.view.clone()).collect(),
    };
    (
        view,
        ReviewedBatch {
            cancel: Arc::new(AtomicBool::new(false)),
            items,
            _imports: imports,
        },
    )
}
type InstallPreviewAndBatch = (InstallPreview, ReviewedBatch);

#[tauri::command]
pub(crate) async fn plugins_install_prepare(
    window: WebviewWindow,
    app: AppHandle,
    manager: State<'_, PluginManager>,
    installer: State<'_, LocalPluginInstaller>,
    mode: String,
    upload_ids: Option<Vec<String>>,
) -> Result<Option<InstallPreview>, PluginError> {
    ensure_main(&window)?;
    let mut imports = Vec::new();
    let sources = if mode == "uploads" {
        let mut uploads = installer
            .uploads
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Importer unavailable."))?;
        for id in upload_ids.unwrap_or_default() {
            let mut upload = uploads
                .remove(&id)
                .ok_or_else(|| plugin_error("INVALID_REQUEST", "Imported file has expired."))?;
            upload.file.take();
            if upload.received != upload.size {
                return Err(plugin_error(
                    "INVALID_REQUEST",
                    "Imported file is incomplete.",
                ));
            }
            imports.push(upload.source);
        }
        imports.iter().map(|source| source.path.clone()).collect()
    } else {
        let picker = tauri::async_runtime::spawn_blocking({
            let mode = mode.clone();
            move || {
                let dialog = app.dialog().file();
                match mode.as_str() {
                    "files" => dialog
                        .add_filter("Wonderland Plugin", &["wplug"])
                        .blocking_pick_files(),
                    "directory" => dialog
                        .set_title("选择包含 .wplug 的目录（不含子目录）")
                        .blocking_pick_folder()
                        .map(|path| vec![path]),
                    "development" if cfg!(debug_assertions) => dialog
                        .set_title("选择插件源码开发目录")
                        .blocking_pick_folder()
                        .map(|path| vec![path]),
                    _ => None,
                }
            }
        })
        .await
        .map_err(io_error)?;
        let Some(paths) = picker else {
            return Ok(None);
        };
        paths
            .into_iter()
            .map(|path| path.into_path().map_err(io_error))
            .collect::<Result<Vec<_>, _>>()?
    };
    let manager = manager.inner().clone();
    let (view, batch) = tauri::async_runtime::spawn_blocking(move || {
        prepare(&manager, sources, mode == "development", imports)
    })
    .await
    .map_err(io_error)?;
    installer
        .running
        .lock()
        .map_err(io_error)?
        .insert(view.batch_id.clone(), batch.cancel.clone());
    installer
        .previews
        .lock()
        .map_err(|_| plugin_error("INTERNAL", "Importer unavailable."))?
        .insert(view.batch_id.clone(), batch);
    Ok(Some(view))
}

#[tauri::command]
pub(crate) fn plugins_install_upload_open(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    installer: State<'_, LocalPluginInstaller>,
    name: String,
    size: u64,
) -> Result<String, PluginError> {
    ensure_main(&window)?;
    if !package_file(Path::new(&name)) {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Only .wplug imports are supported.",
        ));
    }
    let id = next_id();
    let directory = manager.inner.app_data_dir.join("plugin-imports");
    fs::create_dir_all(&directory).map_err(io_error)?;
    let path = directory.join(format!("{id}.wplug"));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(io_error)?;
    installer
        .uploads
        .lock()
        .map_err(|_| plugin_error("INTERNAL", "Importer unavailable."))?
        .insert(
            id.clone(),
            ImportUpload {
                file: Some(file),
                source: ImportFile { path, label: name },
                size,
                received: 0,
            },
        );
    Ok(id)
}

#[tauri::command]
pub(crate) fn plugins_install_upload_write(
    window: WebviewWindow,
    installer: State<'_, LocalPluginInstaller>,
    upload_id: String,
    offset: u64,
    content_base64: String,
) -> Result<(), PluginError> {
    ensure_main(&window)?;
    if content_base64.len() > 350_000 {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Import chunks must not exceed 256 KiB.",
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_base64)
        .map_err(io_error)?;
    let mut uploads = installer
        .uploads
        .lock()
        .map_err(|_| plugin_error("INTERNAL", "Importer unavailable."))?;
    let upload = uploads
        .get_mut(&upload_id)
        .ok_or_else(|| plugin_error("INVALID_REQUEST", "Imported file has expired."))?;
    if upload.received != offset || bytes.len() as u64 > upload.size.saturating_sub(upload.received)
    {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Import offset or size mismatch.",
        ));
    }
    upload
        .file
        .as_mut()
        .ok_or_else(|| plugin_error("INVALID_REQUEST", "Import is already closed."))?
        .write_all(&bytes)
        .map_err(io_error)?;
    upload.received += bytes.len() as u64;
    Ok(())
}

#[tauri::command]
pub(crate) fn plugins_install_discard(
    window: WebviewWindow,
    installer: State<'_, LocalPluginInstaller>,
    batch_id: Option<String>,
    upload_ids: Option<Vec<String>>,
) -> Result<(), PluginError> {
    ensure_main(&window)?;
    if let Some(id) = batch_id {
        if let Some(cancel) = installer.running.lock().map_err(io_error)?.get(&id) {
            cancel.store(true, Ordering::Release);
        }
        installer.previews.lock().map_err(io_error)?.remove(&id);
        installer
            .running
            .lock()
            .map_err(io_error)?
            .retain(|_, flag| Arc::strong_count(flag) > 1);
    }
    let mut uploads = installer.uploads.lock().map_err(io_error)?;
    for id in upload_ids.unwrap_or_default() {
        if let Some(mut upload) = uploads.remove(&id) {
            upload.file.take();
        }
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn plugins_install_run(
    window: WebviewWindow,
    manager: State<'_, PluginManager>,
    installer: State<'_, LocalPluginInstaller>,
    batch_id: String,
    selected: Vec<String>,
) -> Result<InstallBatchResult, PluginError> {
    ensure_main(&window)?;
    let batch = installer
        .previews
        .lock()
        .map_err(io_error)?
        .remove(&batch_id)
        .ok_or_else(|| {
            plugin_error(
                "INVALID_REQUEST",
                "Installation preview has expired; select packages again.",
            )
        })?;
    let selected: BTreeSet<_> = selected.into_iter().collect();
    let mut plugin_ids = BTreeSet::new();
    for key in &selected {
        let item = batch
            .items
            .iter()
            .find(|item| &item.view.key == key && item.view.error.is_none())
            .ok_or_else(|| {
                plugin_error("INVALID_REQUEST", "Select valid packages from the preview.")
            })?;
        if !plugin_ids.insert(item.view.manifest.as_ref().unwrap().id.clone()) {
            return Err(plugin_error(
                "PLUGIN_CONFLICT",
                "Select only one package for each plugin ID.",
            ));
        }
    }
    let cancel = batch.cancel.clone();
    let manager = manager.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        run_batch(&manager, &batch_id, batch, selected, cancel)
    })
    .await
    .map_err(io_error)?;
    // Finished cancellation flags are no longer needed.
    installer
        .running
        .lock()
        .map_err(io_error)?
        .retain(|_, flag| Arc::strong_count(flag) > 1);
    result
}

fn run_batch(
    manager: &PluginManager,
    batch_id: &str,
    batch: ReviewedBatch,
    selected: BTreeSet<String>,
    cancel: Arc<AtomicBool>,
) -> Result<InstallBatchResult, PluginError> {
    let _work = manager.admit_work()?;
    let _management = manager.inner.management.try_lock().map_err(|_| {
        plugin_error(
            "PLUGIN_BUSY",
            "Another plugin management change is in progress.",
        )
    })?;
    let mut results: Vec<_> = batch
        .items
        .iter()
        .map(|item| InstallItemResult {
            key: item.view.key.clone(),
            status: "pending".into(),
            runtime: None,
            error: None,
        })
        .collect();
    let emit = |results: &[InstallItemResult]| {
        manager.inner.environment.emit(
            "plugins:install-progress",
            json!({"batchId":batch_id,"items":results}),
        )
    };
    for (index, item) in batch.items.iter().enumerate() {
        let result = &mut results[index];
        if !selected.contains(&item.view.key) {
            result.status = "skipped".into();
        } else if cancel.load(Ordering::Acquire) {
            result.status = "cancelled".into();
        } else {
            result.status = "running".into();
            emit(&results);
            let outcome = (|| {
                if fingerprint(&item.path)? != item.fingerprint
                    || fingerprint(&item.original)? != item.fingerprint
                {
                    return Err(plugin_error(
                        "SOURCE_CHANGED",
                        "Package changed after review; select it again.",
                    ));
                }
                let manifest = item.view.manifest.as_ref().unwrap();
                let current = manager
                    .snapshots()
                    .into_iter()
                    .find(|state| state.manifest.id == manifest.id);
                let review_state = |state: &Option<PluginRuntimeState>| {
                    state.as_ref().map(|state| json!({"version":state.manifest.version,"source":state.installation_source,"key":state.signature,"enabled":state.enabled}))
                };
                if review_state(&current) != review_state(&item.previous) {
                    return Err(plugin_error(
                        "INSTALLATION_CHANGED",
                        "Installed plugin changed after review; select packages again.",
                    ));
                }
                manager.install_transaction_locked(
                    item.path.clone(),
                    true,
                    Some(manifest),
                    None,
                    PluginInstallSource {
                        kind: "local".into(),
                        origin: item.view.source.clone(),
                        author: None,
                    },
                    true,
                )
            })();
            let result = &mut results[index];
            match outcome {
                Ok(states) => {
                    result.status = "installed".into();
                    result.runtime = item
                        .view
                        .manifest
                        .as_ref()
                        .and_then(|manifest| {
                            states.iter().find(|state| state.manifest.id == manifest.id)
                        })
                        .map(|state| state.runtime);
                }
                Err(error) => {
                    result.status = "failed".into();
                    result.error = Some(error);
                }
            }
        }
        emit(&results);
    }
    Ok(InstallBatchResult {
        items: results,
        states: manager.snapshots(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_directories_expand_once_and_paths_deduplicate() {
        let root = std::env::temp_dir().join(format!("wla-batch-{}", next_id()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("one.wplug"), b"one").unwrap();
        fs::write(root.join("TWO.WPLUG"), b"two").unwrap();
        fs::write(root.join("nested/ignored.wplug"), b"ignored").unwrap();
        fs::write(root.join("picture.png"), b"image").unwrap();
        let expanded = expand_sources(vec![root.clone(), root.join("one.wplug")], false);
        assert_eq!(expanded.len(), 2);
        assert!(expanded.iter().all(|(_, error)| error.is_none()));
        let before = fingerprint(&root.join("one.wplug")).unwrap();
        fs::write(root.join("one.wplug"), b"changed").unwrap();
        assert_ne!(fingerprint(&root.join("one.wplug")).unwrap(), before);
        let canonical = root.canonicalize().unwrap();
        assert!(canonical.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        fs::remove_dir_all(canonical).unwrap();
    }
}

impl LocalPluginInstaller {
    pub(crate) fn new(app_data_dir: &Path) -> Self {
        let directory = app_data_dir.join("plugin-imports");
        if let (Ok(root), Ok(entries)) = (directory.canonicalize(), fs::read_dir(&directory)) {
            for entry in entries.flatten() {
                let path = entry.path();
                let generated = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| {
                        value.split('-').count() == 2
                            && value.split('-').all(|part| {
                                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                            })
                    });
                if !generated
                    || !path
                        .extension()
                        .is_some_and(|ext| ext == "wplug" || ext == "review")
                {
                    continue;
                }
                if let (Ok(metadata), Ok(target)) =
                    (fs::symlink_metadata(&path), path.canonicalize())
                {
                    if metadata.is_symlink() || !target.starts_with(&root) || target == root {
                        continue;
                    }
                    if metadata.is_dir() {
                        let _ = fs::remove_dir_all(target);
                    } else if metadata.is_file() {
                        let _ = fs::remove_file(target);
                    }
                }
            }
        }
        Self::default()
    }
}
#[tauri::command]
pub(crate) fn plugins_install_reset(
    window: WebviewWindow,
    installer: State<'_, LocalPluginInstaller>,
) -> Result<(), PluginError> {
    ensure_main(&window)?;
    installer.previews.lock().map_err(io_error)?.clear();
    let mut uploads = installer.uploads.lock().map_err(io_error)?;
    for upload in uploads.values_mut() {
        upload.file.take();
    }
    uploads.clear();
    installer
        .running
        .lock()
        .map_err(io_error)?
        .retain(|_, flag| Arc::strong_count(flag) > 1);
    Ok(())
}

#[cfg(all(test, windows))]
mod process_tests {
    use super::super::direct_process_tests::{Fixture, TestEnvironment};
    use super::*;
    use std::sync::mpsc;

    fn incoming(fixture: &Fixture, id: &str) -> PathBuf {
        let original = fixture
            .root
            .join("installed-plugins")
            .join(id)
            .join("0.0.0");
        let incoming = fixture.root.join("incoming").join(id);
        fs::create_dir_all(incoming.join("backend")).unwrap();
        for file in ["backend/fixture.exe", "contract.json"] {
            fs::copy(original.join(file), incoming.join(file)).unwrap();
        }
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(original.join("manifest.json")).unwrap()).unwrap();
        manifest["version"] = json!("0.0.0");
        manifest["author"] = json!("YueFChen");
        fs::write(
            incoming.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let files = ["backend/fixture.exe", "contract.json", "manifest.json"].into_iter().map(|name| json!({"path":name,"sha256":hex::encode(Sha256::digest(fs::read(incoming.join(name)).unwrap()))})).collect::<Vec<_>>();
        fs::write(
            incoming.join("checksums.json"),
            serde_json::to_vec(&json!({"algorithm":"sha256","files":files})).unwrap(),
        )
        .unwrap();
        incoming
    }

    #[test]
    #[ignore = "Requires the real direct-plugin process fixture"]
    fn partial_failure_preserves_data_identity_and_continues_independent_commits() {
        let fixture = Fixture::new(Arc::new(TestEnvironment::default()));
        let sources = ["a", "b", "c"].map(|id| incoming(&fixture, id));
        let duplicate = fixture.root.join("same-dropped-package");
        fs::create_dir_all(duplicate.join("backend")).unwrap();
        for file in [
            "backend/fixture.exe",
            "contract.json",
            "manifest.json",
            "checksums.json",
        ] {
            fs::copy(sources[0].join(file), duplicate.join(file)).unwrap();
        }
        let imported = [sources[0].clone(), duplicate];
        let (deduplicated, reviewed) = prepare(
            &fixture.manager,
            imported.to_vec(),
            true,
            imported
                .iter()
                .map(|path| ImportFile {
                    path: path.clone(),
                    label: "a.wplug".into(),
                })
                .collect(),
        );
        assert_eq!(
            deduplicated.items.len(),
            1,
            "duplicate drop staging must not create a conflict"
        );
        drop(reviewed);
        let sentinel = fixture.root.join("plugin-data/b/sentinel");
        fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
        fs::write(&sentinel, b"business data").unwrap();
        fixture
            .manager
            .inner
            .state
            .lock()
            .unwrap()
            .preferences
            .publisher_identities = json!({"b":"ab".repeat(32)});
        let (view, batch) = prepare(&fixture.manager, sources.to_vec(), true, Vec::new());
        assert!(view.items.iter().all(|item| item.error.is_none()));
        // The private reviewed copy remains stable even when an author rebuilds the source.
        let reviewed = batch
            .items
            .iter()
            .find(|item| item.view.manifest.as_ref().unwrap().id == "b")
            .unwrap();
        let original_digest = fingerprint(&reviewed.path).unwrap();
        fs::write(
            sources[1].join("backend/fixture.exe"),
            b"changed after review",
        )
        .unwrap();
        assert_eq!(fingerprint(&reviewed.path).unwrap(), original_digest);
        let selected = view.items.iter().map(|item| item.key.clone()).collect();
        let cancel = batch.cancel.clone();
        let result = run_batch(&fixture.manager, &view.batch_id, batch, selected, cancel).unwrap();
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.status.as_str())
                .collect::<Vec<_>>(),
            vec!["installed", "failed", "installed"],
            "{}",
            serde_json::to_string(&result.items).unwrap()
        );
        assert_eq!(
            result.items[1].error.as_ref().unwrap().code,
            "SOURCE_CHANGED"
        );
        for (id, version) in [("a", "0.0.0"), ("b", "0.0.0"), ("c", "0.0.0")] {
            assert_eq!(
                result
                    .states
                    .iter()
                    .find(|state| state.manifest.id == id)
                    .unwrap()
                    .manifest
                    .version,
                version
            );
        }
        assert_eq!(fs::read(sentinel).unwrap(), b"business data");
        assert_eq!(
            fixture
                .manager
                .inner
                .state
                .lock()
                .unwrap()
                .preferences
                .publisher_identities["b"],
            json!("ab".repeat(32))
        );
    }

    #[test]
    #[ignore = "Requires the real direct-plugin process fixture"]
    fn cancellation_finishes_current_transaction_and_blocks_upgrade_for_the_whole_batch() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let environment = Arc::new(TestEnvironment {
            install_started: Some(started_tx),
            install_release: Some(Mutex::new(release_rx)),
            ..Default::default()
        });
        let fixture = Fixture::new(environment);
        let sources = ["a", "b", "c"].map(|id| incoming(&fixture, id));
        let (view, batch) = prepare(&fixture.manager, sources.to_vec(), true, Vec::new());
        let selected = view.items.iter().map(|item| item.key.clone()).collect();
        let cancel = batch.cancel.clone();
        let worker_cancel = cancel.clone();
        let manager = fixture.manager.clone();
        let worker = thread::spawn(move || {
            run_batch(&manager, &view.batch_id, batch, selected, worker_cancel)
        });
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(
            fixture.manager.begin_upgrade().err().unwrap().code,
            "UPGRADE_BUSY"
        );
        assert_eq!(
            fixture.manager.set_enabled("b", false).err().unwrap().code,
            "PLUGIN_BUSY"
        );
        cancel.store(true, Ordering::Release);
        release_tx.send(()).unwrap();
        let result = worker.join().unwrap().unwrap();
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.status.as_str())
                .collect::<Vec<_>>(),
            vec!["installed", "cancelled", "cancelled"],
            "{}",
            serde_json::to_string(&result.items).unwrap()
        );
        assert!(fixture.manager.begin_upgrade().is_ok());
    }

    #[test]
    #[ignore = "Requires the real direct-plugin process fixture"]
    fn rejected_handshake_exits_before_restoring_the_previous_package() {
        let fixture = Fixture::new(Arc::new(TestEnvironment::default()));
        let source = incoming(&fixture, "a");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(source.join("manifest.json")).unwrap()).unwrap();
        manifest["version"] = json!("0.0.1"); // The fixture deliberately still announces 0.0.0.
        fs::write(
            source.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let files=["backend/fixture.exe","contract.json","manifest.json"].into_iter().map(|name|json!({"path":name,"sha256":hex::encode(Sha256::digest(fs::read(source.join(name)).unwrap()))})).collect::<Vec<_>>();
        fs::write(
            source.join("checksums.json"),
            serde_json::to_vec(&json!({"algorithm":"sha256","files":files})).unwrap(),
        )
        .unwrap();
        let (view, batch) = prepare(&fixture.manager, vec![source], true, Vec::new());
        assert!(view.items[0].error.is_none());
        let cancel = batch.cancel.clone();
        let result = run_batch(
            &fixture.manager,
            &view.batch_id,
            batch,
            [view.items[0].key.clone()].into_iter().collect(),
            cancel,
        )
        .unwrap();
        assert_eq!(result.items[0].status, "failed");
        assert!(
            !result.items[0]
                .error
                .as_ref()
                .unwrap()
                .message
                .contains("rollback also failed")
        );
        let state = result
            .states
            .iter()
            .find(|state| state.manifest.id == "a")
            .unwrap();
        assert_eq!(state.manifest.version, "0.0.0");
        assert_eq!(state.runtime, RuntimeState::Running);
        assert!(!fixture.root.join("installed-plugins/a/0.0.1").exists());
    }
}
