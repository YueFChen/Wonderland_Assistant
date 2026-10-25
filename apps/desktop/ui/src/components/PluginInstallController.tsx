import { useEffect, useRef, useState } from 'react'
import { Dialog, DialogButton } from '@wonderland/ui'
import { t } from '../i18n'
import './pluginInstall.css'
import { isWebClient, listen } from '../core/transport'
import { pluginApi, usePlugins } from '../plugins/api'
import { pluginManagement } from '../plugins/managementState'
import { usePluginManagement } from '../plugins/management'
import { installSignatureDescription, chooseInstallCandidate, filesFromDrop, initialInstallSelection, INSTALL_REQUEST, localInstall, type DropEntry, type InstallMode, type InstallPreview, type InstallResult } from '../plugins/localInstall'

const text = (error: unknown) => error && typeof error === 'object' && 'message' in error ? String(error.message) : String(error)
const resultsText: Record<string, string> = { pending: '等待安装', running: '正在安装', installed: '安装成功', failed: '安装失败', skipped: '未选择', cancelled: '已取消后续安装' }

/** Core chrome owns package drops; native drag interception is disabled so business uploads work. */
export function PluginInstallController() {
  const management = usePluginManagement()
  const { setStates } = usePlugins()
  const [preview, setPreview] = useState<InstallPreview | null>(null)
  const [selected, setSelected] = useState(new Set<string>())
  const [results, setResults] = useState<InstallResult[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [phase, setPhase] = useState('')
  const [error, setError] = useState('')
  const occupied = useRef(false)
  const batch = useRef<string | undefined>(undefined)
  const uploads = useRef<string[]>([])
  const cancelled = useRef(false)

  async function show(mode?: InstallMode, files?: { file: File; name: string }[]) {
    if (occupied.current || pluginManagement.getSnapshot()) return
    occupied.current = true; cancelled.current = false
    setSelected(new Set()); setBusy(true); setPhase(files ? '正在导入拖入的安装包…' : '正在选择并检查安装包…'); setError(''); setResults(null)
    try {
      if (files) {
        for (const { file, name } of files) {
          if (cancelled.current) break
          setPhase(`正在导入 ${name}`)
          const id = await localInstall.uploadOpen(name, file.size)
          uploads.current.push(id)
          for (let offset = 0; offset < file.size && !cancelled.current; offset += 256 * 1024) {
            const bytes = new Uint8Array(await file.slice(offset, offset + 256 * 1024).arrayBuffer())
            let binary = ''
            for (let index = 0; index < bytes.length; index += 8192) binary += String.fromCharCode(...bytes.subarray(index, index + 8192))
            await localInstall.uploadWrite(id, offset, btoa(binary))
          }
        }
      }
      if (cancelled.current) { await localInstall.discard(undefined, uploads.current); uploads.current = []; occupied.current = false; return }
      const next = await localInstall.prepare(files ? 'uploads' : mode!, files ? uploads.current : undefined)
      uploads.current = []
      if (cancelled.current) { await localInstall.discard(next?.batchId); occupied.current=false; return }
      batch.current = next?.batchId
      setPreview(next)
      if (next) setSelected(initialInstallSelection(next.items))
      else occupied.current = false
    } catch (cause) {
      setError(text(cause)); await localInstall.discard(undefined, uploads.current).catch(() => {}); uploads.current = []
      occupied.current = false
    } finally { setBusy(false); setPhase('') }
  }
  useEffect(() => {
    if (isWebClient) return
    void localInstall.reset().catch(() => {})
    const request = (event: Event) => { void show((event as CustomEvent<InstallMode>).detail) }
    const drop = (event: DragEvent) => {
      if (event.defaultPrevented || !event.dataTransfer) return
      const target = event.target instanceof Element ? event.target : null
      if (target?.closest('input,textarea,[contenteditable=true],[data-file-drop-target]')) return
      const entries = [...event.dataTransfer.items].map(item => (item as unknown as { webkitGetAsEntry?: () => DropEntry | null }).webkitGetAsEntry?.() ?? null)
      const files = [...event.dataTransfer.files]
      event.preventDefault()
      if (!entries.some(entry => entry?.isDirectory || entry?.isFile && /\.wplug$/i.test(entry.name)) && !files.some(file => /\.wplug$/i.test(file.name))) return
      event.preventDefault()
      if (occupied.current) return
      void filesFromDrop(entries, files).then(files => { if (files.length) void show(undefined, files) }).catch(cause => setError(text(cause)))
    }
    // Do not stop propagation: component handlers retain their ordinary file behavior.
    const over = (event: DragEvent) => { if (event.dataTransfer?.types.includes('Files')) event.preventDefault() }
    window.addEventListener(INSTALL_REQUEST, request)
    window.addEventListener('dragover', over)
    window.addEventListener('drop', drop)
    let disposed = false
    let stop: (() => void) | undefined
    void listen<{ batchId: string; items: InstallResult[] }>('plugins:install-progress', ({ payload }) => { if (payload.batchId === batch.current) setResults(payload.items) }).then(unlisten => { if (disposed) unlisten(); else stop = unlisten }).catch(() => {})
    let stopCatalog: (() => void) | undefined
    void listen<{ operationId: string; phase: string; downloadedBytes: number; totalBytes: number; cancellable: boolean }>('plugins:catalog-install-progress', ({ payload }) => {
      pluginManagement.progress(payload)
      const current = pluginManagement.getSnapshot()
      // Cancellation may arrive before IPC has registered the download job.
      if (current?.id === payload.operationId && current.cancelRequested && payload.cancellable) void pluginApi.cancelCatalogInstall(payload.operationId).catch(() => {})
    }).then(unlisten => { if (disposed) unlisten(); else stopCatalog = unlisten }).catch(() => {})
    return () => { disposed = true; stop?.(); stopCatalog?.(); window.removeEventListener(INSTALL_REQUEST, request); window.removeEventListener('dragover', over); window.removeEventListener('drop', drop) }
  }, [])

  async function close() {
    if (busy) { cancelled.current = true; await localInstall.discard(batch.current).catch(cause => setError(text(cause))); return }
    await localInstall.discard(batch.current).catch(() => {})
    setPreview(null); setResults(null); setError(''); batch.current = undefined; occupied.current = false
  }
  async function install() {
    if (!preview || !selected.size) return
    const operation = pluginManagement.begin({ id: preview.batchId, label: '安装本地插件', kind: 'local' })
    if (!operation) return
    setBusy(true); setError(''); setPhase('正在逐包安装…')
    try {
      const result = await localInstall.run(preview.batchId, [...selected])
      setResults(result.items); setStates(result.states)
    } catch (cause) { setError(text(cause)+' 请重新选择安装包。'); setPreview(null); setResults(null); batch.current=undefined; occupied.current=false; await pluginApi.states().then(setStates).catch(() => {}) }
    finally { setBusy(false); setPhase(''); operation.finish() }
  }
  if (isWebClient) return null
  const finished = results && results.every(result => !['pending', 'running'].includes(result.status))
  const multiple = (preview?.items.length ?? 0) > 1
  const hasReplacement = preview?.items.some(item => selected.has(item.key) && item.existing)
  return <Dialog open={Boolean(preview || busy || error)} title={t(finished ? 'settings.plugins.batch.resultTitle' : 'settings.plugins.batch.title')} size={multiple ? 'xl' : 'md'} dismissible={!busy} onClose={() => void close()} closeLabel={t('common.close')}
    footer={<>
      {multiple && <span className="app-dialog-meta" role="status">{results ? t('settings.plugins.batch.summary', { success: results.filter(result => result.status === 'installed').length, failure: results.filter(result => result.status === 'failed').length, skipped: results.filter(result => ['skipped', 'cancelled'].includes(result.status)).length }) : t('settings.plugins.batch.selected', { count: selected.size })}</span>}
      <DialogButton data-dialog-autofocus onClick={() => void close()}>{t(busy && preview ? 'settings.plugins.batch.cancelRemaining' : results ? 'common.done' : 'common.cancel')}</DialogButton>
      {!results && preview && <DialogButton variant="primary" aria-busy={busy} disabled={busy || Boolean(management) || !selected.size} onClick={() => void install()}>{t(multiple ? 'settings.plugins.batch.install' : 'settings.plugins.batch.installSingle')}</DialogButton>}
    </>}>
    <div className="space-y-3">
      {phase && <p role="status" className="text-sm text-ink-muted">{phase}</p>}
      {error && <p role="alert" className="app-dialog-notice" data-tone="danger">{error}</p>}
      {hasReplacement && !results && <p className="app-dialog-notice">{t('settings.plugins.batch.replacement')}</p>}
      <div className="install-candidates" data-single={!multiple}>
      {preview?.items.map((item, index) => {
        const group = item.manifest ? preview.items.filter(other => !other.error && other.manifest?.id === item.manifest!.id) : []
        const unresolved = group.length > 1 && !group.some(other => selected.has(other.key))
        const result = results?.find(result => result.key === item.key)
        const id = 'install-' + preview.batchId + '-' + item.key
        const itemError = item.error ?? result?.error
        const unreadableArchive = itemError && /\bZIP\b|EOCD/.test(itemError.message)
        const originalKey = item.existing?.signature.expectedPublicKey
        const identityChanged = originalKey && item.signaturePublicKey && originalKey !== item.signaturePublicKey
        return <article key={item.key} className="install-candidate" data-selected={selected.has(item.key)} data-invalid={Boolean(item.error)}>
          {multiple && <input id={id} type="checkbox" disabled={busy || Boolean(results) || Boolean(item.error)} checked={selected.has(item.key)} onChange={() => setSelected(value => chooseInstallCandidate(preview.items, value, item.key))} />}
          <div className="min-w-0">
            <div className="install-candidate-heading">{multiple ? <label htmlFor={id}>{item.manifest?.name ?? item.source.split(/[\\/]/).at(-1)}</label> : <h3>{item.manifest?.name ?? item.source.split(/[\\/]/).at(-1)}</h3>}<span className="install-candidate-version">{item.existing?.manifest.version === item.manifest?.version ? t('settings.plugins.batch.overwriteVersion', { version: item.manifest?.version ?? '' }) : (item.existing ? item.existing.manifest.version + ' → ' : '') + (item.manifest?.version ?? '')}</span></div>
            {item.manifest && <div className="install-candidate-meta"><span>{item.manifest.author ?? t('workspace.authorUnknown')}</span><span>{item.manifest.id}</span>{!item.error && <span className="install-signature" data-tone={identityChanged ? 'danger' : undefined}>{t(identityChanged ? 'settings.plugins.batch.identityChanged' : item.signaturePublicKey ? 'settings.plugins.batch.signed' : 'settings.plugins.batch.unsigned')}</span>}</div>}
            {item.existing?.installationSource?.kind === 'catalog' && !results && <p className="install-candidate-status">{t('settings.plugins.batch.sourceChange')}</p>}
            {unresolved && !results && preview.items.findIndex(other => other.manifest?.id === item.manifest?.id) === index && <p className="install-candidate-status">{t('settings.plugins.batch.conflict')}</p>}
            {itemError && <p role="alert" className="install-candidate-status" data-tone="danger">{unreadableArchive ? t('settings.plugins.batch.unreadableArchive') : itemError.message}</p>}
            {result && <p role="status" className="install-candidate-status" data-tone={result.status === 'failed' ? 'danger' : undefined}>{resultsText[result.status] ?? result.status}{result.status === 'installed' ? result.runtime === 'running' ? ' · 已运行' : result.runtime === 'failed' ? ' · 启动失败' : ' · 未运行' : ''}</p>}
            <details className="app-dialog-details"><summary>{t('settings.plugins.batch.details')}</summary><dl className="app-dialog-facts">
              <div><dt>{t('settings.plugins.batch.file')}</dt><dd>{item.source}</dd></div>
              {itemError && <div><dt>{t('settings.plugins.batch.diagnostic')}</dt><dd>{itemError.message}</dd></div>}
              {!item.error && <div><dt>{t('settings.plugins.batch.signature')}</dt><dd>{installSignatureDescription(item)}</dd></div>}
              {item.signaturePublicKey && <div><dt>{t('settings.plugins.batch.publicKey')}</dt><dd className="font-mono">{item.signaturePublicKey}</dd></div>}
              {identityChanged && <div><dt>{t('settings.plugins.batch.originalKey')}</dt><dd className="font-mono">{originalKey}</dd></div>}
              {item.existing?.installationSource && <div><dt>{t('settings.plugins.batch.previousSource')}</dt><dd>{item.existing.installationSource.origin}</dd></div>}
            </dl></details>
          </div>
        </article>
      })}
      </div>
      {!preview?.items.length && !busy && !error && <p className="text-sm text-ink-muted">{t('settings.plugins.batch.empty')}</p>}
    </div>
  </Dialog>
}
