use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use wonderland_kernel::{AppContext, KernelError};

use crate::commands::{ensure_main_window, ipc, ipc_async};

const CONFIG_FILE: &str = "game-launcher.json";

#[derive(Debug, Serialize, Deserialize)]
struct LauncherConfig {
    path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameLauncherSnapshot {
    path: Option<String>,
    available: bool,
    supported: bool,
    kind: Option<&'static str>,
    source: Option<&'static str>,
}

#[tauri::command]
pub fn game_launcher_get(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<GameLauncherSnapshot, KernelError> {
    ensure_main_window(&window)?;
    ipc("game_launcher_get", || snapshot(&context.app_data_dir))
}

#[tauri::command]
pub async fn game_launcher_select(
    window: WebviewWindow,
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<GameLauncherSnapshot, KernelError> {
    ensure_main_window(&window)?;
    let app_data_dir = context.app_data_dir.clone();
    let picker = app
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("选择 HoYoPlay、原神启动器或游戏本体");
    #[cfg(target_os = "windows")]
    let picker = picker.add_filter("原神可执行文件", &["exe"]);
    ipc_async("game_launcher_select", async move {
        let selected = tauri::async_runtime::spawn_blocking(move || picker.blocking_pick_file())
            .await
            .map_err(|error| KernelError::Other(format!("游戏选择器异常退出：{error}")))?;
        let Some(selected) = selected else {
            return snapshot(&app_data_dir);
        };
        let path = selected
            .into_path()
            .map_err(|_| KernelError::InvalidInput)?;
        validate_launcher(&path)?;
        save_launcher(&app_data_dir, &path)?;
        snapshot(&app_data_dir)
    })
    .await
}

#[tauri::command]
pub fn game_launcher_launch(
    window: WebviewWindow,
    context: State<'_, AppContext>,
) -> Result<(), KernelError> {
    ensure_main_window(&window)?;
    ipc("game_launcher_launch", || {
        let path = resolve_launcher(&context.app_data_dir)?
            .map(|(path, _)| path)
            .ok_or(KernelError::InvalidInput)?;
        validate_launcher(&path)?;
        let working_directory = path.parent().unwrap_or_else(|| Path::new("."));
        Command::new(&path)
            .current_dir(working_directory)
            .spawn()
            .map(|_| ())
            .map_err(|error| KernelError::Other(format!("无法启动原神：{error}")))
    })
}

fn snapshot(app_data_dir: &Path) -> Result<GameLauncherSnapshot, KernelError> {
    let selected = resolve_launcher(app_data_dir)?;
    let kind = selected
        .as_ref()
        .and_then(|(path, _)| executable_kind(path));
    let source = selected.as_ref().map(|(_, source)| *source);
    Ok(GameLauncherSnapshot {
        path: selected.map(|(path, _)| path.to_string_lossy().into_owned()),
        available: kind.is_some(),
        supported: cfg!(target_os = "windows"),
        kind,
        source,
    })
}

fn resolve_launcher(app_data_dir: &Path) -> Result<Option<(PathBuf, &'static str)>, KernelError> {
    if let Some(config) = read_launcher(app_data_dir)? {
        let path = PathBuf::from(config.path);
        if validate_launcher(&path).is_ok() {
            return Ok(Some((path, "manual")));
        }
    }
    Ok(discover_from_registry().map(|path| (path, "registry")))
}

fn read_launcher(app_data_dir: &Path) -> Result<Option<LauncherConfig>, KernelError> {
    match fs::read(app_data_dir.join(CONFIG_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| KernelError::LocalData(format!("游戏入口配置无法读取：{error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(KernelError::LocalData(format!(
            "游戏入口配置无法读取：{error}"
        ))),
    }
}

fn save_launcher(app_data_dir: &Path, path: &Path) -> Result<(), KernelError> {
    fs::create_dir_all(app_data_dir)
        .map_err(|error| KernelError::LocalData(format!("无法创建应用数据目录：{error}")))?;
    let config = serde_json::to_vec_pretty(&LauncherConfig {
        path: path.to_string_lossy().into_owned(),
    })
    .map_err(|error| KernelError::LocalData(format!("游戏入口配置无法序列化：{error}")))?;
    fs::write(app_data_dir.join(CONFIG_FILE), config)
        .map_err(|error| KernelError::LocalData(format!("游戏入口配置无法保存：{error}")))
}

fn executable_kind(path: &Path) -> Option<&'static str> {
    let file_name = path.file_name()?.to_str()?;
    if file_name.eq_ignore_ascii_case("HoYoPlay.exe")
        || file_name.eq_ignore_ascii_case("launcher.exe")
    {
        Some("launcher")
    } else if file_name.eq_ignore_ascii_case("YuanShen.exe")
        || file_name.eq_ignore_ascii_case("GenshinImpact.exe")
    {
        Some("game")
    } else {
        None
    }
}

fn validate_launcher(path: &Path) -> Result<(), KernelError> {
    if !path.is_file() || executable_kind(path).is_none() {
        return Err(KernelError::InvalidInput);
    }
    Ok(())
}

#[cfg(windows)]
fn discover_from_registry() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    const APP_PATHS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";
    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    let roots = [
        RegKey::predef(HKEY_CURRENT_USER),
        RegKey::predef(HKEY_LOCAL_MACHINE),
    ];
    let mut locations = Vec::new();
    for root in &roots {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            if let Ok(key) = root.open_subkey_with_flags(APP_PATHS, KEY_READ | view) {
                for exe in ["HoYoPlay.exe", "YuanShen.exe", "GenshinImpact.exe"] {
                    if let Ok(item) = key.open_subkey(exe)
                        && let Ok(value) = item.get_value::<String, _>("")
                    {
                        locations.push(PathBuf::from(value.trim().trim_matches('"')));
                    }
                }
            }
            if let Ok(key) = root.open_subkey_with_flags(UNINSTALL, KEY_READ | view) {
                for entry in key.enum_keys().flatten() {
                    let Ok(item) = key.open_subkey(entry) else {
                        continue;
                    };
                    let name = item
                        .get_value::<String, _>("DisplayName")
                        .unwrap_or_default();
                    if !name.contains("原神")
                        && !name.contains("HoYoPlay")
                        && !name.contains("Genshin Impact")
                    {
                        continue;
                    }
                    if let Ok(icon) = item.get_value::<String, _>("DisplayIcon") {
                        locations.push(PathBuf::from(
                            icon.split(',')
                                .next()
                                .unwrap_or_default()
                                .trim()
                                .trim_matches('"'),
                        ));
                    }
                    if let Ok(location) = item.get_value::<String, _>("InstallLocation") {
                        locations.extend(candidates_from_install_dir(Path::new(
                            location.trim_matches('"'),
                        )));
                    }
                }
            }
        }
    }
    locations
        .into_iter()
        .find(|path| !path.to_string_lossy().starts_with(r"\\") && validate_launcher(path).is_ok())
}

#[cfg(not(windows))]
fn discover_from_registry() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn candidates_from_install_dir(directory: &Path) -> Vec<PathBuf> {
    let folders = [
        "",
        "Genshin Impact game",
        "Genshin Impact",
        "原神 Game",
        "原神",
    ];
    let names = [
        "HoYoPlay.exe",
        "launcher.exe",
        "YuanShen.exe",
        "GenshinImpact.exe",
    ];
    folders
        .iter()
        .flat_map(|folder| {
            names.iter().map(move |name| {
                if folder.is_empty() {
                    directory.join(name)
                } else {
                    directory.join(folder).join(name)
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::executable_kind;
    use std::path::Path;

    #[test]
    fn accepts_known_launchers_and_game_executables() {
        assert_eq!(executable_kind(Path::new("HoYoPlay.exe")), Some("launcher"));
        assert_eq!(executable_kind(Path::new("YuanShen.exe")), Some("game"));
        assert_eq!(
            executable_kind(Path::new("GenshinImpact.exe")),
            Some("game")
        );
        assert_eq!(executable_kind(Path::new("other.exe")), None);
    }
}
