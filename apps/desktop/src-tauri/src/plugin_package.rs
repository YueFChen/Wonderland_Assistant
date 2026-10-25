//! Plugin package scanning, validation, and atomic installation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use wonderland_kernel::logging::{info, warn};
use wonderland_plugin_protocol::{
    PROTOCOL_SUPPORTED_VERSIONS, PluginManifest, PluginUiCommandEffect, PluginUiContributionKind,
    UI_BRIDGE_SUPPORTED_VERSIONS, UI_BRIDGE_VERSION,
};
use zip::ZipArchive;

use crate::plugin_schema;

const MAX_FILES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_CONTRACT_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Debug, Clone)]
pub(crate) struct InspectedPlugin {
    pub manifest: PluginManifest,
    pub contract: Value,
    pub contract_sha256: String,
    pub compatible: bool,
    pub directory: PathBuf,
    pub signature_public_key: Option<String>,
}

/// Retain the previous bytes until the manager commits preferences and activation.
pub(crate) struct PackageCommit {
    installed_root: PathBuf,
    destination: PathBuf,
    backup: Option<PathBuf>,
    recovery: Option<crate::plugin_install_recovery::InstallRecovery>,
    activated: bool,
    backup_moved: bool,
    finished: bool,
}

impl PackageCommit {
    pub(crate) fn rollback(&mut self) -> Result<(), String> {
        if self.finished {
            return Ok(());
        }
        if let Some(recovery) = &self.recovery {
            recovery.rollback()?;
            self.finished = true;
            return Ok(());
        }
        if self.activated && self.destination.exists() {
            ensure_real_directory(&self.destination)?;
            self.check_target(&self.destination)?;
            fs::remove_dir_all(&self.destination)
                .map_err(|error| format!("Cannot remove uncommitted plugin: {error}"))?;
        }
        if self.backup_moved
            && let Some(backup) = &self.backup
        {
            self.check_target(backup)?;
            fs::rename(backup, &self.destination)
                .map_err(|error| format!("Cannot restore retained plugin package: {error}"))?;
        }
        self.finished = true;
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<(), String> {
        if let Some(recovery) = self.recovery.as_mut() {
            recovery.commit()?;
            self.finished = true;
            if let Err(error) = recovery.cleanup() {
                warn!(reason = %error, "Committed installation cleanup will be retried on startup");
            }
            return Ok(());
        }
        self.finished = true;
        if let Some(backup) = self.backup.take() {
            let cleanup = self
                .check_target(&backup)
                .and_then(|()| fs::remove_dir_all(&backup).map_err(|error| error.to_string()));
            if let Err(error) = cleanup {
                warn!(path = %backup.display(), reason = %error, "Retained plugin backup needs cleanup");
            }
        }
        Ok(())
    }

    fn check_target(&self, target: &Path) -> Result<(), String> {
        let root = self
            .installed_root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let target = target.canonicalize().map_err(|error| error.to_string())?;
        if target == root || !target.starts_with(root) {
            return Err("Package recovery path escaped the installation root.".into());
        }
        Ok(())
    }
}

impl Drop for PackageCommit {
    fn drop(&mut self) {
        if let Err(error) = self.rollback() {
            warn!(path = %self.destination.display(), reason = %error, "Plugin package rollback needs recovery; retained bytes were not discarded");
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ScanFailure {
    pub plugin_id: String,
    pub code: &'static str,
    pub phase: &'static str,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Checksums {
    algorithm: String,
    files: Vec<ChecksumFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChecksumFile {
    path: String,
    sha256: String,
}

pub(crate) fn inspect_directory(
    directory: &Path,
    allow_missing_checksums: bool,
) -> Result<InspectedPlugin, String> {
    ensure_real_directory(directory)?;
    validate_tree(directory)?;

    let manifest_path = directory.join("manifest.json");
    let manifest_bytes = read_bounded(&manifest_path, MAX_MANIFEST_BYTES)?;
    let manifest = decode_installed_manifest(&manifest_bytes)?;
    if manifest.manifest_version == 3 {
        validate_manifest(&manifest)?;
    }

    let contract_path = safe_join(directory, &manifest.contract)?;
    let contract_bytes = read_bounded(&contract_path, MAX_CONTRACT_BYTES)?;
    let contract: Value = serde_json::from_slice(&contract_bytes)
        .map_err(|error| format!("contract.json is invalid: {error}"))?;
    validate_contract(&contract)?;

    verify_package_checksums(directory, allow_missing_checksums)?;
    let signature_public_key = directory_signature(directory)?;
    let contract_sha256 = digest_hex(&contract_bytes);
    Ok(InspectedPlugin {
        compatible: is_compatible(&manifest),
        manifest,
        contract,
        contract_sha256,
        directory: directory.to_owned(),
        signature_public_key,
    })
}

pub(crate) fn manifest_for_install_source(source: &Path) -> Result<PluginManifest, String> {
    let bytes = if source.is_dir() {
        read_bounded(&source.join("manifest.json"), MAX_MANIFEST_BYTES)?
    } else {
        let file =
            File::open(source).map_err(|error| format!("Cannot open plugin package: {error}"))?;
        let mut archive = ZipArchive::new(file)
            .map_err(|error| format!("Plugin package is not a valid ZIP: {error}"))?;
        let mut entry = archive
            .by_name("manifest.json")
            .map_err(|error| format!("Plugin package has no root manifest.json: {error}"))?;
        if entry.size() > MAX_MANIFEST_BYTES {
            return Err("Plugin manifest is too large.".to_owned());
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Cannot read plugin manifest: {error}"))?;
        bytes
    };
    let manifest: PluginManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("manifest.json is invalid: {error}"))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub(crate) fn install_archive(
    install_id: u64,
    source: &Path,
    installed_root: &Path,
    overwrite: bool,
    reviewed_manifest: &PluginManifest,
    recovery_config: Option<&Path>,
) -> Result<(InspectedPlugin, PackageCommit), String> {
    let plugin_id = reviewed_manifest.id.as_str();
    let version = reviewed_manifest.version.as_str();
    let staging = run_install_stage(install_id, plugin_id, version, "staging_prepare", || {
        staging_directory(installed_root)
    })?;
    let result = (|| {
        run_install_stage(install_id, plugin_id, version, "archive_extraction", || {
            extract_archive(source, &staging)
        })?;
        let inspected =
            run_install_stage(install_id, plugin_id, version, "staging_validation", || {
                inspect_directory(&staging, false)
            })?;
        run_install_stage(install_id, plugin_id, version, "manifest_review", || {
            ensure_reviewed_manifest(reviewed_manifest, &inspected.manifest)
        })?;
        let commit = run_install_stage(install_id, plugin_id, version, "atomic_commit", || {
            commit_staging(
                &staging,
                installed_root,
                &inspected.manifest,
                overwrite,
                recovery_config,
            )
        })?;
        let destination = install_path(installed_root, &inspected.manifest);
        let installed = run_install_stage(
            install_id,
            plugin_id,
            version,
            "installed_validation",
            || inspect_directory(&destination, false),
        )?;
        Ok((installed, commit))
    })();
    if result.is_err()
        && let Err(error) = fs::remove_dir_all(&staging)
    {
        warn!(
            install_id,
            plugin_id,
            version,
            phase = "staging_cleanup",
            reason = %error,
            "插件包安装失败后清理暂存目录失败"
        );
    }
    result
}

pub(crate) fn install_development_directory(
    install_id: u64,
    source: &Path,
    installed_root: &Path,
    overwrite: bool,
    reviewed_manifest: &PluginManifest,
    recovery_config: Option<&Path>,
) -> Result<(InspectedPlugin, PackageCommit), String> {
    if !cfg!(debug_assertions) {
        return Err("Development-directory installation is disabled in release builds.".to_owned());
    }
    let plugin_id = reviewed_manifest.id.as_str();
    let version = reviewed_manifest.version.as_str();
    run_install_stage(install_id, plugin_id, version, "source_validation", || {
        ensure_real_directory(source)
    })?;
    let staging = run_install_stage(install_id, plugin_id, version, "staging_prepare", || {
        staging_directory(installed_root)
    })?;
    let result = (|| {
        run_install_stage(install_id, plugin_id, version, "development_copy", || {
            copy_package_tree(source, &staging)
        })?;
        let inspected =
            run_install_stage(install_id, plugin_id, version, "staging_validation", || {
                inspect_directory(&staging, true)
            })?;
        run_install_stage(install_id, plugin_id, version, "manifest_review", || {
            ensure_reviewed_manifest(reviewed_manifest, &inspected.manifest)
        })?;
        let commit = run_install_stage(install_id, plugin_id, version, "atomic_commit", || {
            commit_staging(
                &staging,
                installed_root,
                &inspected.manifest,
                overwrite,
                recovery_config,
            )
        })?;
        let destination = install_path(installed_root, &inspected.manifest);
        let installed = run_install_stage(
            install_id,
            plugin_id,
            version,
            "installed_validation",
            || inspect_directory(&destination, true),
        )?;
        Ok((installed, commit))
    })();
    if result.is_err()
        && let Err(error) = fs::remove_dir_all(&staging)
    {
        warn!(
            install_id,
            plugin_id,
            version,
            phase = "staging_cleanup",
            reason = %error,
            "插件包安装失败后清理暂存目录失败"
        );
    }
    result
}

fn run_install_stage<T>(
    install_id: u64,
    plugin_id: &str,
    version: &str,
    phase: &'static str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    info!(
        install_id,
        plugin_id,
        version,
        phase,
        status = "started",
        "插件包安装阶段开始"
    );
    let started_at = std::time::Instant::now();
    match operation() {
        Ok(value) => {
            info!(
                install_id,
                plugin_id,
                version,
                phase,
                status = "complete",
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "插件包安装阶段完成"
            );
            Ok(value)
        }
        Err(error) => {
            warn!(
                install_id,
                plugin_id,
                version,
                phase,
                status = "failed",
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                reason = %error,
                "插件包安装阶段失败"
            );
            Err(error)
        }
    }
}

pub(crate) fn ensure_reviewed_manifest(
    reviewed: &PluginManifest,
    actual: &PluginManifest,
) -> Result<(), String> {
    let reviewed = serde_json::to_value(reviewed)
        .map_err(|_| "Cannot compare the reviewed plugin manifest.".to_owned())?;
    let actual = serde_json::to_value(actual)
        .map_err(|_| "Cannot compare the installed plugin manifest.".to_owned())?;
    if reviewed == actual {
        Ok(())
    } else {
        Err("Plugin manifest changed after review; review the package again.".to_owned())
    }
}

pub(crate) fn scan_installed_root(
    root: &Path,
    active_versions: &BTreeMap<String, String>,
) -> Vec<Result<InspectedPlugin, ScanFailure>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if active_versions.is_empty() {
                return Vec::new();
            }
            return active_versions
                .keys()
                .map(|id| {
                    Err(ScanFailure {
                        plugin_id: id.clone(),
                        code: "PLUGIN_SCAN_IO",
                        phase: "installed_root",
                        message: format!("Cannot scan plugin directory: {error}"),
                        retryable: true,
                    })
                })
                .collect();
        }
        Err(error) => {
            return if active_versions.is_empty() {
                vec![Err(ScanFailure {
                    plugin_id: String::new(),
                    code: "PLUGIN_SCAN_IO",
                    phase: "installed_root",
                    message: format!("Cannot scan plugin directory: {error}"),
                    retryable: true,
                })]
            } else {
                active_versions
                    .keys()
                    .map(|id| {
                        Err(ScanFailure {
                            plugin_id: id.clone(),
                            code: "PLUGIN_SCAN_IO",
                            phase: "installed_root",
                            message: format!("Cannot scan plugin directory: {error}"),
                            retryable: true,
                        })
                    })
                    .collect()
            };
        }
    };
    let mut plugins = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if active_versions.is_empty() {
                    plugins.push(Err(ScanFailure {
                        plugin_id: String::new(),
                        code: "PLUGIN_SCAN_IO",
                        phase: "installed_root",
                        message: format!("Cannot read plugin directory entry: {error}"),
                        retryable: true,
                    }));
                } else {
                    plugins.extend(active_versions.keys().map(|id| {
                        Err(ScanFailure {
                            plugin_id: id.clone(),
                            code: "PLUGIN_SCAN_IO",
                            phase: "installed_root",
                            message: format!("Cannot read plugin directory entry: {error}"),
                            retryable: true,
                        })
                    }));
                }
                continue;
            }
        };
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.starts_with(".staging-") {
            continue;
        }
        plugins.push(scan_installed_plugin(
            root,
            &id,
            active_versions.get(&id).map(String::as_str),
        ));
    }
    plugins
}

