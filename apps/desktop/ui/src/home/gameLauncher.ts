import { invoke } from '@tauri-apps/api/core'

export interface GameLauncherSnapshot {
  path: string | null
  available: boolean
  supported: boolean
  kind: 'launcher' | 'game' | null
  source: 'manual' | 'registry' | null
}

/** Resolves a selected or registered launcher/game executable. */
export const gameLauncherApi = {
  snapshot: () => invoke<GameLauncherSnapshot>('game_launcher_get'),
  select: () => invoke<GameLauncherSnapshot>('game_launcher_select'),
  launch: () => invoke<void>('game_launcher_launch'),
}
