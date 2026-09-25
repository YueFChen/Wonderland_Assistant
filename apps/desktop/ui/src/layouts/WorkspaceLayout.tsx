import { Command, Grid2X2, House, PanelLeftClose, PanelLeftOpen, Search, Settings } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link, NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { SidebarHost, SidebarPanel, SidebarProvider } from '@wonderland/ui'

import { creatorBadge, currentAccount, displayName, firstRole } from '../account/display'
import { useAccount } from '../account/useAccount'
import { UserAvatar } from '../account/UserAvatar'
import { CommandPalette } from '../components/CommandPalette'
import { PluginSurface } from '../pages/PluginPage'
import { buildContributionRegistry, findContribution, openContribution } from '../plugins/contributions'
import { pluginIcon } from '../plugins/icons'
import { usePlugins } from '../plugins/api'
import { orderContributions, setActiveSidebarContribution, useWorkspaceLayout } from '../plugins/workspaceLayout'
import { t } from '../i18n'
import { useTheme } from '../theme/ThemeProvider'
import './workspace.css'

const MAX_PINNED_RAIL_ITEMS = 5
const OPEN_PALETTE_EVENT = 'wonderland:open-command-palette'

/** Core owns the Workspace shell; activities stay in isolated plugin frames in the main surface. */
export function WorkspaceLayout() {
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const { snapshot } = useAccount()
  const { states } = usePlugins()
  const { layout, toggleCollapsed } = useWorkspaceLayout()
  const { resolved } = useTheme()
  const [paletteOpen, setPaletteOpen] = useState(false)
  const registry = useMemo(() => buildContributionRegistry(states), [states])
  const ordered = useMemo(() => orderContributions(registry, layout.orderIds), [registry, layout.orderIds])
  const activityEntries = useMemo(() => ordered.filter((item) => item.kind === 'activity'), [ordered])
  const pinned = activityEntries
    .filter((item) => layout.pinnedIds.includes(item.id) && !layout.hiddenIds.includes(item.id))
    .slice(0, MAX_PINNED_RAIL_ITEMS)
  const sidebarContribution = layout.activeSidebarContributionId
    ? findContribution(registry, layout.activeSidebarContributionId)
    : undefined
  const activeSidebarView = sidebarContribution?.kind === 'view' && sidebarContribution.status === 'ready'
    ? sidebarContribution
    : undefined

  const closePalette = useCallback(() => setPaletteOpen(false), [])
  const selectContribution = useCallback((id: string) => {
    setPaletteOpen(false)
    openContribution(registry, id, navigate)
  }, [navigate, registry])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === 'p') {
        event.preventDefault()
        setPaletteOpen(true)
      }
    }
    const onOpenPalette = () => setPaletteOpen(true)
    window.addEventListener('keydown', onKeyDown)
    window.addEventListener(OPEN_PALETTE_EVENT, onOpenPalette)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      window.removeEventListener(OPEN_PALETTE_EVENT, onOpenPalette)
    }
  }, [])

  const account = currentAccount(snapshot)
  const role = firstRole(account)
  const nickname = displayName(account)
  const badge = creatorBadge(account)
  const accountSubtitle = role
    ? `Lv.${role.level} · ${role.region_name}`
    : account
      ? t('common.profilePendingSync')
      : t('workspace.clickToLogin')

  return (
    <SidebarProvider>
      <div className="relative z-10 flex h-full min-h-0">
        <aside className="workspace-rail" data-collapsed={layout.collapsed} aria-label={t('workspace.title')}>
          <div className="workspace-rail-head">
            <Link
              to="/workspace/account"
              className="workspace-rail-account"
              aria-label={layout.collapsed ? (nickname ?? t('common.notLoggedIn')) : undefined}
              title={layout.collapsed ? (nickname ?? t('common.notLoggedIn')) : undefined}
            >
              <UserAvatar name={role?.nickname} src={account?.avatar_url} size={38} />
              {!layout.collapsed && (
                <span className="workspace-rail-account-copy">
                  <span className="workspace-rail-account-name">
                    <span className="truncate">{nickname ?? t('common.notLoggedIn')}</span>
                    {badge && <span className="workspace-rail-badge">{badge}</span>}
                  </span>
                  <span className="workspace-rail-account-subtitle">{accountSubtitle}</span>
                </span>
              )}
            </Link>
            <button
              type="button"
              className="workspace-rail-toggle"
              aria-label={layout.collapsed ? t('workspace.expandSidebar') : t('workspace.collapseSidebar')}
              title={layout.collapsed ? t('workspace.expandSidebar') : t('workspace.collapseSidebar')}
              aria-expanded={!layout.collapsed}
              aria-controls="workspace-navigation"
              onClick={toggleCollapsed}
            >
              {layout.collapsed ? <PanelLeftOpen aria-hidden /> : <PanelLeftClose aria-hidden />}
            </button>
          </div>

          <nav id="workspace-navigation" className="workspace-rail-nav" aria-label={t('workspace.tools')}>
            <NavLink to="/" end className="workspace-rail-link" aria-label={layout.collapsed ? t('common.backToHome') : undefined} title={layout.collapsed ? t('common.backToHome') : undefined}>
              <House aria-hidden />
              {!layout.collapsed && <span>{t('common.backToHome')}</span>}
            </NavLink>
            <NavLink to="/workspace" end className="workspace-rail-link" aria-label={layout.collapsed ? t('workspace.allTools') : undefined} title={layout.collapsed ? t('workspace.allTools') : undefined}>
              <Grid2X2 aria-hidden />
              {!layout.collapsed && <span>{t('workspace.allTools')}</span>}
            </NavLink>
            <button
              type="button"
              className="workspace-rail-link workspace-rail-action"
              onClick={() => setPaletteOpen(true)}
              aria-label={layout.collapsed ? t('workspace.search') : undefined}
              title={layout.collapsed ? `${t('workspace.search')} · Ctrl+Shift+P` : undefined}
            >
              <Search aria-hidden />
              {!layout.collapsed && <><span className="flex-1 text-left">{t('workspace.search')}</span><Command className="h-3.5 w-3.5 opacity-50" aria-hidden /></>}
            </button>

            <div className={layout.collapsed ? 'workspace-rail-divider' : 'workspace-rail-section'}>
              {!layout.collapsed && t('workspace.pinned')}
            </div>
            {pinned.length === 0 ? (
              layout.collapsed
                ? <span className="workspace-rail-empty-icon" title={t('workspace.noPinnedTools')}><Grid2X2 aria-hidden /></span>
                : <p className="workspace-rail-empty">{t('workspace.noPinnedTools')}</p>
            ) : pinned.map((contribution) => {
              const Icon = pluginIcon(contribution.icon)
              const linkContent = <>
                <Icon aria-hidden />
                {!layout.collapsed && <span className="workspace-rail-link-label">{contribution.title}</span>}
                {!layout.collapsed && contribution.status !== 'ready' && <span className="workspace-rail-status-dot" data-status={contribution.status} aria-label={t(`workspace.contributionStatus.${contribution.status}`)} />}
              </>
              return (
                <NavLink
                  key={contribution.id}
                  to={contribution.href}
                  className="workspace-rail-link"
                  aria-label={layout.collapsed ? contribution.title : undefined}
                  title={layout.collapsed ? `${contribution.title} · ${t(`workspace.contributionStatus.${contribution.status}`)}` : undefined}
                >
                  {linkContent}
                </NavLink>
              )
            })}
          </nav>

          <div className="workspace-rail-footer">
            <NavLink to="/workspace/settings" className="workspace-rail-link" aria-label={layout.collapsed ? t('settings.title') : undefined} title={layout.collapsed ? t('settings.title') : undefined}>
              <Settings aria-hidden />
              {!layout.collapsed && <span>{t('settings.title')}</span>}
            </NavLink>
          </div>
        </aside>

        <main className="workspace-main" data-plugin-surface={pathname.startsWith('/workspace/plugin/') ? 'true' : undefined}>
          <Outlet />
        </main>

        <SidebarPanel
          id="core.plugin-sidebar"
          title={activeSidebarView?.title ?? t('workspace.sidebar')}
          open={Boolean(activeSidebarView)}
          onClose={() => setActiveSidebarContribution(null)}
        >
          {activeSidebarView && (
            <div className="relative min-h-0 flex-1">
              <PluginSurface
                contribution={activeSidebarView}
                active
                resolvedTheme={resolved}
                surface="view"
                onOpenView={setActiveSidebarContribution}
              />
            </div>
          )}
        </SidebarPanel>
        <SidebarHost />
        <CommandPalette open={paletteOpen} contributions={activityEntries} onClose={closePalette} onSelect={selectContribution} />
      </div>
    </SidebarProvider>
  )
}
