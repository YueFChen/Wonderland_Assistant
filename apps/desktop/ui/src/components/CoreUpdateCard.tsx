import { useEffect, useState } from 'react'
import { getVersion } from '@tauri-apps/api/app'
import { relaunch } from '@tauri-apps/plugin-process'
import { check, type Update } from '@tauri-apps/plugin-updater'
import { Download, RefreshCw } from 'lucide-react'

import { t } from '../i18n'

/** Manual, signature-verified updates from the latest GitHub release. */
export function CoreUpdateCard() {
  const [version, setVersion] = useState('')
  const [update, setUpdate] = useState<Update | null>(null)
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [progress, setProgress] = useState<number | null>(null)

  useEffect(() => { void getVersion().then(setVersion).catch(() => undefined) }, [])

  const checkForUpdate = async () => {
    if (busy) return
    setBusy(true)
    setMessage('')
    try {
      if (update) await update.close()
      const next = await check({ timeout: 15000 })
      setUpdate(next)
      setMessage(next ? t('settings.update.available', { version: next.version }) : t('settings.update.current'))
    } catch (cause) {
      setUpdate(null)
      setMessage(t('settings.update.checkFailed', { reason: String(cause) }))
    } finally {
      setBusy(false)
    }
  }

  const installUpdate = async () => {
    if (!update || busy) return
    setBusy(true)
    setMessage(t('settings.update.downloading'))
    let downloaded = 0
    let total = 0
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started') total = event.data.contentLength ?? 0
        if (event.event === 'Progress') downloaded += event.data.chunkLength
        if (event.event === 'Finished') setMessage(t('settings.update.installing'))
        setProgress(total > 0 ? Math.min(100, Math.round(downloaded * 100 / total)) : null)
      })
      await relaunch()
    } catch (cause) {
      setMessage(t('settings.update.installFailed', { reason: String(cause) }))
      setBusy(false)
    }
  }

  return (
    <section className="glass-card flex h-full flex-col justify-between rounded-card p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold text-ink">{t('settings.update.title')}</h2>
          <p className="mt-1 text-xs leading-relaxed text-ink-muted">{t('settings.update.description')}</p>
          {version && <p className="mt-1.5 text-[11px] text-ink-faint">{t('settings.update.version', { version })}</p>}
        </div>
        <div className="flex flex-wrap gap-2">
          <button type="button" disabled={busy} onClick={() => void checkForUpdate()} className="inline-flex shrink-0 items-center gap-2 rounded-lg border border-glass-line px-3 py-2 text-xs text-ink hover:bg-glass-hover disabled:opacity-50">
            <RefreshCw className="h-3.5 w-3.5" aria-hidden />{t('settings.update.check')}
          </button>
          {update && <button type="button" disabled={busy} onClick={() => void installUpdate()} className="inline-flex shrink-0 items-center gap-2 rounded-lg bg-brand-600 px-3 py-2 text-xs text-white hover:bg-brand-500 disabled:opacity-50">
            <Download className="h-3.5 w-3.5" aria-hidden />{t('settings.update.install')}
          </button>}
        </div>
      </div>
      {message && <p role="status" className="mt-3 break-words text-xs text-ink-muted">{message}{progress !== null && busy ? ` ${progress}%` : ''}</p>}
      {update?.body && <p className="mt-2 line-clamp-3 whitespace-pre-wrap text-xs text-ink-faint">{update.body}</p>}
    </section>
  )
}
