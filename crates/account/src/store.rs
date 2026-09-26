//! 账号数据分区：账号元数据可读，凭据本体加密。
//!
//! ```text
//! <app_data>/account/
//!   accounts.json          # 账号元数据与当前账号（可读，不含凭据）
//!   current                # 旧版本当前账号记录，仅用于迁移读取
//!   credentials/<key>.bin  # DPAPI 加密后的 cookie 快照
//! ```

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use wonderland_kernel::logging::warn;
use wonderland_kernel::{Account, KernelError, StoredCookie};

/// 账号凭据与元数据的读写入口。
pub struct CredentialStore {
    root: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AccountsFile {
    accounts: Vec<Account>,
    /// The selected account is stored with the account list so a single file replacement commits
    /// both pieces of state together. `None` means the file predates this field.
    #[serde(default)]
    current_account: Option<String>,
}

impl CredentialStore {
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            root: app_data_dir.join("account"),
        }
    }

    fn credentials_dir(&self) -> PathBuf {
        self.root.join("credentials")
    }

    fn accounts_path(&self) -> PathBuf {
        self.root.join("accounts.json")
    }

    fn current_path(&self) -> PathBuf {
        self.root.join("current")
    }

    fn credential_path(&self, account_key: &str) -> PathBuf {
        self.credentials_dir()
            .join(format!("{}.bin", sanitize(account_key)))
    }

    /// 写入加密凭据。
    pub fn save_credentials(
        &self,
        account_key: &str,
        cookies: &[StoredCookie],
    ) -> Result<(), KernelError> {
        let plain = serde_json::to_vec(cookies).map_err(store_error)?;
        let sealed = wonderland_secret::seal(&plain).map_err(crypto_error)?;
        fs::create_dir_all(self.credentials_dir()).map_err(store_error)?;
        write_atomic(&self.credential_path(account_key), &sealed)
    }

    /// 读回凭据；解密失败说明文件被破坏或换了 Windows 用户。
    pub fn load_credentials(&self, account_key: &str) -> Result<Vec<StoredCookie>, KernelError> {
        let sealed = fs::read(self.credential_path(account_key)).map_err(store_error)?;
        let plain = wonderland_secret::unseal(&sealed).map_err(crypto_error)?;
        serde_json::from_slice(&plain).map_err(store_error)
    }

    /// 读取账号元数据；文件缺失或损坏时返回空列表。
    pub fn load_accounts(&self) -> Vec<Account> {
        let Ok(bytes) = fs::read(self.accounts_path()) else {
            return Vec::new();
        };
        match serde_json::from_slice::<AccountsFile>(&bytes) {
            Ok(file) => file.accounts,
            Err(error) => {
                warn!(
                    reason = %error,
                    path = %self.accounts_path().display(),
                    "账号元数据解析失败，按空列表启动"
                );
                Vec::new()
            }
        }
    }

    pub fn current(&self) -> Option<String> {
        if let Ok(bytes) = fs::read(self.accounts_path())
            && let Ok(file) = serde_json::from_slice::<AccountsFile>(&bytes)
            && let Some(current) = file.current_account
        {
            return (!current.is_empty()).then_some(current);
        }

        // Read the former sidecar for accounts.json files written before current_account existed.
        fs::read_to_string(self.current_path())
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    pub fn save_account_state(
        &self,
        accounts: &[Account],
        current_account: Option<&str>,
    ) -> Result<(), KernelError> {
        let bytes = serde_json::to_vec_pretty(&AccountsFile {
            accounts: accounts.to_vec(),
            // Empty string represents a committed logged-out state. A missing field remains the
            // marker for a legacy file that should consult the old sidecar.
            current_account: Some(current_account.unwrap_or_default().to_owned()),
        })
        .map_err(store_error)?;
        fs::create_dir_all(&self.root).map_err(store_error)?;
        write_atomic(&self.accounts_path(), &bytes)?;
        let _ = fs::remove_file(self.current_path());
        Ok(())
    }

    /// Remove credentials and account metadata as one recoverable operation. The credential is
    /// first moved aside; startup restores it if the account record still exists, or cleans it up
    /// if the account state was committed before the process stopped.
    pub fn remove_account_state(
        &self,
        account_key: &str,
        accounts: &[Account],
        current_account: Option<&str>,
    ) -> Result<(), KernelError> {
        let credential = self.credential_path(account_key);
        let staged = credential.with_file_name(format!(
            "{}.removing",
            credential.file_name().unwrap_or_default().to_string_lossy()
        ));
        let has_credential = credential.exists();
        if has_credential {
            if staged.exists() {
                return Err(KernelError::CredentialStore(
                    "账号凭据清理正在等待恢复；请重启后重试".to_owned(),
                ));
            }
            fs::rename(&credential, &staged).map_err(store_error)?;
        }

        if let Err(error) = self.save_account_state(accounts, current_account) {
            if has_credential {
                let _ = fs::rename(&staged, &credential);
            }
            return Err(error);
        }

        if has_credential && let Err(error) = fs::remove_file(&staged) {
            warn!(reason = %error, "账号已移除，凭据临时文件将在下次启动清理");
        }
        Ok(())
    }

    pub fn recover_pending_removals(&self, accounts: &[Account]) {
        let Ok(bytes) = fs::read(self.accounts_path()) else {
            return;
        };
        if serde_json::from_slice::<AccountsFile>(&bytes).is_err() {
            return;
        }
        let Ok(entries) = fs::read_dir(self.credentials_dir()) else {
            return;
        };
        let active_keys = accounts
            .iter()
            .map(|account| format!("{}.bin", sanitize(&account.account_key)))
            .collect::<std::collections::HashSet<_>>();
        for entry in entries.flatten() {
            let staged = entry.path();
            if staged.extension().and_then(|extension| extension.to_str()) != Some("removing") {
                continue;
            }
            let Some(original_name) = staged.file_stem() else {
                continue;
            };
            let original_name = original_name.to_string_lossy().into_owned();
            let original = staged.with_file_name(&original_name);
            if active_keys.contains(&original_name) && !original.exists() {
                if let Err(error) = fs::rename(&staged, original) {
                    warn!(reason = %error, "无法恢复账号凭据");
                }
            } else if let Err(error) = fs::remove_file(staged) {
                warn!(reason = %error, "无法清理已移除账号的凭据");
            }
        }
    }
}

/// 使用临时文件替换目标文件。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), KernelError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("{}.{}.tmp", std::process::id(), nonce));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(store_error)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(store_error(error));
    }
    drop(file);
    if let Err(error) = replace_file(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(store_error(error));
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

/// 账号主键会成为文件名，只保留安全字符。
fn sanitize(account_key: &str) -> String {
    let cleaned: String = account_key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

/// 账号分区读写错误，不包含凭据内容。
fn store_error(error: impl std::fmt::Display) -> KernelError {
    warn!(reason = %error, "账号存储读写失败");
    KernelError::CredentialStore(error.to_string())
}

/// DPAPI 加解密失败：同样只带底层原因。
fn crypto_error(error: String) -> KernelError {
    warn!(reason = %error, "凭据加解密失败");
    KernelError::CredentialStore(error)
}
