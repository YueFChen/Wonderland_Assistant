import { Download, RefreshCw } from 'lucide-react'

import { t } from '../i18n'
import { useCoreUpdate } from './CoreUpdateProvider'

/** Manual, signature-verified updates from the latest GitHub release. */
export function CoreUpdateCard() {
  const { version, update, checking, installing, progress, message, checkForUpdate, installUpdate } = useCoreUpdate()
  const busy = checking || installing

  return (
    <section className="glass-card flex h-full flex-col justify-between rounded-card p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold text-ink">{t('settings.update.title')}</h2>
          <p className="mt-1 text-xs leading-relaxed text-ink-muted">{t('settings.update.description')}</p>
          {version && <p className="mt-1.5 text-[11px] text-ink-faint">{t('settings.update.version', { version })}</p>}
        </div>
        <div className="flex flex-wrap gap-2">
          <button type="button" disabled={busy} onClick={() => void checkForUpdate(false)} className="inline-flex shrink-0 items-center gap-2 rounded-lg border border-glass-line px-3 py-2 text-xs text-ink hover:bg-glass-hover disabled:opacity-50">
            <RefreshCw className="h-3.5 w-3.5" aria-hidden />{t('settings.update.check')}
          </button>
          {update && <button type="button" disabled={busy} onClick={() => void installUpdate()} className="inline-flex shrink-0 items-center gap-2 rounded-lg bg-brand-600 px-3 py-2 text-xs text-white hover:bg-brand-500 disabled:opacity-50">
            <Download className="h-3.5 w-3.5" aria-hidden />{t('settings.update.install')}
          </button>}
        </div>
      </div>
      {message && <p role="status" className="mt-3 break-words text-xs text-ink-muted">{message}{progress !== null && installing ? ` ${progress}%` : ''}</p>}
      {update?.body && <p className="mt-2 line-clamp-3 whitespace-pre-wrap text-xs text-ink-faint">{update.body}</p>}
    </section>
  )
}