pub(crate) fn scan_installed_plugin(
    root: &Path,
    id: &str,
    active_version: Option<&str>,
) -> Result<InspectedPlugin, ScanFailure> {
    ensure_real_directory(&root.join(id))
        .map_err(|message| scan_failure(id, "plugin_directory", message))?;
    if crate::plugin_install_recovery::InstallRecovery::requires_recovery(root, id) {
        return Err(ScanFailure { plugin_id: id.into(), code: "INSTALL_RECOVERY_REQUIRED", phase: "installation_recovery", message: "An interrupted installation needs recovery. Original packages and data have been retained.".into(), retryable: false });
    }
    let plugin_path = root.join(id);
    ensure_real_directory(&plugin_path)
        .map_err(|message| scan_failure(id, "plugin_directory", message))?;
    let versions = fs::read_dir(&plugin_path).map_err(|error| ScanFailure {
        plugin_id: id.to_owned(),
        code: "PLUGIN_SCAN_IO",
        phase: "version_directory",
        message: format!("Cannot read installed versions: {error}"),
        retryable: true,
    })?;

    let mut choices: Vec<(Version, PathBuf)> = Vec::new();
    let mut invalid_version = None;
    for version_entry in versions {
        let version_entry = version_entry.map_err(|error| ScanFailure {
            plugin_id: id.to_owned(),
            code: "PLUGIN_SCAN_IO",
            phase: "version_directory",
            message: format!("Cannot read version entry: {error}"),
            retryable: true,
        })?;
        let version_path = version_entry.path();
        if let Err(message) = ensure_real_directory(&version_path) {
            invalid_version = Some(message);
            continue;
        }
        match Version::parse(&version_entry.file_name().to_string_lossy()) {
            Ok(version) => choices.push((version, version_path)),
            Err(_) => {
                invalid_version =
                    Some("Installed plugin has an invalid version directory.".to_owned())
            }
        }
    }
    choices.sort_by(|left, right| right.0.cmp(&left.0));
    let preferred = active_version.and_then(|active| {
        choices
            .iter()
            .find(|(version, _)| version.to_string() == active)
    });
    if let Some((_, selected)) = preferred.or_else(|| choices.first()) {
        match inspect_directory(selected, cfg!(debug_assertions)) {
            Ok(plugin)
                if plugin.manifest.id == id
                    && plugin.manifest.version
                        == selected
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default() =>
            {
                Ok(plugin)
            }
            Ok(_) => Err(ScanFailure {
                plugin_id: id.to_owned(),
                code: "INVALID_PACKAGE",
                phase: "manifest_validation",
                message: "Manifest ID or version does not match its installation directory."
                    .to_owned(),
                retryable: false,
            }),
            Err(message) => Err(scan_failure(id, "package_validation", message)),
        }
    } else if let Some(message) = invalid_version {
        Err(scan_failure(id, "version_directory", message))
    } else {
        Err(ScanFailure {
            plugin_id: id.to_owned(),
            code: "PLUGIN_PACKAGE_MISSING",
            phase: "version_directory",
            message: "No installed plugin version directory was found.".to_owned(),
            retryable: true,
        })
    }
}

