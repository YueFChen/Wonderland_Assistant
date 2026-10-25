import { Download, RefreshCw } from 'lucide-react'

import { t } from '../i18n'
import { useCoreUpdate } from './CoreUpdateProvider'

/** Automatic discovery with user-initiated, signature-verified installation. */
export function CoreUpdateCard({ compact = false }: { compact?: boolean }) {
  const { version, update, hasUpdate, checking, installing, progress, message, checkForUpdate, installUpdate } = useCoreUpdate()
  const busy = checking || installing
  const Heading = compact ? 'h3' : 'h2'
  const statusMessage = hasUpdate && update && message === t('settings.update.available', { version: update.version }) ? '' : message

  return (
    <section className={compact ? 'px-4 py-4' : 'glass-card flex h-full flex-col justify-between rounded-card p-5'}>
      <div className={compact ? 'flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between' : 'flex flex-wrap items-start justify-between gap-3'}>
        <div className="min-w-0">
          <Heading className="flex items-center gap-2 text-sm font-semibold text-ink">
            {t('settings.update.title')}
            {hasUpdate && update && <span className="app-badge border-[var(--app-accent-line)] bg-[var(--app-accent-soft)] text-[var(--app-accent-ink)]">{t('settings.update.available', { version: update.version })}</span>}
          </Heading>
          {version && <p className="mt-1.5 text-xs text-ink-faint">{t('settings.update.version', { version })}</p>}
        </div>
        <div className="flex shrink-0 flex-wrap gap-2">
          <button type="button" disabled={busy} onClick={() => void checkForUpdate(false)} className="app-button inline-flex shrink-0 items-center">
            <RefreshCw className="h-3.5 w-3.5" aria-hidden />{t('settings.update.check')}
          </button>
          {update && <button type="button" disabled={busy} onClick={() => void installUpdate()} className="app-button app-button-primary inline-flex shrink-0 items-center">
            <Download className="h-3.5 w-3.5" aria-hidden />{t('settings.update.install')}
          </button>}
        </div>
      </div>
      {statusMessage && <p role="status" className="mt-3 break-words text-xs text-ink-muted">{statusMessage}{progress !== null && installing ? ` ${progress}%` : ''}</p>}
      {update?.body && <p className="mt-2 line-clamp-3 whitespace-pre-wrap text-xs text-ink-faint">{update.body}</p>}
    </section>
  )
}
