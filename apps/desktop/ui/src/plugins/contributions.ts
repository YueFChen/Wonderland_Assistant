import type { NavigateFunction } from 'react-router-dom'
import type { PluginRuntimeState, PluginUiContributionKind } from '@wonderland/plugin-protocol'

import { rememberOpenContribution, setActiveSidebarContribution } from './workspaceLayout'

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

/** Core 的统一导航注册表。Manifest v2 中的 ID 始终作为布局和路由的稳定键。 */
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
  if (contribution.kind === 'view' && contribution.status === 'ready') {
    setActiveSidebarContribution(contribution.id)
    navigate('/workspace')
    return true
  }
  if (contribution.kind === 'activity' && contribution.status === 'ready') rememberOpenContribution(contribution.id)
  navigate(contribution.href)
  return true
}

function contributionStatus(state: PluginRuntimeState): ContributionStatus {
  if (state.installation === 'invalid') return 'invalid'
  if (state.installation === 'incompatible') return 'incompatible'
  if ((state.serviceDependencyIssues?.length ?? 0) > 0) return 'failed'
  if (!state.enabled) return 'disabled'
  if (state.runtime === 'failed') return 'failed'
  if (state.runtime === 'starting' || state.runtime === 'stopping') return 'starting'
  if (state.runtime !== 'running') return 'stopped'
  return 'ready'
}
