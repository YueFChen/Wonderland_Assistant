// Generated from Rust by crates/kernel/examples/generate_core_bindings.rs. Do not edit.
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
 * 文件大小以字节为单位。
 */
size: number, };
export type ThemeSettings = { mode: ThemeMode,
/**
 * 自定义背景载荷；仅 [`ThemeMode::Custom`] 时有意义。
 *
 * 切换主题档位时保留自定义背景载荷。
 */
background?: BackgroundKind | null,
/**
 * 自定义背景的不透明度，范围为 0 到 100。
 */
background_opacity: number, };
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
 * 迁移成功后未能清理的旧目录；新目录已激活，旧路径可能只剩部分文件。
 */
last_migration_source: string | null,
/**
 * 迁移已完成但旧目录清理未完全成功时的提示。
 */
last_migration_warning: string | null,
/**
 * 上次启动迁移失败的原因；失败时仍从原目录启动，不丢数据。
 */
last_migration_error: string | null, };
