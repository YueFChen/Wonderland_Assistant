//! 用户数据目录选择、迁移预约和启动期目录迁移。

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wonderland_kernel::{KernelError, UserDataLocationKind, UserDataService, UserDataState};

const CONTROL_FILE: &str = ".user-data-location.json";
const MIGRATION_MARKER_FILE: &str = ".wonderland-migration-in-progress.json";
const PROBE_FILE: &str = ".wonderland-write-probe";

#[derive(Default, Serialize, Deserialize)]
struct ControlFile {
    active_path: Option<String>,
    pending_path: Option<String>,
    last_migration_path: Option<String>,
    #[serde(default)]
    last_migration_source: Option<String>,
    #[serde(default)]
    last_migration_warning: Option<String>,
    last_migration_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct MigrationMarker {
    schema_version: u32,
    source: String,
    target: String,
}

#[derive(Debug, PartialEq, Eq)]
enum TreeEntry {
    Directory,
    File { size: u64, sha256: String },
}

pub struct FsUserDataService {
    default_path: PathBuf,
    state: RwLock<UserDataState>,
}

impl FsUserDataService {
    /// 解析活动目录，并执行上次预约的迁移。必须早于其它持久化服务调用。
    pub fn bootstrap(default_path: PathBuf) -> Result<(Self, PathBuf), KernelError> {
        fs::create_dir_all(&default_path).map_err(storage)?;
        let control_path = default_path.join(CONTROL_FILE);
        let mut control = read_control(&control_path)?;
        cleanup_control_temps(&default_path);
        let mut control_dirty = false;
        let mut active = control
            .active_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| default_path.clone());

        if !active.is_dir() {
            control.last_migration_error = Some(format!(
                "已配置的数据目录不存在：{}；本次已回退到默认目录",
                active.display()
            ));
            active = default_path.clone();
            control.active_path = None;
            control.last_migration_path = None;
            control.last_migration_source = None;
            control.last_migration_warning = None;
            control_dirty = true;
        }

        cleanup_committed_marker(&active);

        if let Some(pending) = control.pending_path.take() {
            let target = validate_stored_path(&pending);
            let target = match target {
                Ok(target) => target,
                Err(error) => {
                    control.last_migration_error = Some(error.to_string());
                    control.last_migration_path = None;
                    write_control(&control_path, &control)?;
                    let state = make_state(&default_path, &active, &control);
                    return Ok((
                        Self {
                            default_path,
                            state: RwLock::new(state),
                        },
                        active,
                    ));
                }
            };
            match migrate(&active, &target, &default_path) {
                Ok(created) => {
                    let previous = active.clone();
                    control.active_path = if same_path(&target, &default_path) {
                        None
                    } else {
                        Some(path_text(&target))
                    };
                    control.last_migration_error = None;
                    control.last_migration_path = Some(path_text(&target));
                    control.last_migration_source = None;
                    control.last_migration_warning = None;
                    if let Err(error) = write_control(&control_path, &control) {
                        if cleanup_created(&created).is_ok() {
                            let _ = fs::remove_file(target.join(MIGRATION_MARKER_FILE));
                        }
                        return Err(error);
                    }
                    let _ = fs::remove_file(target.join(MIGRATION_MARKER_FILE));
                    active = target;
                    if let Err(error) =
                        remove_tree_contents(&previous, same_path(&previous, &default_path))
                    {
                        control.last_migration_source = Some(path_text(&previous));
                        control.last_migration_warning = Some(format!(
                            "迁移已完成并切换到新目录，但旧目录清理未完成：{error}"
                        ));
                        control_dirty = true;
                    }
                }
                Err(error) => {
                    control.last_migration_error = Some(error.to_string());
                    control.last_migration_path = None;
                    control.last_migration_source = None;
                    control.last_migration_warning = None;
                    control_dirty = true;
                }
            }
        }

        if control_dirty {
            // A failed best-effort cleanup must not stop startup after the active path has
            // already been committed. The committed data remains usable even if this warning
            // cannot be persisted.
            let _ = write_control(&control_path, &control);
        }
        fs::create_dir_all(&active).map_err(storage)?;
        let state = make_state(&default_path, &active, &control);
        Ok((
            Self {
                default_path,
                state: RwLock::new(state),
            },
            active,
        ))
    }

