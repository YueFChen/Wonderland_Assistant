import { t } from '../i18n'
import type { Account, AccountSnapshot, GameRole } from './types'

/** 取当前账号。 */
export function currentAccount(snapshot: AccountSnapshot | null): Account | null {
  const key = snapshot?.current_account_key
  if (!key) {
    return null
  }
  return snapshot.accounts.find((account) => account.account_key === key) ?? null
}

/**
 * 首个游戏角色。
 *
 * An account may contain multiple game roles; the summary displays the first role.
 */
export function firstRole(account: Account | null): GameRole | null {
  return account?.game_roles[0] ?? null
}

/** 展示用名字：优先游戏内昵称，其次通行证 ID。 */
export function displayName(account: Account | null): string | null {
  if (!account) {
    return null
  }
  return firstRole(account)?.nickname || account.account_key
}

/** 奇匠等级徽标；未加入创作者中心（接口没给等级）时为 null。 */
export function creatorBadge(account: Account | null): string | null {
  const level = account?.creator_level
  return level ? t('account.creatorBadge', { level }) : null
}

/** 奇匠当前等级经验；两个数都缺时显示占位符。 */
export function creatorExp(account: Account | null): string {
  const current = account?.creator_exp
  if (current == null) {
    return '—'
  }
  const total = account?.creator_exp_total
  return total == null ? String(current) : `${current} / ${total}`
}
