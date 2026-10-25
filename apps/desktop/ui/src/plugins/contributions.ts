import type { NavigateFunction } from 'react-router-dom'
import type { PluginRuntimeState, PluginUiContributionKind } from '@wonderland/plugin-protocol'

import { rememberOpenContribution, setActiveSidebarContribution } from './workspaceLayout.ts'

export type ContributionStatus =
  | 'ready'
  | 'disabled'
  | 'failed'
  | 'starting'
  | 'stopped'
  | 'invalid'
  | 'incompatible'

export interface WorkspaceContribution {
  id: string
  pluginId: string
  contributionId: string
  title: string
  kind: PluginUiContributionKind
  location: string | null
  icon: string
  defaultOrder: number
  href: string
  integrations: readonly string[]
  status: ContributionStatus
  state: PluginRuntimeState
}

/** Core 的统一导航注册表。Contribution ID 始终作为布局和路由的稳定键。 */
export function buildContributionRegistry(
  states: readonly PluginRuntimeState[] | null,
): WorkspaceContribution[] {
  if (!states) return []

  return states
    .filter((state) => state.installation !== 'missing')
    .flatMap((state) => {
      const ui = state.manifest.ui
      if (!ui) return []
      return ui.contributions.map((declaration) => ({
        id: `${state.manifest.id}/${declaration.id}`,
        pluginId: state.manifest.id,
        contributionId: declaration.id,
        title: declaration.title,
        kind: declaration.kind,
        location: declaration.location ?? null,
        icon: declaration.icon ?? state.manifest.icon ?? 'puzzle',
        defaultOrder: declaration.defaultOrder,
        href: `/workspace/plugin/${encodeURIComponent(state.manifest.id)}/${encodeURIComponent(declaration.id)}`,
        integrations: ui.integrations,
        status: contributionStatus(state),
        state,
      }))
    })
    .sort((left, right) => left.defaultOrder - right.defaultOrder || left.id.localeCompare(right.id))
}

export function findContribution(
  contributions: readonly WorkspaceContribution[],
  id: string,
): WorkspaceContribution | undefined {
  return contributions.find((contribution) => contribution.id === id)
}

/** The same open action is used by navigation, the launcher, and Command Palette. */
export function openContribution(
  contributions: readonly WorkspaceContribution[],
  id: string,
  navigate: NavigateFunction,
): boolean {
  const contribution = findContribution(contributions, id)
  if (!contribution) return false
  if (contribution.kind === 'view' && canOpenContribution(contribution)) {
    setActiveSidebarContribution(contribution.id)
    navigate('/workspace')
    return true
  }
  if (contribution.kind === 'activity' && canOpenContribution(contribution)) rememberOpenContribution(contribution.id)
  navigate(contribution.href)
  return true
}

/** Opening a valid enabled page can prepare a cold backend or show its retry state. */
export function canOpenContribution(item: WorkspaceContribution): boolean {
  return item.state.installation === 'installed' && item.state.enabled && item.state.runtime !== 'stopping'
}

function contributionStatus(state: PluginRuntimeState): ContributionStatus {
  if (state.installation === 'checking') return 'failed'
  if (state.installation === 'invalid') return 'invalid'
  if (state.installation === 'incompatible') return 'incompatible'
  if (!state.enabled) return 'disabled'
  if (state.runtime === 'failed') return 'failed'
  if (state.runtime === 'stopping') return 'starting'
  return 'ready'
}
