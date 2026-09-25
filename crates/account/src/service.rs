//! 账号状态管理、凭据持久化与账号资料同步。

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use wonderland_kernel::logging::warn;
use wonderland_kernel::{
    Account, AccountService, AccountSnapshot, AccountStatus, KernelError, LoginCancelReason,
    LoginOutcome, StoredCookie,
};
use wonderland_mihoyo::MihoyoClient;

use crate::cookie::evaluate;
use crate::store::CredentialStore;

/// 基于文件系统的账号服务。
pub struct FsAccountService {
    store: CredentialStore,
    mihoyo: MihoyoClient,
    state: Mutex<State>,
}

struct State {
    status: AccountStatus,
    accounts: Vec<Account>,
    current: Option<String>,
    last_login_failure: Option<LoginCancelReason>,
}

impl FsAccountService {
    /// 从应用数据目录装载账号；目录不存在时以空状态启动。
    pub fn load(app_data_dir: PathBuf) -> Result<Self, KernelError> {
        let store = CredentialStore::new(&app_data_dir);
        let accounts = store.load_accounts();
        let current = store
            .current()
            .filter(|key| accounts.iter().any(|account| &account.account_key == key))
            .or_else(|| accounts.first().map(|account| account.account_key.clone()));
        let status = if current.is_some() {
            AccountStatus::LoggedIn
        } else {
            AccountStatus::LoggedOut
        };

        Ok(Self {
            store,
            mihoyo: MihoyoClient::new()?,
            state: Mutex::new(State {
                status,
                accounts,
                current,
                last_login_failure: None,
            }),
        })
    }
}

