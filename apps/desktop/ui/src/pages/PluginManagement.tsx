import { useEffect, useRef, useState } from 'react'
import type { PluginRuntimeState } from '@wonderland/plugin-protocol'
import { ChevronDown, PackagePlus, Trash2 } from 'lucide-react'
import { useNavigate } from 'react-router-dom'

import { pluginApi } from '../plugins/api'
import { t, type MessageKey } from '../i18n'
import { ToggleSwitch } from '../components/ToggleSwitch'

export function PluginManagement({ states, error, setStates }: {
  states: PluginRuntimeState[] | null
  error: MessageKey | null
  setStates: (states: PluginRuntimeState[]) => void
}) {
  const navigate = useNavigate()
  const [busy, setBusy] = useState('')
  const [failure, setFailure] = useState('')
  const [uninstallTarget, setUninstallTarget] = useState<PluginRuntimeState | null>(null)
  const [deletePluginData, setDeletePluginData] = useState(false)
  const cancelUninstallButton = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!uninstallTarget) return
    cancelUninstallButton.current?.focus()
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && busy !== uninstallTarget.manifest.id) {
        setUninstallTarget(null)
      }
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [uninstallTarget, busy])

  const uninstall = async () => {
    if (!uninstallTarget) return
    const state = uninstallTarget
    setBusy(state.manifest.id)
    setFailure('')
    try {
      setStates(await pluginApi.uninstall(state.manifest.id, deletePluginData))
      setUninstallTarget(null)
    } catch (cause) {
      setFailure(t('settings.plugins.uninstallFailed', { error: errorText(cause) }))
      // Package removal may have completed before optional data cleanup failed.
      try {
        setStates(await pluginApi.states())
      } catch {
        // Keep the last rendered state and show the operation error.
      }
      setUninstallTarget(null)
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
    <>
    <section className="glass-card rounded-2xl border border-glass-line p-3 sm:p-4">
      <header className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2.5">
          <span className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-brand-600/10 text-brand-400">
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
          className="inline-flex items-center gap-1.5 rounded-lg border border-glass-line bg-glass-subtle px-2.5 py-1.5 text-xs font-medium text-ink transition hover:border-brand-500/30 hover:bg-glass-hover"
        >
          <PackagePlus className="h-3.5 w-3.5" aria-hidden />
          {t('settings.plugins.install')}
        </button>
      </header>

      {error && <p role="alert" className="mb-3 rounded-lg border border-[var(--app-danger)]/25 bg-[var(--app-danger)]/5 px-3 py-2 text-xs text-[var(--app-danger)]">{t(error)}</p>}
      {failure && <p role="alert" className="mb-3 rounded-lg border border-[var(--app-danger)]/25 bg-[var(--app-danger)]/5 px-3 py-2 text-xs text-[var(--app-danger)]">{failure}</p>}

      {!states ? (
        <p role="status" className="rounded-xl border border-dashed border-glass-line px-4 py-4 text-center text-sm text-ink-faint">{t('settings.plugins.loading')}</p>
      ) : states.length === 0 ? (
        <p className="rounded-xl border border-dashed border-glass-line px-4 py-4 text-center text-sm text-ink-faint">{t('settings.plugins.empty')}</p>
      ) : (
        <ul className="grid gap-2.5 md:grid-cols-2 2xl:grid-cols-3">
          {states.map((state) => (
            <PluginCard
              key={state.manifest.id}
              state={state}
              busy={busy === state.manifest.id}
              onToggle={(enabled) => void setEnabled(state, enabled)}
              onUninstall={() => {
                setDeletePluginData(false)
                setUninstallTarget(state)
              }}
              onCapabilitiesChange={(capabilities) => void setCapabilities(state, capabilities)}
            />
          ))}
        </ul>
      )}
    </section>
    {uninstallTarget && (
      <div className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50 px-4 py-8 backdrop-blur-sm">
        <section
          role="dialog"
          aria-modal="true"
          aria-labelledby="plugin-uninstall-title"
          className="glass-card w-full max-w-lg rounded-2xl border border-glass-line p-5 shadow-2xl"
        >
          <h2 id="plugin-uninstall-title" className="text-base font-semibold text-ink">
            {t('settings.plugins.uninstallTitle')}
          </h2>
          <p className="mt-2 text-sm leading-relaxed text-ink-muted">
            {t('settings.plugins.uninstallDescription', { name: uninstallTarget.manifest.name })}
          </p>
          <fieldset className="mt-4 space-y-2" disabled={busy === uninstallTarget.manifest.id}>
            <legend className="sr-only">{t('settings.plugins.uninstallTitle')}</legend>
            <label className="flex cursor-pointer gap-3 rounded-xl border border-glass-line bg-glass-subtle p-3 transition hover:bg-glass-hover">
              <input
                type="radio"
                name="plugin-uninstall-data"
                checked={!deletePluginData}
                onChange={() => setDeletePluginData(false)}
                className="mt-1 accent-[var(--app-brand)]"
              />
              <span>
                <span className="block text-sm font-medium text-ink">{t('settings.plugins.keepData')}</span>
                <span className="mt-1 block text-xs leading-relaxed text-ink-faint">{t('settings.plugins.keepDataHint')}</span>
              </span>
            </label>
            <label className="flex cursor-pointer gap-3 rounded-xl border border-[var(--app-danger-line)] bg-[var(--app-danger)]/5 p-3 transition hover:bg-[var(--app-danger)]/10">
              <input
                type="radio"
                name="plugin-uninstall-data"
                checked={deletePluginData}
                onChange={() => setDeletePluginData(true)}
                className="mt-1 accent-[var(--app-danger)]"
              />
              <span className="min-w-0">
                <span className="block text-sm font-medium text-[var(--app-danger)]">{t('settings.plugins.deleteData')}</span>
                <span className="mt-1 block text-xs leading-relaxed text-ink-faint">{t('settings.plugins.deleteDataHint')}</span>
              </span>
            </label>
          </fieldset>
          <p className="mt-3 break-all rounded-lg bg-glass-subtle px-3 py-2 font-mono text-[11px] leading-relaxed text-ink-faint">
            {t('settings.plugins.uninstallDataPath', { path: uninstallTarget.pluginDataDirectory ?? '—' })}
          </p>
          <footer className="mt-5 flex justify-end gap-2">
            <button
              ref={cancelUninstallButton}
              type="button"
              disabled={busy === uninstallTarget.manifest.id}
              onClick={() => setUninstallTarget(null)}
              className="rounded-lg border border-glass-line px-3 py-2 text-xs font-medium text-ink-muted transition hover:bg-glass-hover disabled:opacity-50"
            >
              {t('common.cancel')}
            </button>
            <button
              type="button"
              disabled={busy === uninstallTarget.manifest.id}
              onClick={() => void uninstall()}
              className={`inline-flex items-center gap-2 rounded-lg border px-3 py-2 text-xs font-medium transition disabled:opacity-50 ${deletePluginData ? 'border-[var(--app-danger-line)] bg-[var(--app-danger)]/10 text-[var(--app-danger)] hover:bg-[var(--app-danger)]/15' : 'border-brand-500/25 bg-brand-500/10 text-brand-400 hover:bg-brand-500/15'}`}
            >
              <Trash2 className="h-3.5 w-3.5" aria-hidden />
              {busy === uninstallTarget.manifest.id
                ? t('settings.plugins.uninstalling')
                : deletePluginData
                  ? t('settings.plugins.uninstallAndDeleteConfirm')
                  : t('settings.plugins.uninstallConfirm')}
            </button>
          </footer>
        </section>
      </div>
    )}
    </>
  )
}

