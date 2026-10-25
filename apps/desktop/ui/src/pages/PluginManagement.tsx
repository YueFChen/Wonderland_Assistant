import { Dialog, DialogButton, Drawer } from '@wonderland/ui'
import { useEffect, useRef, useState } from 'react'
import type { PluginRuntimeState } from '@wonderland/plugin-protocol'
import { Clipboard, Info, PackagePlus, RotateCw, Search, Trash2 } from 'lucide-react'
import { useNavigate } from 'react-router-dom'

import { pluginApi, usePlugins, type RetainedPluginData } from '../plugins/api'
import { pluginManagement } from '../plugins/managementState'
import { PluginManagementActivity, usePluginManagement } from '../plugins/management'
import { PluginIcon } from '../plugins/icons'
import { t, type MessageKey } from '../i18n'
import { ToggleSwitch } from '../components/ToggleSwitch'
import { isWebClient } from '../core/transport'

export function PluginManagement({ states, error, setStates }: {
  states: PluginRuntimeState[] | null
  error: MessageKey | null
  setStates: (states: PluginRuntimeState[]) => void
}) {
  const navigate = useNavigate()
  const operation = usePluginManagement()
  const busy = operation?.id ?? ''
  const { refresh } = usePlugins()
  const [failure, setFailure] = useState('')
  const [uninstallTarget, setUninstallTarget] = useState<PluginRuntimeState | null>(null)
  const [deletePluginData, setDeletePluginData] = useState(false)
  const cancelUninstallButton = useRef<HTMLButtonElement>(null)
  const [query, setQuery] = useState('')
  const [detailsId, setDetailsId] = useState<string | null>(null)
  const [retained, setRetained] = useState<RetainedPluginData[]>([])
  const [cleanupTarget, setCleanupTarget] = useState<RetainedPluginData | null>(null)
  const signatureLabels: Record<string,string> = {confirmed:'身份已确认',unconfirmed:'签名有效，身份未确认',unsigned:'未签名',invalid:'签名无效',identity_unavailable:'身份记录不可用'}
  const copyDetail = async () => { if(!detail)return;try{await navigator.clipboard.writeText(JSON.stringify({pluginId:detail.manifest.id,error:detail.lastError,scan:detail.scanDiagnostic},null,2))}catch{setFailure('复制失败，请选中诊断内容后手动复制。')} }
  const detail = states?.find(state => state.manifest.id === detailsId)
  const visible = states?.filter(state => !query.trim() || [state.manifest.id, state.manifest.name, state.manifest.author ?? ''].some(value => value.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())))
  useEffect(() => {
    if (isWebClient) return
    let active = true
    void pluginApi.retainedData().then(value => { if (active) setRetained(value) }).catch(cause => { if (active) setFailure(`保留数据读取失败：${errorText(cause)}`) })
    return () => { active = false }
  }, [states])

  const uninstall = async () => {
    if (!uninstallTarget) return
    const state = uninstallTarget
    const job = pluginManagement.begin({ id: 'remove-' + state.manifest.id, label: '卸载插件', kind: 'management' })
    if (!job) return
    setFailure('')
    try {
      setStates(await pluginApi.uninstall(state.manifest.id, deletePluginData))
      setUninstallTarget(null)
    } catch (cause) {
      setFailure(t('settings.plugins.uninstallFailed', { error: errorText(cause) }))
      // Package removal may have completed before optional data cleanup failed.
      try {
        const current = await pluginApi.states()
        setStates(current)
        if (!current.some(item => item.manifest.id === state.manifest.id)) setFailure(`插件包已移除，部分数据清理未完成，可在保留数据中重试：${errorText(cause)}`)
      } catch {
        // Keep the last rendered state and show the operation error.
      }
      setUninstallTarget(null)
    } finally {
      job.finish()
    }
  }

  const setEnabled = async (state: PluginRuntimeState, enabled: boolean) => {
    const job = pluginManagement.begin({ id: 'enable-' + state.manifest.id, label: '修改启用状态', kind: 'management' })
    if (!job) return
    setFailure('')
    try {
      setStates(await pluginApi.setEnabled(state.manifest.id, enabled))
    } catch (cause) {
      setFailure(t('settings.plugins.saveFailed') + `：${errorText(cause)}`)
    } finally {
      job.finish()
    }
  }

  const retryScan = async (state: PluginRuntimeState) => {
    const job = pluginManagement.begin({ id: 'scan-' + state.manifest.id, label: '重新检查插件', kind: 'management' })
    if (!job) return
    setFailure('')
    try {
      setStates(await pluginApi.retryScan(state.manifest.id))
    } catch (cause) {
      setFailure(t('settings.plugins.scan.retryFailed', { error: errorText(cause) }))
    } finally {
      job.finish()
    }
  }

  const retryRuntime = async (state: PluginRuntimeState) => {
    const job = pluginManagement.begin({ id: 'retry-' + state.manifest.id, label: '重试启动', kind: 'management' })
    if (!job) return
    setFailure('')
    try { setStates(await pluginApi.retryRuntime(state.manifest.id)) }
    catch (cause) { setFailure(errorText(cause)) }
    finally { job.finish() }
  }

  const cleanup = async () => {
    if (!cleanupTarget) return
    const job = pluginManagement.begin({ id: 'cleanup-' + cleanupTarget.id, label: '清理保留数据', kind: 'management' })
    if (!job) return
    setFailure('')
    try { setRetained(await pluginApi.cleanupData(cleanupTarget.id)); setCleanupTarget(null) }
    catch (cause) { setFailure(`清理未完成，可保留此记录后重试：${errorText(cause)}`); void pluginApi.retainedData().then(setRetained).catch(() => {}) }
    finally { job.finish() }
  }

  return (
    <>
    <section className="glass-card app-ornamented p-4 sm:p-5">
      <header className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2.5">
          <span className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-brand-600/10 text-brand-400">
            <PackagePlus className="h-4 w-4" aria-hidden />
          </span>
          <div className="min-w-0">
            <h2 className="text-sm font-semibold text-ink">{t('settings.plugins.title')}</h2>
          </div>
        </div>
        <div className="flex items-center gap-2">
        <button type="button" className="app-button app-button-quiet" aria-label="刷新插件状态" title="刷新插件状态" onClick={refresh}><RotateCw className="h-3.5 w-3.5" aria-hidden /></button>
        {!isWebClient && <button type="button" disabled={Boolean(busy)} onClick={() => navigate('/workspace/plugins/install?filter=updates')} className="app-button">检查更新</button>}
        {!isWebClient && <button
          type="button"
          onClick={() => navigate('/workspace/plugins/install')}
          className="app-button inline-flex items-center"
          disabled={Boolean(busy)}
        >
          <PackagePlus className="h-3.5 w-3.5" aria-hidden />
          {t('settings.plugins.install')}
        </button>}
        </div>
      </header>
      <PluginManagementActivity />
      {states && states.length > 0 && <label className="app-search-field mb-3 flex items-center gap-2"><Search className="h-4 w-4" aria-hidden /><input type="search" className="min-w-0 flex-1 bg-transparent outline-none" aria-label="搜索已安装插件" placeholder="搜索插件" value={query} onChange={event => setQuery(event.target.value)} /></label>}

      {error && <p role="alert" className="mb-3 rounded-lg border border-[var(--app-danger)]/25 bg-[var(--app-danger)]/5 px-3 py-2 text-xs text-[var(--app-danger)]">{t(error)}</p>}
      {failure && <p role="alert" className="mb-3 rounded-lg border border-[var(--app-danger)]/25 bg-[var(--app-danger)]/5 px-3 py-2 text-xs text-[var(--app-danger)]">{failure}</p>}

      {!states ? (
        <p role="status" className="rounded-xl border border-dashed border-glass-line px-4 py-4 text-center text-sm text-ink-faint">{t('settings.plugins.loading')}</p>
      ) : states.length === 0 ? (
        <p className="rounded-xl border border-dashed border-glass-line px-4 py-4 text-center text-sm text-ink-faint">{t('settings.plugins.empty')}</p>
      ) : (
        <ul className="grid gap-2.5 md:grid-cols-2 2xl:grid-cols-3">
          {visible?.map((state) => (
            <PluginCard
              key={state.manifest.id}
              state={state}
              busy={Boolean(busy)}
              onDetails={() => setDetailsId(state.manifest.id)}
              onToggle={(enabled) => void setEnabled(state, enabled)}
              onUninstall={() => {
                setDeletePluginData(false)
                setUninstallTarget(state)
              }}
              onRetryScan={() => void retryScan(state)}
            />
          ))}
        </ul>
      )}
      {states && states.length > 0 && visible?.length === 0 && <p className="py-6 text-center text-sm text-ink-muted">没有匹配的插件</p>}
      {!isWebClient && retained.length > 0 && <details className="app-dialog-details mt-4"><summary>保留数据 · {retained.length}</summary><ul className="space-y-2">{retained.map(item => <li key={item.id} className="flex flex-wrap items-center justify-between gap-2"><span className="text-sm text-ink">{item.id}<span className="ml-2 text-xs text-ink-muted">{[item.hasData && '数据', item.hasSecrets && '插件密钥', item.hasPublisherIdentity && '发布公钥'].filter(Boolean).join(' · ')}</span></span><button type="button" disabled={Boolean(busy)} className="app-button app-button-danger" onClick={() => {setFailure('');setCleanupTarget(item)}}>清理</button></li>)}</ul></details>}
    </section>
    <Dialog open={Boolean(uninstallTarget)} title={t('settings.plugins.uninstallTitle')} size="sm" closeLabel={t('common.close')} dismissible={!busy} initialFocus={cancelUninstallButton} onClose={() => setUninstallTarget(null)} footer={<>
      <DialogButton ref={cancelUninstallButton} disabled={Boolean(busy)} onClick={() => setUninstallTarget(null)}>{t('common.cancel')}</DialogButton>
      <DialogButton variant="danger" disabled={Boolean(busy)} onClick={() => void uninstall()}><Trash2 className="h-3.5 w-3.5" aria-hidden />{t(busy ? 'settings.plugins.uninstalling' : deletePluginData ? 'settings.plugins.uninstallAndDeleteConfirm' : 'settings.plugins.uninstallConfirm')}</DialogButton>
    </>}>
      {uninstallTarget && <>
        <p className="app-dialog-description">{t('settings.plugins.uninstallDescription', { name: uninstallTarget.manifest.name })}</p>
        <fieldset className="mt-4 space-y-2" disabled={Boolean(busy)}>
          <legend className="sr-only">{t('settings.plugins.uninstallTitle')}</legend>
          <label className="flex cursor-pointer items-start gap-3 rounded-xl border border-glass-line p-3 hover:bg-glass-subtle">
            <input type="radio" name="plugin-uninstall-data" checked={!deletePluginData} onChange={() => setDeletePluginData(false)} className="mt-1 accent-[var(--app-accent)]" />
            <span className="block text-sm font-medium text-ink">{t('settings.plugins.keepData')}</span>
          </label>
          <label className="flex cursor-pointer items-start gap-3 rounded-xl border border-glass-line p-3 hover:bg-glass-subtle">
            <input type="radio" name="plugin-uninstall-data" checked={deletePluginData} onChange={() => setDeletePluginData(true)} className="mt-1 accent-[var(--app-danger)]" />
            <span><span className="block text-sm font-medium text-ink">{t('settings.plugins.deleteData')}</span>{deletePluginData && <span className="mt-0.5 block text-xs text-[var(--app-danger)]">{t('settings.plugins.deleteDataHint')}</span>}</span>
          </label>
        </fieldset>
        {uninstallTarget.pluginDataDirectory && <details className="app-dialog-details"><summary>{t('settings.plugins.dataDirectory')}</summary><p className="break-all font-mono text-xs">{uninstallTarget.pluginDataDirectory}</p></details>}
      </>}
    </Dialog>
    <Drawer open={Boolean(detail)} title={detail?.manifest.name ?? '插件详情'} onClose={() => setDetailsId(null)} closeLabel="关闭插件详情" size="lg">
      {detail && <div className="space-y-5 text-sm text-ink"><p className="text-ink-muted">{detail.manifest.description}</p><dl className="app-dialog-facts"><div><dt>插件</dt><dd>{detail.manifest.id} · {detail.manifest.version}</dd></div><div><dt>作者</dt><dd>{detail.manifest.author ?? '未提供'}</dd></div><div><dt>状态</dt><dd>{detail.installation === 'installed' ? '已安装' : detail.installation === 'checking' ? '检查中' : detail.installation === 'incompatible' ? '不兼容' : '包异常'} · {detail.enabled && detail.runtime === 'stopped' ? '待用' : t(`settings.plugins.runtime.${detail.runtime}`)}</dd></div><div><dt>来源</dt><dd>{detail.installationSource?.origin ?? '未记录'}</dd></div><div><dt>数据目录</dt><dd>{detail.pluginDataDirectory}</dd></div><div><dt>实际签名</dt><dd>{signatureLabels[detail.signature.state] ?? detail.signature.state}{detail.signature.publicKey && detail.signature.publicKey !== detail.signature.expectedPublicKey && <code className="mt-1 block break-all">{detail.signature.publicKey}</code>}</dd></div>{detail.signature.expectedPublicKey && <div><dt>发布公钥</dt><dd className="break-all font-mono text-xs">{detail.signature.expectedPublicKey}</dd></div>}</dl>
      {detail.lifecycle && <dl className="app-dialog-facts">
        <div><dt>启动策略</dt><dd>{({on_demand:'按需启动',background_restore:'后台恢复',on_startup:'随 Core 启动'})[detail.lifecycle.policy]}{detail.lifecycle.policySource === 'legacy' ? ' · 旧包兼容' : ''}</dd></div>
        <div><dt>后台恢复</dt><dd>{({unknown:'待核对',not_required:'无需恢复',required:'需要恢复'})[detail.lifecycle.backgroundRestore]}</dd></div>
        {detail.lifecycle.activeActivities > 0 && <div><dt>后台活动</dt><dd>{detail.lifecycle.activeActivities}</dd></div>}
        {detail.lifecycle.diagnostic && <div><dt>生命周期诊断</dt><dd>{detail.lifecycle.diagnostic}</dd></div>}
      </dl>}
      {!!detail.manifest.dependencies?.length && <section><h3 className="mb-2 font-semibold">依赖</h3><ul className="space-y-1">{detail.manifest.dependencies.map(dependency => <li key={dependency.pluginId}>{dependency.pluginId} · {dependency.versionReq}{dependency.optional ? ' · 可选' : ''}</li>)}</ul>{detail.dependencyIssues?.map(issue => <p className="mt-2 text-ink-muted" key={issue}>{issue}</p>)}</section>}
      {(detail.lastError || detail.scanDiagnostic) && <section><h3 className="mb-2 font-semibold">诊断</h3><pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-glass-line p-3 text-xs">{JSON.stringify(detail.scanDiagnostic ?? detail.lastError, null, 2)}</pre><div className="mt-3 flex gap-2"><button type="button" className="app-button" onClick={() => void copyDetail()}>复制诊断</button>{detail.runtime === 'failed' && !detail.scanDiagnostic && <button type="button" className="app-button" disabled={Boolean(busy)} onClick={() => void retryRuntime(detail)}>重试启动</button>}{detail.scanDiagnostic && <button type="button" className="app-button" disabled={Boolean(busy)} onClick={() => void retryScan(detail)}>{detail.scanDiagnostic.code === 'INSTALL_RECOVERY_REQUIRED' ? '重试恢复' : '重新检查'}</button>}</div></section>}
      </div>}
    </Drawer>
    <Dialog open={Boolean(cleanupTarget)} title="清理保留数据" size="sm" closeLabel="关闭确认窗口" dismissible={!busy} onClose={() => setCleanupTarget(null)} footer={<><DialogButton disabled={Boolean(busy)} data-dialog-autofocus onClick={() => setCleanupTarget(null)}>取消</DialogButton><DialogButton variant="danger" disabled={Boolean(busy)} onClick={() => void cleanup()}>删除数据和密钥</DialogButton></>}>
      <p className="app-dialog-description">删除 {cleanupTarget?.id} 保留的业务数据、插件密钥和固定发布公钥，此操作无法撤销。</p>{cleanupTarget && <details className="app-dialog-details"><summary>数据目录</summary><p className="break-all font-mono text-xs">{cleanupTarget.dataDirectory}</p></details>}{failure && <p className="app-dialog-notice mt-3" data-tone="danger" role="alert">{failure}</p>}
    </Dialog>
    </>
  )
}

