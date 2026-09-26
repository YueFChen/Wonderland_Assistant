import { useEffect, useState } from 'react'

import { networkApi, type ProxyMode, type ProxySettingsView } from '../network/api'
import { t } from '../i18n'

const MODES: ProxyMode[] = ['system', 'direct', 'custom']

export function NetworkProxyCard() {
  const [settings, setSettings] = useState<ProxySettingsView | null>(null)
  const [mode, setMode] = useState<ProxyMode>('system')
  const [address, setAddress] = useState('')
  const [bypass, setBypass] = useState('')
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [clearCredentials, setClearCredentials] = useState(false)
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState('')
  const [testResult, setTestResult] = useState<{ success: boolean; message: string } | null>(null)

  useEffect(() => {
    let live = true
    void networkApi.proxyGet().then((next) => {
      if (!live) return
      setSettings(next)
      setMode(next.mode)
      setAddress(next.address ?? '')
      setBypass(next.bypass)
    }).catch((cause) => {
      if (live) setFailure(errorText(cause))
    })
    return () => { live = false }
  }, [])

  const save = async () => {
    setBusy(true)
    setFailure('')
    setTestResult(null)
    try {
      const next = await networkApi.proxySet({
        mode,
        address,
        bypass,
        username,
        password,
        clearCredentials,
      })
      setSettings(next)
      setMode(next.mode)
      setAddress(next.address ?? '')
      setBypass(next.bypass)
      setUsername('')
      setPassword('')
      setClearCredentials(false)
    } catch (cause) {
      setFailure(errorText(cause))
    } finally {
      setBusy(false)
    }
  }

  const test = async () => {
    setBusy(true)
    setFailure('')
    setTestResult(null)
    try {
      const result = await networkApi.proxyTest()
      setTestResult({
        success: result.success,
        message: t(result.success ? 'settings.proxy.testSuccess' : `settings.proxy.test.${result.outcome}`, {
          route: t(`settings.proxy.mode.${result.route}`),
          duration: String(result.durationMs),
          status: String(result.httpStatus ?? 0),
        }),
      })
    } catch (cause) {
      setFailure(errorText(cause))
    } finally {
      setBusy(false)
    }
  }

  const configuredVariables = settings?.environmentVariables.filter((item) => item.configured) ?? []
  const hasNewCredentials = Boolean(username || password)
  const canSaveCredentials = !hasNewCredentials || Boolean(username && password)
  const hasUnsavedChanges = Boolean(settings && (
    mode !== settings.mode
    || address !== (settings.address ?? '')
    || bypass !== settings.bypass
    || hasNewCredentials
    || clearCredentials
  ))

  return (
    <section className="glass-card rounded-card p-6">
      <div className="max-w-3xl">
        <h2 className="text-base font-semibold text-ink">{t('settings.proxy.title')}</h2>
        <p className="mt-1 text-xs leading-relaxed text-ink-faint">{t('settings.proxy.hint')}</p>
      </div>

      {!settings ? (
        <p className="mt-4 text-sm text-ink-faint">{t('settings.proxy.loading')}</p>
      ) : (
        <div className="mt-5 space-y-4">
          <div className="flex flex-wrap items-center gap-2" role="radiogroup" aria-label={t('settings.proxy.modeLabel')}>
            {MODES.map((item) => (
              <button
                key={item}
                type="button"
                role="radio"
                aria-checked={mode === item}
                disabled={busy}
                onClick={() => setMode(item)}
                className={`rounded-lg border px-3 py-2 text-xs transition disabled:opacity-60 ${
                  mode === item
                    ? 'border-brand-400/70 bg-brand-500/15 text-ink'
                    : 'border-glass-line text-ink-muted hover:bg-glass-hover'
                }`}
              >
                {t(`settings.proxy.mode.${item}`)}
              </button>
            ))}
          </div>

          {mode === 'custom' && (
            <div className="grid gap-3 md:grid-cols-2">
              <label className="space-y-1 text-xs text-ink-muted md:col-span-2">
                <span>{t('settings.proxy.address')}</span>
                <input
                  value={address}
                  disabled={busy}
                  onChange={(event) => setAddress(event.target.value)}
                  placeholder="http://127.0.0.1:7890"
                  autoComplete="url"
                  className="w-full rounded-lg border border-[var(--app-field-border)] bg-[var(--app-field)] px-3 py-2 text-sm text-ink outline-none focus:border-brand-400"
                />
              </label>
              <label className="space-y-1 text-xs text-ink-muted md:col-span-2">
                <span>{t('settings.proxy.bypass')}</span>
                <input
                  value={bypass}
                  disabled={busy}
                  onChange={(event) => setBypass(event.target.value)}
                  placeholder="localhost,127.0.0.1,.example.com"
                  className="w-full rounded-lg border border-[var(--app-field-border)] bg-[var(--app-field)] px-3 py-2 text-sm text-ink outline-none focus:border-brand-400"
                />
              </label>
              <label className="space-y-1 text-xs text-ink-muted">
                <span>{t('settings.proxy.username')}</span>
                <input
                  value={username}
                  disabled={busy || clearCredentials}
                  onChange={(event) => setUsername(event.target.value)}
                  autoComplete="username"
                  className="w-full rounded-lg border border-[var(--app-field-border)] bg-[var(--app-field)] px-3 py-2 text-sm text-ink outline-none focus:border-brand-400"
                />
              </label>
              <label className="space-y-1 text-xs text-ink-muted">
                <span>{t('settings.proxy.password')}</span>
                <input
                  type="password"
                  value={password}
                  disabled={busy || clearCredentials}
                  onChange={(event) => setPassword(event.target.value)}
                  autoComplete="new-password"
                  className="w-full rounded-lg border border-[var(--app-field-border)] bg-[var(--app-field)] px-3 py-2 text-sm text-ink outline-none focus:border-brand-400"
                />
              </label>
              {settings.credentialsConfigured && (
                <div className="text-xs text-ink-faint md:col-span-2">
                  {settings.credentialsAvailable
                    ? t(settings.credentialsMatchProxy
                      ? 'settings.proxy.credentialsSaved'
                      : 'settings.proxy.credentialsBoundElsewhere')
                    : t('settings.proxy.credentialsUnavailable')}
                  {' '}{t('settings.proxy.credentialsReplaceHint')}
                </div>
              )}
              {(hasNewCredentials || settings.credentialsConfigured) && (
                <p className="text-xs leading-relaxed text-amber-200/80 md:col-span-2">
                  {t('settings.proxy.authWarning')}
                </p>
              )}
            </div>
          )}

          {settings.credentialsConfigured && mode !== 'custom' && (
            <p className="text-xs text-ink-faint">
              {settings.credentialsAvailable
                ? t('settings.proxy.credentialsSavedInactive')
                : t('settings.proxy.credentialsUnavailable')}
            </p>
          )}
          {settings.credentialsConfigured && (
            <label className="flex items-center gap-2 text-xs text-ink-muted">
              <input
                type="checkbox"
                checked={clearCredentials}
                disabled={busy}
                onChange={(event) => setClearCredentials(event.target.checked)}
              />
              {t('settings.proxy.clearCredentials')}
            </label>
          )}

          {mode === 'system' && (
            <div className="rounded-lg border border-glass-line bg-glass-subtle p-3 text-xs text-ink-muted">
              <p>{t('settings.proxy.environmentHint')}</p>
              {settings.windowsSystemProxy.supported && (
                <p className="mt-2 text-ink-faint">
                  {settings.windowsSystemProxy.enabled
                    ? t(settings.windowsSystemProxy.serverConfigured
                      ? 'settings.proxy.windowsProxyEnabled'
                      : 'settings.proxy.windowsProxyMissingServer')
                    : t('settings.proxy.windowsProxyDisabled')}
                </p>
              )}
              <p className="mt-2 text-ink-faint">
                {configuredVariables.length
                  ? t('settings.proxy.environmentFound', { names: configuredVariables.map((item) => item.name).join(', ') })
                  : t('settings.proxy.environmentNone')}
              </p>
            </div>
          )}

          <div className="rounded-lg border border-glass-line bg-glass-subtle p-3 text-xs leading-relaxed text-ink-muted">
            <p>{t('settings.proxy.coreRoutes')}</p>
            <p className="mt-1">{t('settings.proxy.pluginBoundary')}</p>
          </div>

          {settings.warning && <p role="alert" className="text-xs text-[var(--app-danger)]">{settings.warning}</p>}
          {failure && <p role="alert" className="text-sm text-[var(--app-danger)]">{failure}</p>}
          {testResult && (
            <p role="status" className={`text-sm ${testResult.success ? 'text-ink-muted' : 'text-[var(--app-danger)]'}`}>
              {testResult.message}
            </p>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              disabled={busy || !canSaveCredentials}
              onClick={() => void save()}
              className="rounded-lg bg-brand-600 px-4 py-2 text-xs font-medium text-white hover:bg-brand-500 disabled:cursor-default disabled:opacity-50"
            >
              {t('settings.proxy.save')}
            </button>
            <button
              type="button"
              disabled={busy || !settings || hasUnsavedChanges}
              onClick={() => void test()}
              className="rounded-lg border border-glass-line px-4 py-2 text-xs text-ink-muted hover:bg-glass-hover hover:text-ink disabled:cursor-default disabled:opacity-50"
            >
              {busy ? t('settings.proxy.working') : t('settings.proxy.testButton')}
            </button>
          </div>
          {hasUnsavedChanges && (
            <p className="text-xs text-ink-faint">{t('settings.proxy.saveBeforeTest')}</p>
          )}
        </div>
      )}
    </section>
  )
}

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message: unknown }).message)
  }
  return cause instanceof Error ? cause.message : String(cause)
}
