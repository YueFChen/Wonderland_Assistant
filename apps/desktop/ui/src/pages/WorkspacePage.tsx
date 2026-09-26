import { useEffect, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react'
import { ArrowRight, ArrowLeftRight, ArrowUp, ArrowDown, GripVertical, Pin, Search, Puzzle, RotateCcw, Eye, EyeOff } from 'lucide-react'
import { useLocation, useNavigate, useSearchParams } from 'react-router-dom'

import { pluginIcon } from '../plugins/icons'
import { buildContributionRegistry, openContribution } from '../plugins/contributions'
import { orderContributions, useWorkspaceLayout } from '../plugins/workspaceLayout'
import { pluginApi, usePlugins } from '../plugins/api'
import { PluginManagement } from './PluginManagement'
import { t } from '../i18n'
import './WorkspacePage.css'

/** Workspace launcher exposes one primary Activity per plugin; auxiliary Views stay in Core-owned slots. */
export function WorkspacePage() {
  const location = useLocation()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const managingPlugins = searchParams.get('view') === 'plugins'
  const { states, error, setStates } = usePlugins()
  const { layout, togglePinned, toggleHidden, move, reorderActivities, reset } = useWorkspaceLayout()
  const [pluginAuthors, setPluginAuthors] = useState<Record<string, string>>({})
  const [recoveryNotice, setRecoveryNotice] = useState('')
  const [draggedActivityId, setDraggedActivityId] = useState<string | null>(null)
  const [dropTargetId, setDropTargetId] = useState<string | null>(null)
  const [previewOrderIds, setPreviewOrderIds] = useState<string[] | null>(null)
  const [dragPreview, setDragPreview] = useState<{
    activityId: string
    left: number
    top: number
    width: number
    height: number
  } | null>(null)
  const workspaceRef = useRef<HTMLElement | null>(null)
  const dragPreviewRef = useRef<HTMLDivElement | null>(null)
  useEffect(() => {
    if (managingPlugins) return
    let live = true
    void pluginApi.catalog()
      .then((snapshot) => {
        if (live) {
          setPluginAuthors(Object.fromEntries(snapshot.plugins.map(({ entry }) => [entry.id, entry.author])))
        }
      })
      .catch(() => {
        // Local or unlisted plugins remain visible with the unknown-author fallback.
      })
    return () => { live = false }
  }, [managingPlugins])
  useEffect(() => {
    const routeState = location.state as {
      pluginPageRecovery?: { pluginId?: unknown; contributionId?: unknown }
    } | null
    const recovery = routeState?.pluginPageRecovery
    if (typeof recovery?.pluginId !== 'string' || typeof recovery.contributionId !== 'string') return
    setRecoveryNotice(t('workspace.recovery.unavailable', {
      pluginId: recovery.pluginId,
      contributionId: recovery.contributionId,
    }))
  }, [location.state])
  const activityDrag = useRef<{
    pointerId: number
    sourceId: string
    grabOffsetX: number
    grabOffsetY: number
    lastTargetId: string | null
    hoverTargetId: string | null
    orderIds: string[]
  } | null>(null)
  const registry = buildContributionRegistry(states)
  const activities = orderContributions(
    registry.filter((item) => item.kind === 'activity'),
    layout.activityOrderIds,
  )
  const previewActivities = previewOrderIds
    ? previewOrderIds.map((id) => activities.find((item) => item.id === id)).filter((item) => item !== undefined)
    : activities
  const pinned = orderContributions(
    activities.filter((item) => layout.pinnedIds.includes(item.id)),
    layout.orderIds,
  )
  const continueActivity = layout.activeContributionId
    ? registry.find((item) => item.id === layout.activeContributionId && item.kind === 'activity' && item.status === 'ready')
    : undefined
  const dragPreviewActivity = dragPreview
    ? activities.find((item) => item.id === dragPreview.activityId)
    : undefined
  const DragPreviewIcon = dragPreviewActivity ? pluginIcon(dragPreviewActivity.icon) : null

  const open = (id: string) => openContribution(registry, id, navigate)
  const endActivityDrag = () => {
    setDraggedActivityId(null)
    setDropTargetId(null)
    setPreviewOrderIds(null)
    setDragPreview(null)
  }
  const startActivityDrag = (event: ReactPointerEvent<HTMLButtonElement>, id: string) => {
    if (event.button !== 0) return
    event.preventDefault()
    const sourceCard = event.currentTarget.closest<HTMLElement>('[data-activity-id]')
    const bounds = sourceCard?.getBoundingClientRect()
    if (!sourceCard || !bounds || !workspaceRef.current) return
    workspaceRef.current.setPointerCapture(event.pointerId)
    activityDrag.current = {
      pointerId: event.pointerId,
      sourceId: id,
      grabOffsetX: event.clientX - bounds.left,
      grabOffsetY: event.clientY - bounds.top,
      lastTargetId: null,
      hoverTargetId: null,
      orderIds: activities.map((item) => item.id),
    }
    setDragPreview({ activityId: id, left: bounds.left, top: bounds.top, width: bounds.width, height: bounds.height })
    setPreviewOrderIds(null)
    setDropTargetId(null)
    setDraggedActivityId(id)
  }
  useLayoutEffect(() => {
    if (!dragPreview || !dragPreviewRef.current) return
    dragPreviewRef.current.style.left = `${dragPreview.left}px`
    dragPreviewRef.current.style.top = `${dragPreview.top}px`
    dragPreviewRef.current.style.width = `${dragPreview.width}px`
    dragPreviewRef.current.style.height = `${dragPreview.height}px`
  }, [dragPreview])

  const moveActivityDrag = (event: ReactPointerEvent<HTMLElement>) => {
    const drag = activityDrag.current
    if (!drag || drag.pointerId !== event.pointerId) return
    event.preventDefault()
    if (dragPreviewRef.current) {
      dragPreviewRef.current.style.left = `${event.clientX - drag.grabOffsetX}px`
      dragPreviewRef.current.style.top = `${event.clientY - drag.grabOffsetY}px`
    }
    const cardsAtPoint = document.elementsFromPoint(event.clientX, event.clientY)
      .map((element) => element.closest<HTMLElement>('[data-activity-id]'))
    const target = cardsAtPoint.find((card) => card && card.dataset.activityId !== drag.sourceId)
    if (!target) {
      if (drag.hoverTargetId) {
        drag.hoverTargetId = null
      }
      return
    }
    const targetId = target?.dataset.activityId ?? null
    if (!targetId || targetId === drag.sourceId) return
    if (drag.hoverTargetId === targetId) return
    drag.hoverTargetId = targetId
    drag.lastTargetId = targetId
    setDropTargetId(targetId)

    const orderedIds = swapActivityOrder(drag.orderIds, drag.sourceId, targetId)
    if (orderedIds) {
      drag.orderIds = orderedIds
      setPreviewOrderIds(orderedIds)
    }
  }
  const finishActivityDrag = (event: ReactPointerEvent<HTMLElement>) => {
    moveActivityDrag(event)
    const drag = activityDrag.current
    if (!drag || drag.pointerId !== event.pointerId) return
    activityDrag.current = null

    const { lastTargetId, orderIds } = drag
    if (!lastTargetId || !activities.some((item) => item.id === lastTargetId)) {
      endActivityDrag()
      return
    }

    reorderActivities(orderIds)
    endActivityDrag()
  }
  const cancelActivityDrag = (event: ReactPointerEvent<HTMLElement>) => {
    if (activityDrag.current?.pointerId !== event.pointerId) return
    activityDrag.current = null
    endActivityDrag()
  }

  return (
    <section
      ref={workspaceRef}
      className="mx-auto w-full max-w-7xl space-y-10"
      onPointerMove={moveActivityDrag}
      onPointerUp={finishActivityDrag}
      onPointerCancel={cancelActivityDrag}
      onLostPointerCapture={cancelActivityDrag}
    >
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
            onClick={() => navigate(managingPlugins ? '/workspace' : '/workspace?view=plugins')}
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
      {recoveryNotice && !managingPlugins && (
        <div role="status" className="mb-5 flex flex-wrap items-center gap-3 rounded-lg border border-glass-line bg-glass px-4 py-3 text-sm text-ink-muted">
          <p className="min-w-0 flex-1">{recoveryNotice}</p>
          <button type="button" onClick={() => setRecoveryNotice('')} className="shrink-0 rounded-md px-2 py-1 text-ink hover:bg-glass-hover">{t('common.dismiss')}</button>
        </div>
      )}

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
          <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4">
            {previewActivities.map((item) => {
              const Icon = pluginIcon(item.icon)
              const pinnedItem = layout.pinnedIds.includes(item.id)
              const hiddenItem = layout.hiddenIds.includes(item.id)
              const isDragging = draggedActivityId === item.id
              return (
                <article
                  key={item.id}
                  data-activity-id={item.id}
                  data-drop-target={dropTargetId === item.id ? 'true' : undefined}
                  style={isDragging && dragPreview ? { height: dragPreview.height } : undefined}
                  className={`activity-card glass-card flex min-h-44 flex-col rounded-xl p-4 transition ${
                    isDragging ? 'activity-card-dragging' : ''
                  }`}
                >
                  {dropTargetId === item.id && <span className="activity-swap-indicator" aria-hidden="true"><ArrowLeftRight className="h-3.5 w-3.5" /></span>}
                  {!isDragging && <>
                  <div className="flex items-start justify-between gap-3">
                    <button type="button" onClick={() => open(item.id)} className="activity-enter flex min-w-0 cursor-pointer items-start gap-3 text-left">
                      <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-brand-600/15 text-brand-400">
                        <Icon className="h-5 w-5" aria-hidden />
                      </span>
                      <span className="min-w-0">
                        <span className="block truncate font-semibold text-ink">{item.title}</span>
                        <span className="mt-1 block truncate text-xs text-ink-faint">{item.state.manifest.name} · {item.id}</span>
                        <span className="mt-0.5 block truncate text-[11px] text-ink-faint">{t('workspace.author', { author: pluginAuthors[item.state.manifest.id] ?? t('workspace.authorUnknown') })}</span>
                      </span>
                    </button>
                    <div className="flex shrink-0 items-center gap-2">
                      <span className="rounded-full border border-glass-line px-2 py-1 text-[10px] text-ink-muted">
                        {t(`workspace.contributionStatus.${item.status}`)}
                      </span>
                      <button
                        type="button"
                        onPointerDown={(event) => startActivityDrag(event, item.id)}
                        className="activity-drag-handle rounded p-1 text-ink-faint transition hover:bg-glass-hover hover:text-brand-400"
                        aria-label={t('workspace.reorderActivity')}
                        title={t('workspace.reorderActivity')}
                      >
                        <GripVertical className="h-4 w-4" aria-hidden />
                      </button>
                    </div>
                  </div>
                  <p className="mt-3 line-clamp-2 flex-1 text-xs leading-5 text-ink-muted">
                    {item.state.manifest.description ?? item.state.manifest.version}
                  </p>
                  <div className="mt-3 flex items-center justify-between gap-2 border-t border-glass-line pt-2.5">
                    <button type="button" onClick={() => open(item.id)} className="activity-enter inline-flex cursor-pointer items-center gap-1 text-xs font-semibold text-brand-400 hover:text-brand-300">
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
                  </>}
                </article>
              )
            })}
          </div>
        )}
      </section>
      {dragPreview && dragPreviewActivity && DragPreviewIcon && (
        <div ref={dragPreviewRef} className="activity-drag-preview glass-card flex min-h-44 flex-col rounded-xl p-4" aria-hidden="true">
          <div className="flex items-start justify-between gap-3">
            <div className="flex min-w-0 items-start gap-3">
              <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-brand-600/15 text-brand-400">
                <DragPreviewIcon className="h-5 w-5" aria-hidden />
              </span>
              <span className="min-w-0">
                <span className="block truncate font-semibold text-ink">{dragPreviewActivity.title}</span>
                <span className="mt-1 block truncate text-xs text-ink-faint">{dragPreviewActivity.state.manifest.name} · {dragPreviewActivity.id}</span>
                <span className="mt-0.5 block truncate text-[11px] text-ink-faint">{t('workspace.author', { author: pluginAuthors[dragPreviewActivity.state.manifest.id] ?? t('workspace.authorUnknown') })}</span>
              </span>
            </div>
            <span className="shrink-0 rounded-full border border-glass-line px-2 py-1 text-[10px] text-ink-muted">
              {t(`workspace.contributionStatus.${dragPreviewActivity.status}`)}
            </span>
          </div>
          <p className="mt-3 line-clamp-2 flex-1 text-xs leading-5 text-ink-muted">
            {dragPreviewActivity.state.manifest.description ?? dragPreviewActivity.state.manifest.version}
          </p>
          <div className="mt-3 flex items-center justify-between gap-2 border-t border-glass-line pt-2.5">
            <span className="inline-flex items-center gap-1 text-xs font-semibold text-brand-400">
              {t('workspace.swapPosition')}
              <ArrowLeftRight className="h-3.5 w-3.5" aria-hidden />
            </span>
            <GripVertical className="h-4 w-4 text-brand-400" aria-hidden />
          </div>
        </div>
      )}
        </>
      )}
    </section>
  )
}

function swapActivityOrder(orderIds: readonly string[], sourceId: string, targetId: string): string[] | null {
  const sourceIndex = orderIds.indexOf(sourceId)
  const targetIndex = orderIds.indexOf(targetId)
  if (sourceIndex < 0 || targetIndex < 0 || sourceIndex === targetIndex) return null

  const swapped = [...orderIds]
  ;[swapped[sourceIndex], swapped[targetIndex]] = [swapped[targetIndex], swapped[sourceIndex]]
  return swapped
}
