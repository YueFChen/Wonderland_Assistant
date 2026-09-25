//! 绑定游戏角色的响应模型。
//!
//! 外部接口字段会随版本增删，可选字段一律给默认值，避免解析直接失败。

pub use wonderland_kernel::GameRole;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct RoleListResponse {
    pub(crate) retcode: i32,
    #[serde(default)]
    pub(crate) message: Option<String>,
    #[serde(default)]
    pub(crate) data: Option<RoleListData>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct RoleListData {
    #[serde(default)]
    pub(crate) list: Vec<RoleListItem>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct RoleListItem {
    #[serde(default)]
    pub(crate) game_biz: String,
    #[serde(default)]
    game_uid: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    region_name: String,
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    level: u32,
}

impl From<RoleListItem> for GameRole {
    fn from(item: RoleListItem) -> Self {
        Self {
            uid: item.game_uid,
            region: item.region,
            region_name: item.region_name,
            nickname: item.nickname,
            level: item.level,
        }
    }
}
