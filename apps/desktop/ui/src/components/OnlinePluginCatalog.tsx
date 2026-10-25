import { useEffect, useMemo, useState, useSyncExternalStore } from 'react'
import { Dialog, DialogButton } from '@wonderland/ui'
import { Download, KeyRound, PackageSearch, RefreshCw, Search, SlidersHorizontal } from 'lucide-react'
import { pluginApi, type PluginCatalogSnapshot } from '../plugins/api'
import { canInstallCatalogItem, createDistributionCatalogState } from '../plugins/distributionCatalogState'
import { listen } from '../core/transport'
import type { PluginRuntimeState } from '@wonderland/plugin-protocol'
import { t, type MessageKey } from '../i18n'
import { createRequestId } from '@wonderland/plugin-ui-sdk'
import { catalogErrorMessageKey } from '../plugins/catalogErrors'
import { catalogAction, catalogInstallMode, catalogActionLabels } from '../plugins/catalogActions'
import { pluginManagement } from '../plugins/managementState'
import { PluginManagementActivity, usePluginManagement } from '../plugins/management'
import './pluginInstall.css'

interface CatalogReview {
  item: PluginCatalogSnapshot['plugins'][number]
  catalog: PluginCatalogSnapshot
  previous: PluginRuntimeState | undefined
  previousSource: string
  dependencies: { id: string; version: string; optional: boolean; status: string }[]
}