    fn update_control(
        &self,
        edit: impl FnOnce(&mut ControlFile) -> Result<(), KernelError>,
    ) -> Result<UserDataState, KernelError> {
        let control_path = self.default_path.join(CONTROL_FILE);
        let mut control = read_control(&control_path)?;
        edit(&mut control)?;
        write_control(&control_path, &control)?;
        let mut state = self.state.write().map_err(|_| poisoned())?;
        state.pending_path = control.pending_path;
        state.last_migration_path = control.last_migration_path;
        state.last_migration_error = control.last_migration_error;
        Ok(state.clone())
    }
}

impl UserDataService for FsUserDataService {
    fn state(&self) -> UserDataState {
        self.state
            .read()
            .map(|value| value.clone())
            .unwrap_or_else(|_| {
                make_state(
                    &self.default_path,
                    &self.default_path,
                    &ControlFile::default(),
                )
            })
    }

    fn schedule_migration(&self, custom_path: Option<&str>) -> Result<UserDataState, KernelError> {
        let current = self.state();
        let target = match custom_path {
            Some(raw) => validate_custom_path(raw)?,
            None => self.default_path.clone(),
        };
        let active = PathBuf::from(&current.active_path);
        if same_path(&target, &active) {
            return self.cancel_migration();
        }
        reject_nested_lexically(&active, &target)?;
        prepare_target(&target, &self.default_path)?;
        reject_nested(&active, &target)?;

        self.update_control(|control| {
            control.pending_path = Some(path_text(&target));
            control.last_migration_error = None;
            control.last_migration_path = None;
            Ok(())
        })
    }

    fn cancel_migration(&self) -> Result<UserDataState, KernelError> {
        self.update_control(|control| {
            control.pending_path = None;
            Ok(())
        })
    }
}

fn make_state(default_path: &Path, active: &Path, control: &ControlFile) -> UserDataState {
    UserDataState {
        active_path: path_text(active),
        default_path: path_text(default_path),
        location: if same_path(default_path, active) {
            UserDataLocationKind::Default
        } else {
            UserDataLocationKind::Custom
        },
        pending_path: control.pending_path.clone(),
        last_migration_path: control.last_migration_path.clone(),
        last_migration_source: control.last_migration_source.clone(),
        last_migration_warning: control.last_migration_warning.clone(),
        last_migration_error: control.last_migration_error.clone(),
    }
}

fn validate_custom_path(raw: &str) -> Result<PathBuf, KernelError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(KernelError::InvalidInput);
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(KernelError::Other(
            "自定义数据目录必须是完整的绝对路径".to_owned(),
        ));
    }
    Ok(path)
}

fn reject_nested_lexically(source: &Path, target: &Path) -> Result<(), KernelError> {
    let source = normalized(source);
    let target = normalized(target);
    if source.starts_with(&target) || target.starts_with(&source) {
        return Err(KernelError::Other("新旧数据目录不能互相包含".to_owned()));
    }
    Ok(())
}

fn reject_nested(source: &Path, target: &Path) -> Result<(), KernelError> {
    if is_reparse_path(source)? || is_reparse_path(target)? {
        return Err(KernelError::Other(
            "源目录和目标目录不能是符号链接、目录联接或其他重解析点".to_owned(),
        ));
    }
    let source = source.canonicalize().map_err(storage)?;
    let target = target.canonicalize().map_err(storage)?;
    if source.starts_with(&target) || target.starts_with(&source) {
        return Err(KernelError::Other("新旧数据目录不能互相包含".to_owned()));
    }
    Ok(())
}

fn prepare_target(target: &Path, default_path: &Path) -> Result<(), KernelError> {
    fs::create_dir_all(target).map_err(storage)?;
    if is_reparse_path(target)? {
        return Err(KernelError::Other(
            "目标目录不能是符号链接、目录联接或其他重解析点".to_owned(),
        ));
    }
    for entry in fs::read_dir(target).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let allowed_control =
            same_path(target, default_path) && entry.file_name().to_string_lossy() == CONTROL_FILE;
        if !allowed_control {
            return Err(KernelError::Other("目标目录必须为空".to_owned()));
        }
    }
    let probe = target.join(PROBE_FILE);
    if let Err(error) = fs::write(&probe, b"probe") {
        let _ = fs::remove_file(&probe);
        return Err(storage(error));
    }
    fs::remove_file(probe).map_err(storage)
}

fn migrate(source: &Path, target: &Path, default_path: &Path) -> Result<Vec<PathBuf>, KernelError> {
    migrate_with(
        source,
        target,
        default_path,
        available_space,
        copy_tree_recording,
    )
}

