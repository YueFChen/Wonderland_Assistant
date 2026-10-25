//! Narrow CLI adapter for the shared plugin distribution service.

use std::io::Read;
use std::time::Duration;

use serde_json::{Value, json};
use wonderland_plugin_protocol::PluginError;

use crate::plugin_distribution::{
    CatalogInstallMode, CatalogInstallRequest, DistributionConfigureRequest, PluginCatalogEntry,
    PluginCatalogSnapshot,
};
use crate::plugin_manager::PluginManager;

pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const MAX_ACCESS_CODE_BYTES: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
enum Command<'a> {
    Catalog,
    Status,
    Source(&'a str),
    ClearSource,
    SetCode,
    ClearCode,
    Install {
        id: &'a str,
        update: bool,
        mode: CatalogInstallMode,
        approve_source_change: bool,
    },
}

fn error(code: &str, message: &str) -> PluginError {
    PluginError {
        code: code.into(),
        message: message.into(),
        details: None,
    }
}

pub(crate) fn is_network_command(args: &[String]) -> bool {
    args.first().is_some_and(|word| word == "plugins")
        && args
            .get(1)
            .is_some_and(|word| matches!(word.as_str(), "catalog" | "install-online" | "update"))
}

fn parse(args: &[String]) -> Option<Result<Command<'_>, PluginError>> {
    if args.first().is_none_or(|word| word != "plugins") {
        return None;
    }
    let kind = args.get(1)?.as_str();
    if !matches!(
        kind,
        "catalog" | "distribution" | "install-online" | "update"
    ) {
        return None;
    }
    Some((|| {
        let invalid = || {
            error(
                "INVALID_REQUEST",
                "Invalid plugin distribution command. Run `wla --help` for usage.",
            )
        };
        let confirmation = || {
            error(
                "CONFIRMATION_REQUIRED",
                "This command changes plugin distribution state. Pass --yes to confirm.",
            )
        };
        let flags = |words: &[String], allowed: &[&str]| -> Result<(), PluginError> {
            let mut seen = std::collections::BTreeSet::new();
            if words
                .iter()
                .any(|word| !allowed.contains(&word.as_str()) || !seen.insert(word.as_str()))
            {
                return Err(invalid());
            }
            if !seen.contains("--yes") {
                return Err(confirmation());
            }
            Ok(())
        };
        match kind {
            "catalog" if args.len() == 2 => Ok(Command::Catalog),
            "distribution" => match args.get(2).map(String::as_str) {
                Some("status") if args.len() == 3 => Ok(Command::Status),
                Some("source") if args.len() >= 4 => {
                    flags(&args[4..], &["--yes"])?;
                    Ok(Command::Source(&args[3]))
                }
                Some("clear-source") => {
                    flags(&args[3..], &["--yes"])?;
                    Ok(Command::ClearSource)
                }
                Some("clear-code") => {
                    flags(&args[3..], &["--yes"])?;
                    Ok(Command::ClearCode)
                }
                Some("set-code") => {
                    flags(&args[3..], &["--yes", "--stdin"])?;
                    if !args[3..].iter().any(|word| word == "--stdin") {
                        return Err(error(
                            "INVALID_REQUEST",
                            "Read the access code with --stdin; never put its value in command arguments.",
                        ));
                    }
                    Ok(Command::SetCode)
                }
                _ => Err(invalid()),
            },
            "install-online" | "update" if args.len() >= 3 => {
                flags(
                    &args[3..],
                    &["--yes", "--approve-source-change", "--repair", "--rollback"],
                )?;
                let repair = args[3..].iter().any(|value| value == "--repair");
                let rollback = args[3..].iter().any(|value| value == "--rollback");
                if repair && rollback || kind == "update" && (repair || rollback) {
                    return Err(invalid());
                }
                let id = args[2].as_str();
                if id.is_empty()
                    || id.len() > 64
                    || !id.as_bytes()[0].is_ascii_lowercase()
                    || !id.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
                    })
                {
                    return Err(error("INVALID_REQUEST", "Plugin ID is invalid."));
                }
                Ok(Command::Install {
                    id,
                    update: kind == "update",
                    mode: if repair {
                        CatalogInstallMode::Repair
                    } else if rollback {
                        CatalogInstallMode::Rollback
                    } else {
                        CatalogInstallMode::Update
                    },
                    approve_source_change: args[3..]
                        .iter()
                        .any(|word| word == "--approve-source-change"),
                })
            }
            _ => Err(invalid()),
        }
    })())
}