function PluginCard({ state, busy, onToggle, onUninstall, onRetryScan, onDetails }: {
  state: PluginRuntimeState
  busy: boolean
  onToggle: (enabled: boolean) => void
  onUninstall: () => void
  onRetryScan: () => void
  onDetails: () => void
}) {
  const [copiedDiagnostic, setCopiedDiagnostic] = useState(false)
  const enabled = state.enabled
  const issue = state.scanDiagnostic?.message ?? state.lastError?.message
  const dependencyIssue = state.dependencyIssues?.join(' · ')
  const copyScanDiagnostic = async () => {
    if (!state.scanDiagnostic) return
    const text = JSON.stringify({
      pluginId: state.manifest.id,
      pluginVersion: state.manifest.version,
      occurredAt: state.lastError?.occurredAt ?? null,
      ...state.scanDiagnostic,
    }, null, 2)
    try {
      await navigator.clipboard.writeText(text)
      setCopiedDiagnostic(true)
      window.setTimeout(() => setCopiedDiagnostic(false), 1800)
    } catch {
      setCopiedDiagnostic(false)
    }
  }
  return (
    <li className="plugin-management-card flex min-w-0 flex-col rounded-xl border border-glass-line bg-glass-subtle p-4">
      <div className="flex flex-1 items-start gap-3">
        <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-brand-600/15 text-brand-400">
          <PluginIcon icon={state.manifest.icon ?? 'puzzle'} pluginId={state.manifest.id} className="h-5 w-5" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <h3 className="truncate text-sm font-semibold text-ink">{state.manifest.name}</h3>
            <span className={`rounded-md border px-1.5 py-0.5 text-xs ${enabled ? 'border-brand-500/25 bg-brand-500/10 text-brand-400' : 'border-glass-line text-ink-faint'}`}>
              {enabled ? t('settings.plugins.enabled') : t('settings.plugins.disabled')}
            </span>
            {state.installation !== 'installed' && (
              <span className="rounded-md border border-[var(--app-danger-line)] px-1.5 py-0.5 text-xs text-[var(--app-danger)]">
                {t(state.installation === 'invalid'
                  ? 'pluginPage.invalid'
                  : state.installation === 'checking'
                    ? state.scanDiagnostic?.retrying
                      ? 'settings.plugins.scan.checking'
                      : 'settings.plugins.scan.waiting'
                    : 'pluginPage.incompatible')}
              </span>
            )}
          </div>
          {!state.scanDiagnostic && (issue || (enabled ? !['running', 'stopped'].includes(state.runtime) : state.runtime !== 'stopped')) && <p className={`mt-1 truncate text-xs ${issue ? 'text-[var(--app-danger)]' : 'text-ink-muted'}`} title={issue ?? undefined}>
            {issue ?? t(`settings.plugins.runtime.${state.runtime}`)}
          </p>}
          {dependencyIssue && <p role="status" className="mt-1 truncate text-xs text-ink-muted" title={dependencyIssue}>{t('settings.plugins.dependencyWarning', { issue: dependencyIssue })}</p>}
          {state.scanDiagnostic && (
            <div className="mt-2 rounded-lg border border-glass-line bg-glass px-2.5 py-2">
              <p className="text-xs font-medium text-ink-muted">
                {state.scanDiagnostic.retrying
                  ? t('settings.plugins.scan.retrying', {
                    attempt: state.scanDiagnostic.retryAttempt,
                    limit: state.scanDiagnostic.retryLimit,
                  })
                  : state.scanDiagnostic.retryable
                    ? t('settings.plugins.scan.retryAvailable')
                    : t('settings.plugins.scan.permanentFailure')}
              </p>
              <p className="mt-1 break-all font-mono text-xs text-ink-faint" title={`${state.scanDiagnostic.code} · ${state.scanDiagnostic.phase}`}>
                {state.scanDiagnostic.code} · {state.scanDiagnostic.phase}
              </p>
              <p className="mt-1 max-h-16 overflow-auto whitespace-pre-wrap break-words text-xs leading-4 text-ink-faint">
                {state.scanDiagnostic.message}
              </p>
              <div className="mt-2 flex gap-2">
                {state.scanDiagnostic.code !== 'PLUGIN_SCAN_RETRY_UNAVAILABLE' && (
                  <button
                    type="button"
                    disabled={busy || state.scanDiagnostic.retrying}
                    onClick={onRetryScan}
                    className="app-button inline-flex items-center"
                  >
                    <RotateCw className="h-3 w-3" aria-hidden />
                    {t('settings.plugins.scan.retry')}
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => void copyScanDiagnostic()}
                  className="app-button inline-flex items-center"
                >
                  <Clipboard className="h-3 w-3" aria-hidden />
                  {copiedDiagnostic
                    ? t('settings.plugins.scan.copied')
                    : t('settings.plugins.scan.copyDiagnostic')}
                </button>
              </div>
            </div>
          )}
          {(!state.manifest.ui || (state.manifest.dependencies?.length ?? 0) > 0) && (
            <p className="mt-1 truncate text-xs text-ink-faint" title={state.manifest.dependencies?.map((dependency) => `${dependency.pluginId} ${dependency.versionReq}`).join('、')}>
              {!state.manifest.ui && t('settings.plugins.serviceOnly')}
              {!state.manifest.ui && state.manifest.dependencies?.length ? ' · ' : ''}
              {state.manifest.dependencies?.length
                ? t('settings.plugins.dependencies', { dependencies: state.manifest.dependencies.map((dependency) => dependency.pluginId).join('、') })
                : ''}
            </p>
          )}
        </div>
        </div>
        <div className="mt-4 flex items-center justify-end gap-3 border-t border-glass-line pt-3">
          <button type="button" className="app-button app-button-quiet mr-auto" onClick={onDetails}><Info className="h-3.5 w-3.5" aria-hidden />详情</button>
          <ToggleSwitch
            label={enabled ? t('settings.plugins.disableAria', { name: state.manifest.name }) : t('settings.plugins.enableAria', { name: state.manifest.name })}
            checked={enabled}
            disabled={busy || state.installation !== 'installed'}
            onChange={onToggle}
          />
          <button type="button" disabled={busy} onClick={onUninstall} className="app-button app-button-danger inline-flex items-center">
            <Trash2 className="h-3 w-3" aria-hidden />
            {t('settings.plugins.uninstall')}
          </button>
        </div>
    </li>
  )
}

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message?: unknown }).message ?? cause)
  }
  return String(cause)
}