fn migrate_with(
    source: &Path,
    target: &Path,
    default_path: &Path,
    available_space: impl Fn(&Path) -> Result<Option<u64>, KernelError>,
    copy_tree: impl Fn(&Path, &Path, bool, &mut Vec<PathBuf>) -> Result<(), KernelError>,
) -> Result<Vec<PathBuf>, KernelError> {
    reject_nested(source, target)?;
    let required = tree_size(source, same_path(source, default_path))?;
    if let Some(available) = available_space(target)?
        && available < required
    {
        return Err(KernelError::Other(format!(
            "目标磁盘空间不足：数据约需 {required} 字节，可用空间为 {available} 字节。原数据未更改。"
        )));
    }
    prepare_migration_target(source, target, default_path)?;
    write_migration_marker(source, target)?;

    let mut created = Vec::new();
    let copy_result = copy_tree(
        source,
        target,
        same_path(source, default_path),
        &mut created,
    )
    .and_then(|()| verify_copied_tree(source, target, default_path));
    if let Err(error) = copy_result {
        let cleanup_error = cleanup_created(&created).err();
        if cleanup_error.is_none() {
            let _ = fs::remove_file(target.join(MIGRATION_MARKER_FILE));
        }
        return match cleanup_error {
            Some(cleanup_error) => Err(KernelError::Other(format!(
                "{error}；部分副本清理失敗，临时标记保留在 {}：{cleanup_error}",
                target.display()
            ))),
            None => Err(error),
        };
    }
    Ok(created)
}

fn prepare_migration_target(
    source: &Path,
    target: &Path,
    default_path: &Path,
) -> Result<(), KernelError> {
    let marker_path = target.join(MIGRATION_MARKER_FILE);
    if marker_path.exists() {
        let marker = read_migration_marker(&marker_path).ok();
        if marker.is_some_and(|marker| {
            marker.schema_version == 1
                && same_path(Path::new(&marker.source), source)
                && same_path(Path::new(&marker.target), target)
        }) {
            return Err(KernelError::Other(format!(
                "目标目录包含上次中断的迁移副本：{}。原数据仍在使用；请检查并清空该目录，或选择新的空目录后重试。",
                target.display()
            )));
        }
        return Err(KernelError::Other(format!(
            "目标目录包含无法识别的迁移标记：{}。为保护其中的数据，Core 不会自动清理。",
            marker_path.display()
        )));
    }
    prepare_target(target, default_path)
}

fn write_migration_marker(source: &Path, target: &Path) -> Result<(), KernelError> {
    let marker = MigrationMarker {
        schema_version: 1,
        source: path_text(source),
        target: path_text(target),
    };
    let bytes = serde_json::to_vec(&marker).map_err(|_| KernelError::InvalidResponse)?;
    let path = target.join(MIGRATION_MARKER_FILE);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(storage)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(path);
        return Err(storage(error));
    }
    Ok(())
}

fn read_migration_marker(path: &Path) -> Result<MigrationMarker, KernelError> {
    let bytes = fs::read(path).map_err(storage)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| KernelError::Other(format!("迁移标记无法解析：{error}")))
}

fn cleanup_committed_marker(active: &Path) {
    let path = active.join(MIGRATION_MARKER_FILE);
    let Ok(marker) = read_migration_marker(&path) else {
        return;
    };
    if marker.schema_version == 1 && same_path(Path::new(&marker.target), active) {
        let _ = fs::remove_file(path);
    }
}

fn tree_size(root: &Path, skip_control: bool) -> Result<u64, KernelError> {
    let (entries, size) = scan_tree(root, skip_control, false, false)?;
    let _ = entries;
    Ok(size)
}

