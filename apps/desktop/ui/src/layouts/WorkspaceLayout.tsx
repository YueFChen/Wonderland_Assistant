import { Command, Grid2X2, House, PanelLeftClose, PanelLeftOpen, Search, Settings } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { SidebarHost, SidebarPanel, SidebarProvider } from '@wonderland/ui'

import { CommandPalette } from '../components/CommandPalette'
import { useCoreUpdate } from '../components/CoreUpdateProvider'
import { PluginSurface } from '../pages/PluginPage'
import { buildContributionRegistry, canOpenContribution, findContribution, openContribution } from '../plugins/contributions'
import { PluginIcon } from '../plugins/icons'
import { usePlugins } from '../plugins/api'
import { orderContributions, setActiveSidebarContribution, useWorkspaceLayout } from '../plugins/workspaceLayout'
import { rememberLastWorkPage } from '../startupMemory'
import { t } from '../i18n'
import { useTheme } from '../theme/ThemeProvider'
import brandAvatar from '../assets/brand-avatar.png'
import './workspace.css'

const OPEN_PALETTE_EVENT = 'wonderland:open-command-palette'

/** Core owns the Workspace shell; activities stay in isolated plugin frames in the main surface. */
export function WorkspaceLayout() {
  const navigate = useNavigate()
  const location = useLocation()
  const { pathname } = location
  const { states } = usePlugins()
  const { layout, toggleCollapsed } = useWorkspaceLayout()
  const { resolved, componentVariables } = useTheme()
  const { hasUpdate, update } = useCoreUpdate()
  const settingsLabel = hasUpdate && update
    ? `${t('settings.title')} · ${t('settings.update.available', { version: update.version })}`
    : t('settings.title')
  const [paletteOpen, setPaletteOpen] = useState(false)
  const registry = useMemo(() => buildContributionRegistry(states), [states])
  const ordered = useMemo(() => orderContributions(registry, layout.orderIds), [registry, layout.orderIds])
  const activityEntries = useMemo(() => ordered.filter((item) => item.kind === 'activity'), [ordered])
  const pinned = activityEntries
    .filter((item) => layout.pinnedIds.includes(item.id) && !layout.hiddenIds.includes(item.id))
  const sidebarContribution = layout.activeSidebarContributionId
    ? findContribution(registry, layout.activeSidebarContributionId)
    : undefined
  const activeSidebarView = sidebarContribution?.kind === 'view' && canOpenContribution(sidebarContribution)
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

  useEffect(() => {
    rememberLastWorkPage(`${location.pathname}${location.search}`)
  }, [location.pathname, location.search])

  return (
    <SidebarProvider>
      <div className="relative z-10 flex h-full min-h-0">
        <aside className="workspace-rail" data-collapsed={layout.collapsed} aria-label={t('workspace.title')}>
          <div className="workspace-rail-head">
            <div className="workspace-rail-brand">
              <img src={brandAvatar} alt="" width={38} height={38} />
              {!layout.collapsed && <span>{t('workspace.title')}</span>}
            </div>
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

            {pinned.length > 0 && <div className={layout.collapsed ? 'workspace-rail-divider' : 'workspace-rail-section'}>
              {!layout.collapsed && t('workspace.pinned')}
            </div>}
            {pinned.map((contribution) => {
              const linkContent = <>
                <PluginIcon icon={contribution.icon} pluginId={contribution.pluginId} className="h-[18px] w-[18px] shrink-0" />
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
            <NavLink to="/workspace/settings" className="workspace-rail-link" aria-label={settingsLabel} title={settingsLabel}>
              <span className="relative inline-flex shrink-0">
                <Settings aria-hidden />
                {hasUpdate && <span aria-hidden className="absolute -right-1 -top-1 h-2 w-2 rounded-full bg-[var(--app-accent)]" />}
              </span>
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
                themeVariables={componentVariables}
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
