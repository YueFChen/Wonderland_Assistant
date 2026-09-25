use serde::{Deserialize, Serialize};

/// Read-only account data returned by the Core account capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    LoggedOut,
    LoggingIn,
    LoggedIn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRoleSummary {
    pub uid: String,
    pub region: String,
    pub region_name: String,
    pub nickname: String,
    pub level: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSummary {
    pub account_key: String,
    pub game_roles: Vec<GameRoleSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub status: AccountStatus,
    pub current_account_key: Option<String>,
    pub accounts: Vec<AccountSummary>,
    pub last_login_failure: Option<String>,
}
