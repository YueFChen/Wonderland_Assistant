import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

import { t } from '../i18n'
import type { Account, AccountSnapshot, KernelErrorPayload } from './types'

/** 账号状态变化事件，与宿主 `account_window::ACCOUNT_CHANGED_EVENT` 对应。 */
export const ACCOUNT_CHANGED_EVENT = 'account-changed'

export const accountApi = {
  snapshot: () => invoke<AccountSnapshot>('account_snapshot'),
  /** 打开登录窗口；每次登录都从零开始，不会继承上一次的会话。 */
  startLogin: () => invoke<void>('account_login_start'),
  cancelLogin: () => invoke<void>('account_login_cancel'),
  /** 拉取账号资料（游戏角色、昵称等）。 */
  sync: (accountKey: string) => invoke<Account>('account_sync', { accountKey }),
  /** 遗忘账号：删除账号记录与本机保存的凭据。 */
  removeAccount: (accountKey: string) => invoke<void>('account_remove', { accountKey }),
  switchAccount: (accountKey: string) => invoke<void>('account_switch', { accountKey }),
}

/** 订阅账号状态变化；返回取消订阅函数。 */
export function onAccountChanged(handler: () => void): Promise<UnlistenFn> {
  return listen(ACCOUNT_CHANGED_EVENT, () => handler())
}

/** 把内核错误映射为可展示文案。 */
export function toErrorMessage(error: unknown): string {
  if (typeof error === 'string') {
    return error
  }
  if (error && typeof error === 'object' && 'message' in error) {
    const payload = error as KernelErrorPayload
    return payload.details
      ? t('common.messageWithDetails', { message: payload.message, details: payload.details })
      : payload.message
  }
  return t('common.unknownError')
}

/** 秒级时间戳 → 本地时间文案。 */
export function formatTimestamp(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString('zh-CN')
}
