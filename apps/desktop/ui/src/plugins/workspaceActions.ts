export const MAX_OPEN_ACTIVITY_SURFACES = 8

export interface WorkspaceLayoutState {
  version: 1
  collapsed: boolean
  pinnedIds: string[]
  hiddenIds: string[]
  orderIds: string[]
  activityOrderIds: string[]
  openContributionIds: string[]
  activeContributionId: string | null
  activeSidebarContributionId: string | null
}

export function emptyWorkspaceLayout(): WorkspaceLayoutState {
  return {
    version: 1, collapsed: false, pinnedIds: [], hiddenIds: [], orderIds: [],
    activityOrderIds: [], openContributionIds: [], activeContributionId: null,
    activeSidebarContributionId: null,
  }
}

export type WorkspaceAction =
  | { type: 'openActivity'; id: string }
  | { type: 'syncActivities'; availableIds: readonly string[]; activeId: string | null }
  | { type: 'sidebar'; id: string | null }
  | { type: 'collapse'; value?: boolean }
  | { type: 'pin'; id: string; value?: boolean }
  | { type: 'hide'; id: string; value?: boolean }
  | { type: 'move'; id: string; direction: -1 | 1 }
  | { type: 'moveBefore'; id: string; before: string }
  | { type: 'reorderActivities'; ids: readonly string[] }
  | { type: 'reorderTabs'; id: string; targetId: string }
  | { type: 'reset' }

/** Pure transitions used by desktop controls and CLI UI commands. */
export function reduceWorkspaceLayout(current: WorkspaceLayoutState, action: WorkspaceAction): WorkspaceLayoutState {
  switch (action.type) {
    case 'openActivity':
      return {
        ...current,
        openContributionIds: current.openContributionIds.includes(action.id)
          ? current.openContributionIds
          : [...current.openContributionIds, action.id].slice(-MAX_OPEN_ACTIVITY_SURFACES),
        activeContributionId: action.id,
      }
    case 'syncActivities': {
      const available = new Set(action.availableIds)
      let openContributionIds = current.openContributionIds.filter((id) => available.has(id))
      if (action.activeId && available.has(action.activeId) && !openContributionIds.includes(action.activeId)) {
        openContributionIds.push(action.activeId)
      }
      openContributionIds = openContributionIds.slice(-MAX_OPEN_ACTIVITY_SURFACES)
      const activeContributionId = action.activeId && openContributionIds.includes(action.activeId)
        ? action.activeId
        : current.activeContributionId && openContributionIds.includes(current.activeContributionId)
          ? current.activeContributionId : null
      if (sameIds(current.openContributionIds, openContributionIds) && current.activeContributionId === activeContributionId) return current
      return { ...current, openContributionIds, activeContributionId }
    }
    case 'sidebar': return { ...current, activeSidebarContributionId: action.id }
    case 'collapse': return { ...current, collapsed: action.value ?? !current.collapsed }
    case 'pin': {
      const pinned = action.value ?? !current.pinnedIds.includes(action.id)
      return {
        ...current,
        pinnedIds: pinned ? unique([...current.pinnedIds, action.id]) : current.pinnedIds.filter((id) => id !== action.id),
        hiddenIds: pinned ? current.hiddenIds.filter((id) => id !== action.id) : current.hiddenIds,
        orderIds: pinned ? unique([...current.orderIds, action.id]) : current.orderIds,
      }
    }
    case 'hide': {
      const hidden = action.value ?? !current.hiddenIds.includes(action.id)
      return {
        ...current,
        hiddenIds: hidden ? unique([...current.hiddenIds, action.id]) : current.hiddenIds.filter((id) => id !== action.id),
        pinnedIds: hidden ? current.pinnedIds.filter((id) => id !== action.id) : current.pinnedIds,
      }
    }
    case 'move': {
      const orderIds = unique([...current.orderIds, ...current.pinnedIds])
      const visible = orderIds.filter((id) => current.pinnedIds.includes(id))
      const index = visible.indexOf(action.id)
      const target = index + action.direction
      if (index < 0 || target < 0 || target >= visible.length) return current
      return { ...current, orderIds: orderIds.map((id) => id === action.id ? visible[target] : id === visible[target] ? action.id : id) }
    }
    case 'moveBefore': {
      if (action.id === action.before) return current
      const pinned = current.pinnedIds.includes(action.id)
      const ids = pinned ? unique([...current.orderIds, ...current.pinnedIds]) : [...current.activityOrderIds]
      const ordered = ids.filter((id) => id !== action.id)
      const index = ordered.indexOf(action.before)
      ordered.splice(index < 0 ? ordered.length : index, 0, action.id)
      return pinned ? { ...current, orderIds: ordered } : { ...current, activityOrderIds: ordered }
    }
    case 'reorderActivities': {
      const ordered = unique(action.ids)
      const orderedSet = new Set(ordered)
      return { ...current, activityOrderIds: [...ordered, ...current.activityOrderIds.filter((id) => !orderedSet.has(id))] }
    }
    case 'reorderTabs': {
      const ids = [...current.openContributionIds]
      const source = ids.indexOf(action.id)
      const target = ids.indexOf(action.targetId)
      if (source < 0 || target < 0 || source === target) return current
      ids.splice(source, 1)
      ids.splice(target, 0, action.id)
      return { ...current, openContributionIds: ids }
    }
    case 'reset': return emptyWorkspaceLayout()
  }
}

export interface WorkspaceCloseResult {
  layout: WorkspaceLayoutState
  closed: boolean
  routeChanged: boolean
  focusId: string | null
}

/** Closing selects the right neighbor, then the nearest left neighbor; inactive closes keep the route. */
export function closeWorkspaceActivity(
  current: WorkspaceLayoutState,
  id: string,
  routeActiveId: string | null,
  availableIds: readonly string[],
): WorkspaceCloseResult {
  const available = new Set(availableIds)
  const open = current.openContributionIds.filter((item) => available.has(item))
  if (routeActiveId && available.has(routeActiveId) && !open.includes(routeActiveId)) open.push(routeActiveId)
  const visible = open.slice(-MAX_OPEN_ACTIVITY_SURFACES)
  const index = visible.indexOf(id)
  if (index < 0) return { layout: current, closed: false, routeChanged: false, focusId: null }
  const remaining = visible.filter((item) => item !== id)
  const fallback = [...visible.slice(index + 1), ...visible.slice(0, index).reverse()]
  const routeChanged = id === routeActiveId
  const activeContributionId = routeChanged
    ? fallback[0] ?? null
    : routeActiveId && remaining.includes(routeActiveId)
      ? routeActiveId
      : current.activeContributionId && remaining.includes(current.activeContributionId)
        ? current.activeContributionId : fallback[0] ?? null
  return {
    layout: { ...current, openContributionIds: remaining, activeContributionId },
    closed: true,
    routeChanged,
    focusId: activeContributionId,
  }
}

function unique(values: readonly string[]) { return [...new Set(values)] }
function sameIds(left: readonly string[], right: readonly string[]) {
  return left.length === right.length && left.every((id, index) => id === right[index])
}
