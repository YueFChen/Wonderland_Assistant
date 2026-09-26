use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State, WebviewWindow};
use tauri_plugin_updater::{Update, UpdaterExt};
use wonderland_kernel::KernelError;

use crate::commands::ensure_main_window;

const PROGRESS_EVENT: &str = "core-update-progress";

#[derive(Default)]
pub struct CoreUpdateState {
    updates: Mutex<HashMap<String, Update>>,
}

impl CoreUpdateState {
    pub fn clear(&self) {
        if let Ok(mut updates) = self.updates.lock() {
            updates.clear();
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreUpdateAvailable {
    pub update_id: String,
    pub version: String,
    pub date: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreUpdateProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub finished: bool,
}

#[tauri::command]
pub async fn core_update_check(
    app: AppHandle,
    window: WebviewWindow,
    updates: State<'_, CoreUpdateState>,
) -> Result<Option<CoreUpdateAvailable>, KernelError> {
    ensure_main_window(&window)?;
    let proxy = wonderland_net::proxy::current_proxy()
        .map_err(|_| KernelError::Transport("代理设置暂不可用".to_owned()))?
        .configuration;
    let updater = app
        .updater_builder()
        .timeout(Duration::from_secs(15))
        .configure_client(move |builder| proxy.apply_to(builder))
        .build()
        .map_err(|_| KernelError::Transport("更新检查客户端初始化失败".to_owned()))?;
    let update = updater.check().await.map_err(|_| {
        KernelError::Transport("更新检查失败；请检查 Core 网络代理设置后重试".to_owned())
    })?;

    let Some(update) = update else {
        updates.clear();
        return Ok(None);
    };
    let update_id = new_update_id()?;
    let response = CoreUpdateAvailable {
        update_id: update_id.clone(),
        version: update.version.clone(),
        date: update.date.map(|date| date.to_string()),
        body: update
            .body
            .as_deref()
            .map(|body| body.chars().take(20_000).collect()),
    };
    updates
        .updates
        .lock()
        .map_err(|_| KernelError::Transport("更新状态暂不可用".to_owned()))?
        .insert(update_id, update);
    Ok(Some(response))
}

#[tauri::command]
pub async fn core_update_install(
    app: AppHandle,
    window: WebviewWindow,
    updates: State<'_, CoreUpdateState>,
    update_id: String,
) -> Result<(), KernelError> {
    ensure_main_window(&window)?;
    if update_id.len() != 32 || !update_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(KernelError::InvalidInput);
    }
    let update = updates
        .updates
        .lock()
        .map_err(|_| KernelError::Transport("更新状态暂不可用".to_owned()))?
        .remove(&update_id)
        .ok_or(KernelError::InvalidInput)?;

    let progress_app = app.clone();
    let mut downloaded = 0u64;
    update
        .download_and_install(
            move |chunk_size, total_bytes| {
                downloaded = downloaded.saturating_add(chunk_size as u64);
                let _ = progress_app.emit(
                    PROGRESS_EVENT,
                    CoreUpdateProgress {
                        downloaded_bytes: downloaded,
                        total_bytes,
                        finished: false,
                    },
                );
            },
            move || {
                let _ = app.emit(
                    PROGRESS_EVENT,
                    CoreUpdateProgress {
                        downloaded_bytes: 0,
                        total_bytes: None,
                        finished: true,
                    },
                );
            },
        )
        .await
        .map_err(|_| {
            KernelError::Transport("更新下载、签名校验或安装失败；请检查网络后重试".to_owned())
        })
}

fn new_update_id() -> Result<String, KernelError> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| KernelError::Transport("无法创建更新会话".to_owned()))?;
    Ok(hex::encode(bytes))
}