fn scan_failure(id: &str, phase: &'static str, message: String) -> ScanFailure {
    let retryable = message.starts_with("Cannot ") || message == "Package path does not exist.";
    ScanFailure {
        plugin_id: id.to_owned(),
        code: if retryable {
            "PLUGIN_SCAN_IO"
        } else {
            "INVALID_PACKAGE"
        },
        phase,
        message,
        retryable,
    }
}

pub(crate) fn install_path(root: &Path, manifest: &PluginManifest) -> PathBuf {
    root.join(&manifest.id).join(&manifest.version)
}

fn staging_directory(root: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("Cannot create plugin install directory: {error}"))?;
    for attempt in 0..8 {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .wrapping_add(attempt);
        let staging = root.join(format!(".staging-{nonce:x}"));
        match fs::create_dir(&staging) {
            Ok(()) => return Ok(staging),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Cannot create plugin staging directory: {error}")),
        }
    }
    Err("Cannot reserve a unique plugin staging directory.".to_owned())
}

fn commit_staging(
    staging: &Path,
    installed_root: &Path,
    manifest: &PluginManifest,
    overwrite: bool,
    recovery_config: Option<&Path>,
) -> Result<PackageCommit, String> {
    let parent = installed_root.join(&manifest.id);
    fs::create_dir_all(&parent)
        .map_err(|error| format!("Cannot create plugin version directory: {error}"))?;
    ensure_real_directory(&parent)?;
    let destination = install_path(installed_root, manifest);
    let existed = destination.exists();
    if existed && !overwrite {
        return Err(format!(
            "Plugin {} version {} is already installed.",
            manifest.id, manifest.version
        ));
    }
    if existed {
        ensure_real_directory(&destination)?;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let backup = existed.then(|| parent.join(format!(".backup-{nonce:x}")));
    let recovery = recovery_config
        .map(|config| {
            crate::plugin_install_recovery::InstallRecovery::prepare(
                installed_root,
                config,
                &manifest.id,
                &manifest.version,
                format!("{nonce:x}"),
                existed.then(|| format!(".backup-{nonce:x}")),
            )
        })
        .transpose()?;
    let mut commit = PackageCommit {
        installed_root: installed_root.into(),
        destination: destination.clone(),
        backup: backup.clone(),
        recovery,
        activated: false,
        backup_moved: false,
        finished: false,
    };
    if let Some(backup) = &backup {
        fs::rename(&destination, backup).map_err(|error| {
            format!("Cannot stage the existing plugin for replacement: {error}")
        })?;
        commit.backup_moved = true;
    }
    fs::rename(staging, &destination)
        .map_err(|error| format!("Cannot activate staged plugin package: {error}"))?;
    commit.activated = true;
    Ok(commit)
}

fn extract_archive(source: &Path, destination: &Path) -> Result<(), String> {
    let file =
        File::open(source).map_err(|error| format!("Cannot open plugin package: {error}"))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| format!("Plugin package is not a valid ZIP: {error}"))?;
    if archive.len() > MAX_FILES + 2 {
        return Err("Plugin package contains too many entries.".to_owned());
    }
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Cannot read plugin package entry: {error}"))?;
        let raw_name = entry
            .name()
            .map_err(|error| format!("Plugin package entry has an invalid name: {error}"))?
            .into_owned();
        if raw_name.contains('\\') || raw_name.contains(':') || raw_name.starts_with('/') {
            return Err("Plugin package contains a non-portable path.".to_owned());
        }
        let relative = Path::new(&raw_name);
        if entry.enclosed_name().is_none() || !valid_package_path(raw_name.trim_end_matches('/')) {
            return Err("Plugin package contains a path traversal entry.".to_owned());
        }
        let Some(mode) = entry.unix_mode() else {
            // Windows-created ZIP entries may omit Unix mode metadata.
            if entry.is_dir() {
                fs::create_dir_all(destination.join(relative))
                    .map_err(|error| format!("Cannot create plugin directory: {error}"))?;
                continue;
            }
            if !names.insert(raw_name.trim_end_matches('/').to_owned()) {
                return Err("Plugin package contains duplicate paths.".to_owned());
            }
            total = total.saturating_add(entry.size());
            if entry.size() > MAX_FILE_BYTES || total > MAX_PACKAGE_BYTES {
                return Err("Plugin package exceeds the allowed uncompressed size.".to_owned());
            }
            write_archive_entry(&mut entry, destination.join(relative), MAX_FILE_BYTES)?;
            continue;
        };
        let file_type = mode & 0o170000;
        if file_type == 0o120000
            || (file_type != 0 && file_type != 0o100000 && file_type != 0o040000)
        {
            return Err("Plugin package contains a link or special filesystem entry.".to_owned());
        }
        if entry.is_dir() {
            fs::create_dir_all(destination.join(relative))
                .map_err(|error| format!("Cannot create plugin directory: {error}"))?;
            continue;
        }
        if file_type == 0o040000 {
            return Err("Plugin package has inconsistent file type metadata.".to_owned());
        }
        if !names.insert(raw_name.clone()) {
            return Err("Plugin package contains duplicate paths.".to_owned());
        }
        total = total.saturating_add(entry.size());
        if entry.size() > MAX_FILE_BYTES || total > MAX_PACKAGE_BYTES {
            return Err("Plugin package exceeds the allowed uncompressed size.".to_owned());
        }
        write_archive_entry(&mut entry, destination.join(relative), MAX_FILE_BYTES)?;
    }
    if names.len() > MAX_FILES {
        return Err("Plugin package contains too many files.".to_owned());
    }
    Ok(())
}

fn write_archive_entry<R: Read>(
    input: &mut R,
    destination: PathBuf,
    max_bytes: u64,
) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "Plugin package entry has no parent directory.".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Cannot create package directory: {error}"))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|error| format!("Cannot create package file: {error}"))?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| format!("Cannot extract package file: {error}"))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > max_bytes {
            return Err("Plugin package file exceeds its uncompressed size limit.".to_owned());
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| format!("Cannot write package file: {error}"))?;
    }
    output
        .flush()
        .map_err(|error| format!("Cannot flush package file: {error}"))
}

