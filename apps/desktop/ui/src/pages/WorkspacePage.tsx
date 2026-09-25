import { useState } from 'react'
import { ArrowRight, ArrowUp, ArrowDown, Pin, Search, Puzzle, RotateCcw, Eye, EyeOff } from 'lucide-react'
import { useNavigate } from 'react-router-dom'

import { pluginIcon } from '../plugins/icons'
import { buildContributionRegistry, openContribution } from '../plugins/contributions'
import { orderContributions, useWorkspaceLayout } from '../plugins/workspaceLayout'
import { usePlugins } from '../plugins/api'
import { PluginManagement } from './PluginManagement'
import { t } from '../i18n'

/** Workspace launcher exposes one primary Activity per plugin; auxiliary Views stay in Core-owned slots. */
export function WorkspacePage() {
  const navigate = useNavigate()
  const [managingPlugins, setManagingPlugins] = useState(false)
  const { states, error, setStates } = usePlugins()
  const { layout, togglePinned, toggleHidden, move, reset } = useWorkspaceLayout()
  const registry = buildContributionRegistry(states)
  const activities = orderContributions(
    registry.filter((item) => item.kind === 'activity'),
    layout.orderIds,
  )
  const pinned = activities.filter((item) => layout.pinnedIds.includes(item.id))
  const continueActivity = layout.activeContributionId
    ? registry.find((item) => item.id === layout.activeContributionId && item.kind === 'activity' && item.status === 'ready')
    : undefined

  const open = (id: string) => openContribution(registry, id, navigate)

  return (
    <section className="mx-auto w-full max-w-7xl space-y-10">
      <header className="mb-9 flex flex-wrap items-end justify-between gap-4">
        <div>
          <p className="mb-2 text-xs font-semibold uppercase tracking-[0.2em] text-brand-400">Wonderland</p>
          <h1 className="text-3xl font-bold tracking-tight text-ink">{t('workspace.title')}</h1>
          <p className="mt-2 max-w-2xl text-sm leading-6 text-ink-muted">
            {states ? t('workspace.launcherHint') : t('workspace.loading')}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={() => window.dispatchEvent(new Event('wonderland:open-command-palette'))}
            className="glass-card inline-flex items-center gap-2 rounded-lg px-3 py-2 text-sm text-ink transition hover:bg-glass-hover"
          >
            <Search className="h-4 w-4" aria-hidden />
            {t('workspace.search')}
            <kbd className="ml-1 rounded border border-glass-line px-1.5 py-0.5 text-[10px] text-ink-faint">Ctrl⇧P</kbd>
          </button>
          <button
            type="button"
            aria-pressed={managingPlugins}
            onClick={() => setManagingPlugins((current) => !current)}
            className={`glass-card inline-flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition hover:bg-glass-hover ${managingPlugins ? 'border-brand-500/30 text-brand-400' : 'border-glass-line text-ink'}`}
          >
            <Puzzle className="h-4 w-4" aria-hidden />
            {managingPlugins ? t('workspace.backToActivities') : t('workspace.pluginManagement')}
          </button>
          <button
            type="button"
            onClick={reset}
            className="glass-card inline-flex items-center gap-2 rounded-lg border border-glass-line px-3 py-2 text-sm text-ink-muted transition hover:bg-glass-hover hover:text-ink"
          >
            <RotateCcw className="h-3.5 w-3.5" aria-hidden />
            {t('workspace.resetLayout')}
          </button>
        </div>
      </header>

      {error && !managingPlugins && <p role="alert" className="mb-5 rounded-lg border border-[var(--app-danger)]/30 bg-[var(--app-danger)]/10 px-4 py-3 text-sm text-[var(--app-danger)]">{t(error)}</p>}

      {managingPlugins ? (
        <PluginManagement states={states} error={error} setStates={setStates} />
      ) : (
        <>
      {pinned.length > 0 && (
        <section className="mb-10">
          <h2 className="mb-3 text-xs font-bold uppercase tracking-[0.16em] text-ink-faint">{t('workspace.pinned')}</h2>
          <div className="flex flex-wrap gap-2">
            {pinned.map((item, index) => {
              const Icon = pluginIcon(item.icon)
              return (
                <div key={item.id} className="glass-card flex items-center gap-1 rounded-xl p-1.5">
                  <button type="button" onClick={() => open(item.id)} className="inline-flex max-w-64 items-center gap-2 rounded-lg px-2 py-1.5 text-sm text-ink hover:bg-glass-hover" title={item.id}>
                    <Icon className="h-4 w-4 shrink-0 text-brand-400" aria-hidden />
                    <span className="truncate">{item.title}</span>
                  </button>
                  <button type="button" disabled={index === 0} onClick={() => move(item.id, -1)} className="rounded-md p-1 text-ink-faint hover:bg-glass-hover hover:text-ink disabled:opacity-30" aria-label={t('workspace.moveUp')} title={t('workspace.moveUp')}>
                    <ArrowUp className="h-3.5 w-3.5" aria-hidden />
                  </button>
                  <button type="button" disabled={index === pinned.length - 1} onClick={() => move(item.id, 1)} className="rounded-md p-1 text-ink-faint hover:bg-glass-hover hover:text-ink disabled:opacity-30" aria-label={t('workspace.moveDown')} title={t('workspace.moveDown')}>
                    <ArrowDown className="h-3.5 w-3.5" aria-hidden />
                  </button>
                </div>
              )
            })}
          </div>
        </section>
      )}

      {continueActivity && (
        <section className="mb-9 flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-brand-500/20 bg-brand-600/5 px-5 py-4">
          <div className="min-w-0">
            <p className="text-[10px] font-bold uppercase tracking-[0.16em] text-brand-400">{t('workspace.continue')}</p>
            <p className="mt-1 truncate text-sm font-semibold text-ink">{continueActivity.title}</p>
          </div>
          <button type="button" onClick={() => open(continueActivity.id)} className="rounded-lg bg-brand-600 px-4 py-2 text-xs font-semibold text-white hover:bg-brand-500">
            {t('workspace.resumeActivity')}
          </button>
        </section>
      )}

      <section>
        <div className="mb-4 flex items-center justify-between gap-3">
          <h2 className="text-xs font-bold uppercase tracking-[0.16em] text-ink-faint">{t('workspace.activities')}</h2>
          <span className="text-xs text-ink-faint">{activities.length}</span>
        </div>
        {!states ? (
          <p role="status" className="glass-card rounded-xl px-5 py-8 text-center text-sm text-ink-muted">{t('workspace.loading')}</p>
        ) : activities.length === 0 ? (
          <div className="glass-card rounded-2xl p-8 text-center">
            <p className="text-sm text-ink-muted">{t('workspace.emptyEnabled')}</p>
          </div>
        ) : (
          <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
            {activities.map((item) => {
              const Icon = pluginIcon(item.icon)
              const pinnedItem = layout.pinnedIds.includes(item.id)
              const hiddenItem = layout.hiddenIds.includes(item.id)
              return (
                <article key={item.id} className="glass-card flex min-h-52 flex-col rounded-2xl p-5 transition hover:border-brand-500/30">
                  <div className="flex items-start justify-between gap-3">
                    <button type="button" onClick={() => open(item.id)} className="flex min-w-0 items-start gap-3 text-left">
                      <span className="grid h-11 w-11 shrink-0 place-items-center rounded-xl bg-brand-600/15 text-brand-400">
                        <Icon className="h-5 w-5" aria-hidden />
                      </span>
                      <span className="min-w-0">
                        <span className="block truncate font-semibold text-ink">{item.title}</span>
                        <span className="mt-1 block truncate text-xs text-ink-faint">{item.state.manifest.name} · {item.id}</span>
                      </span>
                    </button>
                    <span className="shrink-0 rounded-full border border-glass-line px-2 py-1 text-[10px] text-ink-muted">
                      {t(`workspace.contributionStatus.${item.status}`)}
                    </span>
                  </div>
                  <p className="mt-4 line-clamp-2 flex-1 text-sm leading-6 text-ink-muted">
                    {item.state.manifest.description ?? item.state.manifest.version}
                  </p>
                  <div className="mt-4 flex items-center justify-between gap-2 border-t border-glass-line pt-3">
                    <button type="button" onClick={() => open(item.id)} className="inline-flex items-center gap-1 text-xs font-semibold text-brand-400 hover:text-brand-300">
                      {item.status === 'ready' ? t('workspace.openActivity') : t('workspace.viewStatus')}
                      <ArrowRight className="h-3.5 w-3.5" aria-hidden />
                    </button>
                    <div className="flex items-center gap-1">
                      {hiddenItem && <span className="mr-1 text-[10px] text-ink-faint">{t('workspace.hidden')}</span>}
                      <button type="button" onClick={() => togglePinned(item.id)} className={`rounded-md p-1.5 transition hover:bg-glass-hover ${pinnedItem ? 'text-brand-400' : 'text-ink-faint'}`} aria-label={pinnedItem ? t('workspace.unpin') : t('workspace.pin')} title={pinnedItem ? t('workspace.unpin') : t('workspace.pin')}>
                        <Pin className="h-4 w-4" aria-hidden />
                      </button>
                      <button type="button" onClick={() => toggleHidden(item.id)} className="rounded-md p-1.5 text-ink-faint transition hover:bg-glass-hover hover:text-ink" aria-label={hiddenItem ? t('workspace.show') : t('workspace.hide')} title={hiddenItem ? t('workspace.show') : t('workspace.hide')}>
                        {hiddenItem ? <Eye className="h-4 w-4" aria-hidden /> : <EyeOff className="h-4 w-4" aria-hidden />}
                      </button>
                    </div>
                  </div>
                </article>
              )
            })}
          </div>
        )}
      </section>
        </>
      )}
    </section>
  )
}