fn scan_tree(
    root: &Path,
    skip_control: bool,
    skip_marker: bool,
    with_hashes: bool,
) -> Result<(BTreeMap<PathBuf, TreeEntry>, u64), KernelError> {
    fn visit(
        root: &Path,
        current: &Path,
        skip_control: bool,
        skip_marker: bool,
        with_hashes: bool,
        entries: &mut BTreeMap<PathBuf, TreeEntry>,
        size: &mut u64,
    ) -> Result<(), KernelError> {
        for entry in fs::read_dir(current).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(storage)?
                .to_path_buf();
            let name = entry.file_name();
            if name.to_string_lossy() == CONTROL_FILE {
                if skip_control {
                    if relative.components().count() == 1 {
                        continue;
                    }
                } else {
                    return Err(KernelError::Other(format!(
                        "数据目录使用了 Core 保留名称 {CONTROL_FILE}"
                    )));
                }
            }
            if name.to_string_lossy() == MIGRATION_MARKER_FILE {
                if skip_marker && relative.components().count() == 1 {
                    continue;
                }
                if !skip_marker {
                    return Err(KernelError::Other(format!(
                        "数据目录使用了 Core 保留名称 {MIGRATION_MARKER_FILE}"
                    )));
                }
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(storage)?;
            if is_reparse_metadata(&metadata) {
                return Err(KernelError::Other(format!(
                    "数据目录包含不支持迁移的符号链接或重解析点：{}",
                    entry.path().display()
                )));
            }
            if metadata.is_dir() {
                entries.insert(relative.clone(), TreeEntry::Directory);
                visit(
                    root,
                    &entry.path(),
                    false,
                    false,
                    with_hashes,
                    entries,
                    size,
                )?;
            } else if metadata.is_file() {
                *size = size.checked_add(metadata.len()).ok_or_else(|| {
                    KernelError::Other("用户数据目录过大，无法计算迁移空间".to_owned())
                })?;
                let sha256 = if with_hashes {
                    file_sha256(&entry.path())?
                } else {
                    String::new()
                };
                entries.insert(
                    relative,
                    TreeEntry::File {
                        size: metadata.len(),
                        sha256,
                    },
                );
            } else {
                return Err(KernelError::Other(format!(
                    "数据目录包含不支持迁移的特殊文件：{}",
                    entry.path().display()
                )));
            }
        }
        Ok(())
    }

    let mut entries = BTreeMap::new();
    let mut size = 0;
    visit(
        root,
        root,
        skip_control,
        skip_marker,
        with_hashes,
        &mut entries,
        &mut size,
    )?;
    Ok((entries, size))
}

