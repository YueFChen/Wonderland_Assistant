mod browser_session_windows;
mod browser_sessions;
mod byte_streams;
mod cli_ipc;
mod commands;
mod core_update;
mod download_sessions;
mod host_environment;
mod host_services;
mod logging;
mod network;
mod plugin_distribution;
mod plugin_distribution_cli;
mod plugin_install_recovery;
mod plugin_manager;
mod plugin_package;
mod plugin_runtime;
mod plugin_schema;
mod remote_interaction;
mod reveal;
mod state;
#[cfg(test)]
mod test_proxy;
mod theme;
mod tray;
mod ui_external_browser;
mod ui_policy;
mod user_data;
mod web_access;
mod webview_behavior;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Emitter, Manager};
use wonderland_logging::Logging;

/// Main window label shared with `tauri.conf.json` and native commands.
pub const MAIN_WINDOW_LABEL: &str = "main";

#[derive(Clone, Debug)]
struct CliInvocation {
    data_dir: Option<PathBuf>,
    arguments: Vec<OsString>,
    sensitive_payload: Option<cli_ipc::SensitivePayload>,
}

/// Starts the regular desktop Core.
pub fn run_desktop() {
    run_application(desktop_data_dir());
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
/// without constructing a Tauri application or WebView windows.
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

    let mut invocation = match parse_cli_invocation(&raw_args) {
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
    let sensitive_input =
        plugin_distribution_cli::needs_access_code_stdin(&arguments).and_then(|required| {
            if required {
                plugin_distribution_cli::read_access_code(std::io::stdin().lock())
                    .map(|code| Some(cli_ipc::SensitivePayload::DistributionAccessCode(code)))
            } else {
                Ok(None)
            }
        });
    invocation.sensitive_payload = match sensitive_input {
        Ok(payload) => payload,
        Err(error) => {
            let code = cli_ipc::print_error(cli_ipc::CliError {
                code: error.code,
                message: error.message,
                details: error.details,
            });
            std::process::exit(code);
        }
    };
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

    match cli_ipc::try_send_to_desktop_with_sensitive(
        &arguments,
        invocation.data_dir.as_deref(),
        invocation.sensitive_payload.as_ref(),
    ) {
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
        || arguments
            .first()
            .is_some_and(|argument| argument == "app" || argument == "web")
    {
        let code = cli_ipc::print_error(cli_ipc::CliError::new(
            "DESKTOP_NOT_RUNNING",
            "This command needs the desktop Core. Run `wla app open` first.",
        ));
        std::process::exit(code);
    }
    let code = run_headless_cli(invocation);
    if code != 0 {
        std::process::exit(code);
    }
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
        sensitive_payload: None,
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
           web status | start --config-json <json> --yes | stop --yes\n\
           web downloads list | discard <download-id> --yes\n\
           logs <dir|list|tail> [--lines <count>]\n\
           plugins list\n\
           plugins catalog\n\
           plugins distribution status\n\
           plugins distribution source <https-origin> --yes | clear-source --yes\n\
           plugins distribution set-code --stdin --yes | clear-code --yes\n\
           plugins install-online <plugin-id> --yes [--approve-source-change] [--repair|--rollback]\n\
           plugins update <plugin-id> --yes [--approve-source-change]\n\
           plugins call <plugin-id> <method> --input-json <json> --yes [--timeout-ms <ms>]\n\
           plugins ui-check <plugin-id> [ui-relative-path]\n\
           plugins install <path> --yes [--overwrite] [--approve-source-change]\n\
           plugins enable <plugin-id> --yes\n\
           plugins retry <plugin-id> --yes\n\
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
         web downloads lists unclaimed or active files from all browser devices;\n\
         discard explicitly removes a result and cancels any active transfer.\n\
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
         same-ID installs retain existing plugin data and secrets.",
    );
    help.push_str("\nAccess codes are read only from stdin; never include their values in command arguments.\nOnline distribution requires a configured HTTPS source and does not fall back to GitHub.");
    help.push_str("\nPlugin calls and UI command runs accept --timeout-ms with a positive duration; omitted means no additional deadline. Contract and ancestor deadlines still apply.");
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

/// Same platform root as Tauri's app_data_dir, without constructing a Tauri runtime.
fn default_cli_data_dir() -> Result<PathBuf, cli_ipc::CliError> {
    dirs::data_dir()
        .map(|path| path.join("com.wonderland.assistant"))
        .ok_or_else(|| {
            cli_ipc::CliError::new(
                "DATA_DIR_UNAVAILABLE",
                "The operating-system data directory is unavailable. Pass --data-dir explicitly.",
            )
        })
}

/// Reserve the locator, active root and pending target before bootstrap can write any of them.
struct DataRootGuards(#[allow(dead_code)] Vec<cli_ipc::SingleInstanceGuard>);

fn locked_data_roots(
    selected: &std::path::Path,
    initial: cli_ipc::SingleInstanceGuard,
) -> Result<DataRootGuards, cli_ipc::CliError> {
    let resolve = || {
        wonderland_user_data::FsUserDataService::bootstrap_lock_roots(selected)
            .map_err(|error| cli_ipc::CliError::new("DATA_DIR_UNAVAILABLE", error.to_string()))
    };
    let roots = resolve()?;
    let identity = |path: &std::path::Path| {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_lowercase()
    };
    let mut locked = std::collections::BTreeSet::from([identity(selected)]);
    let mut guards = vec![initial];
    for root in &roots {
        if locked.insert(identity(root)) {
            guards.push(cli_ipc::SingleInstanceGuard::acquire(root)?);
        }
    }
    if resolve()? != roots {
        return Err(cli_ipc::CliError::new(
            "DATA_DIR_CHANGED",
            "The data location changed while Core acquired its locks. Retry the command.",
        ));
    }
    Ok(DataRootGuards(guards))
}
fn run_headless_cli(invocation: CliInvocation) -> i32 {
    let run = || -> Result<serde_json::Value, cli_ipc::CliError> {
        let selected = invocation
            .data_dir
            .clone()
            .map(Ok)
            .unwrap_or_else(default_cli_data_dir)?;
        let initial = cli_ipc::SingleInstanceGuard::acquire(&selected)?;
        let _data_roots = locked_data_roots(&selected, initial)?;
        let (context, logging, _network_proxy) = state::bootstrap_headless(selected.clone())
            .map_err(|error| cli_ipc::CliError::new("CORE_START_FAILED", error.to_string()))?;
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|error| cli_ipc::CliError::new("CORE_START_FAILED", error.to_string()))?;
        let manager = plugin_manager::PluginManager::load(
            &context,
            Arc::new(host_environment::HeadlessEnvironment),
            runtime.handle().clone(),
            false,
        )
        .map_err(|error| cli_ipc::CliError {
            code: error.code,
            message: error.message,
            details: error.details,
        })?;
        let arguments = invocation
            .arguments
            .iter()
            .map(|word| word.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let result = plugin_distribution_cli::run(
            &manager,
            &arguments,
            invocation
                .sensitive_payload
                .as_ref()
                .map(cli_ipc::SensitivePayload::access_code),
        )
        .unwrap_or_else(|| plugin_manager::run_cli_command(&manager, &invocation.arguments, false))
        .map_err(|error| cli_ipc::CliError {
            code: error.code,
            message: error.message,
            details: error.details,
        });
        manager.shutdown();
        drop(manager);
        drop(runtime);
        if let Some(logging) = logging {
            logging.shutdown();
        }
        result
    };
    match run() {
        Ok(output) => {
            cli_ipc::print_success(output);
            0
        }
        Err(error) => cli_ipc::print_error(error),
    }
}

fn run_application(data_dir: Option<PathBuf>) {
    let mut context = tauri::generate_context!();
    #[cfg(debug_assertions)]
    let isolated_webview_data_dir = data_dir.clone().map(|path| path.join("webview"));

    // Create configured windows after state initialization, so every window uses
    // the platform browser handler rather than WebView2's suppressed popups.
    let window_labels = context
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
        .collect::<Vec<_>>();

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
                Err(_error) => {
                    let mut wake_succeeded = false;
                    for _ in 0..10 {
                        match cli_ipc::try_wake_desktop(&selected_data_dir) {
                            Ok(Some(true)) => {
                                wake_succeeded = true;
                                break;
                            }
                            Ok(Some(false)) => {
                                wake_succeeded = matches!(
                                    cli_ipc::try_wake_desktop_legacy(&selected_data_dir),
                                    Ok(Some(true))
                                );
                                break;
                            }
                            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                            Err(_) => {
                                wake_succeeded = matches!(
                                    cli_ipc::try_wake_desktop_legacy(&selected_data_dir),
                                    Ok(Some(true))
                                );
                                break;
                            }
                        }
                    }
                    if !wake_succeeded {
                        eprintln!("无法唤醒已运行的 Wonderland Assistant Core 窗口。");
                    }
                    app.handle().exit(0);
                    return Ok(());
                }
            };
            app.manage(
                locked_data_roots(&selected_data_dir, instance_guard)
                    .map_err(|error| std::io::Error::other(error.message))?,
            );
            let (context, logging, network_proxy) = match data_dir.clone() {
                Some(data_dir) => state::bootstrap_with_data_dir(app.handle(), Some(data_dir))?,
                None => state::bootstrap(app.handle())?,
            };
            if let Some(logging) = logging {
                app.manage(logging);
            }
            app.manage(network_proxy);
            app.manage(core_update::CoreUpdateState::default());
            let plugin_manager = plugin_manager::PluginManager::load(
                &context,
                Arc::new(host_environment::DesktopEnvironment::new(
                    app.handle().clone(),
                )),
                tauri::async_runtime::handle().inner().clone(),
                true,
            )
            .map_err(|error| std::io::Error::other(error.message))?;

            let pending_ui_requests = Arc::new(cli_ipc::CliUiPending::default());
            app.manage(Arc::clone(&pending_ui_requests));

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

            app.manage(plugin_manager.clone());
            app.manage(plugin_manager::local_install::LocalPluginInstaller::new(
                &context.app_data_dir,
            ));
            app.manage(web_access::WebAccessService::default());
            {
                tray::initialize(app)?;
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

            for label in &window_labels {
                let window_config = app
                    .config()
                    .app
                    .windows
                    .iter()
                    .find(|window| window.label == *label)
                    .expect("configured window should remain available");
                let builder = ui_external_browser::with_external_browser(
                    tauri::WebviewWindowBuilder::from_config(app.handle(), window_config)?,
                );
                #[cfg(debug_assertions)]
                let builder = if let Some(profile_dir) = isolated_webview_data_dir.as_ref() {
                    builder.data_directory(profile_dir.join(label))
                } else {
                    builder
                };
                let window = builder.build()?;
                webview_behavior::configure(&window)?;
            }
            if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let close_event_window = main_window.clone();
                main_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = close_event_window.emit("core-close-requested", ());
                    }
                });
            }
            plugin_manager.start_enabled();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            web_access::web_access_status,
            web_access::web_access_start,
            web_access::web_access_stop,
            plugin_manager::plugins_list,
            plugin_manager::retained_data::plugins_retained_data,
            plugin_manager::retained_data::plugins_cleanup_data,
            plugin_manager::plugins_retry_scan,
            plugin_manager::plugins_retry_runtime,
            plugin_manager::plugins_catalog_list,
            plugin_manager::plugins_catalog_install,
            plugin_manager::plugins_catalog_cancel,
            plugin_manager::plugins_distribution_settings,
            plugin_manager::plugins_distribution_configure,
            plugin_manager::local_install::plugins_install_reset,
            plugin_manager::local_install::plugins_install_prepare,
            plugin_manager::local_install::plugins_install_upload_open,
            plugin_manager::local_install::plugins_install_upload_write,
            plugin_manager::local_install::plugins_install_discard,
            plugin_manager::local_install::plugins_install_run,
            plugin_manager::plugins_remove,
            plugin_manager::plugins_set_enabled,
            plugin_manager::plugins_ui_url,
            plugin_manager::plugin_call,
            plugin_manager::plugin_cancel,
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
            tray::core_exit,
            tray::tray_sync_pinned,
            user_data::user_data_get,
            user_data::user_data_reveal_dir,
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
            eprintln!("桌面壳启动失败：{error}");
            return;
        }
    };
    let event_handler = |app: &tauri::AppHandle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(manager) = app.try_state::<plugin_manager::PluginManager>() {
                manager.shutdown();
            }
            if let Some(logging) = app.try_state::<Logging>() {
                logging.shutdown();
            }
        }
    };

    app.run(event_handler);
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

    #[test]
    fn access_code_invocation_debug_redacts_the_dedicated_payload() {
        let mut invocation = parse_cli_invocation(&[
            "plugins".into(),
            "distribution".into(),
            "set-code".into(),
            "--stdin".into(),
            "--yes".into(),
        ])
        .unwrap();
        invocation.sensitive_payload = Some(cli_ipc::SensitivePayload::DistributionAccessCode(
            "private-access-code".into(),
        ));
        assert!(!format!("{invocation:?}").contains("private-access-code"));
        assert!(
            !invocation
                .arguments
                .iter()
                .any(|word| word.to_string_lossy().contains("private-access-code"))
        );
    }
}
