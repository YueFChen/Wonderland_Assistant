use std::fmt;

/// Stable error categories used by plugin business libraries and the protocol adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginFailure {
    SessionExpired,
    Timeout,
    Connection,
    Http(u16),
    Business { code: i64, message: String },
    InvalidResponse,
    InvalidInput,
    PluginDisabled,
    Archive(String),
    Settings(String),
    LocalData(String),
    Busy,
    NotInitialized,
    NotLoggedIn,
    LoginInProgress,
    NoLoginInProgress,
    CredentialStore(String),
    LoginWindow(String),
    Transport(String),
    AccountNotFound(String),
    Other(String),
}

impl PluginFailure {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SessionExpired => "session_expired",
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::Http(_) => "http",
            Self::Business { .. } => "business",
            Self::InvalidResponse => "invalid_response",
            Self::InvalidInput => "invalid_input",
            Self::PluginDisabled => "plugin_disabled",
            Self::Archive(_) => "archive",
            Self::Settings(_) => "settings",
            Self::LocalData(_) => "local_data",
            Self::Busy => "busy",
            Self::NotInitialized => "not_initialized",
            Self::NotLoggedIn => "not_logged_in",
            Self::LoginInProgress => "login_in_progress",
            Self::NoLoginInProgress => "no_login_in_progress",
            Self::CredentialStore(_) => "credential_store",
            Self::LoginWindow(_) => "login_window",
            Self::Transport(_) => "transport",
            Self::AccountNotFound(_) => "account_not_found",
            Self::Other(_) => "other",
        }
    }
}

impl fmt::Display for PluginFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionExpired => write!(f, "登录已过期，请重新登录；本地归档仍可查看"),
            Self::Timeout => write!(f, "请求超时，请重试"),
            Self::Connection => write!(f, "网络连接失败，请重试"),
            Self::Http(code) => write!(f, "服务返回 HTTP {code}"),
            Self::Business { code, message } => {
                if message.is_empty() {
                    write!(f, "官方接口返回业务码 {code}")
                } else {
                    write!(f, "官方接口返回业务码 {code}：{message}")
                }
            }
            Self::InvalidResponse => write!(f, "官方接口数据结构已变化或分页不完整"),
            Self::InvalidInput => write!(f, "请求参数无效或角色不属于此账号"),
            Self::PluginDisabled => write!(f, "插件本次启动未启用"),
            Self::Archive(detail) => write!(f, "本地归档读写失败：{detail}"),
            Self::Settings(detail) => write!(f, "应用设置读写失败：{detail}"),
            Self::LocalData(detail) => write!(f, "本地留存的数据不可用：{detail}"),
            Self::Busy => write!(f, "已有操作正在进行"),
            Self::NotInitialized => write!(f, "插件服务尚未完成初始化"),
            Self::NotLoggedIn => write!(f, "尚未登录米哈游通行证"),
            Self::LoginInProgress => write!(f, "已有登录流程正在进行"),
            Self::NoLoginInProgress => write!(f, "当前没有进行中的登录流程"),
            Self::CredentialStore(detail) => write!(f, "凭据读写失败：{detail}"),
            Self::LoginWindow(detail) => write!(f, "登录窗口不可用：{detail}"),
            Self::Transport(detail) => write!(f, "网络请求失败：{detail}"),
            Self::AccountNotFound(key) => write!(f, "账号不存在：{key}"),
            Self::Other(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for PluginFailure {}
