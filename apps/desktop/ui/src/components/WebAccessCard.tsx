import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Copy, Globe2, ShieldCheck } from 'lucide-react'
import { usePlugins } from '../plugins/api'
import { t } from '../i18n'
import { WebConnectionPanel } from './WebConnectionPanel'
import { isRemotelyShareable } from '../plugins/remoteAccess'

interface WebOptions { mode: 'loopback' | 'lan'; port: number; publicUrl: string; pluginIds: string[]; accessKey: string }
interface WebStatus { running: boolean; options: WebOptions | null; localUrl: string | null; accessToken: string | null; lanAddresses: { interfaceName: string; ip: string; url: string }[]; addressError: string | null }
const fieldClass = 'mt-2 w-full rounded-lg border border-glass-line bg-[var(--app-field)] px-3 py-2 text-sm text-ink'

export function WebAccessCard() {
  const { states } = usePlugins()
  const [status, setStatus] = useState<WebStatus | null>(null)
  const [options, setOptions] = useState<WebOptions>({ mode: 'loopback', port: 17890, publicUrl: '', pluginIds: [], accessKey: '' })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [copied, setCopied] = useState(false)
  const revision = useRef(0)
  const changing = useRef(false)
  useEffect(() => {
    let live = true
    const read = () => {
      if (changing.current) return
      const current = ++revision.current
      void invoke<WebStatus>('web_access_status').then((next) => {
        if (!live || current !== revision.current) return
        setStatus(next)
        if (next.options) setOptions(next.options)
      }).catch((cause) => { if (live && current === revision.current) setError(String(cause)) })
    }
    read()
    const timer = setInterval(read, 5000)
    return () => { live = false; clearInterval(timer) }
  }, [])
  const change = async () => {
    changing.current = true
    revision.current++
    setBusy(true)
    setError('')
    setCopied(false)
    try {
      const next = status?.options
        ? await invoke<WebStatus>('web_access_stop')
        : await invoke<WebStatus>('web_access_start', { options: { ...options, pluginIds: options.pluginIds.filter((id) => states?.some((item) => item.manifest.id === id && isRemotelyShareable(item))) } })
      setStatus(next)
    } catch (cause) { setError(String(cause)) }
    finally { changing.current = false; setBusy(false) }
  }
  const copy = async () => {
    if (!status?.accessToken) return
    try { await navigator.clipboard.writeText(status.accessToken); setCopied(true) }
    catch { setError(t('webAccess.copyFailed')) }
  }
  const running = Boolean(status?.options)
  return <section className="glass-card mt-4 rounded-card p-5">
    <div className="flex flex-wrap items-center justify-between gap-4">
      <div className="min-w-0"><h3 className="flex items-center gap-2 text-sm font-semibold text-ink"><Globe2 size={18} />{t('webAccess.title')}</h3><p className="mt-2 text-xs leading-relaxed text-ink-muted">{t('webAccess.description')}</p></div>
      <button type="button" onClick={() => void change()} disabled={busy || !status} className="rounded-lg bg-brand-600 px-4 py-2 text-sm font-semibold text-white disabled:opacity-50">{t(busy ? 'webAccess.pending' : running ? 'webAccess.stop' : 'webAccess.start')}</button>
    </div>
    <fieldset disabled={running || busy} className="mt-5 grid gap-4 sm:grid-cols-2 disabled:opacity-70">
      <label className="text-xs text-ink-muted">{t('webAccess.mode')}<select className={fieldClass} value={options.mode} onChange={(e) => setOptions({ ...options, mode: e.target.value as WebOptions['mode'] })}><option value="loopback">{t('webAccess.loopback')}</option><option value="lan">{t('webAccess.lan')}</option></select></label>
      <label className="text-xs text-ink-muted">{t('webAccess.port')}<input className={fieldClass} type="number" min="1" max="65535" value={options.port} onChange={(e) => setOptions({ ...options, port: Number(e.target.value) })} /></label>
      <label className="text-xs text-ink-muted sm:col-span-2">{t('webAccess.publicUrl')}<input className={fieldClass} maxLength={1024} placeholder="https://core.example.com" value={options.publicUrl} onChange={(e) => setOptions({ ...options, publicUrl: e.target.value.trim() })} /></label>
      <div className="sm:col-span-2">
        <label className="text-xs text-ink-muted">{t('webAccess.optionalKey')}<input className={fieldClass} type="password" autoComplete="off" maxLength={1024} placeholder={t('webAccess.keyPlaceholder')} value={options.accessKey} onChange={(e) => setOptions({ ...options, accessKey: e.target.value })} /></label>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <p className="text-xs text-ink-faint">{t('webAccess.keyHint')}</p>
          <button type="button" className="text-xs text-[var(--app-accent)] underline" onClick={() => setOptions({ ...options, accessKey: Array.from(crypto.getRandomValues(new Uint8Array(32)), (value) => value.toString(16).padStart(2, '0')).join('') })}>{t('webAccess.generateKey')}</button>
        </div>
      </div>
      <div className="sm:col-span-2"><p className="mb-2 text-xs font-semibold text-ink">{t('webAccess.plugins')}</p><p className="mb-3 text-xs leading-relaxed text-ink-muted">{t('webAccess.pluginHint')}</p>
        <div className="flex flex-wrap gap-3">{states?.filter((item) => isRemotelyShareable(item)).map((item) => <label key={item.manifest.id} className="flex items-center gap-2 rounded-lg border border-glass-line px-3 py-2 text-sm text-ink"><input type="checkbox" checked={options.pluginIds.includes(item.manifest.id)} onChange={(e) => setOptions({ ...options, pluginIds: e.target.checked ? [...options.pluginIds, item.manifest.id] : options.pluginIds.filter((id) => id !== item.manifest.id) })} />{item.manifest.name}</label>)}</div>
        {states && !states.some((item) => isRemotelyShareable(item)) && <p className="text-xs text-ink-faint">{t('webAccess.noPlugins')}</p>}
      </div>
    </fieldset>
    {running && <div className="mt-5 space-y-3 rounded-xl border border-glass-line bg-glass-subtle p-4 text-xs">
      <p role="status" className="font-semibold text-ink">{t(status?.running ? 'webAccess.running' : 'webAccess.interrupted')}</p>
      <p className="break-all text-ink-muted">{t('webAccess.local')} <a href={status?.localUrl ?? '#'} target="_blank" rel="noreferrer" className="text-[var(--app-accent)] underline">{status?.localUrl}</a></p>
      {status?.running && <WebConnectionPanel token={status.accessToken ?? ''} publicUrl={status.options?.publicUrl ?? ''} addresses={status.lanAddresses} />}
      {status?.addressError && <p role="alert" className="text-[var(--app-danger)]">{status.addressError}</p>}
      {status?.accessToken ? <label className="block text-ink-muted">{t('webAccess.key')}<div className="mt-2 flex gap-2"><input type="password" readOnly value={status.accessToken} className="min-w-0 flex-1 rounded-lg border border-glass-line bg-[var(--app-field)] px-3 py-2 text-ink" /><button type="button" className="flex items-center gap-2 rounded-lg border border-glass-line px-3 py-2 text-ink" onClick={() => void copy()}><Copy size={14} />{t(copied ? 'webAccess.copied' : 'webAccess.copy')}</button></div></label> : <p className="text-ink-muted">{t('webAccess.keyNotRequired')}</p>}
    </div>}
    <p className="mt-4 flex items-start gap-2 text-xs leading-relaxed text-ink-muted"><ShieldCheck size={16} className="mt-0.5 shrink-0" />{t('webAccess.security')}</p>
    <p className="mt-2 text-xs leading-relaxed text-ink-faint">{t('webAccess.proxyHint')}</p>
    {error && <p role="alert" className="mt-3 break-words text-sm text-[var(--app-danger)]">{error}</p>}
  </section>
}
