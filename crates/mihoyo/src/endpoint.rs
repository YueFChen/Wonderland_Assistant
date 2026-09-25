//! 接口端点。官方会调整路径与参数，集中在此便于比对与替换。

/// 米游社 API 主机（国服）：账号绑定、通行证相关。
pub const API_TAKUMI: &str = "https://api-takumi.mihoyo.com";

/// 奇匠中心 API 主机（国服）：UGC 作品与数据。
pub const API_MICREATOR: &str = "https://api-micreator.mihoyo.com";

/// 原神（国服）的游戏标识。
pub const GAME_BIZ_HK4E_CN: &str = "hk4e_cn";

/// 奇匠中心接口的公共查询参数（语言 + 游戏标识）。
///
/// 账号侧（本 crate）用它拼请求。插件侧的奇匠业务接口走
/// `AccountService::authed_get`，路径与参数由插件自己持有，不引用本常量。
pub const MICREATOR_QUERY_BASE: &str = "lang=zh-cn&game_biz=hk4e_cn";

/// 绑定游戏账号列表：按 LToken 鉴权，返回该账号绑定的各游戏角色。
pub const PATH_GAME_ROLES: &str = "/binding/api/getUserGameRolesByCookie";

/// 创作者中心：个人资料。`need_ra_game_info=true` 时额外返回游戏内昵称与头像。
pub const PATH_CREATOR_BASIC_INFO: &str = "/kolugc_hch/common/v1/creator/basic_info";

/// 创作者中心：奇匠等级与经验。
pub const PATH_CREATOR_LEVEL_DETAIL: &str = "/kolugc_hch/common/v1/creator_level/get_level_detail";