/// Validate the complete command before consuming stdin, including mutation confirmation.
pub(crate) fn needs_access_code_stdin(args: &[String]) -> Result<bool, PluginError> {
    match parse(args) {
        Some(result) => result.map(|command| command == Command::SetCode),
        None => Ok(false),
    }
}

pub(crate) fn validate_access_code(code: &str) -> Result<(), PluginError> {
    if code.is_empty()
        || code.len() > MAX_ACCESS_CODE_BYTES
        || !code.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(error(
            "INVALID_REQUEST",
            "The access code must contain 1 to 4096 visible ASCII bytes without whitespace.",
        ));
    }
    Ok(())
}

pub(crate) fn read_access_code(reader: impl Read) -> Result<String, PluginError> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_ACCESS_CODE_BYTES + 3) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            error(
                "INVALID_REQUEST",
                "The access code could not be read from stdin.",
            )
        })?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| error("INVALID_REQUEST", "The access code input is invalid."))?;
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    validate_access_code(text)?;
    Ok(text.to_owned())
}

fn install_entry<'a>(
    snapshot: &'a PluginCatalogSnapshot,
    id: &str,
) -> Result<&'a PluginCatalogEntry, PluginError> {
    if snapshot.status == "unconfigured" {
        return Err(error(
            "DISTRIBUTION_NOT_CONFIGURED",
            "Configure a plugin distribution source before installing online.",
        ));
    }
    if snapshot.status == "rate_limited" {
        let mut failure = error(
            "RATE_LIMITED",
            "The distribution service rate limit was reached. Retry after the indicated delay.",
        );
        failure.details = snapshot
            .retry_after_seconds
            .map(|seconds| json!({"retryAfterSeconds": seconds}));
        return Err(failure);
    }
    let item = snapshot
        .plugins
        .iter()
        .find(|item| item.registration.id == id);
    if matches!(snapshot.status.as_str(), "ready" | "invalid_response")
        && let Some(failure) = item.and_then(|item| item.error.as_ref())
        && matches!(
            failure.code.as_str(),
            "INVALID_UPDATE_SIGNATURE"
                | "INVALID_UPDATE_MANIFEST"
                | "INVALID_CATALOG"
                | "INVALID_DISTRIBUTION_SOURCE"
                | "RESOURCE_LIMIT"
                | "NOT_AVAILABLE"
                | "INCOMPATIBLE_CORE"
        )
    {
        // These errors are created by the shared service with fixed safe messages.
        // Never expose an unrecognized provider error as a CLI result.
        return Err(failure.clone());
    }
    if snapshot.status == "invalid_response" {
        return Err(error(
            "INVALID_CATALOG",
            "The online catalog returned invalid metadata.",
        ));
    }
    if snapshot.stale || snapshot.status != "ready" {
        return Err(error(
            "DISTRIBUTION_UNAVAILABLE",
            "Refresh the online catalog before installing this plugin.",
        ));
    }
    item.and_then(|item| item.entry.as_ref()).ok_or_else(|| {
        error(
            "NOT_AVAILABLE",
            "The plugin is not currently available from the configured source.",
        )
    })
}

