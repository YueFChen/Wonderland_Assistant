//! 主题设置、背景载荷和背景库数据模型。

use serde::{Deserialize, Serialize};

/// 主题档位。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub enum ThemeMode {
    /// 浅色。
    Light,
    /// 深色。
    #[default]
    Dark,
    /// 跟随系统，在浅色与深色之间切换。
    System,
    /// 自定义背景（背景图或纯色）。
    Custom,
}

/// 自定义背景的载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub enum BackgroundKind {
    /// 背景图，取自背景库。`id` 是内核管理的文件名，不是任意路径。
    Image { id: String },
    /// 纯色背景，`#RRGGBB`。
    Color { hex: String },
}

/// 背景库里的一张背景图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct BackgroundAsset {
    /// 库内主键，同时也是落盘文件名。
    pub id: String,
    /// 导入时的原始文件名，仅供展示。
    pub name: String,
    /// 文件字节数。
    ///
    /// 文件大小以字节为单位。
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub size: u64,
}

/// 主题设置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct ThemeSettings {
    #[serde(default)]
    pub mode: ThemeMode,
    /// 自定义背景载荷；仅 [`ThemeMode::Custom`] 时有意义。
    ///
    /// 切换主题档位时保留自定义背景载荷。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<BackgroundKind>,
}

/// 主题设置 + 解析出的背景图 URL，供前端直接渲染。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct ThemeState {
    pub settings: ThemeSettings,
    /// 当前背景图的 data URL；仅当档位为自定义、载荷为图片且文件存在时给出。
    pub background_url: Option<String>,
}