fn copy_package_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let mut stack = vec![(source.to_owned(), destination.to_owned())];
    let mut files = 0_usize;
    let mut total = 0_u64;
    while let Some((current_source, current_destination)) = stack.pop() {
        for entry in fs::read_dir(&current_source)
            .map_err(|error| format!("Cannot read development package: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("Cannot read development package entry: {error}"))?;
            let source_path = entry.path();
            let metadata = fs::symlink_metadata(&source_path)
                .map_err(|error| format!("Cannot inspect development package entry: {error}"))?;
            let destination_path = current_destination.join(entry.file_name());
            if metadata.file_type().is_symlink() {
                return Err("Development package contains a symbolic link.".to_owned());
            }
            if metadata.is_dir() {
                fs::create_dir(&destination_path).map_err(|error| {
                    format!("Cannot create development package directory: {error}")
                })?;
                stack.push((source_path, destination_path));
            } else if metadata.is_file() {
                files += 1;
                total = total.saturating_add(metadata.len());
                if files > MAX_FILES || metadata.len() > MAX_FILE_BYTES || total > MAX_PACKAGE_BYTES
                {
                    return Err("Development package exceeds the allowed size.".to_owned());
                }
                fs::copy(&source_path, &destination_path)
                    .map_err(|error| format!("Cannot copy development package file: {error}"))?;
            } else {
                return Err("Development package contains a special filesystem entry.".to_owned());
            }
        }
    }
    Ok(())
}

fn directory_signature(directory: &Path) -> Result<Option<String>, String> {
    let signature = directory.join("signature.json");
    if !signature.exists() {
        return Ok(None);
    }
    let checksums = read_bounded(&directory.join("checksums.json"), 4 * 1024 * 1024)?;
    let bytes = read_bounded(&signature, 4096)?;
    wonderland_plugin_signing::verify(&checksums, &bytes).map(|verified| Some(verified.public_key))
}

pub(crate) fn signature_for_install_source(source: &Path) -> Result<Option<String>, String> {
    if source.is_dir() {
        return directory_signature(source);
    }
    let mut archive =
        ZipArchive::new(File::open(source).map_err(|_| "Cannot open package signature")?)
            .map_err(|_| "Cannot inspect package signature")?;
    if archive.index_for_name("signature.json").is_none() {
        return Ok(None);
    }
    let mut read = |name: &str, limit: u64| -> Result<Vec<u8>, String> {
        let entry = archive
            .by_name(name)
            .map_err(|_| "INVALID_PACKAGE_SIGNATURE: missing content metadata")?;
        if entry.size() > limit {
            return Err("INVALID_PACKAGE_SIGNATURE: metadata too large".into());
        }
        let mut bytes = Vec::new();
        entry
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "INVALID_PACKAGE_SIGNATURE: cannot read metadata")?;
        if bytes.len() as u64 > limit {
            return Err("INVALID_PACKAGE_SIGNATURE: metadata too large".into());
        }
        Ok(bytes)
    };
    let checksums = read("checksums.json", 4 * 1024 * 1024)?;
    let signature = read("signature.json", 4096)?;
    wonderland_plugin_signing::verify(&checksums, &signature)
        .map(|verified| Some(verified.public_key))
}

fn verify_package_checksums(directory: &Path, allow_missing_checksums: bool) -> Result<(), String> {
    let checksums_path = directory.join("checksums.json");
    let checksums_exists = checksums_path.is_file();
    if !checksums_exists && allow_missing_checksums && !directory.join("signature.json").exists() {
        return Ok(());
    }
    if !checksums_exists {
        return Err("Plugin package must include checksums.json.".to_owned());
    }

    let checksum_bytes = read_bounded(&checksums_path, 4 * 1024 * 1024)?;
    let checksums: Checksums = serde_json::from_slice(&checksum_bytes)
        .map_err(|error| format!("checksums.json is invalid: {error}"))?;
    if checksums.algorithm != "sha256" {
        return Err("Unsupported checksum algorithm.".to_owned());
    }
    if checksums.files.is_empty() {
        return Err("Checksum table must list all package files.".to_owned());
    }
    let mut listed = BTreeMap::new();
    let mut previous: Option<String> = None;
    for item in checksums.files {
        if !valid_package_path(&item.path) || !validate_hex_hash(&item.sha256) {
            return Err("Checksum table contains an unsafe path.".to_owned());
        }
        if previous
            .as_ref()
            .is_some_and(|previous| previous >= &item.path)
        {
            return Err("Checksum paths must be sorted and unique.".to_owned());
        }
        previous = Some(item.path.clone());
        listed.insert(item.path, item.sha256);
    }

    let mut actual = list_package_files(directory)?;
    actual.remove("checksums.json");
    actual.remove("signature.json");
    let expected: BTreeSet<String> = listed.keys().cloned().collect();
    if actual != expected {
        return Err("Checksum path set does not match the package contents.".to_owned());
    }
    for (relative, expected_hash) in listed {
        let path = safe_join(directory, &relative)?;
        if digest_file(&path)? != expected_hash {
            return Err(format!("Checksum mismatch for '{relative}'."));
        }
    }

    Ok(())
}

fn validate_tree(directory: &Path) -> Result<(), String> {
    let files = list_package_files(directory)?;
    if files.is_empty() || files.len() > MAX_FILES {
        return Err("Plugin package file count is outside the allowed range.".to_owned());
    }
    let mut total = 0_u64;
    for relative in &files {
        if !valid_package_path(relative) {
            return Err(format!(
                "Plugin package contains unsupported path '{relative}'."
            ));
        }
        if !relative.contains('/')
            && ![
                "manifest.json",
                "contract.json",
                "checksums.json",
                "signature.json",
            ]
            .contains(&relative.as_str())
        {
            return Err("Plugin package contains an unsupported root file.".to_owned());
        }
        let path = safe_join(directory, relative)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("Cannot inspect plugin package file: {error}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Plugin package files must be regular files.".to_owned());
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(format!("Plugin package file '{relative}' is too large."));
        }
        total = total.saturating_add(metadata.len());
        if total > MAX_PACKAGE_BYTES {
            return Err("Plugin package exceeds the allowed total size.".to_owned());
        }
        if !allowed_package_file(relative) {
            return Err(format!(
                "Plugin package contains an unsupported file '{relative}'."
            ));
        }
    }
    for required in ["manifest.json", "contract.json"] {
        if !files.contains(required) {
            return Err(format!("Plugin package is missing '{required}'."));
        }
    }
    let manifest_bytes = read_bounded(&directory.join("manifest.json"), MAX_MANIFEST_BYTES)?;
    // Tree inspection also runs before installed-package compatibility is known.
    // Identify old metadata without requiring the current executable API shape.
    let manifest = decode_installed_manifest(&manifest_bytes)?;
    if manifest.ui.is_none() && files.iter().any(|path| path.starts_with("ui/")) {
        return Err("A service-only plugin package must not include UI assets.".to_owned());
    }
    let mut required_entries = vec![manifest.backend.entry.as_str()];
    if let Some(ui) = &manifest.ui {
        required_entries.push(ui.entry.as_str());
    }
    for required in required_entries {
        if !files.contains(required) {
            return Err(format!("Plugin package is missing entry '{}'.", required));
        }
    }
    Ok(())
}