impl AccountService for FsAccountService {
    fn authed_get<'a>(
        &'a self,
        account_key: &'a str,
        path: &'a str,
        query: &'a [(String, String)],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, KernelError>> + Send + 'a>> {
        Box::pin(async move {
            if !self
                .lock()
                .accounts
                .iter()
                .any(|a| a.account_key == account_key)
            {
                return Err(KernelError::AccountNotFound(account_key.to_owned()));
            }
            // Restrict authenticated requests to validated paths on the fixed API host.
            if !path.starts_with('/')
                || path.starts_with("//")
                || path.contains("..")
                || !path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_-".contains(&b))
            {
                return Err(KernelError::InvalidInput);
            }
            let cookies = self.store.load_credentials(account_key)?;
            let header =
                wonderland_mihoyo::cookie_header_for(&cookies, "api-micreator.mihoyo.com", path);
            if header.is_empty() {
                return Err(KernelError::NotLoggedIn);
            }
            let bytes = wonderland_net::HttpClient::new()?
                .get_bytes(
                    &format!("{}{path}", wonderland_mihoyo::API_MICREATOR),
                    &[
                        ("cookie", &header),
                        ("accept", "application/json, text/plain, */*"),
                        ("origin", "https://act.mihoyo.com"),
                        ("referer", "https://act.mihoyo.com/"),
                    ],
                    query,
                )
                .await?;
            if !self
                .lock()
                .accounts
                .iter()
                .any(|a| a.account_key == account_key)
            {
                return Err(KernelError::AccountNotFound(account_key.to_owned()));
            }
            let envelope: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| KernelError::InvalidResponse)?;
            if envelope.get("retcode").and_then(|v| v.as_i64()) == Some(-100) {
                return Err(KernelError::SessionExpired);
            }
            Ok(bytes)
        })
    }

    fn snapshot(&self) -> AccountSnapshot {
        let state = self.lock();
        AccountSnapshot {
            status: state.status,
            current_account_key: state.current.clone(),
            accounts: state.accounts.clone(),
            last_login_failure: state.last_login_failure,
        }
    }

    fn begin_login(&self) -> Result<(), KernelError> {
        let mut state = self.lock();
        if state.status.is_logging_in() {
            return Err(KernelError::LoginInProgress);
        }
        state.status = AccountStatus::LoggingIn;
        state.last_login_failure = None;
        Ok(())
    }

    fn submit_login(&self, cookies: Vec<StoredCookie>) -> Result<LoginOutcome, KernelError> {
        if !self.lock().status.is_logging_in() {
            return Err(KernelError::NoLoginInProgress);
        }

        let Some(credentials) = evaluate(&cookies) else {
            return Ok(LoginOutcome::Pending);
        };

        self.store
            .save_credentials(&credentials.account_key, &cookies)?;

        let now = unix_now();
        let mut state = self.lock();
        match state
            .accounts
            .iter_mut()
            .find(|account| account.account_key == credentials.account_key)
        {
            Some(account) => {
                account.mid = credentials.mid.clone().or_else(|| account.mid.clone());
                account.updated_at = now;
            }
            None => state.accounts.push(Account {
                account_key: credentials.account_key.clone(),
                mid: credentials.mid.clone(),
                game_roles: Vec::new(),
                avatar_url: None,
                creator_level: None,
                creator_exp: None,
                creator_exp_total: None,
                updated_at: now,
            }),
        }
        state.current = Some(credentials.account_key.clone());
        state.status = AccountStatus::LoggedIn;

        self.store.save_accounts(&state.accounts)?;
        self.store.set_current(state.current.as_deref())?;

        Ok(LoginOutcome::Completed {
            account_key: credentials.account_key,
        })
    }

    fn sync_account_info<'a>(
        &'a self,
        account_key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Account, KernelError>> + Send + 'a>> {
        Box::pin(async move {
            let cookies = self.store.load_credentials(account_key)?;
            if cookies.is_empty() {
                return Err(KernelError::NotLoggedIn);
            }

            let roles = self.mihoyo.game_roles(&cookies).await?;
            // Network requests complete before locking account state.
            let profile = match roles.first() {
                Some(role) => match self
                    .mihoyo
                    .creator_profile(&cookies, &role.uid, &role.region)
                    .await
                {
                    Ok(profile) => Some(profile),
                    Err(error) => {
                        warn!(code = error.code(), reason = %error, "创作者资料拉取失败");
                        None
                    }
                },
                None => None,
            };

            let mut state = self.lock();
            let account = state
                .accounts
                .iter_mut()
                .find(|account| account.account_key == account_key)
                .ok_or_else(|| KernelError::AccountNotFound(account_key.to_owned()))?;
            account.game_roles = roles;
            if let Some(profile) = profile {
                if profile.avatar_url.is_some() {
                    account.avatar_url = profile.avatar_url;
                }
                account.creator_level = profile.level;
                account.creator_exp = profile.exp;
                account.creator_exp_total = profile.exp_total;
            }
            account.updated_at = unix_now();
            let updated = account.clone();
            self.store.save_accounts(&state.accounts)?;
            Ok(updated)
        })
    }

    fn cancel_login(&self, reason: LoginCancelReason) {
        let mut state = self.lock();
        if !state.status.is_logging_in() {
            return;
        }
        state.status = if state.current.is_some() {
            AccountStatus::LoggedIn
        } else {
            AccountStatus::LoggedOut
        };
        state.last_login_failure = Some(reason);
    }

    fn remove_account(&self, account_key: &str) -> Result<(), KernelError> {
        let mut state = self.lock();
        let index = state
            .accounts
            .iter()
            .position(|account| account.account_key == account_key)
            .ok_or_else(|| KernelError::AccountNotFound(account_key.to_owned()))?;

        self.store.delete_credentials(account_key)?;
        state.accounts.remove(index);
        if state.current.as_deref() == Some(account_key) {
            state.current = state
                .accounts
                .first()
                .map(|account| account.account_key.clone());
        }
        state.status = if state.current.is_some() {
            AccountStatus::LoggedIn
        } else {
            AccountStatus::LoggedOut
        };

        self.store.save_accounts(&state.accounts)?;
        self.store.set_current(state.current.as_deref())?;
        Ok(())
    }

    fn switch_account(&self, account_key: &str) -> Result<(), KernelError> {
        let mut state = self.lock();
        if !state
            .accounts
            .iter()
            .any(|account| account.account_key == account_key)
        {
            return Err(KernelError::AccountNotFound(account_key.to_owned()));
        }
        state.current = Some(account_key.to_owned());
        state.status = AccountStatus::LoggedIn;
        self.store.set_current(state.current.as_deref())
    }
}

impl FsAccountService {
    /// 锁中毒也继续工作：账号状态不可因一次 panic 而整体失效。
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}