fn copy_tree_recording(
    source: &Path,
    target: &Path,
    skip_control: bool,
    created: &mut Vec<PathBuf>,
) -> Result<(), KernelError> {
    for entry in fs::read_dir(source).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        if entry.file_name().to_string_lossy() == CONTROL_FILE {
            if skip_control {
                continue;
            }
            return Err(KernelError::Other(format!(
                "数据目录使用了 Core 保留名称 {CONTROL_FILE}"
            )));
        }
        if entry.file_name().to_string_lossy() == MIGRATION_MARKER_FILE {
            return Err(KernelError::Other(format!(
                "数据目录使用了 Core 保留名称 {MIGRATION_MARKER_FILE}"
            )));
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(storage)?;
        if is_reparse_metadata(&metadata) {
            return Err(KernelError::Other(format!(
                "数据目录包含不支持迁移的符号链接或重解析点：{}",
                entry.path().display()
            )));
        }
        let destination = target.join(entry.file_name());
        if metadata.is_dir() {
            fs::create_dir(&destination).map_err(storage)?;
            created.push(destination.clone());
            copy_tree_recording(&entry.path(), &destination, false, created)?;
            fs::set_permissions(&destination, metadata.permissions()).map_err(storage)?;
        } else if metadata.is_file() {
            copy_file_verified(&entry.path(), &destination, created)?;
        } else {
            return Err(KernelError::Other(format!(
                "数据目录包含不支持迁移的特殊文件：{}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn copy_file_verified(
    source: &Path,
    target: &Path,
    created: &mut Vec<PathBuf>,
) -> Result<(), KernelError> {
    let metadata = fs::metadata(source).map_err(storage)?;
    let mut input = File::open(source).map_err(storage)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)
        .map_err(storage)?;
    created.push(target.to_path_buf());
    let mut hasher = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(storage)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).map_err(storage)?;
        hasher.update(&buffer[..count]);
        copied = copied
            .checked_add(count as u64)
            .ok_or_else(|| KernelError::Other("迁移文件过大，无法校验".to_owned()))?;
    }
    output.sync_all().map_err(storage)?;
    fs::set_permissions(target, metadata.permissions()).map_err(storage)?;
    if copied != metadata.len() {
        return Err(KernelError::Other(format!(
            "迁移期间文件大小发生变化：{}",
            source.display()
        )));
    }
    let source_hash = format!("{:x}", hasher.finalize());
    let target_hash = file_sha256(target)?;
    if source_hash != target_hash {
        return Err(KernelError::Other(format!(
            "迁移校验失败：{}",
            source.display()
        )));
    }
    Ok(())
}

fn verify_copied_tree(
    source: &Path,
    target: &Path,
    default_path: &Path,
) -> Result<(), KernelError> {
    let (source_entries, _) = scan_tree(source, same_path(source, default_path), false, true)?;
    let (target_entries, _) = scan_tree(target, same_path(target, default_path), true, true)?;
    if source_entries != target_entries {
        return Err(KernelError::Other(
            "迁移校验失败：源目录与目标目录的文件清单不一致。".to_owned(),
        ));
    }
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, KernelError> {
    let mut file = File::open(path).map_err(storage)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(storage)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn cleanup_created(paths: &[PathBuf]) -> Result<(), KernelError> {
    for path in paths.iter().rev() {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(storage(error)),
        };
        if is_reparse_metadata(&metadata) {
            remove_symlink(path)?;
        } else if metadata.is_dir() {
            fs::remove_dir(path).map_err(storage)?;
        } else {
            fs::remove_file(path).map_err(storage)?;
        }
    }
    Ok(())
}

fn remove_tree_contents(root: &Path, keep_control: bool) -> Result<(), KernelError> {
    for entry in fs::read_dir(root).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        if keep_control && entry.file_name().to_string_lossy() == CONTROL_FILE {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(storage)?;
        if is_reparse_metadata(&metadata) {
            remove_symlink(&path)?;
        } else if metadata.is_dir() {
            fs::remove_dir_all(&path).map_err(storage)?;
        } else {
            fs::remove_file(&path).map_err(storage)?;
        }
    }
    if !keep_control {
        fs::remove_dir(root).map_err(storage)?;
    }
    Ok(())
}

#[cfg(windows)]
fn remove_symlink(path: &Path) -> Result<(), KernelError> {
    if path.is_dir() {
        fs::remove_dir(path).map_err(storage)
    } else {
        fs::remove_file(path).map_err(storage)
    }
}

#[cfg(not(windows))]
fn remove_symlink(path: &Path) -> Result<(), KernelError> {
    fs::remove_file(path).map_err(storage)
}

fn is_reparse_path(path: &Path) -> Result<bool, KernelError> {
    let metadata = fs::symlink_metadata(path).map_err(storage)?;
    Ok(is_reparse_metadata(&metadata))
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

fn read_control(path: &Path) -> Result<ControlFile, KernelError> {
    let backup = control_backup_path(path);
    let (bytes, recovered) = match fs::read(path) {
        Ok(bytes) => (bytes, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match fs::read(&backup) {
            Ok(bytes) => (bytes, true),
            Err(backup_error) if backup_error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ControlFile::default());
            }
            Err(backup_error) => return Err(storage(backup_error)),
        },
        Err(error) => return Err(storage(error)),
    };
    let control = serde_json::from_slice(&bytes).map_err(|error| {
        KernelError::Settings(format!(
            "用户数据位置记录损坏，原数据目录未更改（{}）：{error}",
            path.display()
        ))
    })?;
    if recovered {
        fs::rename(&backup, path).map_err(storage)?;
    } else if backup.exists() {
        let _ = fs::remove_file(backup);
    }
    Ok(control)
}

fn write_control(path: &Path, control: &ControlFile) -> Result<(), KernelError> {
    let bytes = serde_json::to_vec_pretty(control).map_err(|_| KernelError::InvalidResponse)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), nonce));
    let backup = control_backup_path(path);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(storage)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(storage(error));
    }
    drop(file);

    if path.exists() {
        if backup.exists()
            && let Err(error) = fs::remove_file(&backup)
        {
            let _ = fs::remove_file(&temp);
            return Err(storage(error));
        }
        if let Err(error) = fs::rename(path, &backup) {
            let _ = fs::remove_file(&temp);
            return Err(storage(error));
        }
    }
    if let Err(error) = fs::rename(&temp, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        let _ = fs::remove_file(&temp);
        return Err(storage(error));
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn control_backup_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{name}.bak"))
}

fn cleanup_control_temps(default_path: &Path) {
    let prefix = format!(".{CONTROL_FILE}.");
    let Ok(entries) = fs::read_dir(default_path) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".tmp") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn available_space(path: &Path) -> Result<Option<u64>, KernelError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let path = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut available = 0u64;
        let result = unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            return Err(storage(std::io::Error::last_os_error()));
        }
        Ok(Some(available))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(None)
    }
}

