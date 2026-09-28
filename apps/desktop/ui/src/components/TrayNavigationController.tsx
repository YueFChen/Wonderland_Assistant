import { useEffect, useMemo } from 'react'
import { useNavigate } from 'react-router-dom'
import { isTauri } from '@tauri-apps/api/core'

import { coreApi } from '../core/api'
import { buildContributionRegistry } from '../plugins/contributions'
import { usePlugins } from '../plugins/api'
import { orderContributions, useWorkspaceLayout } from '../plugins/workspaceLayout'

/** Keeps the native tray menu aligned with the currently pinned workspace activities. */
export function TrayNavigationController() {
  const navigate = useNavigate()
  const { states } = usePlugins()
  const { layout } = useWorkspaceLayout()
  const registry = useMemo(() => buildContributionRegistry(states), [states])
  const entries = useMemo(() => orderContributions(
    registry.filter((item) => item.kind === 'activity' && layout.pinnedIds.includes(item.id) && !layout.hiddenIds.includes(item.id)),
    layout.orderIds,
  ).slice(0, 5).map((item) => ({ id: item.id, title: item.title })), [registry, layout.hiddenIds, layout.orderIds, layout.pinnedIds])
  const signature = JSON.stringify(entries)

  useEffect(() => {
    if (!isTauri()) return
    const currentEntries = JSON.parse(signature) as typeof entries
    void coreApi.syncTrayPinned(currentEntries).catch(() => undefined)
  }, [signature])

  useEffect(() => {
    if (!isTauri()) return
    let live = true
    let unlisten: (() => void) | undefined
    void coreApi.onTrayNavigation((action) => {
      if (!live) return
      if (action.kind === 'home') navigate('/')
      else if (action.kind === 'pluginManagement') navigate('/workspace?view=plugins')
      else navigate(`/workspace/plugin/${encodeURIComponent(action.pluginId)}/${encodeURIComponent(action.contributionId)}`)
    }).then((dispose) => {
      if (live) unlisten = dispose
      else dispose()
    })
    return () => {
      live = false
      unlisten?.()
    }
  }, [navigate])

  return null
}
