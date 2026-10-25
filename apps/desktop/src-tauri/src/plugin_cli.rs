//! Core CLI parsing and dispatch into the plugin manager.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wonderland_plugin_protocol::{
    PluginError, PluginRuntimeState, PluginUiCommand, PluginUiCommandEffect, RuntimeState,
};

use super::{
    PluginManager, not_installed, plugin_asset_response_for_manager, plugin_error,
    validate_timeout_ms,
};

/// Implements the plugin-management commands exposed by the Core CLI.
/// Mutating commands require an explicit `--yes`.
pub fn run_cli_command(
    manager: &PluginManager,
    args: &[OsString],
    desktop_running: bool,
) -> Result<Value, PluginError> {
    run_cli_command_with_invocation(manager, args, desktop_running, None)
}

pub(crate) fn run_cli_command_with_invocation(
    manager: &PluginManager,
    args: &[OsString],
    desktop_running: bool,
    invocation: Option<&crate::host_services::HostInvocation>,
) -> Result<Value, PluginError> {
    if let Some(invocation) = invocation {
        invocation.check()?;
    }
    let words = args
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if words.as_slice() == ["core", "status"] {
        let plugins = manager.snapshots();
        let profile_id = hex::encode(Sha256::digest(
            manager
                .inner
                .app_data_dir
                .to_string_lossy()
                .to_lowercase()
                .as_bytes(),
        ));
        return Ok(core_status_payload(
            env!("CARGO_PKG_VERSION"),
            desktop_running,
            &plugins,
            &profile_id[..16],
        ));
    }
    if words.first().is_some_and(|word| word == "logs") {
        return run_cli_logs(manager, &words[1..]);
    }
    if words.first().is_some_and(|word| word == "ui")
        && words.get(1).is_some_and(|word| word == "command")
    {
        return run_cli_ui_command(manager, &words[2..]);
    }
    if words.len() < 2 || words[0] != "plugins" {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Expected a Core, logs, or plugins command. Run `wla --help` for usage.",
        ));
    }

    match words[1].as_str() {
        "list" if words.len() == 2 => Ok(json!({"plugins": manager.snapshots()})),
        "call" if words.len() >= 4 => {
            let mut input = None;
            let mut confirmed = false;
            let mut timeout_ms = None;
            let mut index = 4;
            while index < words.len() {
                match words[index].as_str() {
                    "--yes" if !confirmed => confirmed = true,
                    "--input-json" if input.is_none() => {
                        index += 1;
                        input = Some(
                            serde_json::from_str::<Value>(words.get(index).ok_or_else(|| {
                                plugin_error("INVALID_REQUEST", "--input-json requires JSON.")
                            })?)
                            .map_err(|error| {
                                plugin_error(
                                    "INVALID_REQUEST",
                                    &format!("Invalid input JSON: {error}"),
                                )
                            })?,
                        );
                    }
                    "--timeout-ms" if timeout_ms.is_none() => {
                        index += 1;
                        let value = words.get(index).ok_or_else(|| {
                            plugin_error("INVALID_REQUEST", "--timeout-ms requires milliseconds.")
                        })?;
                        timeout_ms = Some(value.parse::<u64>().map_err(|_| {
                            plugin_error(
                                "INVALID_REQUEST",
                                "--timeout-ms must be a positive integer.",
                            )
                        })?);
                        validate_timeout_ms(timeout_ms)?;
                    }
                    flag => {
                        return Err(plugin_error(
                            "INVALID_REQUEST",
                            &format!("Unknown or repeated call option '{flag}'."),
                        ));
                    }
                }
                index += 1;
            }
            let input = input.ok_or_else(|| {
                plugin_error(
                    "INVALID_REQUEST",
                    "Pass --input-json for the method's contract parameters.",
                )
            })?;
            if !confirmed {
                return Err(plugin_error(
                    "CONFIRMATION_REQUIRED",
                    "Plugin methods can change data. Pass --yes to execute this call.",
                ));
            }
            let request_id = format!(
                "cli-call-{}",
                manager
                    .inner
                    .next_service_request_id
                    .fetch_add(1, Ordering::Relaxed)
            );
            let result = manager.call_with_parent_context(
                &words[2],
                &request_id,
                &words[3],
                input,
                timeout_ms,
                None,
                invocation,
            )?;
            Ok(json!({"pluginId": words[2], "method": words[3], "result": result}))
        }
        "ui-check" if (3..=4).contains(&words.len()) => {
            let plugin_id = &words[2];
            let snapshot = manager
                .snapshots()
                .into_iter()
                .find(|snapshot| snapshot.manifest.id == *plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            let ui = snapshot.manifest.ui.clone().ok_or_else(|| {
                plugin_error(
                    "NO_PLUGIN_UI",
                    "This plugin does not provide a user interface.",
                )
            })?;
            manager.start_target(plugin_id)?;
            let snapshot = manager
                .snapshots()
                .into_iter()
                .find(|snapshot| snapshot.manifest.id == *plugin_id)
                .ok_or_else(|| not_installed(plugin_id))?;
            let ui_url = manager.plugin_ui_url(plugin_id)?;
            let asset = words.get(3).map(String::as_str).unwrap_or(&ui.entry);
            let response_path = format!("/{plugin_id}/{asset}");
            let response = plugin_asset_response_for_manager(Some(manager), &response_path, "GET");
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(tauri::http::header::CONTENT_TYPE)
                .and_then(|header| header.to_str().ok())
                .unwrap_or_default();
            let csp = response
                .headers()
                .get("Content-Security-Policy")
                .and_then(|header| header.to_str().ok())
                .unwrap_or_default();
            let no_sniff = response
                .headers()
                .get(tauri::http::header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|header| header.to_str().ok())
                == Some("nosniff");
            let body = String::from_utf8_lossy(response.body());
            let is_html = content_type.starts_with("text/html")
                && body.to_ascii_lowercase().contains("<html");
            let csp_matches_policy = csp == crate::ui_policy::plugin_csp(plugin_id, None);
            let ok = status == 200 && is_html && no_sniff && csp_matches_policy;
            let report = json!({
                "ok": ok,
                "pluginId": plugin_id,
                "runtime": snapshot.runtime,
                "uiUrl": ui_url,
                "asset": asset,
                "status": status,
                "contentType": content_type,
                "contentSecurityPolicy": csp,
                "noSniff": no_sniff,
                "htmlDocument": is_html,
                "bytes": response.body().len(),
                "bridgeCompatibility": ui.bridge_compatibility,
                "contributions": ui.contributions,
            });
            if ok {
                Ok(report)
            } else {
                Err(PluginError {
                    code: "UI_RESOURCE_CHECK_FAILED".to_owned(),
                    message: format!("Plugin UI asset check failed for '{plugin_id}/{asset}'."),
                    details: Some(report),
                })
            }
        }
        "install" if words.len() >= 3 => {
            let mut confirmed = false;
            let mut overwrite = false;
            let mut approve_source_change = false;
            let mut index = 3;
            while index < args.len() {
                let flag = args[index].to_string_lossy();
                match flag.as_ref() {
                    "--yes" => confirmed = true,
                    "--overwrite" => overwrite = true,
                    "--approve-source-change" => approve_source_change = true,
                    _ => {
                        return Err(plugin_error(
                            "INVALID_REQUEST",
                            &format!("Unknown install option '{flag}'."),
                        ));
                    }
                }
                index += 1;
            }
            if !confirmed {
                return Err(plugin_error(
                    "CONFIRMATION_REQUIRED",
                    "Plugin installation runs local code. Review its source and pass --yes to confirm.",
                ));
            }
            let states = manager.install_cli_package(
                PathBuf::from(&args[2]),
                overwrite,
                approve_source_change,
            )?;
            Ok(json!({ "plugins": states }))
        }
        "retry" if words.len() >= 3 => {
            require_cli_confirmation(&args[3..])?;
            ensure_only_cli_flag(&args[3..], "--yes")?;
            Ok(json!({"plugins":manager.retry_runtime(&words[2])?}))
        }
        "enable" | "disable" if words.len() >= 3 => {
            require_cli_confirmation(&args[3..])?;
            ensure_only_cli_flag(&args[3..], "--yes")?;
            let states = manager.set_enabled(&words[2], words[1] == "enable")?;
            Ok(json!({ "plugins": states }))
        }
        #[cfg(debug_assertions)]
        "test-backend-exit" if words.len() == 4 => {
            require_cli_confirmation(&args[3..])?;
            ensure_only_cli_flag(&args[3..], "--yes")?;
            manager.start_target(&words[2])?;
            let states = manager.inject_backend_exit_for_test(&words[2])?;
            Ok(json!({ "plugins": states }))
        }
        "uninstall" | "remove" if words.len() >= 3 => {
            require_cli_confirmation(&args[3..])?;
            let remove_data = args[3..].iter().any(|argument| argument == "--remove-data");
            for argument in &args[3..] {
                if argument != "--yes" && argument != "--remove-data" {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        &format!("Unknown uninstall option '{}'.", argument.to_string_lossy()),
                    ));
                }
            }
            let states = manager.uninstall(&words[2], remove_data)?;
            Ok(json!({ "ok": true, "plugins": states }))
        }
        _ => Err(plugin_error(
            "INVALID_REQUEST",
            "Unknown or malformed plugins command. Run `wla --help` for usage.",
        )),
    }
}

