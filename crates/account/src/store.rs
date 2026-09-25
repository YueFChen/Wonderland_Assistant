//! 账号数据分区：账号元数据可读，凭据本体加密。
//!
//! ```text
//! <app_data>/account/
//!   accounts.json          # 账号元数据（可读，不含凭据）
//!   current                # 当前账号主键
//!   credentials/<key>.bin  # DPAPI 加密后的 cookie 快照
//! ```

use std::fs;
use std::path::{Path, PathBuf};

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

    pub fn delete_credentials(&self, account_key: &str) -> Result<(), KernelError> {
        let path = self.credential_path(account_key);
        if path.exists() {
            fs::remove_file(path).map_err(store_error)?;
        }
        Ok(())
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

    pub fn save_accounts(&self, accounts: &[Account]) -> Result<(), KernelError> {
        let bytes = serde_json::to_vec_pretty(&AccountsFile {
            accounts: accounts.to_vec(),
        })
        .map_err(store_error)?;
        fs::create_dir_all(&self.root).map_err(store_error)?;
        write_atomic(&self.accounts_path(), &bytes)
    }

    pub fn current(&self) -> Option<String> {
        fs::read_to_string(self.current_path())
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    pub fn set_current(&self, account_key: Option<&str>) -> Result<(), KernelError> {
        fs::create_dir_all(&self.root).map_err(store_error)?;
        match account_key {
            Some(key) => write_atomic(&self.current_path(), key.as_bytes()),
            None => {
                let path = self.current_path();
                if path.exists() {
                    fs::remove_file(path).map_err(store_error)?;
                }
                Ok(())
            }
        }
    }
}

/// 使用临时文件替换目标文件。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), KernelError> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes).map_err(store_error)?;
    if path.exists() {
        fs::remove_file(path).map_err(store_error)?;
    }
    fs::rename(&temp, path).map_err(store_error)
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
