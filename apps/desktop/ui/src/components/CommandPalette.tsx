import { useEffect, useMemo, useRef, useState } from 'react'
import { Search, X } from 'lucide-react'
import { pluginIcon } from '../plugins/icons'
import type { WorkspaceContribution } from '../plugins/contributions'
import { t } from '../i18n'

interface Props {
  open: boolean
  contributions: readonly WorkspaceContribution[]
  onClose: () => void
  onSelect: (id: string) => void
}

export function CommandPalette({ open, contributions, onClose, onSelect }: Props) {
  const [query, setQuery] = useState('')
  const input = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (!open) return
    setQuery('')
    input.current?.focus()
  }, [open])

  useEffect(() => {
    if (!open) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [open, onClose])

  const filtered = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase()
    if (!needle) return contributions
    return contributions.filter((item) =>
      `${item.title} ${item.pluginId} ${item.contributionId} ${item.state.manifest.name}`
        .toLocaleLowerCase()
        .includes(needle),
    )
  }, [contributions, query])

  if (!open) return null

  return (
    <div className="fixed inset-0 z-[80] flex items-start justify-center bg-black/40 px-5 pt-[12vh] backdrop-blur-sm" onMouseDown={onClose}>
      <section
        role="dialog"
        aria-modal="true"
        aria-label={t('workspace.commandPalette')}
        className="glass-card w-full max-w-2xl overflow-hidden rounded-2xl border border-glass-line shadow-2xl"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <label className="flex items-center gap-3 border-b border-glass-line px-5 py-4">
          <Search className="h-5 w-5 shrink-0 text-ink-muted" aria-hidden />
          <input
            ref={input}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t('workspace.searchPlaceholder')}
            className="min-w-0 flex-1 bg-transparent text-base text-ink outline-none placeholder:text-ink-faint"
          />
          <button type="button" onClick={onClose} className="rounded-md p-1 text-ink-muted hover:bg-glass-hover" aria-label={t('common.cancel')}>
            <X className="h-4 w-4" aria-hidden />
          </button>
        </label>
        <div className="max-h-[55vh] overflow-y-auto p-2">
          {filtered.length === 0 ? (
            <p className="px-4 py-8 text-center text-sm text-ink-muted">{t('workspace.noSearchResults')}</p>
          ) : filtered.map((contribution) => {
            const Icon = pluginIcon(contribution.icon)
            return (
              <button
                key={contribution.id}
                type="button"
                className="flex w-full items-center gap-3 rounded-xl px-3 py-3 text-left transition hover:bg-glass-hover focus-visible:outline-2 focus-visible:outline-brand-500"
                onClick={() => onSelect(contribution.id)}
              >
                <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand-600/15 text-brand-400">
                  <Icon className="h-4 w-4" aria-hidden />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-semibold text-ink">{contribution.title}</span>
                  <span className="block truncate text-xs text-ink-faint">{contribution.state.manifest.name} · {contribution.id}</span>
                </span>
                <span className="shrink-0 text-xs text-ink-muted">{t(`workspace.contributionStatus.${contribution.status}`)}</span>
              </button>
            )
          })}
        </div>
        <footer className="border-t border-glass-line px-5 py-2 text-[11px] text-ink-faint">
          Ctrl+Shift+P · Esc
        </footer>
      </section>
    </div>
  )
}