pub(crate) fn run(
    manager: &PluginManager,
    args: &[String],
    code: Option<&str>,
) -> Option<Result<Value, PluginError>> {
    if code.is_some() && !matches!(parse(args), Some(Ok(Command::SetCode))) {
        return Some(Err(error(
            "INVALID_REQUEST",
            "Sensitive input is only accepted for the access-code command.",
        )));
    }
    let parsed = parse(args)?;
    Some((|| {
        let command = parsed?;
        let configure = |request| {
            manager.configure_distribution(request).and_then(|view| {
                serde_json::to_value(view)
                    .map_err(|_| error("INTERNAL", "The distribution result could not be encoded."))
            })
        };
        match command {
            Command::Status => serde_json::to_value(manager.distribution_settings()?)
                .map_err(|_| error("INTERNAL", "The distribution result could not be encoded.")),
            Command::Source(source) => configure(DistributionConfigureRequest {
                source_url: Some(source.into()),
                access_code: None,
                clear_access_code: false,
            }),
            Command::ClearSource => configure(DistributionConfigureRequest {
                source_url: Some(String::new()),
                access_code: None,
                clear_access_code: false,
            }),
            Command::ClearCode => configure(DistributionConfigureRequest {
                source_url: None,
                access_code: None,
                clear_access_code: true,
            }),
            Command::SetCode => {
                let code = code.ok_or_else(|| {
                    error(
                        "INVALID_REQUEST",
                        "The access-code command requires its dedicated stdin payload.",
                    )
                })?;
                validate_access_code(code)?;
                configure(DistributionConfigureRequest {
                    source_url: None,
                    access_code: Some(code.into()),
                    clear_access_code: false,
                })
            }
            Command::Catalog => manager.block_on_distribution(async {
                tokio::time::timeout(COMMAND_TIMEOUT, manager.catalog_snapshot())
                    .await
                    .map_err(|_| {
                        error(
                            "DISTRIBUTION_UNAVAILABLE",
                            "The catalog request exceeded its time budget.",
                        )
                    })?
                    .and_then(|snapshot| {
                        serde_json::to_value(snapshot).map_err(|_| {
                            error("INTERNAL", "The catalog result could not be encoded.")
                        })
                    })
            }),
            Command::Install {
                id,
                update,
                mode,
                approve_source_change,
            } => {
                if update
                    && !manager
                        .snapshots()
                        .iter()
                        .any(|state| state.manifest.id == id)
                {
                    return Err(error(
                        "NOT_INSTALLED",
                        "The requested plugin is not installed.",
                    ));
                }
                manager.block_on_distribution(async {
                    tokio::time::timeout(COMMAND_TIMEOUT, async {
                        let snapshot = manager.catalog_snapshot().await?;
                        let entry = install_entry(&snapshot, id)?;
                        let states = manager.install_catalog_request(CatalogInstallRequest { plugin_id: id.into(), version: entry.version.clone(), expected_sha256: entry.sha256.clone(), approved_source_change: approve_source_change, mode, operation_id: None, expected_source_url: Some(entry.source_url.clone()) }).await?;
                        Ok(json!({ "plugins": states }))
                    }).await.map_err(|_| error("DISTRIBUTION_RESULT_PENDING", "The operation did not return within its time budget. Its result is unknown; check `plugins list` before retrying."))?
                })
            }
        }
    })())
}

