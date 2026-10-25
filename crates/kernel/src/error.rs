use std::fmt;

use serde::{Serialize, Serializer};

/// 内核统一错误。
///
/// 跨 IPC 错误使用此类型；前端根据 `code` 判别并展示 `message` 和 `details`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    Timeout,
    Connection,
    Http(u16),
    /// A response or local payload exceeded a Core-enforced size limit.
    ResourceLimit,
    /// 官方接口返回的业务错误码，以及官方附带的可读文案（可能为空）。
    Business {
        code: i64,
        message: String,
    },
    InvalidResponse,
    InvalidInput,
    PluginDisabled,
    /// 本地归档读写失败；`String` 是底层原因。
    Archive(String),
    /// 应用设置读写失败；`String` 是底层原因。
    Settings(String),
    /// 本地留存的数据不可用：版本不匹配、结构损坏或与请求范围不符。
    ///
    /// 与 [`Self::InvalidResponse`] 的区别：后者指**官方接口**返回的结构不符合预期。
    LocalData(String),
    Busy,
    /// 内核未完成初始化。
    NotInitialized,
    /// 网络或接口错误。
    Transport(String),
    /// 其它错误。
    Other(String),
}

impl KernelError {
    /// 稳定的错误码：供前端判别，不随文案变化。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::Http(_) => "http",
            Self::ResourceLimit => "resource_limit",
            Self::Business { .. } => "business",
            Self::InvalidResponse => "invalid_response",
            Self::InvalidInput => "invalid_input",
            Self::PluginDisabled => "plugin_disabled",
            Self::Archive(_) => "archive",
            Self::Settings(_) => "settings",
            Self::LocalData(_) => "local_data",
            Self::Busy => "busy",
            Self::NotInitialized => "not_initialized",
            Self::Transport(_) => "transport",
            Self::Other(_) => "other",
        }
    }

    /// 补充信息：失败原因或上下文，不含任何凭据。
    pub fn details(&self) -> Option<String> {
        match self {
            Self::Http(code) => Some(code.to_string()),
            Self::Business { code, .. } => Some(code.to_string()),
            Self::Archive(detail) | Self::Settings(detail) | Self::LocalData(detail) => {
                Some(detail.clone())
            }
            Self::Transport(detail) | Self::Other(detail) => Some(detail.clone()),
            _ => None,
        }
    }
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "请求超时，请重试"),
            Self::Connection => write!(f, "网络连接失败，请重试"),
            Self::Http(code) => write!(f, "服务返回 HTTP {code}"),
            Self::ResourceLimit => write!(f, "响应超过 Core 支持的大小限制"),
            Self::Business { code, message } => {
                if message.is_empty() {
                    write!(f, "官方接口返回业务码 {code}")
                } else {
                    write!(f, "官方接口返回业务码 {code}：{message}")
                }
            }
            Self::InvalidResponse => write!(f, "响应数据结构无效"),
            Self::InvalidInput => write!(f, "请求参数无效"),
            Self::PluginDisabled => write!(f, "插件本次启动未启用"),
            Self::Archive(detail) => write!(f, "本地归档读写失败：{detail}"),
            Self::Settings(detail) => write!(f, "应用设置读写失败：{detail}"),
            Self::LocalData(detail) => write!(f, "本地留存数据不可用：{detail}"),
            Self::Busy => write!(f, "已有操作正在进行"),
            Self::NotInitialized => write!(f, "内核尚未完成初始化"),
            Self::Transport(detail) => write!(f, "网络请求失败：{detail}"),
            Self::Other(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for KernelError {}

/// 跨 IPC 的错误载荷：`code` 稳定、`message` 可读、`details` 可选。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct ErrorPayload {
    pub code: &'static str,
    pub message: String,
    pub details: Option<String>,
}

impl From<&KernelError> for ErrorPayload {
    fn from(error: &KernelError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
            details: error.details(),
        }
    }
}

impl From<KernelError> for ErrorPayload {
    fn from(error: KernelError) -> Self {
        Self::from(&error)
    }
}

impl Serialize for KernelError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ErrorPayload::from(self).serialize(serializer)
    }
}
