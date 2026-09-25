//! 用户数据目录选择、迁移预约和启动期目录迁移。

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use wonderland_kernel::{KernelError, UserDataLocationKind, UserDataService, UserDataState};

const CONTROL_FILE: &str = ".user-data-location.json";
const PROBE_FILE: &str = ".wonderland-write-probe";

#[derive(Default, Serialize, Deserialize)]
struct ControlFile {
    active_path: Option<String>,
    pending_path: Option<String>,
    last_migration_path: Option<String>,
    last_migration_error: Option<String>,
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
        let mut control = read_control(&control_path);
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
        }

        if let Some(pending) = control.pending_path.take() {
            let target = PathBuf::from(pending);
            match migrate(&active, &target, &default_path) {
                Ok(()) => {
                    control.active_path = if same_path(&target, &default_path) {
                        None
                    } else {
                        Some(path_text(&target))
                    };
                    control.last_migration_error = None;
                    control.last_migration_path = Some(path_text(&target));
                    if let Err(error) = write_control(&control_path, &control) {
                        remove_tree_contents(&target, same_path(&target, &default_path));
                        return Err(error);
                    }
                    remove_tree_contents(&active, same_path(&active, &default_path));
                    active = target;
                }
                Err(error) => {
                    control.last_migration_error = Some(error.to_string());
                    control.last_migration_path = None;
                }
            }
        }

        write_control(&control_path, &control)?;
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
        let mut control = read_control(&control_path);
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
        reject_nested(&active, &target)?;
        prepare_target(&target, &self.default_path)?;

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

fn reject_nested(source: &Path, target: &Path) -> Result<(), KernelError> {
    let source = normalized(source);
    let target = normalized(target);
    if source.starts_with(&target) || target.starts_with(&source) {
        return Err(KernelError::Other("新旧数据目录不能互相包含".to_owned()));
    }
    Ok(())
}

fn prepare_target(target: &Path, default_path: &Path) -> Result<(), KernelError> {
    fs::create_dir_all(target).map_err(storage)?;
    for entry in fs::read_dir(target).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let allowed_control =
            same_path(target, default_path) && entry.file_name().to_string_lossy() == CONTROL_FILE;
        if !allowed_control {
            return Err(KernelError::Other("目标目录必须为空".to_owned()));
        }
    }
    let probe = target.join(PROBE_FILE);
    fs::write(&probe, b"probe").map_err(storage)?;
    fs::remove_file(probe).map_err(storage)
}

fn migrate(source: &Path, target: &Path, default_path: &Path) -> Result<(), KernelError> {
    reject_nested(source, target)?;
    prepare_target(target, default_path)?;
    if let Err(error) = copy_tree(source, target, same_path(source, default_path)) {
        remove_tree_contents(target, same_path(target, default_path));
        return Err(error);
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path, skip_control: bool) -> Result<(), KernelError> {
    for entry in fs::read_dir(source).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        if skip_control && entry.file_name().to_string_lossy() == CONTROL_FILE {
            continue;
        }
        let kind = entry.file_type().map_err(storage)?;
        if kind.is_symlink() {
            return Err(KernelError::Other(
                "数据目录中含有不支持迁移的符号链接".to_owned(),
            ));
        }
        let destination = target.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir_all(&destination).map_err(storage)?;
            copy_tree(&entry.path(), &destination, false)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), destination).map_err(storage)?;
        }
    }
    Ok(())
}

fn remove_tree_contents(root: &Path, keep_control: bool) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if keep_control && entry.file_name().to_string_lossy() == CONTROL_FILE {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = fs::remove_dir_all(path);
        } else {
            let _ = fs::remove_file(path);
        }
    }
    if !keep_control {
        let _ = fs::remove_dir(root);
    }
}

fn read_control(path: &Path) -> ControlFile {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_control(path: &Path, control: &ControlFile) -> Result<(), KernelError> {
    let bytes = serde_json::to_vec_pretty(control).map_err(|_| KernelError::InvalidResponse)?;
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes).map_err(storage)?;
    if path.exists() {
        fs::remove_file(path).map_err(storage)?;
    }
    fs::rename(temp, path).map_err(storage)
}

fn normalized(path: &Path) -> PathBuf {
    PathBuf::from(path_text(path).to_lowercase())
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
        fs::create_dir_all(default.join("plugins")).unwrap();
        fs::write(default.join("settings.json"), b"settings").unwrap();
        fs::write(default.join("plugins/data.json"), b"plugin").unwrap();

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
        assert_eq!(fs::read(custom.join("settings.json")).unwrap(), b"settings");
        assert_eq!(
            fs::read(custom.join("plugins/data.json")).unwrap(),
            b"plugin"
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
            fs::read(default.join("plugins/data.json")).unwrap(),
            b"plugin"
        );
        assert!(!custom.exists());
        fs::remove_dir_all(default).unwrap();
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
}