fn list_package_files(directory: &Path) -> Result<BTreeSet<String>, String> {
    let mut files = BTreeSet::new();
    let mut stack = vec![(directory.to_owned(), String::new())];
    while let Some((path, relative_prefix)) = stack.pop() {
        for entry in
            fs::read_dir(&path).map_err(|error| format!("Cannot scan plugin package: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("Cannot read plugin package entry: {error}"))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| format!("Cannot inspect plugin package entry: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err("Plugin package contains a symbolic link.".to_owned());
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if relative_prefix.is_empty() {
                name
            } else {
                format!("{relative_prefix}/{name}")
            };
            if !valid_package_path(&relative) {
                return Err("Plugin package contains a non-portable path.".to_owned());
            }
            if metadata.is_dir() {
                if relative != "ui"
                    && relative != "backend"
                    && !relative.starts_with("ui/")
                    && !relative.starts_with("backend/")
                {
                    return Err(
                        "Plugin package directories must be under ui/ or backend/.".to_owned()
                    );
                }
                stack.push((entry.path(), relative));
            } else if metadata.is_file() {
                if !files.insert(relative) {
                    return Err("Plugin package contains duplicate normalized paths.".to_owned());
                }
            } else {
                return Err("Plugin package contains a special filesystem entry.".to_owned());
            }
        }
    }
    Ok(files)
}

fn validate_manifest(manifest: &PluginManifest) -> Result<(), String> {
    if manifest.remote_access == Some(true) && !manifest.supports_remote_access() {
        return Err("Remote access requires backend.supportsServiceContext to be true.".into());
    }
    if manifest.manifest_version != 3 {
        return Err("Unsupported plugin manifest version.".to_owned());
    }
    if !valid_plugin_id(&manifest.id) {
        return Err("Plugin ID is invalid.".to_owned());
    }
    if manifest.name.trim().is_empty() || manifest.name.chars().count() > 120 {
        return Err("Plugin name is invalid.".to_owned());
    }
    if let Some(description) = &manifest.description
        && description.chars().count() > 500
    {
        return Err("Plugin description is too long.".to_owned());
    }
    for (field, value, limit) in [
        ("author", &manifest.author, 120),
        ("license", &manifest.license, 256),
    ] {
        if let Some(value) = value
            && (value.trim().is_empty()
                || value.chars().count() > limit
                || value.chars().any(char::is_control))
        {
            return Err(format!("Plugin {field} is invalid."));
        }
    }
    if let Some(homepage) = &manifest.homepage {
        let url = tauri::Url::parse(homepage).map_err(|_| "Plugin homepage is invalid.")?;
        if homepage.len() > 2048
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || homepage.chars().any(char::is_control)
        {
            return Err("Plugin homepage must be an HTTP(S) URL without credentials.".into());
        }
    }
    Version::parse(&manifest.version)
        .map_err(|_| "Plugin version is not valid semantic versioning.".to_owned())?;
    let min_core = Version::parse(&manifest.host_compatibility.min_core_version)
        .map_err(|_| "Minimum Core version is invalid.".to_owned())?;
    let max_core = Version::parse(&manifest.host_compatibility.max_core_version_exclusive)
        .map_err(|_| "Maximum Core version is invalid.".to_owned())?;
    if min_core >= max_core {
        return Err("Plugin Core compatibility range is invalid.".to_owned());
    }
    let min_protocol = Version::parse(&manifest.host_compatibility.protocol.min_version)
        .map_err(|_| "Minimum protocol version is invalid.".to_owned())?;
    let max_protocol = Version::parse(&manifest.host_compatibility.protocol.max_version_exclusive)
        .map_err(|_| "Maximum protocol version is invalid.".to_owned())?;
    if min_protocol >= max_protocol {
        return Err("Plugin protocol compatibility range is invalid.".to_owned());
    }
    if manifest.platform.os != "windows"
        || !["x86_64", "aarch64"].contains(&manifest.platform.architecture.as_str())
        || manifest.platform.abi != "msvc"
    {
        return Err("Plugin package declares an unsupported platform.".to_owned());
    }

    if let Some(ui) = &manifest.ui {
        if ui.entry != "ui/index.html" || !ui.entry.starts_with("ui/") {
            return Err("Plugin UI entry must be ui/index.html in manifest v3.".to_owned());
        }
        let min_ui_bridge = Version::parse(&ui.bridge_compatibility.min_version)
            .map_err(|_| "Minimum UI bridge version is invalid.".to_owned())?;
        let max_ui_bridge = Version::parse(&ui.bridge_compatibility.max_version_exclusive)
            .map_err(|_| "Maximum UI bridge version is invalid.".to_owned())?;
        if min_ui_bridge >= max_ui_bridge {
            return Err("Plugin UI bridge compatibility range is invalid.".to_owned());
        }
        let command_bridge_version = Version::parse(UI_BRIDGE_VERSION)
            .expect("the Core UI bridge version is a valid semantic version");
        if ui
            .contributions
            .iter()
            .any(|contribution| !contribution.commands.is_empty())
            && min_ui_bridge < command_bridge_version
        {
            return Err(format!(
                "Plugins that declare UI commands must require bridge version {UI_BRIDGE_VERSION} or newer."
            ));
        }
        if ui.integrations.len() > 8 {
            return Err("Plugin requests too many UI integrations.".to_owned());
        }
        let allowed_integrations = ["theme.followHost", "workspace.sidebar"];
        let mut integrations = BTreeSet::new();
        for integration in &ui.integrations {
            if !allowed_integrations.contains(&integration.as_str())
                || !integrations.insert(integration)
            {
                return Err(format!(
                    "Plugin requests an unknown or duplicate UI integration '{integration}'."
                ));
            }
        }
        if ui.contributions.is_empty() || ui.contributions.len() > 64 {
            return Err("A UI plugin must declare between one and 64 UI contributions.".to_owned());
        }
        let mut contribution_ids = BTreeSet::new();
        let mut activity_count = 0;
        let has_sidebar_integration = ui
            .integrations
            .iter()
            .any(|integration| integration == "workspace.sidebar");
        for contribution in &ui.contributions {
            if !valid_plugin_id(&contribution.id) || !contribution_ids.insert(&contribution.id) {
                return Err(format!(
                    "Plugin contribution ID '{}' is invalid or duplicated.",
                    contribution.id
                ));
            }
            match contribution.kind {
                PluginUiContributionKind::Activity => {
                    activity_count += 1;
                    if contribution.location.is_some() {
                        return Err(format!(
                            "Activity '{}' cannot declare a location.",
                            contribution.id
                        ));
                    }
                }
                PluginUiContributionKind::View => {
                    if contribution.location.as_deref() != Some("workspace.sidebar")
                        || !has_sidebar_integration
                    {
                        return Err(format!(
                            "View '{}' must declare workspace.sidebar and request the workspace.sidebar integration.",
                            contribution.id
                        ));
                    }
                }
            }
            if contribution.title.trim().is_empty() || contribution.title.chars().count() > 120 {
                return Err(format!(
                    "Plugin contribution '{}' has an invalid title.",
                    contribution.id
                ));
            }
            if !(-1_000..=1_000).contains(&contribution.default_order) {
                return Err(format!(
                    "Plugin contribution '{}' defaultOrder is out of range.",
                    contribution.id
                ));
            }
            if contribution.commands.len() > 64 {
                return Err(format!(
                    "Contribution '{}' declares too many UI commands.",
                    contribution.id
                ));
            }
            let mut command_ids = BTreeSet::new();
            for command in &contribution.commands {
                if !valid_method(&command.id) || !command_ids.insert(&command.id) {
                    return Err(format!(
                        "UI command ID '{}' is invalid or duplicated in contribution '{}'.",
                        command.id, contribution.id
                    ));
                }
                if command.title.trim().is_empty() || command.title.chars().count() > 120 {
                    return Err(format!("UI command '{}' has an invalid title.", command.id));
                }
                if command.input_schema.get("type").and_then(Value::as_str) != Some("object") {
                    return Err(format!(
                        "UI command '{}' input schema must describe an object.",
                        command.id
                    ));
                }
                if serde_json::to_vec(&command.input_schema)
                    .is_ok_and(|schema| schema.len() > 64 * 1024)
                {
                    return Err(format!(
                        "UI command '{}' input schema exceeds 64 KiB.",
                        command.id
                    ));
                }
                plugin_schema::ensure_supported_in(&command.input_schema, &command.input_schema)
                    .map_err(|error| {
                        format!(
                            "UI command '{}' has an unsupported input schema: {error}",
                            command.id
                        )
                    })?;
                if matches!(command.effect, PluginUiCommandEffect::Mutating)
                    && command
                        .input_schema
                        .get("readOnly")
                        .and_then(Value::as_bool)
                        == Some(true)
                {
                    return Err(format!(
                        "Mutating UI command '{}' cannot declare a read-only input schema.",
                        command.id
                    ));
                }
            }
        }
        if activity_count != 1 {
            return Err(
                "A UI plugin must declare exactly one primary Activity contribution.".to_owned(),
            );
        }
    }
    if !manifest.backend.entry.starts_with("backend/")
        || !manifest
            .backend
            .entry
            .to_ascii_lowercase()
            .ends_with(".exe")
        || manifest.backend.transport != "stdio-ndjson-v1"
    {
        return Err("Plugin backend entry or transport is invalid for this platform.".to_owned());
    }
    if manifest.contract != "contract.json" {
        return Err("Plugin contract must be the root contract.json file.".to_owned());
    }
    if manifest.dependencies.len() > 32 {
        return Err("Plugin declares too many dependencies.".into());
    }
    let mut ids = BTreeSet::new();
    for dependency in &manifest.dependencies {
        if !valid_plugin_id(&dependency.plugin_id) || !ids.insert(&dependency.plugin_id) {
            return Err("Plugin dependency ID is invalid or duplicated.".into());
        }
        VersionReq::parse(&dependency.version_req)
            .map_err(|_| "Plugin dependency range is invalid.")?;
        if dependency.methods.len() > 128
            || dependency
                .methods
                .iter()
                .any(|method| !valid_method(method))
            || dependency.methods.iter().collect::<BTreeSet<_>>().len() != dependency.methods.len()
        {
            return Err("Plugin dependency methods are invalid or duplicated.".into());
        }
    }
    Ok(())
}

/// Read old metadata for display only; old backends never execute on the new API.
fn decode_installed_manifest(bytes: &[u8]) -> Result<PluginManifest, String> {
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("manifest.json is invalid: {error}"))?;
    let version = value
        .get("manifestVersion")
        .and_then(Value::as_u64)
        .ok_or("Plugin manifest has no format version.")?;
    if version != 3 {
        if ![1, 2].contains(&version) {
            return Err("Unsupported plugin manifest format.".into());
        }
        let object = value
            .as_object_mut()
            .ok_or("Plugin manifest is not an object.")?;
        for field in ["capabilities", "networkPublicHosts", "provides", "requires"] {
            object.remove(field);
        }
        object.insert("dependencies".into(), serde_json::json!([]));
    }
    let manifest: PluginManifest = serde_json::from_value(value)
        .map_err(|error| format!("manifest.json is invalid: {error}"))?;
    if !valid_plugin_id(&manifest.id)
        || Version::parse(&manifest.version).is_err()
        || manifest.contract != "contract.json"
    {
        return Err("Plugin manifest identity or contract path is invalid.".into());
    }
    Ok(manifest)
}

