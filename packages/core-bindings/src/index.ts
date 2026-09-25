// Generated from Rust by crates/kernel/examples/generate_bindings.rs. Do not edit.
export type AccountStatus = "logged_out" | "logging_in" | "logged_in";
export type LoginCancelReason = "window_closed" | "timeout" | "cancelled" | "failed";
export type GameRole = { 
/**
 * 游戏内 UID。
 */
uid: string, 
/**
 * 区服代码，如 `cn_gf01`。
 */
region: string, 
/**
 * 区服名称，如"天空岛"。
 */
region_name: string, 
/**
 * Game display name.
 */
nickname: string, 
/**
 * 冒险等级。
 */
level: number, };
export type Account = { 
/**
 * 主键：米哈游通行证账号 ID，取自 cookie `account_id` / `ltuid`。
 */
account_key: string, 
/**
 * 米游社 mid；仅 V2 cookie 提供。
 */
mid: string | null, 
/**
 * 绑定的游戏角色，由 [`AccountService::sync_account_info`] 拉取。
 */
game_roles: Array<GameRole>, 
/**
 * Avatar URL; `null` when unavailable.
 */
avatar_url: string | null, 
/**
 * 奇匠等级；未加入创作者中心时为 `None`。
 */
creator_level: number | null, 
/**
 * 奇匠当前等级已获得经验。
 */
creator_exp: number | null, 
/**
 * 奇匠升到下一级所需经验。
 */
creator_exp_total: number | null, 
/**
 * 最近一次资料更新时间（Unix 秒）。
 */
updated_at: number, };
export type AccountSnapshot = { status: AccountStatus, current_account_key: string | null, accounts: Array<Account>, 
/**
 * 最近一次未完成的登录原因；下一次登录开始时清除。
 */
last_login_failure: LoginCancelReason | null, };
export type ErrorPayload = { code: string, message: string, details: string | null, };
export type ThemeMode = "light" | "dark" | "system" | "custom";
export type BackgroundKind = { "kind": "image", id: string, } | { "kind": "color", hex: string, };
export type BackgroundAsset = { 
/**
 * 库内主键，同时也是落盘文件名。
 */
id: string, 
/**
 * 导入时的原始文件名，仅供展示。
 */
name: string, 
/**
 * 文件字节数。
 *
 * 与 `Account::updated_at` 同口径：给 ts-rs 标注成 `number`，
 * 否则 `u64` 会生成 `bigint`，前端拿它算不出可读大小。上限 20 MB，精度无虞。
 */
size: number, };
export type ThemeSettings = { mode: ThemeMode, 
/**
 * 自定义背景载荷；仅 [`ThemeMode::Custom`] 时有意义。
 *
 * 切到内置档位时**保留**它，这样用户切回自定义能拿回上次的背景。
 */
background?: BackgroundKind | null, };
export type ThemeState = { settings: ThemeSettings, 
/**
 * 当前背景图的 data URL；仅当档位为自定义、载荷为图片且文件存在时给出。
 */
background_url: string | null, };
export type LogLevel = "error" | "warn" | "info" | "debug" | "trace";
export type LogSettings = { 
/**
 * 唯一用户可配项；保留策略与旋转周期是内核常量，不进设置页。
 */
level: LogLevel, };
export type UserDataLocationKind = "default" | "custom";
export type UserDataState = { active_path: string, default_path: string, location: UserDataLocationKind, 
/**
 * 已预约、将在下次启动执行的目标目录。
 */
pending_path: string | null, 
/**
 * 最近一次成功迁移的目标目录，供设置页确认结果。
 */
last_migration_path: string | null, 
/**
 * 上次启动迁移失败的原因；失败时仍从原目录启动，不丢数据。
 */
last_migration_error: string | null, };
