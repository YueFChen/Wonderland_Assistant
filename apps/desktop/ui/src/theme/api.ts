import { invoke } from '@tauri-apps/api/core'
import type { BackgroundAsset, ThemeSettings, ThemeState } from '@wonderland/core-bindings'

/** Host API for theme preferences and the custom background library. */
export const themeApi = {
  get: () => invoke<ThemeState>('theme_get'),
  set: (settings: ThemeSettings) => invoke<ThemeState>('theme_set', { settings }),
  /** 背景库清单（不含图片数据）。 */
  backgrounds: () => invoke<BackgroundAsset[]>('theme_backgrounds'),
  /** 单张背景图的缩略图 data URL，按需取。 */
  backgroundUrl: (id: string) => invoke<string | null>('theme_background_url', { id }),
  /** 导入一张背景图并立即设为当前背景；`name` 是原始文件名。 */
  addBackground: (dataUrl: string, name: string) =>
    invoke<ThemeState>('theme_add_background', { dataUrl, name }),
  selectBackground: (id: string) => invoke<ThemeState>('theme_select_background', { id }),
  removeBackground: (id: string) => invoke<ThemeState>('theme_remove_background', { id }),
}
