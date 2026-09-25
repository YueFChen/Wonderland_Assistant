//! 账号状态、数据模型与服务接口。原始凭据仅由账号服务和宿主处理。

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::error::KernelError;

/// 账号登录态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    /// 未登录。
    LoggedOut,
    /// 登录窗口已打开，等待用户完成登录。
    LoggingIn,
    /// 已登录，凭据已在本机。
    LoggedIn,
}

impl AccountStatus {
    pub fn is_logging_in(self) -> bool {
        matches!(self, Self::LoggingIn)
    }
}

/// 登录流程未能走到终态的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LoginCancelReason {
    /// 用户关闭了登录窗口。
    WindowClosed,
    /// 等待超时。
    Timeout,
    /// 用户主动取消。
    Cancelled,
    /// 登录引擎失败。
    Failed,
}

/// 一条 cookie。
///
/// 只保留内核需要的字段；凭据本体仅在账号服务与宿主之间流转，不进入插件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
}

/// 绑定的游戏角色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct GameRole {
    /// 游戏内 UID。
    pub uid: String,
    /// 区服代码，如 `cn_gf01`。
    pub region: String,
    /// 区服名称，如"天空岛"。
    pub region_name: String,
    /// 游戏内昵称。
    pub nickname: String,
    /// 冒险等级。
    pub level: u32,
}

/// 已保存的账号（**不含凭据本体**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct Account {
    /// 主键：米哈游通行证账号 ID，取自 cookie `account_id` / `ltuid`。
    pub account_key: String,
    /// 米游社 mid；仅 V2 cookie 提供。
    pub mid: Option<String>,
    /// 绑定的游戏角色，由 [`AccountService::sync_account_info`] 拉取。
    pub game_roles: Vec<GameRole>,
    /// 头像 URL；不可用时为 `None`。
    pub avatar_url: Option<String>,
    /// 奇匠等级；未加入创作者中心时为 `None`。
    #[serde(default)]
    pub creator_level: Option<u32>,
    /// 奇匠当前等级已获得经验。
    #[serde(default)]
    pub creator_exp: Option<u32>,
    /// 奇匠升到下一级所需经验。
    #[serde(default)]
    pub creator_exp_total: Option<u32>,
    /// 最近一次资料更新时间（Unix 秒）。
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub updated_at: i64,
}

/// 账号状态快照，供壳层与插件读取。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct AccountSnapshot {
    pub status: AccountStatus,
    pub current_account_key: Option<String>,
    pub accounts: Vec<Account>,
    /// 最近一次未完成的登录原因；下一次登录开始时清除。
    pub last_login_failure: Option<LoginCancelReason>,
}

/// 提交一次 cookie 快照的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LoginOutcome {
    /// 凭据尚未齐全，继续等待。
    Pending,
    /// 凭据齐全，登录完成。
    Completed { account_key: String },
}

/// 账号体系暴露给内核其它部分的能力。
///
/// 宿主负责打开登录窗口并把读到的 cookie 交给 `submit_login`；
/// 凭据判定、加密落盘、账号与当前账号管理都在实现侧完成。
pub trait AccountService: Send + Sync + 'static {
    /// 后端专用、绑定显式账号的只读鉴权请求。实现限定官方主机，插件拿不到凭据。
    fn authed_get<'a>(
        &'a self,
        account_key: &'a str,
        path: &'a str,
        query: &'a [(String, String)],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, KernelError>> + Send + 'a>>;

    /// 读取当前账号状态。
    fn snapshot(&self) -> AccountSnapshot;

    /// 开始一次登录。已有进行中的登录时返回 [`KernelError::LoginInProgress`]。
    fn begin_login(&self) -> Result<(), KernelError>;

    /// 提交一次 cookie 快照；凭据齐全时加密落盘并返回 [`LoginOutcome::Completed`]。
    fn submit_login(&self, cookies: Vec<StoredCookie>) -> Result<LoginOutcome, KernelError>;

    /// 拉取并回写账号资料（游戏角色、头像与奇匠等级）。
    ///
    /// 返回装箱 future 以支持 trait object 调用。
    fn sync_account_info<'a>(
        &'a self,
        account_key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Account, KernelError>> + Send + 'a>>;

    /// 结束进行中的登录流程；没有进行中的流程时为空操作。
    fn cancel_login(&self, reason: LoginCancelReason);

    /// 遗忘账号：删除账号记录与已保存的凭据。
    ///
    /// 删掉的是当前账号时，当前账号让给剩余列表的首个；没有剩余则回到未登录。
    fn remove_account(&self, account_key: &str) -> Result<(), KernelError>;

    /// 切换当前账号。
    fn switch_account(&self, account_key: &str) -> Result<(), KernelError>;
}