/// Returns the Core-supported versions a plugin declares it can use, newest first.
/// Compatibility checks and UI launch negotiation must use this same range rule.
pub(crate) fn supported_versions_in_range(
    supported_versions: &[&str],
    min_version: &str,
    max_version_exclusive: &str,
) -> Vec<String> {
    let (Ok(min), Ok(max)) = (
        Version::parse(min_version),
        Version::parse(max_version_exclusive),
    ) else {
        return Vec::new();
    };
    if min >= max {
        return Vec::new();
    }

    supported_versions
        .iter()
        .rev()
        .filter(|version| {
            Version::parse(version).is_ok_and(|version| version >= min && version < max)
        })
        .map(|version| (*version).to_owned())
        .collect()
}

fn is_compatible(manifest: &PluginManifest) -> bool {
    let Ok(core) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        return false;
    };
    let Ok(min_core) = Version::parse(&manifest.host_compatibility.min_core_version) else {
        return false;
    };
    let Ok(max_core) = Version::parse(&manifest.host_compatibility.max_core_version_exclusive)
    else {
        return false;
    };
    let protocol_compatible = !supported_versions_in_range(
        PROTOCOL_SUPPORTED_VERSIONS,
        &manifest.host_compatibility.protocol.min_version,
        &manifest.host_compatibility.protocol.max_version_exclusive,
    )
    .is_empty();
    let ui_compatible = manifest.ui.as_ref().is_none_or(|ui| {
        !supported_versions_in_range(
            UI_BRIDGE_SUPPORTED_VERSIONS,
            &ui.bridge_compatibility.min_version,
            &ui.bridge_compatibility.max_version_exclusive,
        )
        .is_empty()
    });
    let expected_os = if cfg!(windows) {
        "windows"
    } else {
        std::env::consts::OS
    };
    let expected_arch = std::env::consts::ARCH;
    let expected_abi = if cfg!(windows) {
        "msvc"
    } else {
        std::env::consts::OS
    };
    manifest.manifest_version == 3
        && core >= min_core
        && core < max_core
        && protocol_compatible
        && ui_compatible
        && manifest.platform.os == expected_os
        && manifest.platform.architecture == expected_arch
        && manifest.platform.abi == expected_abi
}

pub(crate) fn manifest_is_compatible(manifest: &PluginManifest) -> bool {
    is_compatible(manifest)
}

fn validate_contract(contract: &Value) -> Result<(), String> {
    let object = contract
        .as_object()
        .ok_or_else(|| "contract.json must be an object.".to_owned())?;
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err("Unsupported plugin contract schema version.".to_owned());
    }
    if object
        .keys()
        .any(|key| !["schemaVersion", "$defs", "methods", "events"].contains(&key.as_str()))
    {
        return Err("contract.json contains an unknown top-level field.".to_owned());
    }
    let methods = object
        .get("methods")
        .and_then(Value::as_object)
        .ok_or_else(|| "contract.json must define a methods object.".to_owned())?;
    let events = object
        .get("events")
        .and_then(Value::as_object)
        .ok_or_else(|| "contract.json must define an events object.".to_owned())?;
    if let Some(definitions) = object.get("$defs") {
        let definitions = definitions
            .as_object()
            .ok_or_else(|| "contract.json '$defs' must be an object.".to_owned())?;
        for definition in definitions.values() {
            plugin_schema::ensure_supported_in(definition, contract)
                .map_err(|error| format!("Contract definition: {error}"))?;
        }
    }
    if methods.len() > 512 || events.len() > 512 {
        return Err("Plugin contract contains too many methods or events.".to_owned());
    }
    for (name, method) in methods {
        if !valid_method(name) {
            return Err(format!("Invalid method name '{name}'."));
        }
        let method = method
            .as_object()
            .ok_or_else(|| format!("Method '{name}' must be an object."))?;
        if method
            .keys()
            .any(|key| !["params", "result", "timeoutMs", "localOnly"].contains(&key.as_str()))
        {
            return Err(format!("Method '{name}' contains an unknown field."));
        }
        let params = method
            .get("params")
            .ok_or_else(|| format!("Method '{name}' is missing params schema."))?;
        let result = method
            .get("result")
            .ok_or_else(|| format!("Method '{name}' is missing result schema."))?;
        plugin_schema::ensure_supported_in(params, contract)
            .map_err(|error| format!("Method '{name}' params: {error}"))?;
        plugin_schema::ensure_supported_in(result, contract)
            .map_err(|error| format!("Method '{name}' result: {error}"))?;
        if method
            .get("localOnly")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(format!("Method '{name}' localOnly must be a boolean."));
        }
        if let Some(value) = method.get("timeoutMs").filter(|value| !value.is_null()) {
            let timeout = value
                .as_u64()
                .filter(|timeout| *timeout > 0)
                .ok_or_else(|| {
                    format!("Method '{name}' timeoutMs must be a positive integer or null.")
                })?;
            if std::time::Instant::now()
                .checked_add(std::time::Duration::from_millis(timeout))
                .is_none()
            {
                return Err(format!("Method '{name}' timeoutMs cannot be represented."));
            }
        }
    }
    for (name, event) in events {
        if !valid_topic(name) {
            return Err(format!("Invalid event topic '{name}'."));
        }
        let event = event
            .as_object()
            .ok_or_else(|| format!("Event '{name}' must be an object."))?;
        if event.keys().any(|key| key != "payload") {
            return Err(format!("Event '{name}' contains an unknown field."));
        }
        let payload = event
            .get("payload")
            .ok_or_else(|| format!("Event '{name}' is missing payload schema."))?;
        plugin_schema::ensure_supported_in(payload, contract)
            .map_err(|error| format!("Event '{name}' payload: {error}"))?;
    }
    Ok(())
}

