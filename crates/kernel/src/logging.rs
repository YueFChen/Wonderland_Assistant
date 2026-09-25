//! 日志级别、配置类型、敏感值包装与日志宏出口。

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 日志级别。
///
/// 序列化名称同时用于设置文件、前端绑定和过滤指令。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub enum LogLevel {
    /// 仅错误。
    Error,
    /// 错误与警告。
    Warn,
    /// 错误、警告与信息；发布构建的默认值。
    Info,
    /// 以上再加调试细节；开发构建的默认值。
    Debug,
    /// 最详细，含逐页进度等高频事件。
    Trace,
}

impl LogLevel {
    /// 过滤指令文本，与序列化名同源：实现层不必再抄一份映射。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// 从未被用户设置过时的默认级别：只由构建档位决定。
///
/// `release` 决定发布构建默认级别，其他构建默认使用 `Debug`。
fn default_level(debug_assertions: bool) -> LogLevel {
    if debug_assertions {
        LogLevel::Debug
    } else {
        LogLevel::Info
    }
}

/// 日志设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct LogSettings {
    /// 唯一用户可配项；保留策略与旋转周期是内核常量，不进设置页。
    pub level: LogLevel,
}

impl Default for LogSettings {
    /// 与 `#[serde(default)]` 同源：字段缺失时走这里。档位只决定「从未设置过」时的起点，
    /// 用户改过之后以 `settings.json` 的持久值为准。
    fn default() -> Self {
        Self {
            level: default_level(cfg!(debug_assertions)),
        }
    }
}

/// 日志目录：`<app_data>/logs`，与 `settings.json`、账号分区并列。
pub fn log_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("logs")
}

/// 日志保留天数；保留文件数为该值加一。
pub const LOG_RETAIN_DAYS: usize = 14;

/// 轮转文件名前缀，完整形态为 `wonderland-assistant.log.<YYYY-MM-DD>`。
pub const LOG_FILE_PREFIX: &str = "wonderland-assistant.log";

/// 敏感值包装类型；`Debug` 和 `Display` 输出占位文本。
///
/// 用于 cookie 值、授权头和口令等敏感数据。该类型不实现 `Serialize`。
///
/// 该类型不实现 `Serialize`，避免凭据进入 IPC 或设置文件。
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// 包住一个凭据。
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// 显式取值；只在真正需要凭据的调用点使用。
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// 脱敏占位文本。
const REDACTED: &str = "<redacted>";

pub use tracing::{debug, error, info, instrument, trace, warn};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_level_follows_build_profile() {
        assert_eq!(default_level(true), LogLevel::Debug);
        assert_eq!(default_level(false), LogLevel::Info);
        assert_eq!(
            LogSettings::default().level,
            default_level(cfg!(debug_assertions))
        );
    }

    #[test]
    fn settings_default_when_field_missing() {
        let parsed: LogSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, LogSettings::default());
        assert_eq!(parsed.level, default_level(cfg!(debug_assertions)));
        let pinned: LogSettings = serde_json::from_str(r#"{"level":"trace"}"#).unwrap();
        assert_eq!(pinned.level, LogLevel::Trace);
    }

    #[test]
    fn level_round_trips_as_snake_case() {
        for (level, name) in [
            (LogLevel::Error, "error"),
            (LogLevel::Warn, "warn"),
            (LogLevel::Info, "info"),
            (LogLevel::Debug, "debug"),
            (LogLevel::Trace, "trace"),
        ] {
            let json = serde_json::to_string(&level).unwrap();
            assert_eq!(json, format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<LogLevel>(&json).unwrap(), level);
            assert_eq!(level.as_str(), name);
        }
    }

    #[test]
    fn dir_and_limits_are_fixed_by_contract() {
        assert_eq!(log_dir(Path::new("data")), Path::new("data").join("logs"));
        assert_eq!(LOG_RETAIN_DAYS, 14);
        assert_eq!(LOG_FILE_PREFIX, "wonderland-assistant.log");
    }

    #[test]
    fn secret_never_prints_its_value() {
        let secret = Secret::new("ltoken=abc123".to_owned());
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{secret}"), "<redacted>");
        assert!(!format!("{secret:?}").contains("abc123"));
        assert!(!format!("{secret}").contains("abc123"));
        assert_eq!(secret.expose(), "ltoken=abc123");
    }
}
