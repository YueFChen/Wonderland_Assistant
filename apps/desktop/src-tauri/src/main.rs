#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account_window;
mod commands;
mod game_launcher;
mod logging;
mod plugin_manager;
mod plugin_package;
mod plugin_runtime;
mod plugin_schema;
mod reveal;
mod state;
mod theme;
mod user_data;

#[cfg(debug_assertions)]
use std::path::PathBuf;
use tauri::Manager;
use wonderland_logging::Logging;

/// 主窗口 label，与 `tauri.conf.json` 保持一致。
pub const MAIN_WINDOW_LABEL: &str = "main";

fn main() {
    tauri::Builder::default()
        .register_uri_scheme_protocol("plugin-asset", |context, request| {
            plugin_manager::plugin_asset_response(
                context.app_handle(),
                request.uri().path(),
                request.method().as_str(),
            )
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let (context, logging) = state::bootstrap(app.handle())?;
            if let Some(logging) = logging {
                app.manage(logging);
            }
            let plugin_manager =
                plugin_manager::PluginManager::load(&context, app.handle().clone())
                    .map_err(|error| std::io::Error::other(error.message))?;
            #[cfg(debug_assertions)]
            if let Some(source) = std::env::var_os("WONDERLAND_DEV_PLUGIN_DIR") {
                plugin_manager
                    .install_debug_directory(PathBuf::from(source))
                    .map_err(|error| {
                        std::io::Error::other(format!(
                            "Cannot install the configured development plugin: {}",
                            error.message
                        ))
                    })?;
            }
            plugin_manager.start_enabled();
            app.manage(plugin_manager);
            app.manage(context);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            plugin_manager::plugins_list,
            plugin_manager::plugins_catalog_list,
            plugin_manager::plugins_catalog_install,
            plugin_manager::plugins_install,
            plugin_manager::plugins_remove,
            plugin_manager::plugins_set_enabled,
            plugin_manager::plugins_set_capabilities,
            plugin_manager::plugins_ui_url,
            plugin_manager::plugin_call,
            plugin_manager::plugin_cancel,
            commands::account_snapshot,
            commands::account_login_start,
            commands::account_sync,
            commands::account_login_cancel,
            commands::account_switch,
            commands::account_remove,
            game_launcher::game_launcher_get,
            game_launcher::game_launcher_select,
            game_launcher::game_launcher_launch,
            theme::theme_get,
            theme::theme_set,
            theme::theme_backgrounds,
            theme::theme_background_url,
            theme::theme_add_background,
            theme::theme_select_background,
            theme::theme_remove_background,
            logging::logging_get,
            logging::logging_dir,
            logging::logging_set_level,
            logging::logging_reveal_dir,
            user_data::user_data_get,
            user_data::user_data_migrate_custom,
            user_data::user_data_migrate_default,
            user_data::user_data_cancel_migration,
            user_data::user_data_choose_directory,
            user_data::settings_restart,
        ])
        .build(tauri::generate_context!())
        .expect("桌面壳启动失败")
        .run(|app, event| {
            // Drain queued records before process exit.
            if let tauri::RunEvent::Exit = event
                && let Some(logging) = app.try_state::<Logging>()
            {
                logging.shutdown();
            }
        });
}
