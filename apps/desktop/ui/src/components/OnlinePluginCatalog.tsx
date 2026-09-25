import { useEffect, useMemo, useState } from 'react'
import { Download, PackageSearch, RefreshCw, Search } from 'lucide-react'

import { pluginApi, type PluginCatalogSnapshot } from '../plugins/api'
import type { PluginRuntimeState } from '@wonderland/plugin-protocol'
import { t } from '../i18n'

export function OnlinePluginCatalog({ onInstalled }: {
  onInstalled: (states: PluginRuntimeState[]) => void
}) {
  const [catalog, setCatalog] = useState<PluginCatalogSnapshot | null>(null)
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState('')
  const [failure, setFailure] = useState('')
  const [query, setQuery] = useState('')

  const refresh = async () => {
    if (loading || busy) return
    setLoading(true)
    setFailure('')
    try {
      setCatalog(await pluginApi.catalog())
    } catch (cause) {
      setFailure(errorText(cause))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { void refresh() }, [])

  const visiblePlugins = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase()
    return (catalog?.plugins ?? []).filter(({ entry }) =>
      normalized === '' || [entry.name, entry.id, entry.description, entry.author]
        .some((value) => value.toLocaleLowerCase().includes(normalized)),
    )
  }, [catalog, query])

  const install = async (item: PluginCatalogSnapshot['plugins'][number]) => {
    const { entry } = item
    const requested = entry.capabilities.length > 0
      ? entry.capabilities.map((capability) => `• ${capability}`).join('\n')
      : t('settings.plugins.catalog.noCapabilities')
    const confirmation = t('settings.plugins.catalog.confirm', {
      name: entry.name,
      version: entry.version,
      capabilities: requested,
    })
    if (!window.confirm(confirmation)) return

    setBusy(entry.id)
    setFailure('')
    try {
      const states = await pluginApi.installCatalog(entry.id, entry.version, entry.sha256, entry.capabilities)
      onInstalled(states)
      setCatalog((current) => current && ({
        ...current,
        plugins: current.plugins.map((candidate) => candidate.entry.id === entry.id
          ? { ...candidate, installedVersion: entry.version, installable: false }
          : candidate),
      }))
    } catch (cause) {
      setFailure(t('settings.plugins.installFailed', { error: errorText(cause) }))
    } finally {
      setBusy('')
    }
  }

  return (
    <section className="glass-card rounded-2xl border border-glass-line p-4">
      <header className="mb-3 flex flex-wrap items-start justify-between gap-3">
        <div className="flex min-w-0 items-start gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand-600/10 text-brand-400">
            <PackageSearch className="h-4 w-4" aria-hidden />
          </span>
          <div className="min-w-0">
            <h2 className="text-sm font-semibold text-ink">{t('settings.plugins.catalog.title')}</h2>
            <p className="mt-0.5 max-w-2xl text-xs leading-relaxed text-ink-faint">{t('settings.plugins.catalog.description')}</p>
            {catalog?.stale && <p role="status" className="mt-1.5 text-[11px] text-ink-muted">{t('settings.plugins.catalog.cached')}</p>}
          </div>
        </div>
        <button
          type="button"
          disabled={loading || busy !== ''}
          onClick={() => void refresh()}
          className="inline-flex shrink-0 items-center gap-2 rounded-lg border border-glass-line bg-glass-subtle px-3 py-2 text-xs font-medium text-ink transition hover:bg-glass-hover disabled:opacity-50"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} aria-hidden />
          {loading ? t('settings.plugins.catalog.refreshing') : t('settings.plugins.catalog.refresh')}
        </button>
      </header>

      {failure && <p role="alert" className="mb-3 rounded-lg border border-[var(--app-danger)]/25 bg-[var(--app-danger)]/5 px-3 py-2 text-xs text-[var(--app-danger)]">{failure}</p>}

      {catalog && catalog.plugins.length > 0 && (
        <label className="mb-2.5 flex items-center gap-2 rounded-lg border border-glass-line bg-glass-subtle px-3 py-1.5 text-ink-faint">
          <Search className="h-4 w-4 shrink-0" aria-hidden />
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t('settings.plugins.catalog.search')}
            className="min-w-0 flex-1 bg-transparent text-xs text-ink outline-none placeholder:text-ink-faint"
          />
        </label>
      )}

      {!catalog && loading ? (
        <p role="status" className="rounded-xl border border-dashed border-glass-line px-3 py-4 text-center text-xs text-ink-faint">{t('settings.plugins.catalog.loading')}</p>
      ) : !catalog ? null : catalog.plugins.length === 0 ? (
        <p className="rounded-xl border border-dashed border-glass-line px-3 py-4 text-center text-xs text-ink-faint">{t('settings.plugins.catalog.empty')}</p>
      ) : visiblePlugins.length > 0 ? (
        <ul className="grid gap-2.5 md:grid-cols-2 2xl:grid-cols-3">
          {visiblePlugins.map((item) => {
            const { entry } = item
            const installed = item.installedVersion === entry.version
            return (
              <li key={`${entry.id}@${entry.version}`} className="flex min-w-0 flex-col justify-between rounded-xl border border-glass-line bg-glass-subtle p-3">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <h3 className="min-w-0 break-words text-xs font-semibold text-ink">{entry.name}</h3>
                    <span className="rounded-full border border-glass-line px-2 py-0.5 text-[10px] text-ink-faint">v{entry.version}</span>
                    {!item.compatible && <span className="rounded-full border border-[var(--app-danger-line)] px-2 py-0.5 text-[10px] text-[var(--app-danger)]">{t('settings.plugins.catalog.incompatible')}</span>}
                  </div>
                  <p className="mt-0.5 break-all text-[10px] text-ink-faint">{entry.id} · {t('settings.plugins.catalog.author', { author: entry.author })}</p>
                  <p className="mt-1.5 line-clamp-2 text-[11px] leading-4 text-ink-muted">{entry.description}</p>
                  {item.installedVersion && (
                    <p className="mt-1.5 text-[10px] text-ink-faint">{t('settings.plugins.catalog.installedVersion', { version: item.installedVersion })}</p>
                  )}
                  <p className="mt-1 text-[10px] text-ink-faint">{t('settings.plugins.catalog.capabilities', { count: entry.capabilities.length })}</p>
                </div>
                <div className="mt-2.5 flex flex-wrap items-center justify-between gap-2 border-t border-glass-line pt-2">
                  <span className="text-[10px] text-ink-faint">{entry.platform.architecture} · {formatBytes(entry.sizeBytes)}</span>
                  <button
                    type="button"
                    disabled={!item.installable || busy !== '' || loading}
                    onClick={() => void install(item)}
                    className="inline-flex items-center gap-1.5 rounded-lg bg-brand-600 px-2.5 py-1.5 text-[11px] font-medium text-white transition hover:bg-brand-500 disabled:cursor-default disabled:opacity-50"
                  >
                    <Download className="h-3.5 w-3.5" aria-hidden />
                    {busy === entry.id
                      ? t('settings.plugins.catalog.installing')
                      : installed
                        ? t('settings.plugins.catalog.installed')
                        : item.installable && item.installedVersion
                        ? t('settings.plugins.catalog.update')
                          : item.installedVersion
                            ? t('settings.plugins.catalog.noUpdate')
                            : item.compatible
                              ? t('settings.plugins.catalog.install')
                              : t('settings.plugins.catalog.incompatible')}
                  </button>
                </div>
              </li>
            )
          })}
        </ul>
      ) : (
        <p className="rounded-xl border border-dashed border-glass-line px-3 py-4 text-center text-xs text-ink-faint">{t('settings.plugins.catalog.noResults')}</p>
      )}
    </section>
  )
}

function formatBytes(bytes: number): string {
  return bytes < 1024 * 1024
    ? `${Math.max(1, Math.round(bytes / 1024))} KB`
    : `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message: unknown }).message)
  }
  return cause instanceof Error ? cause.message : String(cause)
}
