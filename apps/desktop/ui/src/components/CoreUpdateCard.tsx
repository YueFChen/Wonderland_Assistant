import { useEffect, useState } from 'react'
import { getVersion } from '@tauri-apps/api/app'
import { relaunch } from '@tauri-apps/plugin-process'
import { Download, RefreshCw } from 'lucide-react'

import { t } from '../i18n'
import { networkApi, type CoreUpdateAvailable } from '../network/api'

/** Manual, signature-verified updates from the latest GitHub release. */
export function CoreUpdateCard() {
  const [version, setVersion] = useState('')
  const [update, setUpdate] = useState<CoreUpdateAvailable | null>(null)
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [progress, setProgress] = useState<number | null>(null)

  useEffect(() => { void getVersion().then(setVersion).catch(() => undefined) }, [])

  useEffect(() => {
    let live = true
    let unlisten: (() => void) | undefined
    void networkApi.onUpdateInvalidated(() => {
      setUpdate(null)
      setMessage(t('settings.update.proxyChanged'))
    }).then((dispose) => {
      if (live) unlisten = dispose
      else dispose()
    })
    return () => {
      live = false
      unlisten?.()
    }
  }, [])

  const checkForUpdate = async () => {
    if (busy) return
    setBusy(true)
    setMessage('')
    try {
      const next = await networkApi.updateCheck()
      setUpdate(next)
      setMessage(next ? t('settings.update.available', { version: next.version }) : t('settings.update.current'))
    } catch (cause) {
      setUpdate(null)
      setMessage(t('settings.update.checkFailed', { reason: errorText(cause) }))
    } finally {
      setBusy(false)
    }
  }

  const installUpdate = async () => {
    if (!update || busy) return
    setBusy(true)
    setMessage(t('settings.update.downloading'))
    let unlisten: (() => void) | undefined
    try {
      unlisten = await networkApi.onUpdateProgress((event) => {
        if (event.finished) {
          setMessage(t('settings.update.installing'))
          setProgress(null)
        } else if (event.totalBytes && event.totalBytes > 0) {
          setProgress(Math.min(100, Math.round(event.downloadedBytes * 100 / event.totalBytes)))
        } else {
          setProgress(null)
        }
      })
      await networkApi.updateInstall(update.updateId)
      await relaunch()
    } catch (cause) {
      setMessage(t('settings.update.installFailed', { reason: errorText(cause) }))
      setBusy(false)
    } finally {
      unlisten?.()
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

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message: unknown }).message)
  }
  return cause instanceof Error ? cause.message : String(cause)
}
