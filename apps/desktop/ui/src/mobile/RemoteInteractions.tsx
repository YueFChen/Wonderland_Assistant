import { useEffect, useRef, useState } from 'react'
import { Download, FolderOpen, X } from 'lucide-react'
import { webDownload, webRequest } from '../core/transport'

interface Interaction {
  id: string
  pluginId: string
  kind: 'pick' | 'read' | 'download' | 'open_url' | 'notice'
  payload: { name?: string; size?: number; extensions?: string[]; maxBytes?: number; handle?: string; offset?: number; length?: number; url?: string; message?: string }
}

/** Core-owned controls live outside plugin sandboxes; no key or file handle enters an iframe. */
export function RemoteInteractions({ names }: { names: Map<string, string> }) {
  const [actions, setActions] = useState<Interaction[]>([])
  const [error, setError] = useState('')
  const [expanded, setExpanded] = useState(true)
  const [busy, setBusy] = useState<string | null>(null)
  const [downloads, setDownloads] = useState<Record<string, string>>({})
  const files = useRef(new Map<string, { file: File; expires: number }>())
  const blobs = useRef(new Map<string, { url: string; expires: number }>())
  const mounted = useRef(false)
  const reply = (id: string, result: unknown) => webRequest(`interactions/${encodeURIComponent(id)}/reply`, result)

  useEffect(() => {
    let live = true
    mounted.current = true
    let timer: ReturnType<typeof setTimeout>
    const poll = async () => {
      try {
        const next = await webRequest<Interaction[]>('interactions')
        if (!live) return
        for (const [id, value] of files.current) if (value.expires < Date.now()) files.current.delete(id)
        const activeIds = new Set(next.map((item) => item.id))
        for (const [id, value] of blobs.current) if (value.expires < Date.now() || !activeIds.has(id)) { URL.revokeObjectURL(value.url); blobs.current.delete(id); setDownloads((old) => { const copy = { ...old }; delete copy[id]; return copy }) }
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
        // Connectivity status is already shown by the workspace. Retry without duplicating banners.
      } finally { if (live) timer = setTimeout(() => void poll(), 350) }
    }
    void poll()
    return () => {
      live = false
      mounted.current = false
      clearTimeout(timer)
      files.current.clear()
      for (const value of blobs.current.values()) URL.revokeObjectURL(value.url)
      blobs.current.clear()
    }
  }, [])

  const perform = async (id: string, work: () => Promise<unknown>) => {
    setBusy(id); setError('')
    try { await work() } catch (cause) { if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { if (mounted.current) setBusy(null) }
  }
  const choose = async (action: Interaction, file: File) => {
    const heldBytes = [...files.current.values()].reduce((sum, item) => sum + item.file.size, 0)
    if (file.size > (action.payload.maxBytes ?? 0) || heldBytes + file.size > 128 * 1024 * 1024 || files.current.size >= 8) throw new Error('文件超过本次导入或当前设备的大小限制。')
    const extensions = action.payload.extensions ?? []
    if (extensions.length && !extensions.some((ext) => file.name.toLowerCase().endsWith(`.${ext.toLowerCase()}`))) throw new Error('请选择插件支持的文件类型。')
    const handle = Array.from(crypto.getRandomValues(new Uint8Array(32)), (v) => v.toString(16).padStart(2, '0')).join('')
    files.current.set(handle, { file, expires: Date.now() + 300_000 })
    try { await reply(action.id, { handle, name: file.name, size: file.size }) }
    catch (cause) { files.current.delete(handle); throw cause }
  }
  const prepare = async (action: Interaction) => {
    const blob = await webDownload(`interactions/${encodeURIComponent(action.id)}/content`)
    if (!mounted.current) return
    const url = URL.createObjectURL(blob)
    const previous = blobs.current.get(action.id)
    if (previous) URL.revokeObjectURL(previous.url)
    blobs.current.set(action.id, { url, expires: Date.now() + 300_000 })
    setDownloads((old) => ({ ...old, [action.id]: url }))
  }
  if (!actions.length) return null
  return <aside className="remote-interactions glass-card" aria-label="远程文件与链接">
    <button className="remote-interactions-heading" onClick={() => setExpanded(!expanded)} aria-expanded={expanded}><FolderOpen size={18} />当前设备 · 文件与链接（{actions.length}）</button>
    {expanded && <div className="remote-interactions-body">
      <p>导入从此设备选择，导出保存到此设备。下载保留 5 分钟。</p>
      {actions.map((action) => <section key={action.id} className="remote-interaction">
        <header><strong>{names.get(action.pluginId) ?? action.pluginId}</strong><button aria-label="取消或移除" disabled={busy === action.id} onClick={() => void perform(action.id, () => reply(action.id, { cancelled: true }))}><X size={16} /></button></header>
        {action.kind === 'pick' && <label>选择要导入的文件（45 秒内）<small>上限 {Math.round((action.payload.maxBytes ?? 0) / 1024 / 1024)} MiB</small><input type="file" aria-label="从当前设备选择文件" accept={action.payload.extensions?.map((ext) => `.${ext}`).join(',')} disabled={busy === action.id} onChange={(event) => { const file = event.target.files?.[0]; event.target.value = ''; if (file) void perform(action.id, () => choose(action, file)) }} /></label>}
        {action.kind === 'download' && <><p>{action.payload.name} <small>({Math.ceil((action.payload.size ?? 0) / 1024)} KiB)</small></p>{downloads[action.id]
          ? <a className="remote-file-action" href={downloads[action.id]} download={action.payload.name}>保存到本设备</a>
          : <button className="remote-file-action" disabled={busy === action.id} onClick={() => void perform(action.id, () => prepare(action))}><Download size={16} />{busy === action.id ? '正在准备…' : '准备下载'}</button>}</>}
        {action.kind === 'open_url' && <a className="remote-file-action" href={action.payload.url} target="_blank" rel="noopener noreferrer">在此设备打开链接</a>}
        {action.kind === 'notice' && <p>{action.payload.message}</p>}
      </section>)}
      {error && <p role="alert" className="mobile-error">{error}</p>}
    </div>}
  </aside>
}