fn core_status_payload(
    version: &str,
    desktop_running: bool,
    plugins: &[PluginRuntimeState],
    profile_id: &str,
) -> Value {
    json!({
        "version": version,
        "desktopRunning": desktop_running,
        "pluginCount": plugins.len(),
        "enabledPluginCount": plugins.iter().filter(|plugin| plugin.enabled).count(),
        "runningPluginCount": plugins.iter().filter(|plugin| plugin.runtime == RuntimeState::Running).count(),
        "profileId": profile_id,
    })
}

fn run_cli_ui_command(manager: &PluginManager, args: &[String]) -> Result<Value, PluginError> {
    if args.len() != 2 || args[0] != "list" {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Expected `ui command list <plugin>/<contribution>`. UI command execution requires an open desktop Core.",
        ));
    }
    let (plugin_id, contribution_id) = parse_contribution_selector(&args[1])?;
    let plugin = manager
        .snapshots()
        .into_iter()
        .find(|plugin| plugin.manifest.id == plugin_id)
        .ok_or_else(|| not_installed(&plugin_id))?;
    let contribution = plugin
        .manifest
        .ui
        .as_ref()
        .and_then(|ui| {
            ui.contributions
                .iter()
                .find(|item| item.id == contribution_id)
        })
        .ok_or_else(|| {
            plugin_error(
                "CONTRIBUTION_NOT_FOUND",
                "Plugin contribution is not registered.",
            )
        })?;
    Ok(json!({
        "pluginId": plugin_id,
        "contributionId": contribution_id,
        "commands": contribution.commands,
    }))
}