fn valid_plugin_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

fn valid_method(value: &str) -> bool {
    value.split('.').all(|part| {
        !part.is_empty()
            && part.as_bytes()[0].is_ascii_lowercase()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

fn valid_topic(value: &str) -> bool {
    valid_method(value)
}

fn allowed_package_file(path: &str) -> bool {
    matches!(
        path,
        "manifest.json" | "contract.json" | "checksums.json" | "signature.json"
    ) || path.starts_with("ui/")
        || path.starts_with("backend/")
}

fn valid_package_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 240
        || value.contains('\\')
        || value.contains(':')
        || value.starts_with('/')
    {
        return false;
    }
    let path = Path::new(value);
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if !valid_package_path(relative) {
        return Err("Package path is unsafe.".to_owned());
    }
    let path = root.join(relative);
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("Cannot resolve package directory: {error}"))?;
    let canonical_parent = path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .ok_or_else(|| "Package path does not exist.".to_owned())?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err("Package path escapes the installation directory.".to_owned());
    }
    Ok(path)
}

pub(crate) fn ensure_real_directory(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("Cannot inspect directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Plugin installation path must be a real directory.".to_owned());
    }
    Ok(())
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes {
        return Err(format!(
            "{} is not a regular file or exceeds its size limit.",
            path.display()
        ));
    }
    fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))
}

