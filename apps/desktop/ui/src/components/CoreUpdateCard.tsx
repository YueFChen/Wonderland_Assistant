import { FileText, RefreshCw } from 'lucide-react'

import { t } from '../i18n'
import { useCoreUpdate } from './CoreUpdateProvider'
import { releaseDateLabel, releaseNotesPreview } from '../core/releaseNotes'

/** Automatic discovery with user-initiated, signature-verified installation. */
export function CoreUpdateCard({ compact = false }: { compact?: boolean }) {
  const { version, update, hasUpdate, checking, installing, progress, message, checkForUpdate, showUpdateDetails } = useCoreUpdate()
  const busy = checking || installing
  const Heading = compact ? 'h3' : 'h2'
  const statusMessage = hasUpdate && update && message === t('settings.update.available', { version: update.version }) ? '' : message
  const preview = releaseNotesPreview(update?.body)
  const published = update ? releaseDateLabel(update.date) : null

  return (
    <section className={compact ? 'px-4 py-4' : 'glass-card flex h-full flex-col justify-between rounded-card p-5'}>
      <div className={compact ? 'flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between' : 'flex flex-wrap items-start justify-between gap-3'}>
        <div className="min-w-0">
          <Heading className="flex items-center gap-2 text-sm font-semibold text-ink">
            {t('settings.update.title')}
            {hasUpdate && update && <span className="app-badge border-[var(--app-accent-line)] bg-[var(--app-accent-soft)] text-[var(--app-accent-ink)]">{t('settings.update.available', { version: update.version })}</span>}
          </Heading>
          {version && <p className="mt-1.5 text-xs text-ink-faint">{t('settings.update.version', { version })}</p>}
          {published && <p className="mt-1 text-xs text-ink-faint"><time dateTime={update?.date ?? undefined}>{t('settings.update.published', { date: published })}</time></p>}
        </div>
        <div className="flex shrink-0 flex-wrap gap-2">
          <button type="button" disabled={busy} onClick={() => void checkForUpdate(true)} className="app-button inline-flex shrink-0 items-center">
            <RefreshCw className="h-3.5 w-3.5" aria-hidden />{t('settings.update.check')}
          </button>
          {update && <button type="button" onClick={showUpdateDetails} className="app-button app-button-primary inline-flex shrink-0 items-center">
            <FileText className="h-3.5 w-3.5" aria-hidden />{t('settings.update.details')}
          </button>}
        </div>
      </div>
      {statusMessage && <p role="status" className="mt-3 break-words text-xs text-ink-muted">{statusMessage}{progress !== null && installing ? ` ${progress}%` : ''}</p>}
      {update && <p className="mt-3 line-clamp-3 text-xs leading-5 text-ink-muted">{preview || t('settings.update.notesEmpty')}</p>}
    </section>
  )
}
