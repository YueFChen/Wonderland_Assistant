//! Plugin package scanning, validation, and atomic installation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use semver::Version;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use wonderland_plugin_protocol::{
    PluginManifest, PluginUiCommandEffect, PluginUiContributionKind, UI_BRIDGE_VERSION,
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
    let manifest: PluginManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("manifest.json is invalid: {error}"))?;
    validate_manifest(&manifest)?;

    let contract_path = safe_join(directory, &manifest.contract)?;
    let contract_bytes = read_bounded(&contract_path, MAX_CONTRACT_BYTES)?;
    let contract: Value = serde_json::from_slice(&contract_bytes)
        .map_err(|error| format!("contract.json is invalid: {error}"))?;
    validate_contract(&contract)?;
    validate_provided_service_methods(&manifest, &contract)?;

    verify_package_checksums(directory, allow_missing_checksums)?;
    let contract_sha256 = digest_hex(&contract_bytes);
    Ok(InspectedPlugin {
        compatible: is_compatible(&manifest),
        manifest,
        contract,
        contract_sha256,
        directory: directory.to_owned(),
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
    source: &Path,
    installed_root: &Path,
    overwrite: bool,
) -> Result<InspectedPlugin, String> {
    let staging = staging_directory(installed_root)?;
    let result = (|| {
        extract_archive(source, &staging)?;
        let inspected = inspect_directory(&staging, false)?;
        commit_staging(&staging, installed_root, &inspected.manifest, overwrite)?;
        let destination = install_path(installed_root, &inspected.manifest);
        inspect_directory(&destination, false)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub(crate) fn install_development_directory(
    source: &Path,
    installed_root: &Path,
    overwrite: bool,
) -> Result<InspectedPlugin, String> {
    if !cfg!(debug_assertions) {
        return Err("Development-directory installation is disabled in release builds.".to_owned());
    }
    ensure_real_directory(source)?;
    let staging = staging_directory(installed_root)?;
    let result = (|| {
        copy_package_tree(source, &staging)?;
        let inspected = inspect_directory(&staging, true)?;
        commit_staging(&staging, installed_root, &inspected.manifest, overwrite)?;
        let destination = install_path(installed_root, &inspected.manifest);
        inspect_directory(&destination, true)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub(crate) fn scan_installed_root(
    root: &Path,
    active_versions: &BTreeMap<String, String>,
) -> Vec<Result<InspectedPlugin, (String, String)>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            return vec![Err((
                String::new(),
                format!("Cannot scan plugin directory: {error}"),
            ))];
        }
    };
    let mut plugins = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                plugins.push(Err((
                    String::new(),
                    format!("Cannot read plugin directory entry: {error}"),
                )));
                continue;
            }
        };
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.starts_with(".staging-") {
            continue;
        }
        let path = entry.path();
        if let Err(error) = ensure_real_directory(&path) {
            plugins.push(Err((id, error)));
            continue;
        }
        let versions = match fs::read_dir(&path) {
            Ok(versions) => versions,
            Err(error) => {
                plugins.push(Err((
                    id,
                    format!("Cannot read installed versions: {error}"),
                )));
                continue;
            }
        };
        let mut choices: Vec<(Version, PathBuf)> = Vec::new();
        let mut invalid_version = None;
        for version_entry in versions {
            let version_entry = match version_entry {
                Ok(version_entry) => version_entry,
                Err(error) => {
                    invalid_version = Some(format!("Cannot read version entry: {error}"));
                    continue;
                }
            };
            let version_path = version_entry.path();
            if ensure_real_directory(&version_path).is_err() {
                invalid_version =
                    Some("Installed plugin version is not a regular directory.".to_owned());
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
        let preferred = active_versions.get(&id).and_then(|active| {
            choices
                .iter()
                .find(|(version, _)| version.to_string() == *active)
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
                    plugins.push(Ok(plugin))
                }
                Ok(_) => plugins.push(Err((
                    id,
                    "Manifest ID or version does not match its installation directory.".to_owned(),
                ))),
                Err(error) => plugins.push(Err((id, error))),
            }
        } else if let Some(error) = invalid_version {
            plugins.push(Err((id, error)));
        }
    }
    plugins
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
) -> Result<(), String> {
    let parent = installed_root.join(&manifest.id);
    fs::create_dir_all(&parent)
        .map_err(|error| format!("Cannot create plugin version directory: {error}"))?;
    ensure_real_directory(&parent)?;
    let destination = install_path(installed_root, manifest);
    if destination.exists() {
        if !overwrite {
            return Err(format!(
                "Plugin {} version {} is already installed.",
                manifest.id, manifest.version
            ));
        }
        ensure_real_directory(&destination)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let backup = parent.join(format!(".backup-{nonce:x}"));
        fs::rename(&destination, &backup).map_err(|error| {
            format!("Cannot stage the existing plugin for replacement: {error}")
        })?;
        if let Err(error) = fs::rename(staging, &destination) {
            let rollback = fs::rename(&backup, &destination);
            return Err(match rollback {
                Ok(()) => format!("Cannot activate replacement plugin: {error}"),
                Err(rollback_error) => format!(
                    "Cannot activate replacement plugin: {error}; restoring the original failed: {rollback_error}"
                ),
            });
        }
        let _ = fs::remove_dir_all(backup);
        return Ok(());
    }
    fs::rename(staging, &destination)
        .map_err(|error| format!("Cannot activate staged plugin package: {error}"))
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
        let raw_name = entry.name().to_owned();
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

fn verify_package_checksums(directory: &Path, allow_missing_checksums: bool) -> Result<(), String> {
    let checksums_path = directory.join("checksums.json");
    let checksums_exists = checksums_path.is_file();
    if !checksums_exists && allow_missing_checksums {
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
            && !["manifest.json", "contract.json", "checksums.json"].contains(&relative.as_str())
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
    let manifest: PluginManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("manifest.json is invalid: {error}"))?;
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
    if manifest.manifest_version != 2 {
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
    if let Some(icon) = &manifest.icon
        && (icon.is_empty()
            || icon.len() > 64
            || !icon.as_bytes()[0].is_ascii_lowercase()
            || !icon
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
    {
        return Err("Plugin icon key is invalid.".to_owned());
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
            return Err("Plugin UI entry must be ui/index.html in manifest v2.".to_owned());
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
            if contribution.icon.as_deref().is_some_and(|icon| {
                icon.is_empty()
                    || icon.len() > 64
                    || !icon.as_bytes()[0].is_ascii_lowercase()
                    || !icon.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            }) {
                return Err(format!(
                    "Plugin contribution '{}' has an invalid icon key.",
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
    } else if manifest.provides.is_empty() {
        return Err("A plugin without UI must provide at least one service.".to_owned());
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
    if manifest.capabilities.len() > 32 {
        return Err("Plugin requests too many capabilities.".to_owned());
    }
    let allowed = allowed_capabilities();
    let mut unique = BTreeSet::new();
    for capability in &manifest.capabilities {
        if !allowed.contains(capability.as_str()) || !unique.insert(capability) {
            return Err(format!(
                "Plugin requests an unknown or duplicate capability '{capability}'."
            ));
        }
    }
    if manifest.provides.len() > 32 || manifest.requires.len() > 32 {
        return Err("Plugin declares too many services.".to_owned());
    }
    let mut provided_ids = BTreeSet::new();
    for service in &manifest.provides {
        if !valid_service_id(&service.id) || !provided_ids.insert(&service.id) {
            return Err(format!(
                "Provided service ID '{}' is invalid or duplicated.",
                service.id
            ));
        }
        Version::parse(&service.version)
            .map_err(|_| format!("Provided service '{}' has an invalid version.", service.id))?;
        if service.methods.is_empty() || service.methods.len() > 128 {
            return Err(format!(
                "Provided service '{}' must export between one and 128 methods.",
                service.id
            ));
        }
        let mut methods = BTreeSet::new();
        for method in &service.methods {
            if !valid_method(method) || !methods.insert(method) {
                return Err(format!(
                    "Provided service '{}' has an invalid or duplicate method.",
                    service.id
                ));
            }
        }
    }
    let mut required_ids = BTreeSet::new();
    for requirement in &manifest.requires {
        if !valid_service_id(&requirement.id) || !required_ids.insert(&requirement.id) {
            return Err(format!(
                "Required service ID '{}' is invalid or duplicated.",
                requirement.id
            ));
        }
        let min = Version::parse(&requirement.min_version).map_err(|_| {
            format!(
                "Required service '{}' has an invalid minimum version.",
                requirement.id
            )
        })?;
        let max = Version::parse(&requirement.max_version_exclusive).map_err(|_| {
            format!(
                "Required service '{}' has an invalid maximum version.",
                requirement.id
            )
        })?;
        if min >= max {
            return Err(format!(
                "Required service '{}' has an invalid version range.",
                requirement.id
            ));
        }
        if requirement.methods.is_empty() || requirement.methods.len() > 128 {
            return Err(format!(
                "Required service '{}' must declare between one and 128 methods.",
                requirement.id
            ));
        }
        let mut methods = BTreeSet::new();
        for method in &requirement.methods {
            if !valid_method(method) || !methods.insert(method) {
                return Err(format!(
                    "Required service '{}' has an invalid or duplicate method.",
                    requirement.id
                ));
            }
        }
    }
    Ok(())
}

fn valid_service_id(value: &str) -> bool {
    value.split('.').count() >= 2
        && !value.is_empty()
        && value.len() <= 128
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

fn validate_provided_service_methods(
    manifest: &PluginManifest,
    contract: &Value,
) -> Result<(), String> {
    let methods = contract
        .get("methods")
        .and_then(Value::as_object)
        .ok_or_else(|| "Plugin contract must define a methods object.".to_owned())?;
    for service in &manifest.provides {
        for method in &service.methods {
            let Some(contract_method) = methods.get(method) else {
                return Err(format!(
                    "Provided service '{}' references missing contract method '{}'.",
                    service.id, method
                ));
            };
            if contract_method
                .get("timeoutMs")
                .and_then(Value::as_u64)
                .is_none_or(|timeout| timeout > 30_000)
            {
                return Err(format!(
                    "Provided service '{}' method '{}' exceeds the 30-second service-call limit.",
                    service.id, method
                ));
            }
        }
    }
    Ok(())
}

fn is_compatible(manifest: &PluginManifest) -> bool {
    let Ok(core) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        return false;
    };
    let Ok(protocol) = Version::parse(wonderland_plugin_protocol::PROTOCOL_VERSION) else {
        return false;
    };
    let Ok(min_core) = Version::parse(&manifest.host_compatibility.min_core_version) else {
        return false;
    };
    let Ok(max_core) = Version::parse(&manifest.host_compatibility.max_core_version_exclusive)
    else {
        return false;
    };
    let Ok(min_protocol) = Version::parse(&manifest.host_compatibility.protocol.min_version) else {
        return false;
    };
    let Ok(max_protocol) =
        Version::parse(&manifest.host_compatibility.protocol.max_version_exclusive)
    else {
        return false;
    };
    let ui_compatible = manifest.ui.as_ref().is_none_or(|ui| {
        let (Ok(ui_bridge), Ok(min_ui_bridge), Ok(max_ui_bridge)) = (
            Version::parse(UI_BRIDGE_VERSION),
            Version::parse(&ui.bridge_compatibility.min_version),
            Version::parse(&ui.bridge_compatibility.max_version_exclusive),
        ) else {
            return false;
        };
        ui_bridge >= min_ui_bridge && ui_bridge < max_ui_bridge
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
    core >= min_core
        && core < max_core
        && protocol >= min_protocol
        && protocol < max_protocol
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
            .any(|key| !["params", "result", "timeoutMs"].contains(&key.as_str()))
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
        let timeout = method
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("Method '{name}' timeoutMs must be an integer."))?;
        if !(1..=86_400_000).contains(&timeout) {
            return Err(format!("Method '{name}' timeoutMs is out of range."));
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

fn allowed_capabilities() -> BTreeSet<&'static str> {
    [
        "account.read",
        "account.authed_get",
        "network.public",
        "network.model",
        "secrets.plugin",
        "files.pick",
        "files.export",
        "files.reveal_own",
        "browser.open_official",
        "services.call",
    ]
    .into_iter()
    .collect()
}

fn allowed_package_file(path: &str) -> bool {
    matches!(path, "manifest.json" | "contract.json" | "checksums.json")
        || path.starts_with("ui/")
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

fn ensure_real_directory(path: &Path) -> Result<(), String> {
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
    fn comment_archive_provider_and_consumer_contracts_validate() {
        let cases = [
            (
                include_str!("../tests/fixtures/comment_collector_manifest.json"),
                include_str!("../tests/fixtures/comment_collector_contract.json"),
            ),
            (
                include_str!("../tests/fixtures/knowledge_library_manifest.json"),
                include_str!("../tests/fixtures/knowledge_library_contract.json"),
            ),
        ];

        for (manifest_json, contract_json) in cases {
            let manifest: PluginManifest = serde_json::from_str(manifest_json).unwrap();
            let contract: Value = serde_json::from_str(contract_json).unwrap();
            validate_manifest(&manifest).unwrap();
            validate_contract(&contract).unwrap();
            validate_provided_service_methods(&manifest, &contract).unwrap();
        }
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
