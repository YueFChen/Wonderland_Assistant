import { useEffect, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react'
import { ArrowRight, ArrowLeftRight, ArrowUp, ArrowDown, GripVertical, Pin, Search, Puzzle, RotateCcw } from 'lucide-react'
import { useLocation, useNavigate, useSearchParams } from 'react-router-dom'

import { PluginIcon } from '../plugins/icons'
import { buildContributionRegistry, openContribution } from '../plugins/contributions'
import { primaryPluginEntries } from '../plugins/pluginEntries'
import { orderContributions, useWorkspaceLayout } from '../plugins/workspaceLayout'
import { usePlugins } from '../plugins/api'
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
  const { layout, togglePinned, move, reorderActivities, reset } = useWorkspaceLayout()
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
  useLayoutEffect(() => {
    if ((location.state as { focusWorkspaceHeading?: boolean } | null)?.focusWorkspaceHeading) {
      workspaceRef.current?.querySelector('h1')?.focus()
    }
  }, [location.key, location.state])
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
    primaryPluginEntries(registry),
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
      className="mx-auto w-full max-w-7xl space-y-8"
      onPointerMove={moveActivityDrag}
      onPointerUp={finishActivityDrag}
      onPointerCancel={cancelActivityDrag}
      onLostPointerCapture={cancelActivityDrag}
    >
      <header className="mb-6 flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 tabIndex={-1} className="app-background-copy text-3xl font-bold tracking-tight text-ink">{t('workspace.title')}</h1>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={() => window.dispatchEvent(new Event('wonderland:open-command-palette'))}
            className="app-button"
            aria-keyshortcuts="Control+Shift+P Meta+Shift+P"
            title={`${t('workspace.search')} · Ctrl+Shift+P`}
          >
            <Search className="h-4 w-4" aria-hidden />
            {t('workspace.search')}
          </button>
          <button
            type="button"
            aria-pressed={managingPlugins}
            onClick={() => navigate(managingPlugins ? '/workspace' : '/workspace?view=plugins')}
            className="app-button"
          >
            <Puzzle className="h-4 w-4" aria-hidden />
            {managingPlugins ? t('workspace.backToActivities') : t('workspace.pluginManagement')}
          </button>
          <button
            type="button"
            onClick={reset}
            className="app-button inline-flex items-center"
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
          <button type="button" onClick={() => setRecoveryNotice('')} className="app-button app-button-quiet shrink-0">{t('common.dismiss')}</button>
        </div>
      )}

      {managingPlugins ? (
        <PluginManagement states={states} error={error} setStates={setStates} />
      ) : (
        <>
      {pinned.length > 0 && (
        <section className="mb-10">
          <h2 className="app-background-copy mb-3 text-xs font-bold uppercase tracking-[0.16em] text-ink-faint">{t('workspace.pinned')}</h2>
          <div className="flex flex-wrap gap-2">
            {pinned.map((item, index) => {
              return (
                <div key={item.id} className="glass-card flex items-center gap-1 rounded-xl p-1.5">
                  <button type="button" onClick={() => open(item.id)} className="app-button app-button-quiet inline-flex max-w-64 items-center" title={item.id}>
                    <PluginIcon icon={item.icon} pluginId={item.pluginId} className="h-4 w-4 shrink-0 text-brand-400" />
                    <span className="truncate">{item.title}</span>
                  </button>
                  <button type="button" disabled={index === 0} onClick={() => move(item.id, -1)} className="app-icon-button" aria-label={t('workspace.moveUp')} title={t('workspace.moveUp')}>
                    <ArrowUp className="h-3.5 w-3.5" aria-hidden />
                  </button>
                  <button type="button" disabled={index === pinned.length - 1} onClick={() => move(item.id, 1)} className="app-icon-button" aria-label={t('workspace.moveDown')} title={t('workspace.moveDown')}>
                    <ArrowDown className="h-3.5 w-3.5" aria-hidden />
                  </button>
                </div>
              )
            })}
          </div>
        </section>
      )}

      {continueActivity && (
        <section className="glass-card mb-9 flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-brand-500/20 px-5 py-4">
          <div className="min-w-0">
            <p className="text-xs font-bold uppercase tracking-[0.16em] text-brand-400">{t('workspace.continue')}</p>
            <p className="mt-1 truncate text-sm font-semibold text-ink">{continueActivity.title}</p>
          </div>
          <button type="button" onClick={() => open(continueActivity.id)} className="app-button app-button-primary">
            {t('workspace.resumeActivity')}
          </button>
        </section>
      )}

      <section>
        <div className="mb-4 flex items-center justify-between gap-3">
          <h2 className="app-background-copy text-xs font-bold uppercase tracking-[0.16em] text-ink-faint">{t('workspace.activities')}</h2>
        </div>
        {!states ? (
          <p role="status" className="glass-card rounded-xl px-5 py-8 text-center text-sm text-ink-muted">{t('workspace.loading')}</p>
        ) : activities.length === 0 ? (
          <div className="glass-card rounded-2xl p-8 text-center">
            <p className="text-sm text-ink-muted">{t('workspace.emptyEnabled')}</p>
          </div>
        ) : (
          <div className="grid gap-2.5 md:grid-cols-2 2xl:grid-cols-3">
            {previewActivities.map((item) => {
              const pinnedItem = layout.pinnedIds.includes(item.id)
              const isDragging = draggedActivityId === item.id
              return (
                <article
                  key={item.id}
                  data-activity-id={item.id}
                  data-drop-target={dropTargetId === item.id ? 'true' : undefined}
                  style={isDragging && dragPreview ? { height: dragPreview.height } : undefined}
                  className={`activity-card glass-card flex min-h-40 flex-col rounded-xl p-3 transition ${
                    isDragging ? 'activity-card-dragging' : ''
                  }`}
                >
                  {dropTargetId === item.id && <span className="activity-swap-indicator" aria-hidden="true"><ArrowLeftRight className="h-3.5 w-3.5" /></span>}
                  {!isDragging && <>
                  <div className="flex items-start justify-between gap-3">
                    <button type="button" onClick={() => open(item.id)} className="activity-enter flex min-w-0 cursor-pointer items-start gap-3 text-left">
                      <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-brand-600/15 text-brand-400">
                        <PluginIcon icon={item.icon} pluginId={item.pluginId} className="h-5 w-5" />
                      </span>
                      <span className="min-w-0">
                        <span className="block truncate font-semibold text-ink">{item.title}</span>
                        {item.state.manifest.name !== item.title && <span className="mt-1 block truncate text-xs text-ink-faint">{item.state.manifest.name}</span>}
                      </span>
                    </button>
                    <div className="flex shrink-0 items-center gap-2">
                      {item.status !== 'ready' && <span className="rounded-md border border-glass-line px-2 py-1 text-xs text-ink-muted">
                        {t(`workspace.contributionStatus.${item.status}`)}
                      </span>}
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
                    <button type="button" onClick={() => togglePinned(item.id)} className="app-icon-button" aria-pressed={pinnedItem} aria-label={pinnedItem ? t('workspace.unpin') : t('workspace.pin')} title={pinnedItem ? t('workspace.unpin') : t('workspace.pin')}>
                      <Pin className={`h-4 w-4${pinnedItem ? ' fill-current' : ''}`} aria-hidden />
                    </button>
                  </div>
                  </>}
                </article>
              )
            })}
          </div>
        )}
      </section>
      {dragPreview && dragPreviewActivity && (
        <div ref={dragPreviewRef} className="activity-drag-preview glass-card flex min-h-40 flex-col rounded-xl p-3" aria-hidden="true">
          <div className="flex items-start justify-between gap-3">
            <div className="flex min-w-0 items-start gap-3">
              <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-brand-600/15 text-brand-400">
                <PluginIcon icon={dragPreviewActivity.icon} pluginId={dragPreviewActivity.pluginId} className="h-5 w-5" />
              </span>
              <span className="min-w-0">
                <span className="block truncate font-semibold text-ink">{dragPreviewActivity.title}</span>
                {dragPreviewActivity.state.manifest.name !== dragPreviewActivity.title && <span className="mt-1 block truncate text-xs text-ink-faint">{dragPreviewActivity.state.manifest.name}</span>}
              </span>
            </div>
            {dragPreviewActivity.status !== 'ready' && <span className="shrink-0 rounded-md border border-glass-line px-2 py-1 text-xs text-ink-muted">
              {t(`workspace.contributionStatus.${dragPreviewActivity.status}`)}
            </span>}
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
