import { useEffect, useRef, useState } from 'react'
import { Download, FolderOpen, X } from 'lucide-react'
import { listen, webDownload, webRequest } from '../core/transport'
import type { RemoteInteraction as Interaction } from '../core/remoteNotifications'
import { RemoteFileReferences } from './remoteFileReferences'

/** Core-owned controls live outside plugin sandboxes; no key or file handle enters an iframe. */
export function RemoteInteractions({ names }: { names: Map<string, string> }) {
  const [actions, setActions] = useState<Interaction[]>([])
  const [error, setError] = useState('')
  const [expanded, setExpanded] = useState(true)
  const [busy, setBusy] = useState<string | null>(null)
  const [downloads, setDownloads] = useState<Record<string, string>>({})
  const files = useRef(new RemoteFileReferences())
  const refresh = useRef<(() => void) | null>(null)
  const mounted = useRef(false)
  const reply = (id: string, result: unknown) => webRequest(`interactions/${encodeURIComponent(id)}/reply`, result)

  useEffect(() => {
    let live = true
    mounted.current = true
    let pending = false
    let again = false
    let needsRefresh = false
    let stopListening: (() => void) | undefined
    const poll = async () => {
      if (!live) return
      if (pending) { again = true; return }
      pending = true
      const fileRevision = files.current.snapshotRevision()
      try {
        const [next, selections] = await Promise.all([
          webRequest<Interaction[]>('interactions'),
          webRequest<{ handles: string[] }>('selected-files'),
        ])
        if (!live) return
        files.current.retain(new Set(selections.handles), fileRevision)
        needsRefresh = false
        const activeIds = new Set(next.map((item) => item.id))
        setDownloads((old) => Object.fromEntries(Object.entries(old).filter(([id]) => activeIds.has(id))))
        setActions(next.filter((item) => item.kind !== 'read'))
        for (const action of next.filter((item) => item.kind === 'read')) {
          const { handle, offset, length } = action.payload
          const selected = files.current.get(handle ?? '')
          if (!selected || !Number.isSafeInteger(offset) || !Number.isSafeInteger(length) || offset! < 0 || length! < 1 || length! > 1024 * 1024 || offset! + length! > selected.file.size) {
            if (live) await reply(action.id, { cancelled: true })
            continue
          }
          const bytes = new Uint8Array(await selected.file.slice(offset, offset! + length!).arrayBuffer())
          if (!live) return
          let binary = ''
          for (let start = 0; start < bytes.length; start += 8192) binary += String.fromCharCode(...bytes.subarray(start, start + 8192))
          await reply(action.id, { contentBase64: btoa(binary) })
          if (offset! + length! === selected.file.size) files.current.delete(handle!)
        }
      } catch {
        needsRefresh = true
        // Connectivity status is already shown by the workspace. Retry without duplicating banners.
      } finally {
        pending = false
        if (live && again) { again = false; void poll() }
      }
    }
    const reconnect = (event: Event) => {
      if ((event as CustomEvent<string>).detail === 'connected' && needsRefresh) void poll()
    }
    refresh.current = () => { void poll() }
    window.addEventListener('wonderland:web-connection', reconnect)
    void Promise.all([listen('interactions:changed', () => { void poll() }), listen('plugins:changed', () => { void poll() })]).then((stops) => {
      if (live) stopListening = () => stops.forEach((stop) => stop())
      else stops.forEach((stop) => stop())
    })
    void poll()
    return () => {
      live = false
      mounted.current = false
      stopListening?.()
      window.removeEventListener('wonderland:web-connection', reconnect)
      files.current.clear()
      refresh.current = null
    }
  }, [])

  const perform = async (id: string, work: () => Promise<unknown>) => {
    setBusy(id); setError('')
    try { await work() } catch (cause) { if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { if (mounted.current) setBusy(null) }
  }
  const choose = async (action: Interaction, file: File) => {
    if (action.payload.maxBytes != null && file.size > action.payload.maxBytes) throw new Error('文件超过插件为本次导入设置的大小限制。')
    const extensions = action.payload.extensions ?? []
    if (extensions.length && !extensions.some((ext) => file.name.toLowerCase().endsWith(`.${ext.toLowerCase()}`))) throw new Error('请选择插件支持的文件类型。')
    const handle = Array.from(crypto.getRandomValues(new Uint8Array(32)), (v) => v.toString(16).padStart(2, '0')).join('')
    files.current.register(handle, file)
    try { await reply(action.id, { handle, name: file.name, size: file.size }); files.current.confirmed(handle, () => refresh.current?.()) }
    catch (cause) { files.current.delete(handle); throw cause }
  }
  const prepare = async (action: Interaction) => {
    const url = await webDownload(`interactions/${encodeURIComponent(action.id)}/content`)
    if (!mounted.current) return
    setDownloads((old) => ({ ...old, [action.id]: url }))
  }
  if (!actions.length) return null
  return <aside className="remote-interactions glass-card" aria-label="远程文件与链接">
    <button className="remote-interactions-heading" onClick={() => setExpanded(!expanded)} aria-expanded={expanded}><FolderOpen size={18} />当前设备 · 文件与链接（{actions.length}）</button>
    {expanded && <div className="remote-interactions-body">
      {actions.some((action) => action.kind === 'download') && <p>未领取的文件会保留，请下载或移除。</p>}
      {actions.map((action) => <section key={action.id} className="remote-interaction">
        <header><strong>{names.get(action.rootPluginId) ?? action.rootPluginId}{action.rootPluginId !== action.pluginId && <small> · {names.get(action.pluginId) ?? action.pluginId}</small>}</strong><button aria-label="取消或移除" disabled={busy === action.id} onClick={() => void perform(action.id, () => reply(action.id, { cancelled: true }))}><X size={16} /></button></header>
        {action.kind === 'pick' && <label>选择要导入的文件{action.payload.maxBytes != null && <small>{`上限 ${Math.round(action.payload.maxBytes / 1024 / 1024)} MiB`}</small>}<input type="file" aria-label="从当前设备选择文件" accept={action.payload.extensions?.map((ext) => `.${ext}`).join(',')} disabled={busy === action.id} onChange={(event) => { const file = event.target.files?.[0]; event.target.value = ''; if (file) void perform(action.id, () => choose(action, file)) }} /></label>}
        {action.kind === 'download' && <><p>{action.payload.name} <small>({Math.ceil((action.payload.size ?? 0) / 1024)} KiB)</small></p>{downloads[action.id]
          ? <a className="remote-file-action" href={downloads[action.id]} download={action.payload.name}>交给浏览器保存</a>
          : <button className="remote-file-action" disabled={busy === action.id} onClick={() => void perform(action.id, () => prepare(action))}><Download size={16} />{busy === action.id ? '正在准备…' : '准备下载'}</button>}</>}
        {action.kind === 'open_url' && <a className="remote-file-action" href={action.payload.url} target="_blank" rel="noopener noreferrer">在此设备打开链接</a>}
        {action.kind === 'notice' && <p>{action.payload.message}</p>}
      </section>)}
      {error && <p role="alert" className="mobile-error">{error}</p>}
    </div>}
  </aside>
}
