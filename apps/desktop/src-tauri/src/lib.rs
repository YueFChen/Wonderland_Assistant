mod account_window;
mod cli_ipc;
mod commands;
mod core_update;
mod logging;
mod network;
mod plugin_manager;
mod plugin_package;
mod plugin_runtime;
mod plugin_schema;
mod reveal;
mod state;
mod theme;
mod user_data;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use tauri::Manager;
use wonderland_logging::Logging;

/// Main window label shared with `tauri.conf.json` and native commands.
pub const MAIN_WINDOW_LABEL: &str = "main";

#[derive(Clone, Debug)]
struct CliInvocation {
    data_dir: Option<PathBuf>,
    arguments: Vec<OsString>,
}

/// Starts the regular desktop Core.
pub fn run_desktop() {
    run_application(None, desktop_data_dir());
}

/// Allows debug builds to use an isolated profile for Windows WebView integration tests.
/// Release builds always use the operating-system Core data directory.
fn desktop_data_dir() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        let data_dir = std::env::var_os("WONDERLAND_DEV_DATA_DIR").map(PathBuf::from);
        if data_dir.as_ref().is_some_and(|path| !path.is_absolute()) {
            eprintln!("WONDERLAND_DEV_DATA_DIR requires an absolute path.");
            std::process::exit(2);
        }
        data_dir
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// Starts the Core command-line interface. It uses the same Core services as the desktop app,
/// while building no Tauri WebView windows.
pub fn run_cli() {
    let raw_args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if raw_args.len() == 1 && (raw_args[0] == "--version" || raw_args[0] == "-V") {
        println!("wla {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if raw_args.is_empty() || raw_args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_cli_help();
        return;
    }

    let invocation = match parse_cli_invocation(&raw_args) {
        Ok(invocation) => invocation,
        Err(message) => {
            let code = cli_ipc::print_error(cli_ipc::CliError::new(
                "INVALID_REQUEST",
                format!("{message} Run `wla --help` for usage."),
            ));
            std::process::exit(code);
        }
    };

    let arguments = invocation
        .arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if arguments.first().is_some_and(|argument| argument == "app")
        && arguments
            .get(1)
            .is_some_and(|argument| argument == "status")
    {
        match cli_ipc::try_send_to_desktop(&arguments, invocation.data_dir.as_deref()) {
            Ok(Some(response)) => {
                let code = cli_ipc::print_response(response);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            Ok(None) => {
                cli_ipc::print_success(serde_json::json!({
                    "coreVersion": env!("CARGO_PKG_VERSION"),
                    "desktopRunning": false,
                }));
                return;
            }
            Err(error) => {
                let code = cli_ipc::print_error(error);
                std::process::exit(code);
            }
        }
    }

    if arguments.first().is_some_and(|argument| argument == "app")
        && arguments.get(1).is_some_and(|argument| argument == "open")
    {
        if invocation.data_dir.is_some() {
            let code = cli_ipc::print_error(cli_ipc::CliError::new(
                "ALT_PROFILE_UNSUPPORTED",
                "`app open` cannot select an alternate data directory.",
            ));
            std::process::exit(code);
        }
        match cli_ipc::try_send_to_desktop(&arguments, None) {
            Ok(Some(response)) => {
                let code = cli_ipc::print_response(response);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            Ok(None) => {}
            Err(error) => {
                let code = cli_ipc::print_error(error);
                std::process::exit(code);
            }
        }
        if let Err(error) = cli_ipc::launch_desktop() {
            let code = cli_ipc::print_error(error);
            std::process::exit(code);
        }
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            match cli_ipc::try_send_to_desktop(&arguments, None) {
                Ok(Some(response)) => {
                    let code = cli_ipc::print_response(response);
                    if code != 0 {
                        std::process::exit(code);
                    }
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    let code = cli_ipc::print_error(error);
                    std::process::exit(code);
                }
            }
        }
        let code = cli_ipc::print_error(cli_ipc::CliError::new(
            "APP_START_TIMEOUT",
            "The desktop Core did not become ready in time.",
        ));
        std::process::exit(code);
    }

    match cli_ipc::try_send_to_desktop(&arguments, invocation.data_dir.as_deref()) {
        Ok(Some(response)) => {
            let code = cli_ipc::print_response(response);
            if code != 0 {
                std::process::exit(code);
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            let code = cli_ipc::print_error(error);
            std::process::exit(code);
        }
    }

    if (arguments.first().is_some_and(|argument| argument == "ui")
        && !(arguments
            .get(1)
            .is_some_and(|argument| argument == "command")
            && arguments.get(2).is_some_and(|argument| argument == "list")))
        || arguments.first().is_some_and(|argument| argument == "app")
    {
        let code = cli_ipc::print_error(cli_ipc::CliError::new(
            "DESKTOP_NOT_RUNNING",
            "This command needs the desktop Core. Run `wla app open` first.",
        ));
        std::process::exit(code);
    }
    run_application(Some(invocation), None);
}

fn parse_cli_invocation(args: &[OsString]) -> Result<CliInvocation, String> {
    let mut data_dir = None;
    let mut command_args = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--data-dir" {
            index += 1;
            let path = args
                .get(index)
                .map(PathBuf::from)
                .ok_or_else(|| "--data-dir requires an absolute path.".to_owned())?;
            if !path.is_absolute() {
                return Err("--data-dir requires an absolute path.".to_owned());
            }
            data_dir = Some(path);
        } else {
            command_args.push(args[index].clone());
        }
        index += 1;
    }
    if command_args.is_empty() {
        return Err("A Core CLI command is required.".to_owned());
    }
    Ok(CliInvocation {
        data_dir,
        arguments: command_args,
    })
}

fn print_cli_help() {
    println!("{}", cli_help_text());
}

fn cli_help_text() -> String {
    let mut help = String::from(
        "Wonderland Core CLI\n\
        Usage: wla [--data-dir <absolute-path>] <command>\n\
               wla --version\n\
               wla --help\n\n\
         Commands:\n\
           app status | open [--target <route>]\n\
           ui state | navigate <route>\n\
           ui activity <list|open|activate|close> [<plugin>/<contribution>]\n\
           ui sidebar <state|collapse|expand|open|close> [<plugin>/<contribution>]\n\
           ui layout <get|reset|pin|unpin|hide|show|move> [arguments]\n\
           ui theme <get|set> [arguments]\n\
           ui command list <plugin>/<contribution>\n\
           ui command run <plugin>/<contribution> <command-id> --input-json <json> [--yes]\n\
           core status\n\
           logs <dir|list|tail> [--lines <count>]\n\
           plugins list\n\
           plugins permissions list <plugin-id>\n\
           plugins permissions set <plugin-id> --grant <capability>... --yes\n\
           plugins ui-check <plugin-id> [ui-relative-path]\n\
           plugins install <path> --yes [--grant <capability>]... [--overwrite] [--approve-source-change]\n\
           plugins enable <plugin-id> --yes\n\
           plugins disable <plugin-id> --yes\n\
           plugins uninstall <plugin-id> --yes [--remove-data]\n",
    );
    #[cfg(debug_assertions)]
    help.push_str("plugins test-backend-exit <plugin-id> --yes (debug builds only)\n");
    help.push_str(
        "\n\
         --data-dir selects an alternate Core data directory for a separate profile\n\
         or isolated automation run.\n\
         ui-check verifies the plugin entry asset, MIME type, CSP and nosniff headers.\n\
         ",
    );
    #[cfg(debug_assertions)]
    help.push_str(
        "test-backend-exit force-stops one running backend and checks crash reporting.\n",
    );
    help.push_str(
        "\
         Commands targeting an open window are forwarded to the running desktop Core.\n\
         Other commands use the shared Core services. Mutating commands require --yes;\n\
         install grants only the capabilities listed with --grant.",
    );
    help
}

#[cfg(test)]
mod cli_help_tests {
    #[test]
    fn debug_only_commands_are_advertised_only_in_debug_builds() {
        let help = super::cli_help_text();
        assert_eq!(
            help.contains("plugins test-backend-exit"),
            cfg!(debug_assertions)
        );
        assert!(help.contains("plugins list"));
        assert!(!help.contains("plugins status"));
    }
}

fn run_application(cli: Option<CliInvocation>, desktop_data_dir: Option<PathBuf>) {
    let mut context = tauri::generate_context!();
    let cli_mode = cli.is_some();
    #[cfg(debug_assertions)]
    let isolated_webview_data_dir = if cli_mode {
        None
    } else {
        desktop_data_dir.clone().map(|path| path.join("webview"))
    };

    #[cfg(debug_assertions)]
    let isolated_window_labels = if isolated_webview_data_dir.is_some() {
        context
            .config_mut()
            .app
            .windows
            .iter_mut()
            .filter_map(|window| {
                if window.create {
                    window.create = false;
                    Some(window.label.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    if cli_mode {
        context.config_mut().app.windows.clear();
    }

    let cli_exit_code = Arc::new(AtomicI32::new(0));
    let setup_exit_code = Arc::clone(&cli_exit_code);
    let data_dir = cli
        .as_ref()
        .and_then(|invocation| invocation.data_dir.clone())
        .or(desktop_data_dir);
    let cli_args = cli.map(|invocation| invocation.arguments);

    let app = tauri::Builder::default()
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
        .setup(move |app| {
            let selected_data_dir = match data_dir.clone() {
                Some(data_dir) => data_dir,
                None => app.path().app_data_dir()?,
            };
            let instance_guard = match cli_ipc::SingleInstanceGuard::acquire(&selected_data_dir) {
                Ok(guard) => guard,
                Err(error) if !cli_mode => {
                    let _ = cli_ipc::try_send_to_desktop(
                        &["app".to_owned(), "open".to_owned()],
                        Some(&selected_data_dir),
                    );
                    app.handle().exit(0);
                    let _ = error;
                    return Ok(());
                }
                Err(error) => {
                    setup_exit_code.store(cli_ipc::exit_code_for(&error.code), Ordering::Release);
                    cli_ipc::print_error(error);
                    app.handle().exit(setup_exit_code.load(Ordering::Acquire));
                    return Ok(());
                }
            };
            app.manage(instance_guard);
            let (context, logging, network_proxy) = match data_dir.clone() {
                Some(data_dir) => state::bootstrap_with_data_dir(app.handle(), Some(data_dir))?,
                None => state::bootstrap(app.handle())?,
            };
            if let Some(logging) = logging {
                app.manage(logging);
            }
            app.manage(network_proxy);
            app.manage(core_update::CoreUpdateState::default());
            let plugin_manager =
                plugin_manager::PluginManager::load(&context, app.handle().clone())
                    .map_err(|error| std::io::Error::other(error.message))?;

            let pending_ui_requests = Arc::new(cli_ipc::CliUiPending::default());
            app.manage(Arc::clone(&pending_ui_requests));

            #[cfg(debug_assertions)]
            if !cli_mode && let Some(source) = std::env::var_os("WONDERLAND_DEV_PLUGIN_DIR") {
                plugin_manager
                    .install_debug_directory(PathBuf::from(source))
                    .map_err(|error| {
                        std::io::Error::other(format!(
                            "Cannot install the configured development plugin: {}",
                            error.message
                        ))
                    })?;
            }

            app.manage(plugin_manager.clone());
            if !cli_mode {
                let server = cli_ipc::CliIpcServer::start(
                    app.handle(),
                    plugin_manager.clone(),
                    &context.app_data_dir,
                    pending_ui_requests,
                )
                .map_err(|error| std::io::Error::other(error.message))?;
                app.manage(server);
            }
            app.manage(context);

            if cli_mode {
                let arguments = cli_args.as_deref().unwrap_or_default();
                let exit_code = match plugin_manager::run_cli_command(
                    app.handle(),
                    &plugin_manager,
                    arguments,
                    false,
                ) {
                    Ok(output) => {
                        cli_ipc::print_success(output);
                        0
                    }
                    Err(error) => cli_ipc::plugin_error(error),
                };
                setup_exit_code.store(exit_code, Ordering::Release);
                app.handle().exit(exit_code);
            } else {
                #[cfg(debug_assertions)]
                if let Some(profile_dir) = isolated_webview_data_dir.as_ref() {
                    for label in &isolated_window_labels {
                        let window_config = app
                            .config()
                            .app
                            .windows
                            .iter()
                            .find(|window| window.label == *label)
                            .expect("isolated window config should remain available");
                        tauri::WebviewWindowBuilder::from_config(app.handle(), window_config)?
                            .data_directory(profile_dir.join(label))
                            .build()?;
                    }
                }
                plugin_manager.start_enabled();
            }
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
            network::network_proxy_get,
            network::network_proxy_set,
            network::network_proxy_test,
            core_update::core_update_check,
            core_update::core_update_install,
            user_data::user_data_get,
            user_data::user_data_migrate_custom,
            user_data::user_data_migrate_default,
            user_data::user_data_cancel_migration,
            user_data::user_data_choose_directory,
            user_data::settings_restart,
            cli_ipc::cli_ui_ack,
            cli_ipc::cli_ui_ready,
        ])
        .build(context);
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            if cli_mode {
                cli_ipc::print_error(cli_ipc::CliError::new(
                    "CORE_START_FAILED",
                    error.to_string(),
                ));
                std::process::exit(1);
            }
            eprintln!("桌面壳启动失败：{error}");
            return;
        }
    };
    let event_handler = |app: &tauri::AppHandle, event| {
        if let tauri::RunEvent::Exit = event
            && let Some(logging) = app.try_state::<Logging>()
        {
            logging.shutdown();
        }
    };

    if cli_mode {
        let _ = app.run_return(event_handler);
        std::process::exit(cli_exit_code.load(Ordering::Acquire));
    } else {
        app.run(event_handler);
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn parses_an_isolated_data_root_without_changing_command_arguments() {
        let data_dir = std::env::temp_dir().join("wla-test-data");
        let invocation = parse_cli_invocation(&[
            OsString::from("--data-dir"),
            data_dir.clone().into_os_string(),
            OsString::from("plugins"),
            OsString::from("list"),
        ])
        .unwrap();

        assert_eq!(invocation.data_dir, Some(data_dir));
        assert_eq!(
            invocation.arguments,
            [OsString::from("plugins"), OsString::from("list")]
        );
    }

    #[test]
    fn requires_an_absolute_isolated_data_root() {
        let error = parse_cli_invocation(&[
            OsString::from("--data-dir"),
            OsString::from("relative-folder"),
            OsString::from("plugins"),
            OsString::from("list"),
        ])
        .unwrap_err();

        assert!(error.contains("absolute path"));
    }
}
