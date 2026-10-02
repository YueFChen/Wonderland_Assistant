import { useEffect, useMemo, useState, type FormEvent } from 'react'
import { ArrowLeft, ArrowUpRight, CheckCircle2, LayoutGrid, LogOut, Monitor, Moon, PanelLeftClose, PanelLeftOpen, Search, ShieldCheck, Smartphone, Sun, Wifi } from 'lucide-react'
import { useLocation, useNavigate } from 'react-router-dom'
import { connectWeb, disconnectWeb } from '../core/transport'
import { usePlugins } from '../plugins/api'
import { buildContributionRegistry, type WorkspaceContribution } from '../plugins/contributions'
import { primaryPluginEntries } from '../plugins/pluginEntries'
import { PluginIcon } from '../plugins/icons'
import { PluginSurface } from '../pages/PluginPage'
import { t } from '../i18n'
import brand from '../assets/brand-avatar.png'
import { RemoteInteractions } from './RemoteInteractions'

export function MobileApp({ initialConnection = null }: { initialConnection?: Promise<{ version: string }> | null }) {
  const [version, setVersion] = useState('')
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(Boolean(initialConnection))
  const [error, setError] = useState('')
  const [dark, setDark] = useState(() => matchMedia('(prefers-color-scheme: dark)').matches)
  useEffect(() => { document.documentElement.dataset.theme = dark ? 'dark' : 'light' }, [dark])
  useEffect(() => {
    if (!initialConnection) return
    let live = true
    void initialConnection.then((status) => { if (live) setVersion(status.version) })
      .catch((cause) => { if (live) setError(cause instanceof Error ? cause.message : String(cause)) })
      .finally(() => { if (live) setBusy(false) })
    return () => { live = false }
  }, [initialConnection])
  useEffect(() => {
    const stop = () => setVersion('')
    window.addEventListener('wonderland:web-disconnected', stop)
    return () => window.removeEventListener('wonderland:web-disconnected', stop)
  }, [])
  const connect = async (event: FormEvent) => {
    event.preventDefault()
    setBusy(true)
    setError('')
    try {
      const status = await connectWeb(token)
      setVersion(status.version)
      setToken('')
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }
  return <div className="mobile-app" data-connected={Boolean(version)}>
    <header className="mobile-header">
      <a href="#/" className="mobile-brand"><img src={brand} alt="" /><span>Wonderland<small>{t('webClient.subtitle')}</small></span></a>
      <div className="remote-header-actions">
        {version && <span className="remote-desktop-only remote-session"><Monitor size={15} aria-hidden /> CORE {version}</span>}
        <button className="mobile-icon" onClick={() => setDark(!dark)} aria-label={t(dark ? 'mobile.light' : 'mobile.dark')}>{dark ? <Sun size={20} /> : <Moon size={20} />}</button>
      </div>
    </header>
    {version ? <Workspace version={version} dark={dark} /> : <div className="remote-login-layout">
      <aside className="remote-login-brand remote-desktop-only" aria-label="Wonderland Assistant">
        <img src={brand} alt="" />
        <p className="mobile-kicker">WONDERLAND ASSISTANT</p>
        <h1>{t('webClient.welcome')}</h1>
        <p className="mobile-intro">{t('webClient.intro')}</p>
        <div className="remote-login-features"><span><Monitor size={18} aria-hidden />{t('webClient.desktopLayout')}</span><span><ShieldCheck size={18} aria-hidden />{t('webClient.sharedOnly')}</span></div>
      </aside>
      <main className="mobile-login">
      <div className="remote-mobile-only">
      <div className="mobile-kicker"><Smartphone size={16} /> {t('mobile.kicker')}</div>
      <h1>{t('mobile.welcome')}</h1>
      <p className="mobile-intro">{t('mobile.intro')}</p>
      </div>
      <form className="glass-card mobile-connect" onSubmit={(event) => void connect(event)}>
        <div className="mobile-connect-title"><Wifi size={22} /><h2>{t('mobile.connect')}</h2></div>
        <p>{t('mobile.connectHint')}</p>
        <label htmlFor="access-key">{t('webAccess.key')}</label>
        <input id="access-key" type="password" autoComplete="off" spellCheck={false} value={token} onChange={(e) => setToken(e.target.value)} placeholder={t('mobile.keyPlaceholder')} required />
        <button className="mobile-primary" disabled={busy || !token.trim()}>{t(busy ? 'mobile.connecting' : 'mobile.enter')}<ArrowUpRight size={18} /></button>
        {error && <p role="alert" className="mobile-error">{error}</p>}
      </form>
      <p className="mobile-security"><ShieldCheck size={18} />{t('mobile.keyPrivacy')}</p>
    </main></div>}
  </div>
}

function Workspace({ version, dark }: { version: string; dark: boolean }) {
  const { states, error } = usePlugins()
  const [search, setSearch] = useState('')
  const [connection, setConnection] = useState('connected')
  const [missed, setMissed] = useState(false)
  const [collapsed, setCollapsed] = useState(false)
  const navigate = useNavigate()
  const location = useLocation()
  const registry = useMemo(() => buildContributionRegistry(states), [states])
  const active = registry.find((item) => location.pathname === `/tool/${encodeURIComponent(item.pluginId)}/${encodeURIComponent(item.contributionId)}`)
  const allEntries = primaryPluginEntries(registry)
  const entries = allEntries.filter((item) => `${item.title} ${item.state.manifest.name}`.toLowerCase().includes(search.toLowerCase()))
  useEffect(() => {
    const change = (event: Event) => {
      const value = (event as CustomEvent<string>).detail
      setConnection(value)
      if (value === 'missed') setMissed(true)
    }
    window.addEventListener('wonderland:web-connection', change)
    return () => window.removeEventListener('wonderland:web-connection', change)
  }, [])
  const open = (id: string | null) => {
    const item = registry.find((candidate) => candidate.id === id)
    if (item) navigate(`/tool/${encodeURIComponent(item.pluginId)}/${encodeURIComponent(item.contributionId)}`)
  }
  return <div className="remote-workspace">
    <RemoteInteractions names={new Map((states ?? []).map((state) => [state.manifest.id, state.manifest.name]))} />
    <aside className="workspace-rail remote-rail" data-collapsed={collapsed} aria-label={t('webClient.navigation')}>
      <div className="workspace-rail-head">
        {!collapsed && <div className="remote-host"><span><Monitor size={17} aria-hidden />{t('webClient.remoteCore')}</span><small title={window.location.host}>{window.location.host}</small></div>}
        <button className="workspace-rail-toggle" onClick={() => setCollapsed(!collapsed)} aria-label={t(collapsed ? 'workspace.expandSidebar' : 'workspace.collapseSidebar')} title={t(collapsed ? 'workspace.expandSidebar' : 'workspace.collapseSidebar')} aria-expanded={!collapsed} aria-controls="remote-navigation">
          {collapsed ? <PanelLeftOpen aria-hidden /> : <PanelLeftClose aria-hidden />}
        </button>
      </div>
      <nav id="remote-navigation" className="workspace-rail-nav" aria-label={t('workspace.tools')}>
        <button onClick={() => navigate('/')} className={`workspace-rail-link workspace-rail-action${!active ? ' active' : ''}`} aria-current={!active ? 'page' : undefined} title={t('workspace.allTools')} aria-label={t('workspace.allTools')}>
          <LayoutGrid aria-hidden />{!collapsed && <span>{t('workspace.allTools')}</span>}
        </button>
        <div className={collapsed ? 'workspace-rail-divider' : 'workspace-rail-section'}>{!collapsed && t('webClient.sharedTools')}</div>
        {allEntries.map((item) => <button key={item.pluginId} className={`workspace-rail-link workspace-rail-action${active?.pluginId === item.pluginId ? ' active' : ''}`} onClick={() => open(item.id)} title={item.state.manifest.name} aria-label={item.state.manifest.name} aria-current={active?.pluginId === item.pluginId ? 'page' : undefined}>
          <PluginIcon icon={item.icon} className="remote-plugin-icon" />
          {!collapsed && <span className="workspace-rail-link-label">{item.state.manifest.name}</span>}
        </button>)}
      </nav>
      <div className="workspace-rail-footer">
        {!collapsed && <div className="remote-connection" role="status"><span data-connected={connection !== 'reconnecting'} />{t(connection === 'reconnecting' ? 'webClient.reconnecting' : 'mobile.connected')}<small>v{version}</small></div>}
        <button className="workspace-rail-link workspace-rail-action" onClick={disconnectWeb} title={t('mobile.disconnect')} aria-label={t('mobile.disconnect')}><LogOut aria-hidden />{!collapsed && <span>{t('mobile.disconnect')}</span>}</button>
      </div>
    </aside>
    <div className="remote-content">
    {connection === 'reconnecting' && <p role="status" className="mobile-banner">{t('mobile.reconnecting')}</p>}
    {missed && <p role="alert" className="mobile-banner">{t('mobile.eventsMissed')}<button onClick={() => { setMissed(false); navigate('/') }}>{t('mobile.back')}</button></p>}
    {active ? <main className="mobile-tool">
      <div className="mobile-tool-bar"><button className="mobile-icon" onClick={() => navigate('/')} aria-label={t('mobile.back')}><ArrowLeft size={20} /></button><h1>{active.title}</h1><span>{active.state.manifest.name}</span></div>
      <RemotePluginWorkspace key={active.pluginId} active={active} contributions={registry.filter((item) => item.pluginId === active.pluginId)} dark={dark} onOpen={open} />
    </main> : <main className="mobile-workspace">
      <div className="mobile-kicker"><CheckCircle2 size={15} /> CORE {version} · {t('mobile.connected')}</div>
      <div className="remote-workspace-heading"><div><h1>{t('mobile.workspace')}</h1><p className="mobile-intro">{t('mobile.workspaceHint')}</p></div><span className="remote-desktop-only remote-tool-count">{t('webClient.toolCount', { count: allEntries.length })}</span></div>
      <label className="mobile-search"><Search size={19} /><input aria-label={t('mobile.search')} placeholder={t('mobile.search')} value={search} onChange={(e) => setSearch(e.target.value)} /></label>
      {error && <p role="alert" className="mobile-error">{t(error)}</p>}
      {!states && <p role="status" className="mobile-empty">{t('pluginPage.loading')}</p>}
      {states && entries.length === 0 && <div className="glass-card mobile-empty"><LayoutGrid size={28} /><p>{t(search ? 'mobile.noResults' : 'mobile.noTools')}</p></div>}
      <div className="mobile-tools">{entries.map((item) => <button key={item.id} className="glass-card mobile-tool-card" onClick={() => open(item.id)}>
        <div className="mobile-tool-glyph"><PluginIcon icon={item.icon} className="remote-card-icon" /></div><ArrowUpRight size={18} className="mobile-tool-arrow" />
        <strong>{item.state.manifest.name}</strong><span>{item.state.manifest.description}</span><small data-ready={item.status === 'ready'}>{t(item.status === 'ready' ? 'mobile.ready' : 'mobile.unavailableShort')}</small>
      </button>)}</div>
      <p className="mobile-note remote-mobile-only">{t('mobile.pluginHint')}</p>
      <p className="mobile-note remote-desktop-only">{t('webClient.pluginHint')}</p>
    </main>}
    </div>
    <nav className="mobile-nav" aria-label={t('mobile.navigation')}><button onClick={() => navigate('/')} aria-current={!active ? 'page' : undefined}><LayoutGrid size={20} />{t('mobile.workspace')}</button><button onClick={disconnectWeb}><LogOut size={20} />{t('mobile.disconnect')}</button></nav>
  </div>
}

function RemotePluginWorkspace({ active, contributions, dark, onOpen }: {
  active: WorkspaceContribution
  contributions: WorkspaceContribution[]
  dark: boolean
  onOpen: (id: string | null) => void
}) {
  const [opened, setOpened] = useState([active.id])
  useEffect(() => { setOpened((ids) => ids.includes(active.id) ? ids : [...ids, active.id]) }, [active.id])
  // Keep visited surfaces alive so an auxiliary View can publish changes back to its Activity.
  const visibleSurfaces = contributions.filter((item) => opened.includes(item.id) || item.id === active.id)
  return <>
    {contributions.length > 1 && <>
      <label className="mobile-view-selector remote-mobile-only">{t('mobile.pluginPage')}<select value={active.id} onChange={(event) => onOpen(event.target.value)}>{contributions.map((item) => <option key={item.id} value={item.id}>{item.title}</option>)}</select></label>
      <nav className="remote-page-tabs remote-desktop-only" aria-label={t('mobile.pluginPage')}>{contributions.map((item) => <button key={item.id} onClick={() => onOpen(item.id)} aria-current={item.id === active.id ? 'page' : undefined}>{item.title}</button>)}</nav>
    </>}
    <div className="mobile-surface">
      {visibleSurfaces.map((item) => item.status === 'ready'
        ? <PluginSurface key={item.id} contribution={item} active={item.id === active.id} resolvedTheme={dark ? 'dark' : 'light'} surface={item.kind} onOpenView={onOpen} />
        : item.id === active.id && <p key={item.id} role="status" className="mobile-empty">{t('mobile.unavailable')}</p>)}
    </div>
  </>
}