#[cfg(test)]
mod tests {
    #[test]
    fn repair_and_rollback_flags_require_confirmation_and_cannot_be_combined() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };
        assert!(matches!(
            parse(&args(&[
                "plugins",
                "install-online",
                "editor",
                "--yes",
                "--repair"
            ])),
            Some(Ok(Command::Install {
                mode: CatalogInstallMode::Repair,
                ..
            }))
        ));
        assert!(matches!(
            parse(&args(&[
                "plugins",
                "install-online",
                "editor",
                "--yes",
                "--rollback"
            ])),
            Some(Ok(Command::Install {
                mode: CatalogInstallMode::Rollback,
                ..
            }))
        ));
        assert!(
            parse(&args(&["plugins", "install-online", "editor", "--repair"]))
                .unwrap()
                .is_err()
        );
        assert!(
            parse(&args(&[
                "plugins",
                "install-online",
                "editor",
                "--yes",
                "--repair",
                "--rollback"
            ]))
            .unwrap()
            .is_err()
        );
        assert!(
            parse(&args(&[
                "plugins",
                "update",
                "editor",
                "--yes",
                "--rollback"
            ]))
            .unwrap()
            .is_err()
        );
    }
    use super::*;
    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn sensitive_input_never_accepts_position_arguments_or_unconfirmed_mutations() {
        assert!(
            needs_access_code_stdin(&words("plugins distribution set-code --stdin --yes")).unwrap()
        );
        for input in [
            "plugins distribution set-code leaked-code --yes",
            "plugins distribution set-code --stdin",
            "plugins distribution set-code --stdin --yes --stdin",
        ] {
            let failure = needs_access_code_stdin(&words(input)).unwrap_err();
            assert!(!failure.message.contains("leaked-code"));
        }
        assert!(!needs_access_code_stdin(&words("plugins list")).unwrap());
    }

    #[test]
    fn stdin_is_bounded_and_rejects_eof_and_header_injection_without_echo() {
        assert_eq!(
            read_access_code(&b"safe-code\r\n"[..]).unwrap(),
            "safe-code"
        );
        assert_eq!(read_access_code(&b"safe-code"[..]).unwrap(), "safe-code");
        let mut boundary = vec![b'x'; MAX_ACCESS_CODE_BYTES];
        boundary.extend_from_slice(b"\r\n");
        assert_eq!(
            read_access_code(boundary.as_slice()).unwrap().len(),
            MAX_ACCESS_CODE_BYTES
        );
        for input in [
            Vec::new(),
            b"private-code\nAuthorization: other".to_vec(),
            b"private code".to_vec(),
            vec![b'x'; MAX_ACCESS_CODE_BYTES + 1],
            vec![0xff],
        ] {
            let failure = read_access_code(input.as_slice()).unwrap_err();
            assert!(!failure.message.contains("private"));
        }
    }

    fn snapshot(status: &str) -> PluginCatalogSnapshot {
        PluginCatalogSnapshot {
            source_url: Some("https://distribution.example.test".into()),
            generated_at: None,
            fetched_at: None,
            stale: status != "ready",
            status: status.into(),
            credential_state: "none".into(),
            retry_after_seconds: None,
            message: None,
            plugins: Vec::new(),
        }
    }

    #[test]
    fn install_preserves_rate_limit_and_metadata_errors_without_trusting_unknown_errors() {
        use crate::plugin_distribution::{PluginCatalogItem, PluginCatalogRegistration};
        let mut limited = snapshot("rate_limited");
        limited.retry_after_seconds = Some(17);
        let failure = install_entry(&limited, "editor").err().unwrap();
        assert_eq!(failure.code, "RATE_LIMITED");
        assert_eq!(failure.details, Some(json!({"retryAfterSeconds":17})));

        let mut invalid = snapshot("invalid_response");
        invalid.plugins.push(PluginCatalogItem {
            registration: PluginCatalogRegistration {
                id: "editor".into(),
                name: "Editor".into(),
                description: String::new(),
                author: String::new(),
                visibility: "public".into(),
                update_manifest_url: "https://distribution.example.test/v1/plugins/editor/update"
                    .into(),
                signing_public_key: "0".repeat(64),
            },
            entry: None,
            compatible: false,
            installed_version: None,
            installable: false,
            action: "unavailable".into(),
            stale: true,
            error: Some(error(
                "INVALID_UPDATE_SIGNATURE",
                "Fixed safe signature error.",
            )),
        });
        assert_eq!(
            install_entry(&invalid, "editor").err().unwrap().code,
            "INVALID_UPDATE_SIGNATURE"
        );
        invalid.plugins[0].error = Some(error(
            "UNKNOWN_PROVIDER_ERROR",
            "untrusted provider message",
        ));
        let failure = install_entry(&invalid, "editor").err().unwrap();
        assert_eq!(failure.code, "INVALID_CATALOG");
        assert!(!failure.message.contains("untrusted"));
        assert_eq!(
            install_entry(&snapshot("offline"), "editor")
                .err()
                .unwrap()
                .code,
            "DISTRIBUTION_UNAVAILABLE"
        );
    }
}
