import { useState } from 'react'
import type { PluginRuntimeState } from '@wonderland/plugin-protocol'
import { PackagePlus, Trash2 } from 'lucide-react'
import { useNavigate } from 'react-router-dom'

import { pluginApi } from '../plugins/api'
import { t, type MessageKey } from '../i18n'

export function PluginManagement({ states, error, setStates }: {
  states: PluginRuntimeState[] | null
  error: MessageKey | null
  setStates: (states: PluginRuntimeState[]) => void
}) {
  const navigate = useNavigate()
  const [busy, setBusy] = useState('')
  const [failure, setFailure] = useState('')

  const remove = async (state: PluginRuntimeState) => {
    if (!window.confirm(t('settings.plugins.removeConfirm', { name: state.manifest.name }))) return
    setBusy(state.manifest.id)
    setFailure('')
    try {
      setStates(await pluginApi.remove(state.manifest.id))
    } catch (cause) {
      setFailure(t('settings.plugins.removeFailed', { error: errorText(cause) }))
    } finally {
      setBusy('')
    }
  }

  const setEnabled = async (state: PluginRuntimeState, enabled: boolean) => {
    setBusy(state.manifest.id)
    setFailure('')
    try {
      setStates(await pluginApi.setEnabled(state.manifest.id, enabled))
    } catch (cause) {
      setFailure(t('settings.plugins.saveFailed') + `：${errorText(cause)}`)
    } finally {
      setBusy('')
    }
  }

  const setCapabilities = async (state: PluginRuntimeState, capabilities: string[]) => {
    setBusy(state.manifest.id)
    setFailure('')
    try {
      setStates(await pluginApi.setCapabilities(state.manifest.id, capabilities))
    } catch (cause) {
      setFailure(t('settings.plugins.capabilitiesFailed', { error: errorText(cause) }))
    } finally {
      setBusy('')
    }
  }

  return (
    <section className="glass-card rounded-2xl border border-glass-line p-4 sm:p-5">
      <header className="mb-4 flex flex-wrap items-start justify-between gap-3">
        <div className="flex min-w-0 items-start gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand-600/10 text-brand-400">
            <PackagePlus className="h-4 w-4" aria-hidden />
          </span>
          <div className="min-w-0">
            <h2 className="text-sm font-semibold text-ink">{t('settings.plugins.title')}</h2>
            <p className="mt-1 max-w-2xl text-xs leading-relaxed text-ink-faint">{t('settings.plugins.hint')}</p>
          </div>
        </div>
        <button
          type="button"
          onClick={() => navigate('/workspace/plugins/install')}
          className="inline-flex items-center gap-2 rounded-lg border border-glass-line bg-glass-subtle px-3 py-2 text-xs font-medium text-ink transition hover:border-brand-500/30 hover:bg-glass-hover"
        >
          <PackagePlus className="h-3.5 w-3.5" aria-hidden />
          {t('settings.plugins.install')}
        </button>
      </header>

      {error && <p role="alert" className="mb-3 rounded-lg border border-[var(--app-danger)]/25 bg-[var(--app-danger)]/5 px-3 py-2 text-xs text-[var(--app-danger)]">{t(error)}</p>}
      {failure && <p role="alert" className="mb-3 rounded-lg border border-[var(--app-danger)]/25 bg-[var(--app-danger)]/5 px-3 py-2 text-xs text-[var(--app-danger)]">{failure}</p>}

      {!states ? (
        <p role="status" className="rounded-xl border border-dashed border-glass-line px-4 py-6 text-center text-sm text-ink-faint">{t('settings.plugins.loading')}</p>
      ) : states.length === 0 ? (
        <p className="rounded-xl border border-dashed border-glass-line px-4 py-6 text-center text-sm text-ink-faint">{t('settings.plugins.empty')}</p>
      ) : (
        <ul className="grid gap-3 xl:grid-cols-2">
          {states.map((state) => (
            <PluginCard
              key={state.manifest.id}
              state={state}
              busy={busy === state.manifest.id}
              onToggle={(enabled) => void setEnabled(state, enabled)}
              onRemove={() => void remove(state)}
              onCapabilitiesChange={(capabilities) => void setCapabilities(state, capabilities)}
            />
          ))}
        </ul>
      )}
    </section>
  )
}