pub fn validate_cli_ui_command(
    manager: &PluginManager,
    args: &[String],
) -> Result<(String, PluginUiCommand, Value), PluginError> {
    if args.len() < 5 || args[0] != "ui" || args[1] != "command" || args[2] != "run" {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Expected `ui command run <plugin>/<contribution> <command-id> --input-json <json> [--yes]`.",
        ));
    }
    let (plugin_id, contribution_id) = parse_contribution_selector(&args[3])?;
    let command_id = &args[4];
    let mut input_json = None;
    let mut confirmed = false;
    let mut timeout_ms = None;
    let mut index = 5;
    while index < args.len() {
        match args[index].as_str() {
            "--yes" => {
                if confirmed {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        "--yes may be specified once.",
                    ));
                }
                confirmed = true;
            }
            "--input-json" => {
                if input_json.is_some() {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        "--input-json may be specified once.",
                    ));
                }
                index += 1;
                input_json = Some(args.get(index).ok_or_else(|| {
                    plugin_error("INVALID_REQUEST", "--input-json requires a JSON value.")
                })?);
            }
            "--timeout-ms" => {
                if timeout_ms.is_some() {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        "--timeout-ms may be specified once.",
                    ));
                }
                index += 1;
                timeout_ms = Some(
                    args.get(index)
                        .ok_or_else(|| {
                            plugin_error("INVALID_REQUEST", "--timeout-ms requires milliseconds.")
                        })?
                        .parse::<u64>()
                        .map_err(|_| {
                            plugin_error(
                                "INVALID_REQUEST",
                                "--timeout-ms must be a positive integer.",
                            )
                        })?,
                );
                validate_timeout_ms(timeout_ms)?;
            }
            option => {
                return Err(plugin_error(
                    "INVALID_REQUEST",
                    &format!("Unknown UI command option '{option}'."),
                ));
            }
        }
        index += 1;
    }
    let input_json =
        input_json.ok_or_else(|| plugin_error("INVALID_REQUEST", "--input-json is required."))?;
    let input: Value = serde_json::from_str(input_json)
        .map_err(|_| plugin_error("INVALID_REQUEST", "--input-json must contain valid JSON."))?;
    let plugin = manager
        .snapshots()
        .into_iter()
        .find(|plugin| plugin.manifest.id == plugin_id)
        .ok_or_else(|| not_installed(&plugin_id))?;
    let command = plugin
        .manifest
        .ui
        .as_ref()
        .and_then(|ui| {
            ui.contributions
                .iter()
                .find(|item| item.id == contribution_id)
        })
        .and_then(|contribution| {
            contribution
                .commands
                .iter()
                .find(|command| command.id == *command_id)
        })
        .cloned()
        .ok_or_else(|| {
            plugin_error(
                "UI_COMMAND_NOT_FOUND",
                "The command is not declared by this contribution.",
            )
        })?;
    if matches!(command.effect, PluginUiCommandEffect::Mutating) && !confirmed {
        return Err(plugin_error(
            "CONFIRMATION_REQUIRED",
            "This plugin UI command changes user content. Pass --yes to confirm.",
        ));
    }
    crate::plugin_schema::validate(&input, &command.input_schema, &command.input_schema).map_err(
        |error| PluginError {
            code: "SCHEMA_VALIDATION_FAILED".to_owned(),
            message: "The input does not match the plugin command schema.".to_owned(),
            details: Some(json!({ "reason": error })),
        },
    )?;
    Ok((format!("{plugin_id}/{contribution_id}"), command, input))
}

