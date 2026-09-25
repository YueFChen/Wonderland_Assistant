import { LogIn, LogOut, RefreshCw } from 'lucide-react'
import { useState } from 'react'

import { accountApi, formatTimestamp } from '../account/api'
import { creatorBadge, creatorExp, currentAccount, displayName, firstRole } from '../account/display'
import { useAccount } from '../account/useAccount'
import { UserAvatar } from '../account/UserAvatar'
import { t } from '../i18n'
import { ACCOUNT_STATUS_LABEL, LOGIN_CANCEL_REASON_LABEL } from '../i18n/labels'

const BUTTON_CLASS =
  'glass-card inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-medium text-ink transition hover:bg-glass-hover disabled:cursor-not-allowed disabled:opacity-50'

/** 破坏性操作（遗忘账号）二次确认后的样式。 */
const DANGER_BUTTON_CLASS =
  'glass-card inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-medium text-[var(--app-danger)] transition hover:bg-[var(--app-danger-soft)] disabled:cursor-not-allowed disabled:opacity-50'

/**
 * 账号页：账号体系属于内核，页面只展示状态并触发宿主侧流程，不参与登录判定。
 */
export function AccountPage() {
  const { snapshot, error, busy, run } = useAccount()
  const [forgetPending, setForgetPending] = useState<string | null>(null)

  const status = snapshot?.status ?? 'logged_out'
  const account = currentAccount(snapshot)
  const role = firstRole(account)
  const nickname = displayName(account)
  const badge = creatorBadge(account)
  const failure = snapshot?.last_login_failure ?? null

  const subtitle = role
    ? t('account.roleSummary', {
        region: t('account.regionWithCode', { name: role.region_name, code: role.region }),
        level: role.level,
      })
    : account
      ? t('account.passportPending', { accountKey: account.account_key })
      : t('account.loginHint')

  return (
    <section className="w-full">
      <div className="mb-6">
        <h1 className="mb-2 text-xl font-bold text-ink">{t('account.title')}</h1>
        <p className="text-sm leading-relaxed text-ink-muted">
          {t('account.description')}
        </p>
      </div>

      <div className="glass-card animate-rise rounded-card p-6">
        <div className="flex items-center gap-4">
          <UserAvatar name={role?.nickname} src={account?.avatar_url} size={64} />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className="truncate text-lg font-bold text-ink">{nickname ?? t('common.notLoggedIn')}</span>
              {badge ? (
                <span className="shrink-0 rounded-full bg-glass px-2 py-0.5 text-xs whitespace-nowrap text-ink-muted">
                  {badge}
                </span>
              ) : null}
            </div>
            <div className="truncate text-sm text-ink-muted">{subtitle}</div>
          </div>
          <span className="shrink-0 rounded-full bg-glass px-2.5 py-1 text-xs whitespace-nowrap text-ink-muted">
            {ACCOUNT_STATUS_LABEL[status]}
          </span>
        </div>

        <dl className="mt-6 grid grid-cols-2 gap-x-4 gap-y-4 border-t border-glass-line pt-5 sm:grid-cols-3">
          <Field label={t('account.field.uid')} value={role?.uid ?? '—'} />
          <Field
            label={t('account.field.region')}
            value={role ? t('account.regionWithCode', { name: role.region_name, code: role.region }) : '—'}
          />
          <Field label={t('account.field.level')} value={role ? `Lv.${role.level}` : '—'} />
          <Field
            label={t('account.field.creatorLevel')}
            value={account?.creator_level ? `Lv.${account.creator_level}` : '—'}
          />
          <Field label={t('account.field.creatorExp')} value={creatorExp(account)} />
          <Field label={t('account.field.mid')} value={account?.mid ?? '—'} />
          <Field label={t('account.field.accountKey')} value={account?.account_key ?? '—'} />
          <Field
            label={t('account.field.updatedAt')}
            value={account && account.game_roles.length > 0 ? formatTimestamp(account.updated_at) : '—'}
          />
        </dl>

        <div className="mt-6 flex flex-wrap gap-3">
          {status === 'logging_in' ? (
            <button
              type="button"
              className={BUTTON_CLASS}
              disabled={busy}
              onClick={() => void run(() => accountApi.cancelLogin())}
            >
              <LogOut className="h-4 w-4" aria-hidden />
              {t('account.cancelLogin')}
            </button>
          ) : (
            <button
              type="button"
              className={BUTTON_CLASS}
              disabled={busy}
              onClick={() => void run(() => accountApi.startLogin())}
            >
              <LogIn className="h-4 w-4" aria-hidden />
              {account ? t('account.loginOther') : t('account.login')}
            </button>
          )}

          {account ? (
            <button
              type="button"
              className={BUTTON_CLASS}
              disabled={busy}
              onClick={() => void run(() => accountApi.sync(account.account_key))}
            >
              <RefreshCw className="h-4 w-4" aria-hidden />
              {t('account.sync')}
            </button>
          ) : null}
        </div>

        {status === 'logging_in' ? (
          <p className="mt-4 text-sm leading-relaxed text-ink-muted">
            {t('account.loginInProgressHint')}
          </p>
        ) : null}
        {failure ? (
          <p className="mt-4 text-sm text-ink-muted">
            {t('account.lastLoginFailure', { reason: LOGIN_CANCEL_REASON_LABEL[failure] })}
          </p>
        ) : null}
        {error ? <p className="mt-2 text-sm text-[var(--app-danger)]">{error}</p> : null}
      </div>

      <div className="glass-card mt-4 rounded-card p-6">
        <h2 className="mb-1 text-sm font-medium text-ink-muted">{t('account.savedAccounts')}</h2>
        <p className="mb-4 text-xs leading-relaxed text-ink-faint">
          {t('account.savedAccountsHint')}
        </p>
        {snapshot && snapshot.accounts.length > 0 ? (
          <ul className="space-y-4">
            {snapshot.accounts.map((item) => {
              const isCurrent = item.account_key === snapshot.current_account_key
              const itemRole = firstRole(item)
              return (
                <li
                  key={item.account_key}
                  className="flex flex-wrap items-center gap-3 border-b border-glass-line pb-4 last:border-b-0 last:pb-0"
                >
                  <UserAvatar
                    name={itemRole?.nickname ?? item.account_key}
                    src={item.avatar_url}
                    size={40}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="truncate text-sm font-medium text-ink">
                        {displayName(item)}
                      </span>
                      {isCurrent ? (
                        <span className="shrink-0 rounded-full bg-glass px-2 py-0.5 text-xs text-ink-muted">
                          {t('common.current')}
                        </span>
                      ) : null}
                    </div>
                    <div className="truncate text-xs text-ink-faint">
                      {itemRole
                        ? `UID ${itemRole.uid} · ${itemRole.region_name}`
                        : t('account.passportPending', { accountKey: item.account_key })}
                    </div>
                  </div>
                  <div className="flex shrink-0 gap-2">
                    {isCurrent ? null : (
                      <button
                        type="button"
                        className={BUTTON_CLASS}
                        disabled={busy}
                        onClick={() => {
                          setForgetPending(null)
                          void run(() => accountApi.switchAccount(item.account_key))
                        }}
                      >
                        {t('account.switch')}
                      </button>
                    )}
                    {forgetPending === item.account_key ? (
                      <>
                        <button
                          type="button"
                          className={DANGER_BUTTON_CLASS}
                          disabled={busy}
                          onClick={() => {
                            setForgetPending(null)
                            void run(() => accountApi.removeAccount(item.account_key))
                          }}
                        >
                          {t('account.confirmForget')}
                        </button>
                        <button
                          type="button"
                          className={BUTTON_CLASS}
                          disabled={busy}
                          onClick={() => setForgetPending(null)}
                        >
                          {t('common.cancel')}
                        </button>
                      </>
                    ) : (
                      <button
                        type="button"
                        className={BUTTON_CLASS}
                        disabled={busy}
                        onClick={() => setForgetPending(item.account_key)}
                      >
                        {t('account.forget')}
                      </button>
                    )}
                  </div>
                </li>
              )
            })}
          </ul>
        ) : (
          <p className="text-sm text-ink-faint">{t('account.empty')}</p>
        )}
      </div>
    </section>
  )
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-ink-faint">{label}</dt>
      <dd className="truncate text-sm text-ink" title={value}>
        {value}
      </dd>
    </div>
  )
}