fn digest_file(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("Cannot read package file: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("Cannot hash package file: {error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn digest_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validate_hex_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wonderland_plugin_protocol::{PluginUiCommand, PluginUiCommandEffect};

    #[test]
    fn package_commit_retains_original_bytes_until_metadata_success() {
        let root = std::env::temp_dir().join(format!(
            "wla-package-commit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manifest: PluginManifest = serde_json::from_str(include_str!(
            "../tests/fixtures/comment_collector_manifest.json"
        ))
        .unwrap();
        let destination = install_path(&root, &manifest);
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("bytes"), b"original").unwrap();
        let stage = staging_directory(&root).unwrap();
        fs::write(stage.join("bytes"), b"replacement").unwrap();
        let mut commit = commit_staging(&stage, &root, &manifest, true, None).unwrap();
        assert_eq!(fs::read(destination.join("bytes")).unwrap(), b"replacement");
        assert_eq!(
            fs::read(commit.backup.as_ref().unwrap().join("bytes")).unwrap(),
            b"original"
        );
        commit.rollback().unwrap();
        assert_eq!(fs::read(destination.join("bytes")).unwrap(), b"original");
        let stage = staging_directory(&root).unwrap();
        fs::write(stage.join("bytes"), b"final").unwrap();
        commit_staging(&stage, &root, &manifest, true, None)
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(fs::read(destination.join("bytes")).unwrap(), b"final");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_signatures_authenticate_raw_checksums_and_nested_signature_files() {
        let root = std::env::temp_dir().join(format!(
            "wla-package-signature-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("ui")).unwrap();
        fs::write(root.join("ui/signature.json"), b"ordinary nested payload").unwrap();
        let checksums=serde_json::to_vec(&serde_json::json!({"algorithm":"sha256","files":[{"path":"ui/signature.json","sha256":digest_hex(b"ordinary nested payload")}]})).unwrap();
        fs::write(root.join("checksums.json"), &checksums).unwrap();
        assert!(verify_package_checksums(&root, false).is_ok());
        assert_eq!(directory_signature(&root).unwrap(), None);
        let signed = wonderland_plugin_signing::sign(&checksums, &[42; 32]).unwrap();
        fs::write(
            root.join("signature.json"),
            serde_json::to_vec(&signed).unwrap(),
        )
        .unwrap();
        assert_eq!(directory_signature(&root).unwrap(), Some(signed.public_key));
        assert!(verify_package_checksums(&root, false).is_ok());
        let mut changed = checksums.clone();
        changed.push(b'\n');
        fs::write(root.join("checksums.json"), &changed).unwrap();
        assert!(verify_package_checksums(&root, false).is_ok());
        assert!(
            directory_signature(&root)
                .unwrap_err()
                .starts_with("INVALID_PACKAGE_SIGNATURE")
        );
        fs::remove_file(root.join("checksums.json")).unwrap();
        assert!(verify_package_checksums(&root, true).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_access_is_explicit_and_requires_context_support() {
        let mut manifest: PluginManifest = serde_json::from_str(include_str!(
            "../tests/fixtures/comment_collector_manifest.json"
        ))
        .unwrap();
        assert!(!manifest.supports_remote_access());
        validate_manifest(&manifest).unwrap();
        manifest.backend.supports_service_context = Some(true);
        assert!(!manifest.supports_remote_access());
        manifest.remote_access = Some(false);
        assert!(!manifest.supports_remote_access());
        manifest.remote_access = Some(true);
        assert!(manifest.supports_remote_access());
        validate_manifest(&manifest).unwrap();
        manifest.backend.supports_service_context = None;
        assert!(!manifest.supports_remote_access());
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn install_rejects_a_manifest_changed_after_review() {
        let reviewed: PluginManifest = serde_json::from_str(include_str!(
            "../tests/fixtures/comment_collector_manifest.json"
        ))
        .unwrap();
        let mut actual = reviewed.clone();
        ensure_reviewed_manifest(&reviewed, &actual).unwrap();
        actual.name.push_str(" changed");
        assert!(ensure_reviewed_manifest(&reviewed, &actual).is_err());
    }

    #[test]
    fn installed_v2_is_identifiable_but_never_compatible_or_installable() {
        let mut legacy: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/comment_collector_manifest.json"
        ))
        .unwrap();
        legacy["manifestVersion"] = serde_json::json!(2);
        legacy.as_object_mut().unwrap().remove("dependencies");
        legacy["capabilities"] = serde_json::json!(["files.pick"]);
        legacy["provides"] = serde_json::json!([]);
        let bytes = serde_json::to_vec(&legacy).unwrap();
        let manifest = decode_installed_manifest(&bytes).unwrap();
        assert_eq!(manifest.id, "comment_collector");
        assert_eq!(manifest.manifest_version, 2);
        assert!(!is_compatible(&manifest));
        assert!(validate_manifest(&manifest).is_err());
        assert!(serde_json::from_slice::<PluginManifest>(&bytes).is_err());
    }

    #[test]
    fn installed_v2_full_package_scan_is_incompatible_without_rewriting_files() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temporary_root = std::env::temp_dir().join(format!(
            "wla-installed-v2-scan-{}-{nonce}",
            std::process::id()
        ));
        let installed_root = temporary_root.join("installed");
        let package = installed_root.join("comment_collector/0.1.0");
        fs::create_dir_all(package.join("backend")).unwrap();
        fs::create_dir_all(package.join("ui")).unwrap();

        let mut manifest: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/comment_collector_manifest.json"
        ))
        .unwrap();
        manifest["manifestVersion"] = serde_json::json!(2);
        manifest.as_object_mut().unwrap().remove("dependencies");
        manifest["capabilities"] = serde_json::json!(["files.pick", "files.export"]);
        manifest["networkPublicHosts"] = serde_json::json!(["example.com"]);
        manifest["provides"] = serde_json::json!([{
            "serviceId": "archive", "version": "1.0.0", "methods": ["archive.export"]
        }]);
        manifest["requires"] = serde_json::json!([]);
        let mut original_files = BTreeMap::from([
            (
                "manifest.json".to_owned(),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            ),
            (
                "contract.json".to_owned(),
                br#"{"schemaVersion":1,"methods":{},"events":{}}"#.to_vec(),
            ),
            (
                "backend/provider.exe".to_owned(),
                b"legacy backend".to_vec(),
            ),
            ("ui/index.html".to_owned(), b"legacy UI".to_vec()),
        ]);
        let checksum_files = original_files
            .iter()
            .map(|(path, bytes)| serde_json::json!({"path": path, "sha256": digest_hex(bytes)}))
            .collect::<Vec<_>>();
        original_files.insert(
            "checksums.json".to_owned(),
            serde_json::to_vec_pretty(&serde_json::json!({
                "algorithm": "sha256", "files": checksum_files
            }))
            .unwrap(),
        );
        for (path, bytes) in &original_files {
            fs::write(package.join(path), bytes).unwrap();
        }
        let private_data = temporary_root.join("plugin-data/comment_collector/data.json");
        fs::create_dir_all(private_data.parent().unwrap()).unwrap();
        fs::write(&private_data, b"retained plugin data").unwrap();

        let inspected = inspect_directory(&package, false).unwrap();
        assert_eq!(inspected.manifest.manifest_version, 2);
        assert!(!inspected.compatible);
        let selected =
            scan_installed_plugin(&installed_root, "comment_collector", Some("0.1.0")).unwrap();
        assert_eq!(selected.manifest.id, "comment_collector");
        assert!(!selected.compatible);
        let active_versions = BTreeMap::from([("comment_collector".into(), "0.1.0".into())]);
        let scanned = scan_installed_root(&installed_root, &active_versions);
        assert_eq!(scanned.len(), 1);
        let scanned = scanned[0].as_ref().unwrap();
        assert_eq!(scanned.manifest.manifest_version, 2);
        assert!(!scanned.compatible);
        assert!(manifest_for_install_source(&package).is_err());

        for (path, bytes) in &original_files {
            assert_eq!(fs::read(package.join(path)).unwrap(), *bytes, "{path}");
        }
        verify_package_checksums(&package, false).unwrap();
        assert_eq!(fs::read(&private_data).unwrap(), b"retained plugin data");

        // Display-only compatibility decoding must still verify original hashes.
        fs::write(package.join("backend/provider.exe"), b"tampered backend").unwrap();
        let failure =
            scan_installed_plugin(&installed_root, "comment_collector", Some("0.1.0")).unwrap_err();
        assert_eq!(failure.code, "INVALID_PACKAGE");
        assert!(failure.message.contains("Checksum mismatch"));

        fs::remove_dir_all(temporary_root).unwrap();
    }

    #[test]
    fn supported_protocol_ranges_match_versions_the_runtime_can_speak() {
        let core_protocols =
            supported_versions_in_range(PROTOCOL_SUPPORTED_VERSIONS, "0.0.0", "999.0.0");
        let core_bridges =
            supported_versions_in_range(UI_BRIDGE_SUPPORTED_VERSIONS, "0.0.0", "999.0.0");
        assert_eq!(
            core_protocols.first().map(String::as_str),
            Some(wonderland_plugin_protocol::PROTOCOL_VERSION)
        );
        assert_eq!(
            core_bridges.first().map(String::as_str),
            Some(UI_BRIDGE_VERSION)
        );
        assert_eq!(
            supported_versions_in_range(&["1.0.0", "1.1.0"], "1.0.0", "2.0.0"),
            ["1.1.0", "1.0.0"]
        );
        assert_eq!(
            supported_versions_in_range(&["1.0.0", "1.1.0"], "1.0.0", "1.1.0"),
            ["1.0.0"]
        );
        assert!(supported_versions_in_range(&["1.0.0"], "1.1.0", "2.0.0").is_empty());
    }

    #[test]
    fn bridge_compatibility_keeps_the_previous_minor_available() {
        assert_eq!(
            supported_versions_in_range(UI_BRIDGE_SUPPORTED_VERSIONS, "1.0.0", "1.1.0"),
            ["1.0.0"]
        );
        assert_eq!(
            supported_versions_in_range(UI_BRIDGE_SUPPORTED_VERSIONS, "1.1.0", "2.0.0"),
            ["1.1.0"]
        );
    }

    #[test]
    fn path_validation_rejects_windows_and_traversal_paths() {
        for path in [
            "../secret",
            "ui/../../secret",
            "C:/secret",
            "ui\\secret",
            "/root",
        ] {
            assert!(!valid_package_path(path), "accepted {path}");
        }
        for path in ["manifest.json", "ui/index.html", "backend/plugin.exe"] {
            assert!(valid_package_path(path), "rejected {path}");
        }
    }

    #[test]
    fn manifest_id_uses_the_frozen_ascii_grammar() {
        assert!(valid_plugin_id("translator"));
        assert!(valid_plugin_id("plugin_2"));
        assert!(!valid_plugin_id("Upper"));
        assert!(!valid_plugin_id("../plugin"));
    }

    #[test]
    fn plugin_ui_commands_require_a_supported_schema_and_current_bridge() {
        let mut manifest: PluginManifest = serde_json::from_str(include_str!(
            "../tests/fixtures/comment_collector_manifest.json"
        ))
        .unwrap();
        {
            let ui = manifest.ui.as_mut().unwrap();
            ui.bridge_compatibility.min_version = UI_BRIDGE_VERSION.to_owned();
            ui.contributions[0].commands.push(PluginUiCommand {
                id: "archive.export".to_owned(),
                title: "Export archive".to_owned(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"format": {"type": "string", "enum": ["json", "csv"]}},
                    "required": ["format"],
                    "additionalProperties": false
                }),
                effect: PluginUiCommandEffect::Mutating,
            });
        }
        validate_manifest(&manifest).unwrap();

        manifest
            .ui
            .as_mut()
            .unwrap()
            .bridge_compatibility
            .min_version = "1.0.0".to_owned();
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .contains("must require bridge version")
        );

        let ui = manifest.ui.as_mut().unwrap();
        ui.bridge_compatibility.min_version = UI_BRIDGE_VERSION.to_owned();
        ui.contributions[0].commands[0].input_schema = serde_json::json!({
            "type": "object",
            "format": "not-supported"
        });
        assert!(
            validate_manifest(&manifest)
                .unwrap_err()
                .contains("unsupported input schema")
        );
    }

    #[test]
    fn checksum_hash_shape_is_strict() {
        assert!(validate_hex_hash(&"a".repeat(64)));
        assert!(!validate_hex_hash(&"A".repeat(64)));
        assert!(!validate_hex_hash(&"a".repeat(63)));
    }
}
