import { invoke } from '@tauri-apps/api/core'
import type { LogLevel, LogSettings } from '@wonderland/core-bindings'

/**
 * 日志设置与日志目录的宿主接口。
 *
 * 级别改动即时生效并写入本机；命令不接受前端传路径，「打开日志目录」由后端给出位置。
 */
export const loggingApi = {
  get: () => invoke<LogSettings>('logging_get'),
  dir: () => invoke<string>('logging_dir'),
  /** 改级别：持久化 → 即时生效 → 返回新设置。 */
  setLevel: (level: LogLevel) => invoke<LogSettings>('logging_set_level', { level }),
  revealDir: () => invoke<void>('logging_reveal_dir'),
}
