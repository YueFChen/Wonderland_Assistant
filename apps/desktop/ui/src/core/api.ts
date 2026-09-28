import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export type TrayPinnedEntry = { id: string; title: string }

export type TrayNavigation =
  | { kind: 'home' }
  | { kind: 'pluginManagement' }
  | { kind: 'contribution'; pluginId: string; contributionId: string }

export type TrayCloseRequestedHandler = () => void

export const coreApi = {
  syncTrayPinned: (entries: TrayPinnedEntry[]) => invoke<void>('tray_sync_pinned', { entries }),
  exit: () => invoke<void>('core_exit'),
  onTrayNavigation: (handler: (navigation: TrayNavigation) => void) =>
    listen<TrayNavigation>('core-tray-navigation', (event) => handler(event.payload)),
  onCloseRequested: (handler: TrayCloseRequestedHandler) =>
    listen('core-close-requested', handler),
}
