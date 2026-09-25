//! 创作者中心（奇匠中心）个人资料的响应模型。
//!
//! 字段名取自官方前端包（`creator/basic_info` 与 `creator_level/get_level_detail`），
//! 官方会随版本调整，因此可选字段一律给默认值，缺字段不会导致解析失败。

/// 创作者资料：账号页展示用的头像与奇匠等级。
#[derive(Debug, Clone, Default)]
pub struct CreatorProfile {
    /// 游戏内头像（`ra_game_info.avatar_url`）。
    pub avatar_url: Option<String>,
    /// 奇匠等级；未加入创作者中心时为 `None`。
    pub level: Option<u32>,
    /// 当前等级已获得经验。
    pub exp: Option<u32>,
    /// 升到下一级所需经验。
    pub exp_total: Option<u32>,
}

/// 官方统一响应外壳。
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Envelope<T> {
    pub(crate) retcode: i32,
    /// 官方附带的可读文案，业务失败时带进错误。
    #[serde(default)]
    pub(crate) message: Option<String>,
    pub(crate) data: Option<T>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct BasicInfo {
    /// 奇匠等级；接口也可能把它放进等级详情里，两处都取。
    #[serde(default)]
    pub(crate) kolugc_level: u32,
    #[serde(default)]
    pub(crate) ra_game_info: Option<RaGameInfo>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct RaGameInfo {
    #[serde(default)]
    pub(crate) avatar_url: String,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct LevelDetail {
    #[serde(default)]
    pub(crate) cur_level: u32,
    #[serde(default)]
    pub(crate) cur_exp: u32,
    #[serde(default)]
    pub(crate) total_exp: u32,
}

/// 0 在官方字段里同时表示"未获得"与"缺省"，统一收敛成 `None`。
pub(crate) fn non_zero(value: u32) -> Option<u32> {
    (value > 0).then_some(value)
}