function PluginCard({ state, busy, onToggle, onRemove, onCapabilitiesChange }: {
  state: PluginRuntimeState
  busy: boolean
  onToggle: (enabled: boolean) => void
  onRemove: () => void
  onCapabilitiesChange: (capabilities: string[]) => void
}) {
  const enabled = state.enabled
  const issue = state.lastError?.message ?? state.serviceDependencyIssues?.join(' · ')
  return (
    <li className="min-w-0 rounded-xl border border-glass-line bg-glass-subtle p-3 transition hover:border-brand-500/25">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="truncate text-sm font-semibold text-ink">{state.manifest.name}</h3>
            <span className={`rounded-full border px-2 py-0.5 text-[10px] ${enabled ? 'border-brand-500/25 bg-brand-500/10 text-brand-400' : 'border-glass-line text-ink-faint'}`}>
              {enabled ? t('settings.plugins.enabled') : t('settings.plugins.disabled')}
            </span>
            {state.installation !== 'installed' && (
              <span className="rounded-full border border-[var(--app-danger-line)] px-2 py-0.5 text-[10px] text-[var(--app-danger)]">
                {t(state.installation === 'invalid' ? 'pluginPage.invalid' : 'pluginPage.incompatible')}
              </span>
            )}
          </div>
          <p className="mt-1 break-all text-[11px] text-ink-faint">{state.manifest.id} · v{state.manifest.version}</p>
          <p className={`mt-2 text-xs ${issue ? 'text-[var(--app-danger)]' : 'text-ink-muted'}`}>
            {issue ?? t(`settings.plugins.runtime.${state.runtime}`)}
          </p>
          {state.manifest.provides && state.manifest.provides.length > 0 && (
            <p className="mt-1 text-[11px] text-ink-faint">
              {t('settings.plugins.provides', { services: state.manifest.provides.map((service) => `${service.id} v${service.version}`).join('、') })}
            </p>
          )}
          {!state.manifest.ui && <p className="mt-1 text-[11px] text-ink-faint">{t('settings.plugins.serviceOnly')}</p>}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Switch label={state.manifest.name} checked={enabled} disabled={busy || state.installation !== 'installed'} onChange={onToggle} />
          {state.installation !== 'invalid' && (
            <button type="button" disabled={busy} onClick={onRemove} className="rounded-lg p-2 text-ink-faint transition hover:bg-[var(--app-danger)]/10 hover:text-[var(--app-danger)] disabled:opacity-50" aria-label={t('settings.plugins.remove')} title={t('settings.plugins.remove')}>
              <Trash2 className="h-4 w-4" aria-hidden />
            </button>
          )}
        </div>
      </div>
      {state.manifest.capabilities.length > 0 ? (
        <fieldset className="mt-4 border-t border-dashed border-glass-line pt-3" disabled={busy || state.installation !== 'installed'}>
          <legend className="mb-2 text-[11px] font-medium text-ink-faint">{t('settings.plugins.capabilities')}</legend>
          <div className="flex flex-wrap gap-x-4 gap-y-2">
            {state.manifest.capabilities.map((capability) => (
              <label key={capability} className="flex items-center gap-2 text-xs text-ink-muted">
                <input
                  type="checkbox"
                  checked={state.grantedCapabilities.includes(capability)}
                  onChange={(event) => {
                    const next = new Set(state.grantedCapabilities)
                    if (event.target.checked) next.add(capability)
                    else next.delete(capability)
                    onCapabilitiesChange([...next])
                  }}
                />
                <span>{capability}</span>
              </label>
            ))}
          </div>
        </fieldset>
      ) : (
        <p className="mt-3 border-t border-dashed border-glass-line pt-3 text-[11px] text-ink-faint">{t('settings.plugins.noCapabilities')}</p>
      )}
    </li>
  )
}

function Switch({ label, checked, disabled, onChange }: {
  label: string
  checked: boolean
  disabled: boolean
  onChange: (next: boolean) => void
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={checked ? t('settings.plugins.disableAria', { name: label }) : t('settings.plugins.enableAria', { name: label })}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative h-6 w-11 shrink-0 rounded-full transition disabled:cursor-default disabled:opacity-50 ${checked ? 'bg-brand-500' : 'bg-glass-line-strong'}`}
    >
      <span className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-all ${checked ? 'left-[1.375rem]' : 'left-0.5'}`} />
    </button>
  )
}

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message?: unknown }).message ?? cause)
  }
  return String(cause)
}
