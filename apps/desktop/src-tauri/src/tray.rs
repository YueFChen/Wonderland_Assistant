//! Windows notification-area integration for keeping enabled plugins alive after hiding Core.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, Runtime};

use crate::MAIN_WINDOW_LABEL;

const TRAY_ID: &str = "wonderland-core";
const NAVIGATION_EVENT: &str = "core-tray-navigation";
const MAX_PINNED_ENTRIES: usize = 5;

#[derive(Default)]
pub struct TrayMenuState {
    actions: Mutex<HashMap<String, TrayAction>>,
}

#[derive(Clone)]
struct TrayAction {
    plugin_id: String,
    contribution_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PinnedEntry {
    id: String,
    title: String,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum TrayNavigation {
    Home,
    PluginManagement,
    Contribution {
        plugin_id: String,
        contribution_id: String,
    },
}

pub fn initialize(app: &mut App) -> tauri::Result<()> {
    app.manage(TrayMenuState::default());
    let menu = build_menu(app, &[])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Wonderland Assistant")
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                let _ = show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let _ = builder.build(app)?;
    Ok(())
}

#[tauri::command]
pub fn tray_sync_pinned(app: AppHandle, entries: Vec<PinnedEntry>) -> Result<(), String> {
    if entries.len() > MAX_PINNED_ENTRIES {
        return Err("Too many pinned workspace entries.".to_owned());
    }

    let mut actions = HashMap::with_capacity(entries.len());
    let mut menu_entries = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some((plugin_id, contribution_id)) = entry.id.split_once('/') else {
            return Err("A pinned workspace ID is malformed.".to_owned());
        };
        if entry.id.matches('/').count() != 1
            || !valid_id(plugin_id)
            || !valid_id(contribution_id)
            || entry.title.trim().is_empty()
            || entry.title.chars().count() > 120
        {
            return Err("A pinned workspace entry is invalid.".to_owned());
        }

        let menu_id = menu_id(plugin_id, contribution_id);
        if actions
            .insert(
                menu_id.clone(),
                TrayAction {
                    plugin_id: plugin_id.to_owned(),
                    contribution_id: contribution_id.to_owned(),
                },
            )
            .is_some()
        {
            return Err("Pinned workspace entries must be unique.".to_owned());
        }
        menu_entries.push((menu_id, entry.title.trim().to_owned()));
    }

    let menu = build_menu(&app, &menu_entries).map_err(|error| error.to_string())?;
    let tray = app
        .tray_by_id(TRAY_ID)
        .ok_or_else(|| "The system tray is not available.".to_owned())?;
    tray.set_menu(Some(menu))
        .map_err(|error| error.to_string())?;

    let state = app.state::<TrayMenuState>();
    *state
        .actions
        .lock()
        .map_err(|_| "The system tray menu is unavailable.".to_owned())? = actions;
    Ok(())
}

#[tauri::command]
pub fn core_exit(app: AppHandle) {
    app.exit(0);
}

fn build_menu<R: Runtime>(
    manager: &impl Manager<R>,
    pinned_entries: &[(String, String)],
) -> tauri::Result<Menu<R>> {
    let home = MenuItem::with_id(manager, "home", "进入首页", true, None::<&str>)?;
    let plugins = MenuItem::with_id(manager, "plugin-management", "插件管理", true, None::<&str>)?;
    let separator_before_pinned = PredefinedMenuItem::separator(manager)?;
    let pinned = Submenu::new(manager, "固定到侧栏的工作区", true)?;
    if pinned_entries.is_empty() {
        let empty = MenuItem::with_id(
            manager,
            "pinned-empty",
            "暂无固定的工作区",
            false,
            None::<&str>,
        )?;
        pinned.append(&empty)?;
    } else {
        for (id, title) in pinned_entries {
            let item = MenuItem::with_id(manager, id, title, true, None::<&str>)?;
            pinned.append(&item)?;
        }
    }
    let separator_before_exit = PredefinedMenuItem::separator(manager)?;
    let exit = MenuItem::with_id(
        manager,
        "exit",
        "退出 Wonderland Assistant",
        true,
        None::<&str>,
    )?;

    let menu = Menu::new(manager)?;
    menu.append(&home)?;
    menu.append(&plugins)?;
    menu.append(&separator_before_pinned)?;
    menu.append(&pinned)?;
    menu.append(&separator_before_exit)?;
    menu.append(&exit)?;
    Ok(menu)
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();
    let navigation = match id {
        "home" => Some(TrayNavigation::Home),
        "plugin-management" => Some(TrayNavigation::PluginManagement),
        "exit" => {
            app.exit(0);
            return;
        }
        _ => {
            let action = app
                .try_state::<TrayMenuState>()
                .and_then(|state| state.actions.lock().ok()?.get(id).cloned());
            action.map(|action| TrayNavigation::Contribution {
                plugin_id: action.plugin_id,
                contribution_id: action.contribution_id,
            })
        }
    };
    if let Some(navigation) = navigation {
        if show_main_window(app).is_ok() {
            let _ = app.emit(NAVIGATION_EVENT, navigation);
        }
    }
}

pub(crate) fn show_main_window(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "The main window is not available.".to_owned())?;
    window.unminimize().map_err(|error| error.to_string())?;
    window.show().map_err(|error| error.to_string())?;
    let _ = window.set_focus();
    Ok(())
}

fn menu_id(plugin_id: &str, contribution_id: &str) -> String {
    let digest = Sha256::digest(format!("{plugin_id}\0{contribution_id}").as_bytes());
    format!("pinned_{}", hex::encode(&digest[..12]))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}
