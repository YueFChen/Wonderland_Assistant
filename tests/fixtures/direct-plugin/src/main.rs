use std::path::Path;

use serde_json::{Value, json};
use wonderland_plugin_sdk::{PluginError, serve_with_startup};

fn main() {
    let data_dir = std::env::var_os("WONDERLAND_PLUGIN_DATA_DIR")
        .expect("fixture requires its isolated plugin data directory");
    let plugin_id = match Path::new(&data_dir)
        .file_name()
        .and_then(|name| name.to_str())
    {
        Some("a") => "a",
        Some("b") => "b",
        Some("c") => "c",
        Some("blocked") => "blocked",
        _ => panic!("fixture data directory must end in a, b, c or blocked"),
    };
    let data = std::path::PathBuf::from(&data_dir);
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join(format!("started-{}", std::process::id())),
        b"fixture process",
    )
    .unwrap();
    while data.join("pause-before-hello").exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if plugin_id == "blocked" {
        stop_reading_after_handshake(Path::new(&data_dir));
    }
    let startup_data = data.clone();
    let background = std::sync::Arc::new(std::sync::OnceLock::new());
    let startup_host = background.clone();
    let result = serve_with_startup(
        plugin_id,
        "0.0.0",
        include_str!("../contract.json"),
        move |host| {
            if startup_data.join("startup-direct").exists() {
                let error = host.call_plugin("b", "health", json!({})).unwrap_err();
                std::fs::write(startup_data.join("startup-direct-denied"), error.code).unwrap();
            }
            let manifest: Value = serde_json::from_slice(
                &std::fs::read(
                    std::env::current_exe()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("manifest.json"),
                )
                .unwrap(),
            )
            .unwrap();
            if manifest["backend"]["activation"] == "background_restore" {
                let required = std::fs::read_to_string(startup_data.join("restore-state"))
                    .unwrap_or_default()
                    == "true";
                host.set_background_restore(required)
                    .expect("fixture startup restore acknowledgement");
            }
            let _ = startup_host.set(host);
        },
        move |host, method, params, _request_id| {
            match method.as_str() {
                "restore" => {
                    let required = params["required"].as_bool().unwrap();
                    if required {
                        host.set_background_restore(true)?;
                    }
                    std::fs::write(data.join("restore-state"), required.to_string()).unwrap();
                    if !required {
                        host.set_background_restore(false)?;
                    }
                    return Ok(json!({"required":required}));
                }
                "start_task" => {
                    let activity = host.begin_activity("fixture detached task")?;
                    let path = data.clone();
                    let milliseconds = params["milliseconds"].as_u64().unwrap();
                    std::thread::spawn(move || {
                        std::fs::write(path.join("task-started"), b"running").unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
                        std::fs::write(path.join("task-finished"), b"done").unwrap();
                        let _ = activity.finish();
                    });
                    return Ok(json!({"started":true}));
                }
                "defer_task" => {
                    let background = background.get().unwrap().clone();
                    let path = data.clone();
                    std::thread::spawn(move || {
                        while !path.join("admit-deferred").exists() {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        match background.begin_activity("deferred fixture task") {
                            Ok(_activity) => {
                                std::fs::write(path.join("deferred-started"), b"work").unwrap();
                            }
                            Err(error) => {
                                std::fs::write(path.join("deferred-denied"), error.code).unwrap();
                            }
                        }
                    });
                    return Ok(json!({"queued":true}));
                }
                "health" => return Ok(json!(plugin_id)),
                "large" => return Ok(json!("accepted")),
                "pick" => return host.call_core("core.files.pick", json!({})),
                "browser" => return host.call_core("core.browser.open", params),
                "browser_session_probe" => {
                    let session = host.call_core(
                        "core.browser.session.open",
                        json!({
                            "url":params["url"], "profile":"popup-regression", "reset":true,
                            "title":"Browser session regression", "width":800, "height":600
                        }),
                    )?;
                    let result = (|| {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(35);
                        loop {
                            let cookies =
                                host.call_core("core.browser.session.cookies", session.clone())?;
                            let shared = ["popup_direct", "popup_blank", "popup_nested"]
                                .iter()
                                .all(|name| {
                                    cookies.as_array().is_some_and(|cookies| {
                                        cookies.iter().any(|cookie| {
                                            cookie["name"] == *name && cookie["value"] == "1"
                                        })
                                    })
                                });
                            if shared {
                                return Ok(json!({"sharedCookieCount":3}));
                            }
                            if std::time::Instant::now() >= deadline {
                                return Err(PluginError::new(
                                    "PROBE_TIMEOUT",
                                    "Popup cookies did not reach the root session.",
                                ));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    })();
                    host.call_core("core.browser.session.close", session.clone())?;
                    let closed = host
                        .call_core("core.browser.session.cookies", session)
                        .unwrap_err();
                    if closed.code != "BROWSER_SESSION_CLOSED" {
                        return Err(closed);
                    }
                    return result;
                }
                "delay" => {
                    std::thread::sleep(std::time::Duration::from_millis(
                        params["milliseconds"].as_u64().unwrap_or(0),
                    ));
                    return Ok(json!("finished"));
                }
                "bad_result" => return Ok(json!(42)),
                "direct" => {
                    let target = params["pluginId"]
                        .as_str()
                        .ok_or_else(|| PluginError::new("INVALID_INPUT", "Missing target."))?;
                    let method = params["method"]
                        .as_str()
                        .ok_or_else(|| PluginError::new("INVALID_INPUT", "Missing method."))?;
                    return match params["timeoutMs"].as_u64() {
                        Some(timeout) => host.call_plugin_with_timeout(
                            target,
                            method,
                            params["params"].clone(),
                            timeout,
                        ),
                        None => host.call_plugin(target, method, params["params"].clone()),
                    };
                }
                "loop" => {
                    return host.call_plugin(
                        match plugin_id {
                            "a" => "b",
                            "b" => "c",
                            _ => "a",
                        },
                        "loop",
                        json!({}),
                    );
                }
                "echo" => {}
                _ => {
                    return Err(PluginError::new(
                        "METHOD_NOT_FOUND",
                        "Unknown fixture method.",
                    ));
                }
            }
            match plugin_id {
                "a" => host.call_plugin("b", "echo", params),
                "b" => host.call_plugin("c", "echo", params),
                _ => params
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| json!(text))
                    .ok_or_else(|| PluginError::new("INVALID_INPUT", "Missing fixture text.")),
            }
        },
    );
    if let Err(error) = result {
        eprintln!("fixture protocol stopped: {error}");
        std::process::exit(1);
    }
}

/// This owned test child acknowledges only the host hello, then leaves stdin unread.
fn stop_reading_after_handshake(data_dir: &Path) -> ! {
    use std::io::{BufRead, Write};
    let mut hello = String::new();
    std::io::stdin().lock().read_line(&mut hello).unwrap();
    let hello: Value = serde_json::from_str(&hello).unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["pluginId"], "blocked");
    let executable = std::env::current_exe().unwrap();
    let checksums: Value = serde_json::from_slice(
        &std::fs::read(
            executable
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("checksums.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let hash = checksums["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "contract.json")
        .unwrap()["sha256"]
        .as_str()
        .unwrap();
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::write(data_dir.join("stdin-paused"), b"owned fixture ready").unwrap();
    println!(
        "{}",
        json!({"protocol":"wonderland-plugin", "version":"1.0.0", "type":"hello",
            "role":"plugin", "pluginId":"blocked", "pluginVersion":"0.0.0", "contractSha256":hash})
    );
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
