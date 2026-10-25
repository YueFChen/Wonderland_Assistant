import { useCallback, useEffect, useState } from 'react'
import { emptyWorkspaceLayout, MAX_OPEN_ACTIVITY_SURFACES, reduceWorkspaceLayout, type WorkspaceAction, type WorkspaceLayoutState } from './workspaceActions.ts'
export { MAX_OPEN_ACTIVITY_SURFACES, type WorkspaceLayoutState } from './workspaceActions.ts'

const STORAGE_KEY = 'wonderland.workspace.layout.v1'
const CHANGE_EVENT = 'wonderland:workspace-layout-change'

export function readWorkspaceLayout(): WorkspaceLayoutState {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? 'null')
    if (!isRecord(value) || value.version !== 1) return emptyWorkspaceLayout()
    return {
      version: 1,
      collapsed: value.collapsed === true,
      pinnedIds: readIds(value.pinnedIds),
      hiddenIds: readIds(value.hiddenIds),
      orderIds: readIds(value.orderIds),
      activityOrderIds: Array.isArray(value.activityOrderIds)
        ? readIds(value.activityOrderIds)
        : readIds(value.orderIds),
      openContributionIds: readIds(value.openContributionIds).slice(-MAX_OPEN_ACTIVITY_SURFACES),
      activeContributionId: typeof value.activeContributionId === 'string' && value.activeContributionId.length <= 128
        ? value.activeContributionId
        : null,
      activeSidebarContributionId: typeof value.activeSidebarContributionId === 'string' && value.activeSidebarContributionId.length <= 128
        ? value.activeSidebarContributionId
        : null,
    }
  } catch {
    return emptyWorkspaceLayout()
  }
}

export function writeWorkspaceLayout(layout: WorkspaceLayoutState) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(layout))
    window.dispatchEvent(new Event(CHANGE_EVENT))
  } catch {
  }
}

export function rememberOpenContribution(id: string) {
  applyWorkspaceAction({ type: 'openActivity', id })
}

export function setActiveSidebarContribution(id: string | null) {
  applyWorkspaceAction({ type: 'sidebar', id })
}

export function applyWorkspaceAction(action: WorkspaceAction): WorkspaceLayoutState {
  const next = normalizeLayout(reduceWorkspaceLayout(readWorkspaceLayout(), action))
  writeWorkspaceLayout(next)
  return next
}

export function useWorkspaceLayout() {
  const [layout, setLayout] = useState(readWorkspaceLayout)

  useEffect(() => {
    const refresh = () => setLayout(readWorkspaceLayout())
    window.addEventListener(CHANGE_EVENT, refresh)
    window.addEventListener('storage', refresh)
    return () => {
      window.removeEventListener(CHANGE_EVENT, refresh)
      window.removeEventListener('storage', refresh)
    }
  }, [])

  const update = useCallback((change: (current: WorkspaceLayoutState) => WorkspaceLayoutState) => {
    const next = normalizeLayout(change(readWorkspaceLayout()))
    writeWorkspaceLayout(next)
    setLayout(next)
  }, [])

  const dispatch = useCallback((action: WorkspaceAction) => update((current) => reduceWorkspaceLayout(current, action)), [update])
  const toggleCollapsed = useCallback(() => dispatch({ type: 'collapse' }), [dispatch])

  const togglePinned = useCallback((id: string) => {
    dispatch({ type: 'pin', id })
  }, [dispatch])

  const toggleHidden = useCallback((id: string) => {
    dispatch({ type: 'hide', id })
  }, [dispatch])

  const move = useCallback((id: string, direction: -1 | 1) => {
    dispatch({ type: 'move', id, direction })
  }, [dispatch])

  const reorderActivities = useCallback((orderedIds: readonly string[]) => {
    dispatch({ type: 'reorderActivities', ids: orderedIds })
  }, [dispatch])

  const reset = useCallback(() => dispatch({ type: 'reset' }), [dispatch])

  return { layout, toggleCollapsed, togglePinned, toggleHidden, move, reorderActivities, reset, update }
}

export function orderContributions<T extends { id: string; defaultOrder: number }>(
  contributions: readonly T[],
  orderIds: readonly string[],
): T[] {
  const position = new Map(orderIds.map((id, index) => [id, index]))
  return [...contributions].sort((left, right) => {
    const leftPosition = position.get(left.id)
    const rightPosition = position.get(right.id)
    if (leftPosition !== undefined || rightPosition !== undefined) {
      if (leftPosition === undefined) return 1
      if (rightPosition === undefined) return -1
      if (leftPosition !== rightPosition) return leftPosition - rightPosition
    }
    return left.defaultOrder - right.defaultOrder || left.id.localeCompare(right.id)
  })
}

function normalizeLayout(layout: WorkspaceLayoutState): WorkspaceLayoutState {
  return {
    version: 1,
    collapsed: layout.collapsed,
    pinnedIds: unique(layout.pinnedIds),
    hiddenIds: unique(layout.hiddenIds),
    orderIds: unique(layout.orderIds),
    activityOrderIds: unique(layout.activityOrderIds),
    openContributionIds: unique(layout.openContributionIds).slice(-MAX_OPEN_ACTIVITY_SURFACES),
    activeContributionId: layout.activeContributionId,
    activeSidebarContributionId: layout.activeSidebarContributionId,
  }
}

function readIds(value: unknown): string[] {
  if (!Array.isArray(value)) return []
  return unique(value.filter((item): item is string => typeof item === 'string' && item.length <= 128))
}

function unique(values: readonly string[]): string[] {
  return [...new Set(values)]
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}
