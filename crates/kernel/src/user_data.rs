//! 用户数据目录管理契约。
//!
//! 路径选择与迁移属于内核能力：所有插件只消费 [`crate::AppContext::app_data_dir`]，
//! 不各自保存路径。具体文件迁移由无 Tauri 依赖的实现 crate 完成。

use crate::KernelError;
use serde::{Deserialize, Serialize};

#[cfg(feature = "bindings")]
use ts_rs::TS;

/// 当前用户数据目录的来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(TS))]
#[cfg_attr(feature = "bindings", ts(export))]
pub enum UserDataLocationKind {
    Default,
    Custom,
}

/// 设置页展示的用户数据目录状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[cfg_attr(feature = "bindings", ts(export))]
pub struct UserDataState {
    pub active_path: String,
    pub default_path: String,
    pub location: UserDataLocationKind,
    /// 已预约、将在下次启动执行的目标目录。
    pub pending_path: Option<String>,
    /// 最近一次成功迁移的目标目录，供设置页确认结果。
    pub last_migration_path: Option<String>,
    /// 迁移成功后未能清理的旧目录；新目录已激活，旧路径可能只剩部分文件。
    pub last_migration_source: Option<String>,
    /// 迁移已完成但旧目录清理未完全成功时的提示。
    pub last_migration_warning: Option<String>,
    /// 上次启动迁移失败的原因；失败时仍从原目录启动，不丢数据。
    pub last_migration_error: Option<String>,
}

/// 用户数据目录服务。迁移只允许整目录进行，插件不能绕过它单独换路径。
pub trait UserDataService: Send + Sync {
    fn state(&self) -> UserDataState;

    /// 预约迁移。`None` 表示恢复平台默认路径；自定义路径必须是绝对路径。
    ///
    /// 真正迁移发生在下次启动、其它服务打开文件之前。
    fn schedule_migration(&self, custom_path: Option<&str>) -> Result<UserDataState, KernelError>;

    /// 取消尚未执行的迁移。
    fn cancel_migration(&self) -> Result<UserDataState, KernelError>;
}