fn parse_contribution_selector(selector: &str) -> Result<(String, String), PluginError> {
    let (plugin_id, contribution_id) = selector
        .split_once('/')
        .ok_or_else(|| plugin_error("INVALID_REQUEST", "Use <plugin-id>/<contribution-id>."))?;
    if plugin_id.is_empty() || contribution_id.is_empty() || contribution_id.contains('/') {
        return Err(plugin_error(
            "INVALID_REQUEST",
            "Use <plugin-id>/<contribution-id>.",
        ));
    }
    Ok((plugin_id.to_owned(), contribution_id.to_owned()))
}

fn run_cli_logs(manager: &PluginManager, args: &[String]) -> Result<Value, PluginError> {
    let log_dir = manager.inner.app_data_dir.join("logs");
    match args.first().map(String::as_str) {
        Some("dir") if args.len() == 1 => Ok(json!({
            "directory": log_dir,
            "exists": log_dir.is_dir(),
        })),
        Some("list") if args.len() == 1 => {
            let mut files = Vec::new();
            if let Ok(entries) = fs::read_dir(&log_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("wonderland-assistant.log."))
                        && let Ok(metadata) = entry.metadata()
                    {
                        files.push(json!({
                            "name": path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                            "sizeBytes": metadata.len(),
                            "modified": metadata.modified().ok().and_then(|time| {
                                time.duration_since(std::time::UNIX_EPOCH).ok().map(|value| value.as_secs())
                            }),
                        }));
                    }
                }
            }
            files.sort_by(|left, right| right["name"].as_str().cmp(&left["name"].as_str()));
            Ok(json!({ "directory": log_dir, "files": files }))
        }
        Some("tail") => {
            let mut line_count = 100_usize;
            let mut index = 1;
            while index < args.len() {
                if args[index] == "--lines" {
                    index += 1;
                    let value = args.get(index).ok_or_else(|| {
                        plugin_error("INVALID_REQUEST", "--lines requires a count.")
                    })?;
                    line_count = value.parse::<usize>().map_err(|_| {
                        plugin_error(
                            "INVALID_REQUEST",
                            "--lines must be an integer from 1 to 500.",
                        )
                    })?;
                    if !(1..=500).contains(&line_count) {
                        return Err(plugin_error(
                            "INVALID_REQUEST",
                            "--lines must be an integer from 1 to 500.",
                        ));
                    }
                } else {
                    return Err(plugin_error(
                        "INVALID_REQUEST",
                        &format!("Unknown logs tail option '{}'.", args[index]),
                    ));
                }
                index += 1;
            }
            let mut candidates = fs::read_dir(&log_dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("wonderland-assistant.log."))
                })
                .collect::<Vec<_>>();
            candidates.sort();
            let Some(path) = candidates.pop() else {
                return Ok(json!({ "path": null, "lines": [] }));
            };
            let content = fs::read_to_string(&path).map_err(|error| {
                plugin_error("LOG_READ_FAILED", &format!("Cannot read log file: {error}"))
            })?;
            let lines = content
                .lines()
                .rev()
                .take(line_count)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>();
            Ok(json!({ "path": path, "lines": lines }))
        }
        _ => Err(plugin_error(
            "INVALID_REQUEST",
            "Expected `logs dir`, `logs list`, or `logs tail [--lines <1-500>]`.",
        )),
    }
}

fn require_cli_confirmation(args: &[OsString]) -> Result<(), PluginError> {
    if args.iter().any(|argument| argument == "--yes") {
        Ok(())
    } else {
        Err(plugin_error(
            "CONFIRMATION_REQUIRED",
            "This command changes plugin state. Pass --yes to confirm.",
        ))
    }
}

fn ensure_only_cli_flag(args: &[OsString], allowed: &str) -> Result<(), PluginError> {
    if let Some(unknown) = args
        .iter()
        .find(|argument| argument.to_string_lossy() != allowed)
    {
        return Err(plugin_error(
            "INVALID_REQUEST",
            &format!("Unknown command option '{}'.", unknown.to_string_lossy()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod core_status_tests {
    use super::*;

    #[test]
    fn status_reflects_whether_it_was_requested_from_the_desktop_runtime() {
        let plugins = [];
        let standalone = core_status_payload("0.1.0", false, &plugins, "profile");
        let desktop = core_status_payload("0.1.0", true, &plugins, "profile");

        assert_eq!(standalone["desktopRunning"], false);
        assert_eq!(desktop["desktopRunning"], true);
        assert_eq!(desktop["version"], "0.1.0");
        assert_eq!(desktop["pluginCount"], 0);
    }
}