function PluginCard({ state, busy, onToggle, onUninstall, onCapabilitiesChange }: {
  state: PluginRuntimeState
  busy: boolean
  onToggle: (enabled: boolean) => void
  onUninstall: () => void
  onCapabilitiesChange: (capabilities: string[]) => void
}) {
  const enabled = state.enabled
  const issue = state.lastError?.message ?? state.serviceDependencyIssues?.join(' · ')
  const sourceLabel = state.installationSource?.kind === 'catalog'
    ? t('settings.plugins.source.catalog', { author: state.installationSource.author ?? t('workspace.authorUnknown') })
    : state.installationSource?.kind === 'local'
      ? t('settings.plugins.source.local')
      : t('settings.plugins.source.unknown')
  const sourceOrigin = state.installationSource?.origin
  return (
    <li className="flex min-h-44 min-w-0 flex-col justify-between rounded-xl border border-glass-line bg-glass-subtle p-3 transition hover:border-brand-500/25">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <h3 className="truncate text-sm font-semibold text-ink">{state.manifest.name}</h3>
            <span className={`rounded-full border px-1.5 py-0.5 text-[10px] ${enabled ? 'border-brand-500/25 bg-brand-500/10 text-brand-400' : 'border-glass-line text-ink-faint'}`}>
              {enabled ? t('settings.plugins.enabled') : t('settings.plugins.disabled')}
            </span>
            {state.installation !== 'installed' && (
              <span className="rounded-full border border-[var(--app-danger-line)] px-1.5 py-0.5 text-[10px] text-[var(--app-danger)]">
                {t(state.installation === 'invalid' ? 'pluginPage.invalid' : 'pluginPage.incompatible')}
              </span>
            )}
          </div>
          <p className="mt-0.5 truncate text-[11px] text-ink-faint" title={sourceOrigin}>
            {state.manifest.id} · v{state.manifest.version} · {sourceLabel}
          </p>
          <p className={`mt-1 truncate text-[11px] ${issue ? 'text-[var(--app-danger)]' : 'text-ink-muted'}`} title={issue ?? undefined}>
            {issue ?? t(`settings.plugins.runtime.${state.runtime}`)}
          </p>
          {(!state.manifest.ui || (state.manifest.provides && state.manifest.provides.length > 0)) && (
            <p className="mt-1 truncate text-[10px] text-ink-faint" title={state.manifest.provides?.map((service) => `${service.id} v${service.version}`).join('、')}>
              {!state.manifest.ui && t('settings.plugins.serviceOnly')}
              {!state.manifest.ui && state.manifest.provides?.length ? ' · ' : ''}
              {state.manifest.provides?.length
                ? t('settings.plugins.provides', { services: state.manifest.provides.map((service) => `${service.id} v${service.version}`).join('、') })
                : ''}
            </p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <ToggleSwitch
            label={enabled ? t('settings.plugins.disableAria', { name: state.manifest.name }) : t('settings.plugins.enableAria', { name: state.manifest.name })}
            checked={enabled}
            disabled={busy || state.installation !== 'installed'}
            onChange={onToggle}
          />
          <button type="button" disabled={busy} onClick={onUninstall} className="inline-flex items-center gap-1 rounded-lg border border-[var(--app-danger-line)] px-2 py-1.5 text-[11px] text-[var(--app-danger)] transition hover:bg-[var(--app-danger)]/10 disabled:opacity-50">
            <Trash2 className="h-3 w-3" aria-hidden />
            {t('settings.plugins.uninstall')}
          </button>
        </div>
      </div>
      {state.manifest.capabilities.length > 0 ? (
        <details className="group mt-2 border-t border-dashed border-glass-line pt-2">
          <summary className="flex cursor-pointer list-none items-center gap-1.5 text-[10px] font-medium text-ink-faint [&::-webkit-details-marker]:hidden">
            <ChevronDown className="h-3 w-3 transition-transform group-open:rotate-180" aria-hidden />
            {t('settings.plugins.capabilities')} · {state.grantedCapabilities.length}/{state.manifest.capabilities.length}
          </summary>
          <fieldset className="mt-2 flex flex-wrap gap-x-3 gap-y-1.5" disabled={busy || state.installation !== 'installed'}>
            <legend className="sr-only">{t('settings.plugins.capabilities')}</legend>
            {state.manifest.capabilities.map((capability) => (
              <label key={capability} className="flex items-center gap-1.5 text-[11px] text-ink-muted">
                <input
                  type="checkbox"
                  checked={state.grantedCapabilities.includes(capability)}
                  onChange={(event) => {
                    const next = new Set(state.grantedCapabilities)
                    if (event.target.checked) next.add(capability)
                    else next.delete(capability)
                    onCapabilitiesChange([...next])
                  }}
                  className="h-3.5 w-3.5 accent-brand-500"
                />
                <span>{capability}</span>
              </label>
            ))}
          </fieldset>
        </details>
      ) : null}
    </li>
  )
}

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message?: unknown }).message ?? cause)
  }
  return String(cause)
}
