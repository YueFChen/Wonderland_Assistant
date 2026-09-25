//! 主题、背景库和日志设置的服务接口。
//!
//! 由宿主组装；具体设置读写由 `wonderland-config` 实现。

use crate::error::KernelError;
use crate::logging::LogSettings;
use crate::theme::{BackgroundAsset, ThemeSettings, ThemeState};

/// 主题、背景库与日志设置服务。
pub trait SettingsService: Send + Sync {
    /// 当前主题设置。
    fn theme(&self) -> ThemeSettings;

    /// 主题设置 + 当前背景图 data URL。
    fn theme_state(&self) -> Result<ThemeState, KernelError>;

    /// 覆盖主题设置。
    fn set_theme(&self, settings: ThemeSettings) -> Result<ThemeState, KernelError>;

    /// 背景库清单，最近导入的在前。
    fn backgrounds(&self) -> Result<Vec<BackgroundAsset>, KernelError>;

    /// 某张背景图的 data URL；库里没有或文件缺失时为 `None`。
    fn background_url(&self, id: &str) -> Result<Option<String>, KernelError>;

    /// 导入一张背景图（**原图落到应用数据目录**）并立即设为当前背景。
    ///
    /// `name` 是导入时的原始文件名，仅用于界面展示。
    fn add_background(&self, data_url: &str, name: &str) -> Result<ThemeState, KernelError>;

    /// 把库里已有的某张图设为当前背景。
    fn select_background(&self, id: &str) -> Result<ThemeState, KernelError>;

    /// 从库里删除一张背景图，并同时删掉落盘的原图。
    ///
    /// 删的如果是当前背景，则回落到库里最近的另一张；库空了才回落到深色。
    fn remove_background(&self, id: &str) -> Result<ThemeState, KernelError>;

    /// 日志设置。
    fn logging(&self) -> LogSettings;

    /// 覆盖日志设置并落盘，返回落盘后的设置。
    ///
    /// 此方法负责持久化设置；运行时级别由宿主更新。
    fn set_logging(&self, settings: LogSettings) -> Result<LogSettings, KernelError>;
}