fn validate_stored_path(raw: &str) -> Result<PathBuf, KernelError> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(KernelError::Other(
            "迁移记录中的目标目录不是绝对路径；原数据目录未更改".to_owned(),
        ))
    }
}

#[cfg(windows)]
fn normalized(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase())
}

#[cfg(not(windows))]
fn normalized(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn same_path(left: &Path, right: &Path) -> bool {
    normalized(left) == normalized(right)
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn storage(error: impl std::fmt::Display) -> KernelError {
    KernelError::Settings(format!("用户数据目录操作失败：{error}"))
}

fn poisoned() -> KernelError {
    KernelError::Settings("用户数据目录状态已损坏".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "wonderland-user-data-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    #[test]
    fn migrates_to_custom_and_back_on_bootstrap() {
        let default = temp("default");
        let custom = temp("custom");
        let external = temp("external-project");
        fs::create_dir_all(default.join("plugin-data/rich_editor")).unwrap();
        fs::create_dir_all(default.join("plugin-data/translator")).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("user-project.txt"), b"external project").unwrap();
        fs::write(default.join("settings.json"), b"settings").unwrap();
        fs::write(
            default.join("plugin-data/rich_editor/document.json"),
            b"rich editor data",
        )
        .unwrap();
        fs::write(
            default.join("plugin-data/translator/history.sqlite"),
            b"translator data",
        )
        .unwrap();

        let (service, active) = FsUserDataService::bootstrap(default.clone()).unwrap();
        assert_eq!(active, default);
        service
            .schedule_migration(Some(custom.to_str().unwrap()))
            .unwrap();

        let (service, active) = FsUserDataService::bootstrap(default.clone()).unwrap();
        assert_eq!(active, custom);
        assert_eq!(
            service.state().last_migration_path.as_deref(),
            custom.to_str()
        );
        assert_eq!(service.state().last_migration_source, None);
        assert_eq!(service.state().last_migration_warning, None);
        assert_eq!(fs::read(custom.join("settings.json")).unwrap(), b"settings");
        assert_eq!(
            fs::read(custom.join("plugin-data/rich_editor/document.json")).unwrap(),
            b"rich editor data"
        );
        assert_eq!(
            fs::read(custom.join("plugin-data/translator/history.sqlite")).unwrap(),
            b"translator data"
        );
        assert_eq!(
            fs::read(external.join("user-project.txt")).unwrap(),
            b"external project"
        );
        assert!(!default.join("settings.json").exists());

        service.schedule_migration(None).unwrap();
        let (service, active) = FsUserDataService::bootstrap(default.clone()).unwrap();
        assert_eq!(active, default);
        assert_eq!(
            service.state().last_migration_path.as_deref(),
            default.to_str()
        );
        assert_eq!(
            fs::read(default.join("plugin-data/rich_editor/document.json")).unwrap(),
            b"rich editor data"
        );
        assert_eq!(
            fs::read(external.join("user-project.txt")).unwrap(),
            b"external project"
        );
        assert!(!custom.exists());
        fs::remove_dir_all(default).unwrap();
        fs::remove_dir_all(external).unwrap();
    }

    #[test]
    fn rejects_non_empty_and_nested_targets() {
        let default = temp("validation");
        fs::create_dir_all(&default).unwrap();
        let (service, _) = FsUserDataService::bootstrap(default.clone()).unwrap();
        fs::create_dir_all(default.join("nested")).unwrap();
        assert!(
            service
                .schedule_migration(Some(default.join("nested").to_str().unwrap()))
                .is_err()
        );

        let occupied = temp("occupied");
        fs::create_dir_all(&occupied).unwrap();
        fs::write(occupied.join("keep.txt"), b"x").unwrap();
        assert!(
            service
                .schedule_migration(Some(occupied.to_str().unwrap()))
                .is_err()
        );
        fs::remove_dir_all(default).unwrap();
        fs::remove_dir_all(occupied).unwrap();
    }

    #[test]
    fn insufficient_space_does_not_write_a_marker_or_change_the_source() {
        let default = temp("space-default");
        let custom = temp("space-target");
        fs::create_dir_all(&default).unwrap();
        fs::write(default.join("settings.json"), b"settings").unwrap();
        prepare_target(&custom, &default).unwrap();

        let result = migrate_with(
            &default,
            &custom,
            &default,
            |_| Ok(Some(0)),
            copy_tree_recording,
        );

        assert!(result.unwrap_err().to_string().contains("空间不足"));
        assert_eq!(
            fs::read(default.join("settings.json")).unwrap(),
            b"settings"
        );
        assert_eq!(fs::read_dir(&custom).unwrap().count(), 0);
        fs::remove_dir_all(default).unwrap();
        fs::remove_dir_all(custom).unwrap();
    }

    #[test]
    fn failed_copy_removes_only_its_partial_files_and_keeps_the_source() {
        let default = temp("copy-default");
        let custom = temp("copy-target");
        fs::create_dir_all(default.join("plugin-data")).unwrap();
        fs::write(default.join("plugin-data/state.json"), b"user data").unwrap();
        prepare_target(&custom, &default).unwrap();

        let result = migrate_with(
            &default,
            &custom,
            &default,
            |_| Ok(Some(u64::MAX)),
            |source, target, skip_control, created| {
                copy_tree_recording(source, target, skip_control, created)?;
                let partial = target.join("interrupted-copy.tmp");
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&partial)
                    .map_err(storage)?;
                created.push(partial);
                file.write_all(b"partial").map_err(storage)?;
                Err(KernelError::Other("simulated copy failure".to_owned()))
            },
        );

        assert!(result.is_err());
        assert_eq!(
            fs::read(default.join("plugin-data/state.json")).unwrap(),
            b"user data"
        );
        assert_eq!(fs::read_dir(&custom).unwrap().count(), 0);
        fs::remove_dir_all(default).unwrap();
        fs::remove_dir_all(custom).unwrap();
    }

    #[test]
    fn control_update_recovers_previous_file_if_interrupted_mid_replace() {
        let default = temp("control-recovery");
        fs::create_dir_all(&default).unwrap();
        let control_path = default.join(CONTROL_FILE);
        let control = ControlFile {
            active_path: Some(default.join("custom").to_string_lossy().into_owned()),
            ..ControlFile::default()
        };
        write_control(&control_path, &control).unwrap();
        fs::rename(&control_path, control_backup_path(&control_path)).unwrap();

        let recovered = read_control(&control_path).unwrap();
        assert_eq!(recovered.active_path, control.active_path);
        assert!(control_path.is_file());
        assert!(!control_backup_path(&control_path).exists());
        fs::remove_dir_all(default).unwrap();
    }

    #[test]
    fn corrupt_control_fails_closed_without_replacing_user_data() {
        let default = temp("corrupt-control");
        let external = temp("corrupt-control-data");
        fs::create_dir_all(&default).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(default.join(CONTROL_FILE), b"not valid json").unwrap();
        fs::write(external.join("settings.json"), b"keep me").unwrap();

        assert!(FsUserDataService::bootstrap(default.clone()).is_err());
        assert_eq!(
            fs::read(external.join("settings.json")).unwrap(),
            b"keep me"
        );
        assert_eq!(
            fs::read(default.join(CONTROL_FILE)).unwrap(),
            b"not valid json"
        );
        fs::remove_dir_all(default).unwrap();
        fs::remove_dir_all(external).unwrap();
    }

    #[test]
    fn interrupted_migration_keeps_source_and_does_not_overwrite_target_contents() {
        let default = temp("interrupted-default");
        let target = temp("interrupted-target");
        fs::create_dir_all(&default).unwrap();
        fs::write(default.join("settings.json"), b"source data").unwrap();
        let (service, _) = FsUserDataService::bootstrap(default.clone()).unwrap();
        service
            .schedule_migration(Some(target.to_str().unwrap()))
            .unwrap();
        write_migration_marker(&default, &target).unwrap();
        fs::write(target.join("partial-copy.json"), b"partial data").unwrap();

        let (service, active) = FsUserDataService::bootstrap(default.clone()).unwrap();

        assert_eq!(active, default);
        assert!(
            service
                .state()
                .last_migration_error
                .as_deref()
                .unwrap()
                .contains("中断")
        );
        assert_eq!(
            fs::read(default.join("settings.json")).unwrap(),
            b"source data"
        );
        assert_eq!(
            fs::read(target.join("partial-copy.json")).unwrap(),
            b"partial data"
        );
        fs::remove_dir_all(default).unwrap();
        fs::remove_dir_all(target).unwrap();
    }
}
