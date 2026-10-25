import { useEffect, useMemo, useRef, useState } from 'react'
import { Search } from 'lucide-react'
import { Dialog } from '@wonderland/ui'
import { PluginIcon } from '../plugins/icons'
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

  const filtered = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase()
    if (!needle) return contributions
    return contributions.filter((item) =>
      `${item.title} ${item.pluginId} ${item.contributionId} ${item.state.manifest.name}`
        .toLocaleLowerCase()
        .includes(needle),
    )
  }, [contributions, query])

  return <Dialog open={open} title={t('workspace.commandPalette')} size="lg" placement="top" closeLabel={t('common.close')} initialFocus={input} onClose={onClose} bodyClassName="command-palette-body" headerContent={<label className="app-search-field flex min-h-9 items-center gap-3"><Search className="h-4 w-4 shrink-0 text-ink-muted" aria-hidden /><input ref={input} value={query} onChange={event => setQuery(event.target.value)} placeholder={t('workspace.searchPlaceholder')} aria-label={t('workspace.commandPalette')} className="min-w-0 flex-1 bg-transparent text-sm text-ink outline-none placeholder:text-ink-faint" /></label>}>
          {filtered.length === 0 ? (
            <p className="px-4 py-8 text-center text-sm text-ink-muted">{t('workspace.noSearchResults')}</p>
          ) : filtered.map((contribution) => {
            return (
              <button
                key={contribution.id}
                type="button"
                className="flex w-full items-center gap-3 rounded-xl px-3 py-3 text-left transition hover:bg-glass-hover focus-visible:outline-2 focus-visible:outline-brand-500"
                onClick={() => onSelect(contribution.id)}
              >
                <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand-600/15 text-brand-400">
                  <PluginIcon icon={contribution.icon} pluginId={contribution.pluginId} className="h-4 w-4" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-semibold text-ink">{contribution.title}</span>
                  {contribution.state.manifest.name !== contribution.title && <span className="block truncate text-xs text-ink-faint">{contribution.state.manifest.name}</span>}
                </span>
                {contribution.status !== 'ready' && <span className="shrink-0 text-xs text-ink-muted">{t(`workspace.contributionStatus.${contribution.status}`)}</span>}
              </button>
            )
          })}
  </Dialog>
}
