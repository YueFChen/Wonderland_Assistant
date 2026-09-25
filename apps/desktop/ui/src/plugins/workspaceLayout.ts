import { useCallback, useEffect, useState } from 'react'

const STORAGE_KEY = 'wonderland.workspace.layout.v1'
const CHANGE_EVENT = 'wonderland:workspace-layout-change'
export const MAX_OPEN_ACTIVITY_SURFACES = 8

export interface WorkspaceLayoutState {
  version: 1
  collapsed: boolean
  pinnedIds: string[]
  hiddenIds: string[]
  /** Ordering for the pinned workspace rail. */
  orderIds: string[]
  /** Independent, freely arranged order for cards in All Activities. */
  activityOrderIds: string[]
  openContributionIds: string[]
  activeContributionId: string | null
  activeSidebarContributionId: string | null
}

const EMPTY_LAYOUT: WorkspaceLayoutState = {
  version: 1,
  collapsed: false,
  pinnedIds: [],
  hiddenIds: [],
  orderIds: [],
  activityOrderIds: [],
  openContributionIds: [],
  activeContributionId: null,
  activeSidebarContributionId: null,
}

export function readWorkspaceLayout(): WorkspaceLayoutState {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? 'null')
    if (!isRecord(value) || value.version !== 1) return { ...EMPTY_LAYOUT }
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
    return { ...EMPTY_LAYOUT }
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
  const current = readWorkspaceLayout()
  writeWorkspaceLayout({
    ...current,
    openContributionIds: [...current.openContributionIds.filter((item) => item !== id), id].slice(-MAX_OPEN_ACTIVITY_SURFACES),
    activeContributionId: id,
  })
}

export function setActiveSidebarContribution(id: string | null) {
  const current = readWorkspaceLayout()
  writeWorkspaceLayout({ ...current, activeSidebarContributionId: id })
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

  const toggleCollapsed = useCallback(() => {
    update((current) => ({ ...current, collapsed: !current.collapsed }))
  }, [update])

  const togglePinned = useCallback((id: string) => {
    update((current) => {
      const pinned = current.pinnedIds.includes(id)
      return {
        ...current,
        pinnedIds: pinned ? current.pinnedIds.filter((item) => item !== id) : [...current.pinnedIds, id],
        hiddenIds: pinned ? current.hiddenIds : current.hiddenIds.filter((item) => item !== id),
        orderIds: current.orderIds.includes(id) ? current.orderIds : [...current.orderIds, id],
      }
    })
  }, [update])

  const toggleHidden = useCallback((id: string) => {
    update((current) => {
      const hidden = current.hiddenIds.includes(id)
      return {
        ...current,
        hiddenIds: hidden ? current.hiddenIds.filter((item) => item !== id) : [...current.hiddenIds, id],
        pinnedIds: hidden ? current.pinnedIds : current.pinnedIds.filter((item) => item !== id),
      }
    })
  }, [update])

  const move = useCallback((id: string, direction: -1 | 1) => {
    update((current) => {
      const ordered = unique([...current.orderIds, ...current.pinnedIds])
      const index = ordered.indexOf(id)
      const target = index + direction
      if (index < 0 || target < 0 || target >= ordered.length) return current
      ;[ordered[index], ordered[target]] = [ordered[target], ordered[index]]
      return { ...current, orderIds: ordered }
    })
  }, [update])

  const reorderActivities = useCallback((orderedIds: readonly string[]) => {
    update((current) => {
      const ordered = unique(orderedIds)
      const orderedSet = new Set(ordered)
      const retained = current.activityOrderIds.filter((id) => !orderedSet.has(id))
      return { ...current, activityOrderIds: [...ordered, ...retained] }
    })
  }, [update])

  const reset = useCallback(() => update(() => ({ ...EMPTY_LAYOUT })), [update])

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