export function OnlinePluginCatalog({ onInstalled, updatesOnly = false, onFilter }: { onInstalled: (states: PluginRuntimeState[]) => void; updatesOnly?: boolean; onFilter?: (updates: boolean) => void }) {
  const [store] = useState(() => createDistributionCatalogState({ settings: pluginApi.distributionSettings, catalog: pluginApi.catalog, configure: pluginApi.configureDistribution }))
  const { catalog, settings, loading, saving, errorCode } = useSyncExternalStore(store.subscribe, store.getSnapshot)
  const [reading, setBusy] = useState('')
  const operation = usePluginManagement()
  const busy = reading || operation?.id || ''
  const [failure, setFailure] = useState('')
  const [cancelled, setCancelled] = useState(false)
  const [query, setQuery] = useState('')
  const [sourceInput, setSourceInput] = useState('')
  const [accessCodeInput, setAccessCodeInput] = useState('')
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [review, setReview] = useState<CatalogReview | null>(null)
  const [batchReview, setBatchReview] = useState<PluginCatalogSnapshot['plugins'] | null>(null)
  const [batchPrevious, setBatchPrevious] = useState<PluginRuntimeState[]>([])
  const [batchResults, setBatchResults] = useState<{ id: string; status: string; error?: string }[] | null>(null)
  const [retryAt, setRetryAt] = useState(0)
  const [now, setNow] = useState(Date.now)
  const retrySeconds = Math.max(0, Math.ceil((retryAt - now) / 1000))
  const credentialState = catalog?.credentialState ?? settings?.credentialState
  const disabled = loading || busy !== '' || saving
  const refresh = () => { if (disabled || retrySeconds > 0) return; setFailure(''); void store.refresh() }

  useEffect(() => {
    let active = true
    let stop: (() => void) | undefined
    void listen('plugins:distribution-changed', () => {
      if (!active) return
      setAccessCodeInput(''); setFailure(''); setReview(null); setBatchReview(null); store.reset(); void store.refresh()
    }).then(unlisten => { if (!active) { unlisten(); return }; stop = unlisten; void store.refresh() }).catch(() => { if (active) void store.refresh() })
    return () => { active = false; stop?.(); store.invalidate() }
  }, [store])
  useEffect(() => { setSourceInput(settings?.sourceUrl ?? '') }, [settings?.sourceUrl])
  useEffect(() => { const time = Date.now(); setNow(time); setRetryAt(catalog?.status === 'rate_limited' ? time + (catalog.retryAfterSeconds ?? 0) * 1000 : 0) }, [catalog])
  useEffect(() => {
    if (retryAt <= Date.now()) return
    const timer = window.setInterval(() => { setNow(Date.now()); if (Date.now() >= retryAt) window.clearInterval(timer) }, 1000)
    return () => window.clearInterval(timer)
  }, [retryAt])

  const visiblePlugins = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase()
    return (catalog?.plugins ?? []).filter(item => (!updatesOnly || catalogAction(item) === 'update') && (normalized === '' || [item.registration.name, item.registration.id, item.registration.description, item.registration.author].some(value => value.toLocaleLowerCase().includes(normalized))))
  }, [catalog, query, updatesOnly])
  const updates = (catalog?.plugins ?? []).filter(item => catalogAction(item) === 'update' && canInstallCatalogItem(catalog, item))
  const reviewUpdates = async () => {
    if (disabled || !catalog || !updates.length) return
    const snapshot = catalog
    setBusy('batch-review'); setFailure('')
    try {
      const previous = await pluginApi.states()
      if (store.getSnapshot().catalog !== snapshot) return
      setBatchPrevious(previous); setBatchReview(updates)
    } catch (cause) { setFailure(safeFailure(cause)) }
    finally { setBusy('') }
  }

  const reviewInstall = async (item: PluginCatalogSnapshot['plugins'][number]) => {
    if (!catalog || !item.entry || !canInstallCatalogItem(catalog, item) || disabled) return
    const reviewedCatalog = catalog
    setBusy(item.entry.id); setFailure(''); setCancelled(false)
    try {
      const states = await pluginApi.states()
      if (store.getSnapshot().catalog !== reviewedCatalog) return
      const previous = states.find(state => state.manifest.id === item.entry!.id)
      const previousSource = previous?.installationSource?.origin ?? t(previous ? 'settings.plugins.source.unknown' : 'settings.plugins.source.none')
      const dependencies = item.entry.dependencies.map(dependency => {
        const provider = states.find(state => state.manifest.id === dependency.pluginId)
        return { id: dependency.pluginId, version: dependency.versionReq, optional: dependency.optional,
          status: !provider ? t('settings.plugins.catalog.dependencyMissing') : !provider.enabled ? t('settings.plugins.catalog.dependencyDisabled') : t('settings.plugins.catalog.dependencyInstalled', { version: provider.manifest.version }) }
      })
      setReview({ item, catalog: reviewedCatalog, previous, previousSource, dependencies })
    } catch (cause) { setFailure(safeFailure(cause)) } finally { setBusy('') }
  }
  const confirmInstall = async () => {
    if (!review?.item.entry || review.catalog !== catalog || disabled || !canInstallCatalogItem(catalog, review.item)) return
    const entry = review.item.entry
    const operationId = createRequestId()
    const job = pluginManagement.begin({ id: operationId, label: `${catalogActionLabels[catalogAction(review.item)]} ${review.item.registration.name}`, kind: 'catalog', phase: 'checking', cancellable: true })
    if (!job) return
    setFailure(''); setCancelled(false)
    try {
      onInstalled(await pluginApi.installCatalog(entry.id, entry.version, entry.sha256, true, catalogInstallMode(review.item), operationId, entry.sourceUrl))
      setReview(null)
      job.finish()
      if (!store.getSnapshot().loading) await store.refresh()
    } catch (cause) { const cancelled = typeof cause === 'object' && cause !== null && 'code' in cause && cause.code === 'CANCELLED'; setCancelled(cancelled); setFailure(cancelled ? safeFailure(cause) : t('settings.plugins.installFailed', { error: safeFailure(cause) })) } finally { job.finish() }
  }

  const installUpdates = async () => {
    if (!batchReview?.length || disabled) return
    const items = batchReview
    const job = pluginManagement.begin({ id: createRequestId(), label: '批量更新插件', kind: 'catalog', batch: true, totalItems: items.length, position: 0 })
    if (!job) return
    setBatchReview(null); setFailure('')
    const results: { id: string; status: string; error?: string }[] = []
    try {
      for (let index = 0; index < items.length; index++) {
        const item = items[index], entry = item.entry!
        if (job.cancelled()) { results.push({id:entry.id,status:'已取消'}); continue }
        const operationId = createRequestId()
        job.update({ id: operationId, label: `更新 ${item.registration.name}`, phase: 'checking', position: index + 1, downloadedBytes: 0, totalBytes: 0, cancellable: true })
        try { onInstalled(await pluginApi.installCatalog(entry.id, entry.version, entry.sha256, true, 'update', operationId, entry.sourceUrl)); results.push({id:entry.id,status:'更新成功'}) }
        catch (cause) { results.push({id:entry.id,status:typeof cause === 'object' && cause !== null && 'code' in cause && cause.code === 'CANCELLED' ? '已取消' : '更新失败',error:safeFailure(cause)}) }
      }
    } finally { job.finish(); setBatchResults(results); await store.refresh() }
  }
  const notices = [...new Set([
    catalog && catalog.status !== 'ready' ? t(catalogStatusKeys[catalog.status]) : '',
    credentialState && !['none', 'valid', 'unchecked'].includes(credentialState) ? t(`settings.plugins.distribution.credential.${credentialState}`) : '',
    catalog?.stale ? t('settings.plugins.catalog.cached', { time: formatTime(catalog.fetchedAt) }) : '',
    retrySeconds > 0 ? t('settings.plugins.distribution.retryAfter', { seconds: retrySeconds }) : '',
    settings?.warning ?? '',
  ].filter(Boolean))]

  return <>
    <section className="glass-card rounded-2xl border border-glass-line p-4 sm:p-5">
      <header className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2.5"><PackageSearch className="h-4 w-4 text-brand-400" aria-hidden /><h2 className="text-sm font-semibold text-ink">{t('settings.plugins.catalog.title')}</h2>{credentialState === 'valid' && <span className="text-xs text-ink-muted">{t('settings.plugins.catalog.authorized')}</span>}</div>
        <div className="flex items-center gap-2">
          <button type="button" onClick={() => { setFailure(''); setSettingsOpen(true) }} className="app-button app-button-quiet inline-flex items-center"><SlidersHorizontal className="h-3.5 w-3.5" aria-hidden />{t('settings.plugins.catalog.settings')}</button>
          <button type="button" disabled={disabled || retrySeconds > 0} onClick={refresh} className="app-button app-button-quiet inline-flex items-center"><RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} aria-hidden />{t(loading ? 'settings.plugins.catalog.refreshing' : 'settings.plugins.catalog.refresh')}</button>
        </div>
      </header>
      {!review && <PluginManagementActivity />}
      {onFilter && <div className="mb-3 flex flex-wrap items-center gap-2"><div role="group" aria-label="目录筛选" className="flex gap-1"><button type="button" className="app-button" aria-pressed={!updatesOnly} onClick={() => onFilter(false)}>全部</button><button type="button" className="app-button" aria-pressed={updatesOnly} onClick={() => onFilter(true)}>可更新 · {updates.length}</button></div>{updates.length > 0 && <button type="button" disabled={disabled} className="app-button ml-auto" onClick={() => void reviewUpdates()}>更新全部</button>}</div>}
      {notices.length > 0 && <div role="status" aria-live="polite" className="mb-4 space-y-1 text-xs leading-relaxed text-ink-muted">{notices.map(message => <p key={message}>{message}</p>)}</div>}
      {errorCode && <p role="alert" className="app-dialog-notice mb-3" data-tone="danger">{failureForCode(errorCode)}</p>}
      {failure && !review && !settingsOpen && <p role={cancelled ? "status" : "alert"} className="app-dialog-notice mb-3" data-tone={cancelled ? undefined : "danger"}>{failure}</p>}
      {catalog && catalog.plugins.length > 0 && (
        <label className="app-search-field mb-2.5 flex items-center gap-2 rounded-lg border border-glass-line bg-glass-subtle px-3 py-1.5 text-ink-faint">
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
        <p role="status" className="py-10 text-center text-sm text-ink-faint">{t('settings.plugins.catalog.loading')}</p>
      ) : !catalog || catalog.status === 'unconfigured' ? null : catalog.plugins.length === 0 ? (
        <p className="rounded-xl border border-dashed border-glass-line px-3 py-4 text-center text-xs text-ink-faint">{t('settings.plugins.catalog.empty')}</p>
      ) : visiblePlugins.length > 0 ? (
        <ul className="grid gap-2.5 md:grid-cols-2 2xl:grid-cols-3">
          {visiblePlugins.map((item) => {
            const { registration, entry } = item
            const action = catalogAction(item)
            return (
              <li key={registration.id} className="flex min-h-44 min-w-0 flex-col justify-between rounded-xl border border-glass-line bg-glass-subtle p-3">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <h3 className="min-w-0 break-words text-xs font-semibold text-ink">{registration.name}</h3>
                    {registration.visibility === 'restricted' && <span className="rounded-md border border-glass-line px-2 py-0.5 text-xs text-ink-faint">{t('settings.plugins.catalog.restricted')}</span>}
                    {entry
                      ? <span className="rounded-md border border-glass-line px-2 py-0.5 text-xs text-ink-faint">v{entry.version}</span>
                      : <span className="rounded-md border border-glass-line px-2 py-0.5 text-xs text-ink-faint">{t('settings.plugins.catalog.updateUnavailable')}</span>}
                    {entry && !item.compatible && <span className="rounded-md border border-[var(--app-danger-line)] px-2 py-0.5 text-xs text-[var(--app-danger)]">{t('settings.plugins.catalog.incompatible')}</span>}
                  </div>
                  <p className="mt-0.5 break-all text-xs text-ink-faint">{registration.id} · {t('settings.plugins.catalog.author', { author: registration.author })}</p>
                  <p className="mt-1.5 line-clamp-2 text-xs leading-4 text-ink-muted">{registration.description}</p>
                  {item.installedVersion && (
                    <p className="mt-1.5 text-xs text-ink-faint">{t('settings.plugins.catalog.installedVersion', { version: item.installedVersion })}</p>
                  )}
                  {entry && entry.dependencies.length > 0 && <p className="mt-1 text-xs text-ink-faint">{t('settings.plugins.catalog.dependencies', { count: entry.dependencies.length })}</p>}
                  {item.error && <p className="mt-1 text-xs text-[var(--app-danger)]">{failureForCode(item.error.code)}</p>}
                  {item.stale && <p className="mt-1 text-xs text-ink-faint">{t('settings.plugins.catalog.offlineOnly')}</p>}
                </div>
                <div className="mt-2.5 flex flex-wrap items-center justify-between gap-2 border-t border-glass-line pt-2">
                  <span className="text-xs text-ink-faint">{entry ? `${entry.platform.architecture} · ${formatBytes(entry.sizeBytes)}` : t('settings.plugins.catalog.updateUnavailable')}</span>
                  <button
                    type="button"
                    disabled={!canInstallCatalogItem(catalog, item) || busy !== '' || loading || saving}
                    onClick={() => void reviewInstall(item)}
                    className="app-button app-button-primary inline-flex items-center"
                  >
                    <Download className="h-3.5 w-3.5" aria-hidden />
                    {busy === registration.id
                      ? t('settings.plugins.catalog.installing')
                      : !entry
                        ? t('settings.plugins.catalog.updateUnavailable')
                        : item.installable
                            ? catalogActionLabels[action]
                            : catalog.stale || item.stale
                              ? t('settings.plugins.catalog.offlineOnly')
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
    <Dialog open={settingsOpen} title={t('settings.plugins.catalog.settingsTitle')} size="md" closeLabel={t('common.close')} dismissible={!saving && !busy} onClose={() => { setSettingsOpen(false); setAccessCodeInput('') }} footer={<DialogButton disabled={saving || Boolean(busy)} data-dialog-autofocus onClick={() => { setSettingsOpen(false); setAccessCodeInput('') }}>{t('common.done')}</DialogButton>}>
      <form className="catalog-settings-form" onSubmit={event => {
        event.preventDefault()
        if (saving || busy || !settings || sourceInput.trim() === (settings.sourceUrl ?? '')) return
        setAccessCodeInput(''); setFailure(''); void store.configure({ sourceUrl: sourceInput.trim() })
      }}>
        <label htmlFor="plugin-distribution-source">{t('settings.plugins.distribution.source')}</label>
        <div className="catalog-settings-field"><input id="plugin-distribution-source" className="app-input" type="url" value={sourceInput} onChange={event => setSourceInput(event.target.value)} disabled={saving || Boolean(busy) || !settings} placeholder="https://plugins.example.com" autoComplete="off" spellCheck={false} /><DialogButton type="submit" disabled={saving || Boolean(busy) || !settings || sourceInput.trim() === (settings.sourceUrl ?? '')}>{t('common.save')}</DialogButton></div>
        {settings?.credentialConfigured && sourceInput.trim() !== (settings.sourceUrl ?? '') && <p className="text-xs text-ink-muted">{t('settings.plugins.distribution.sourceChange')}</p>}
      </form>
      <form className="catalog-settings-form" onSubmit={event => {
        event.preventDefault()
        if (saving || busy || !settings?.configured || !accessCodeInput.trim()) return
        const accessCode = accessCodeInput.trim(); setAccessCodeInput(''); setFailure(''); void store.configure({ accessCode })
      }}>
        <label htmlFor="plugin-distribution-code" className="flex items-center gap-2"><KeyRound className="h-3.5 w-3.5" aria-hidden />{t('settings.plugins.distribution.code')}</label>
        <div className="catalog-settings-field"><input id="plugin-distribution-code" className="app-input" type="password" value={accessCodeInput} onChange={event => setAccessCodeInput(event.target.value)} disabled={saving || Boolean(busy) || !settings?.configured} placeholder={t(settings?.credentialConfigured ? 'settings.plugins.distribution.replaceCode' : 'settings.plugins.distribution.optionalCode')} autoComplete="off" spellCheck={false} /><DialogButton type="submit" disabled={saving || Boolean(busy) || !settings?.configured || !accessCodeInput.trim()}>{t('common.save')}</DialogButton>{settings?.credentialConfigured && <DialogButton disabled={saving || Boolean(busy)} onClick={() => { setAccessCodeInput(''); setFailure(''); void store.configure({ clearAccessCode: true }) }}>{t('settings.plugins.distribution.clearCode')}</DialogButton>}</div>
        {settings?.expiresAt && <p className="text-xs text-ink-muted">{t('settings.plugins.distribution.expiresAt', { time: formatTime(settings.expiresAt) })}</p>}
      </form>
      {saving && <p className="mt-4 text-xs text-ink-muted" role="status">{t('settings.plugins.distribution.saving')}</p>}
      {errorCode && <p role="alert" className="app-dialog-notice mt-4" data-tone="danger">{failureForCode(errorCode)}</p>}
    </Dialog>
    <Dialog open={Boolean(review)} title={review ? `${catalogActionLabels[catalogAction(review.item)]}插件` : '安装插件'} closeLabel={t('common.close')} size="md" dismissible={!busy} onClose={() => { setReview(null); setFailure('') }} footer={<>
      <DialogButton disabled={Boolean(busy)} data-dialog-autofocus onClick={() => { setReview(null); setFailure('') }}>{t('common.cancel')}</DialogButton>
      <DialogButton variant="primary" aria-busy={Boolean(busy)} disabled={disabled || review?.catalog !== catalog} onClick={() => void confirmInstall()}>{busy ? '安装中' : review ? catalogActionLabels[catalogAction(review.item)] : '安装'}</DialogButton>
    </>}>
      {review && <div className="space-y-4">
        <PluginManagementActivity />
        <div><h3 className="text-base font-semibold text-ink">{review.item.registration.name}</h3><p className="mt-1 text-xs text-ink-muted">{review.item.registration.author} · {review.item.registration.id}</p></div>
        <dl className="app-dialog-facts"><div><dt>{t('settings.plugins.batch.version')}</dt><dd>{review.previous ? review.previous.manifest.version + ' → ' : ''}{review.item.entry?.version}</dd></div><div><dt>{t('settings.plugins.catalog.reviewSource')}</dt><dd>{review.item.entry?.sourceUrl}</dd></div></dl>
        {review.previous && <p className="app-dialog-notice">{t(review.previous.installationSource && (review.previous.installationSource.kind !== 'catalog' || review.previous.installationSource.origin !== review.item.entry?.sourceUrl) ? 'settings.plugins.catalog.sourceReplacement' : 'settings.plugins.batch.replacement')}</p>}
        {catalogAction(review.item) === 'rollback' && <p className="app-dialog-notice">回退只替换程序包，保留业务数据；请确认旧版插件能够读取当前数据。</p>}
        {review.dependencies.length > 0 && <details className="app-dialog-details" open><summary>{t('settings.plugins.catalog.dependencies', { count: review.dependencies.length })}</summary><ul className="space-y-2">{review.dependencies.map(dependency => <li key={dependency.id}><span className="font-medium text-ink">{dependency.id} {dependency.version}</span><p>{dependency.optional ? t('settings.plugins.catalog.optionalDependency') + ' · ' : ''}{dependency.status}</p></li>)}</ul></details>}
        <details className="app-dialog-details"><summary>{t('settings.plugins.catalog.reviewIdentity')}</summary><dl className="app-dialog-facts"><div><dt>{t('settings.plugins.batch.publicKey')}</dt><dd className="font-mono text-xs">{review.item.entry?.signingPublicKey}</dd></div>{review.previous && <div><dt>{t('settings.plugins.batch.previousSource')}</dt><dd>{review.previousSource}</dd></div>}</dl></details>
        {review.catalog !== catalog && <p role="alert" className="app-dialog-notice" data-tone="danger">{t('settings.plugins.distribution.catalogChanged')}</p>}
        {failure && <p role={cancelled ? "status" : "alert"} className="app-dialog-notice" data-tone={cancelled ? undefined : "danger"}>{failure}</p>}
      </div>}
    </Dialog>
    <Dialog open={Boolean(batchReview)} title="批量更新插件" closeLabel="关闭确认窗口" size="md" onClose={() => setBatchReview(null)} footer={<><DialogButton data-dialog-autofocus onClick={() => setBatchReview(null)}>取消</DialogButton><DialogButton variant="primary" disabled={disabled} onClick={() => void installUpdates()}>更新 {batchReview?.length} 个插件</DialogButton></>}><ul className="space-y-3">{batchReview?.map(item => {
      const previous = batchPrevious.find(state => state.manifest.id === item.registration.id)
      const changed = !previous?.installationSource || previous.installationSource.kind !== 'catalog' || previous.installationSource.origin !== item.entry?.sourceUrl
      return <li key={item.registration.id}><strong>{item.registration.name}</strong><p className="text-sm text-ink-muted">{item.installedVersion} → {item.entry?.version}</p><p className="break-all text-xs text-ink-muted">{item.entry?.sourceUrl}</p>{changed && <p className="app-dialog-notice mt-2">{t('settings.plugins.catalog.sourceReplacement')}</p>}</li>
    })}</ul><p className="app-dialog-notice mt-4">逐个更新并保留插件数据，单项失败后继续其余项。</p></Dialog>
    <Dialog open={batchResults !== null} title="批量更新结果" closeLabel="关闭结果窗口" size="md" onClose={() => setBatchResults(null)} footer={<DialogButton onClick={() => setBatchResults(null)}>完成</DialogButton>}><ul className="space-y-3">{batchResults?.map(item => <li key={item.id}><strong>{item.id}</strong><span className="ml-3 text-sm">{item.status}</span>{item.error && <p className="mt-1 text-sm text-ink-muted">{item.error}</p>}</li>)}</ul></Dialog>
  </>
}

function formatBytes(bytes: number): string {
  return bytes < 1024 * 1024
    ? `${Math.max(1, Math.round(bytes / 1024))} KB`
    : `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

const catalogStatusKeys: Record<Exclude<PluginCatalogSnapshot['status'], 'ready'>, MessageKey> = {
  unconfigured: 'settings.plugins.distribution.unconfigured',
  offline: 'settings.plugins.distribution.offline',
  rate_limited: 'settings.plugins.distribution.rateLimited',
  unavailable: 'settings.plugins.distribution.unavailable',
  invalid_response: 'settings.plugins.distribution.invalidResponse',
}

function formatTime(value: string | null): string {
  if (!value) return t('settings.plugins.distribution.unknownTime')
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? t('settings.plugins.distribution.unknownTime') : date.toLocaleString()
}

function safeFailure(cause: unknown): string {
  return failureForCode(typeof cause === 'object' && cause !== null && 'code' in cause && typeof cause.code === 'string' ? cause.code : '')
}

function failureForCode(code: string): string {
  const key = catalogErrorMessageKey(code), label = t(key)
  return key !== 'settings.plugins.distribution.operationFailed' && /^[A-Z_]{1,64}$/.test(code) ? `${label}（${code}）` : label
}
