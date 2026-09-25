import { invoke } from '@tauri-apps/api/core'
import type { UserDataState } from '@wonderland/core-bindings'

export const userDataApi = {
  get: () => invoke<UserDataState>('user_data_get'),
  migrateCustom: (path: string) =>
    invoke<UserDataState>('user_data_migrate_custom', { path }),
  migrateDefault: () => invoke<UserDataState>('user_data_migrate_default'),
  cancelMigration: () => invoke<UserDataState>('user_data_cancel_migration'),
  chooseDirectory: () => invoke<string | null>('user_data_choose_directory'),
  restart: () => invoke<void>('settings_restart'),
}
