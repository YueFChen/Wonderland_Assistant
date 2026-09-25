import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { X } from 'lucide-react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { listen } from '@tauri-apps/api/event'
import { UI_BRIDGE_PROTOCOL as BRIDGE_PROTOCOL, UI_BRIDGE_VERSION } from '@wonderland/plugin-ui-sdk'

import { t } from '../i18n'
import { pluginApi, usePlugins, type PluginEventPayload } from '../plugins/api'
import { buildContributionRegistry, findContribution } from '../plugins/contributions'
import type { WorkspaceContribution } from '../plugins/contributions'
import { MAX_OPEN_ACTIVITY_SURFACES, readWorkspaceLayout, rememberOpenContribution, setActiveSidebarContribution, useWorkspaceLayout } from '../plugins/workspaceLayout'
import { useTheme, type ResolvedTheme } from '../theme/ThemeProvider'

const MAX_CALLS_PER_SECOND = 30
const PLUGIN_UI_EVENT = 'wonderland:plugin-ui-event'
const MAX_PLUGIN_UI_EVENT_BYTES = 64 * 1024

type PluginUiMessage = {
  protocol: typeof BRIDGE_PROTOCOL
  bridgeVersion: string
  pluginId: string
  contributionId: string
  nonce: string
  type: string
  requestId?: string
  method?: string
  params?: unknown
  topic?: string
  viewId?: string
  payload?: unknown
}

/** Core owns activity tabs and mounts each enabled contribution in its own sandboxed frame. */
export function PluginPage() {
  const { pluginId = '', contributionId = '' } = useParams()
  const navigate = useNavigate()
  const { states, error } = usePlugins()
  const { layout, update } = useWorkspaceLayout()
  const { resolved } = useTheme()
  const registry = useMemo(() => buildContributionRegistry(states), [states])
  const activeId = `${pluginId}/${contributionId}`
  const activeContribution = findContribution(registry, activeId)

  useEffect(() => {
    if (activeContribution?.kind === 'view' && activeContribution.status === 'ready') {
      if (layout.activeSidebarContributionId !== activeId) setActiveSidebarContribution(activeId)
      navigate('/workspace', { replace: true })
      return
    }
    if (activeContribution?.kind === 'activity' && activeContribution.status === 'ready') {
      if (!layout.openContributionIds.includes(activeId)) {
        rememberOpenContribution(activeId)
      } else if (layout.activeContributionId !== activeId) {
        update((state) => state.activeContributionId === activeId
          ? state
          : { ...state, activeContributionId: activeId })
      }
    }
  }, [activeContribution?.kind, activeContribution?.status, activeId, layout.activeContributionId, layout.activeSidebarContributionId, layout.openContributionIds, navigate, update])

  const openTabs = useMemo(() => {
    const ids = [...layout.openContributionIds]
    if (activeContribution?.kind === 'activity' && activeContribution.status === 'ready' && !ids.includes(activeId)) ids.push(activeId)
    return ids
      .slice(-MAX_OPEN_ACTIVITY_SURFACES)
      .map((id) => findContribution(registry, id))
      .filter((item): item is WorkspaceContribution => item !== undefined && item.kind === 'activity')
  }, [activeContribution?.kind, activeContribution?.status, activeId, layout.openContributionIds, registry])

  const closeTab = useCallback((id: string) => {
    const current = readWorkspaceLayout()
    const nextIds = current.openContributionIds.filter((item) => item !== id)
    const nextActive = nextIds.map((nextId) => findContribution(registry, nextId)).find((item) => item?.status === 'ready')
    update((state) => ({
      ...state,
      openContributionIds: nextIds,
      activeContributionId: id === activeId ? nextActive?.id ?? null : state.activeContributionId,
    }))
    if (id === activeId) {
      navigate(nextActive?.href ?? '/workspace')
    }
  }, [activeId, navigate, registry, update])

  const selectTab = useCallback((item: WorkspaceContribution) => {
    update((state) => state.activeContributionId === item.id
      ? state
      : { ...state, activeContributionId: item.id })
    navigate(item.href)
  }, [navigate, update])

  if (error) return <p role="alert" className="text-sm text-[var(--app-danger)]">{t(error)}</p>
  if (!states) return <p role="status" className="text-sm text-ink-muted">{t('pluginPage.loading')}</p>
  if (!activeContribution) return <p className="text-sm text-ink-muted">{t('pluginPage.notInstalled')}</p>
  if (activeContribution.status !== 'ready') {
    return <ContributionUnavailable contribution={activeContribution} />
  }
  if (activeContribution.kind !== 'activity') return null

  return (
    <section className="plugin-page-surface flex h-full min-h-[28rem] min-w-0 flex-col">
      <div className="plugin-surface-tabs" role="tablist" aria-label={t('pluginPage.tabs')}>
        {openTabs.map((item) => (
          <div key={item.id} role="presentation" className={`plugin-surface-tab${item.id === activeId ? ' is-active' : ''}`}>
            <button type="button" role="tab" aria-selected={item.id === activeId} onClick={() => selectTab(item)} title={item.title}>
              <span className="max-w-52 truncate">{item.title}</span>
            </button>
            <button type="button" className="plugin-surface-tab-close" aria-label={t('pluginPage.closeTab', { title: item.title })} title={t('pluginPage.closeTab', { title: item.title })} onClick={() => closeTab(item.id)}>
              <X className="h-3.5 w-3.5" aria-hidden />
            </button>
          </div>
        ))}
      </div>
      <div className="relative min-h-0 flex-1">
        {openTabs.map((item) => (
          <PluginSurface
            key={item.id}
            contribution={item}
            active={item.id === activeId}
            resolvedTheme={resolved}
            surface="activity"
            onOpenView={setActiveSidebarContribution}
          />
        ))}
      </div>
    </section>
  )
}

function ContributionUnavailable({ contribution }: { contribution: WorkspaceContribution }) {
  const message = contribution.status === 'invalid'
    ? contribution.state.lastError?.message ?? t('pluginPage.invalid')
    : contribution.status === 'incompatible'
      ? t('pluginPage.incompatible')
      : contribution.status === 'disabled'
        ? t('pluginPage.disabled')
      : contribution.status === 'failed'
          ? t('pluginPage.failed', {
            error: contribution.state.lastError?.message
              ?? contribution.state.serviceDependencyIssues?.join(' ')
              ?? '',
          })
          : t(`settings.plugins.runtime.${contribution.state.runtime}`)
  return (
    <section className="glass-card mx-auto mt-10 w-full max-w-xl rounded-2xl p-7">
      <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-ink-faint">{contribution.state.manifest.name}</p>
      <h1 className="text-xl font-bold text-ink">{contribution.title}</h1>
      <p role={contribution.status === 'failed' || contribution.status === 'invalid' || contribution.status === 'incompatible' ? 'alert' : 'status'} className="mt-3 text-sm leading-6 text-ink-muted">{message}</p>
      <Link to="/workspace/settings" className="mt-5 inline-flex rounded-lg bg-brand-600 px-4 py-2 text-sm font-semibold text-white hover:bg-brand-500">{t('workspace.goToSettings')}</Link>
    </section>
  )
}

export function PluginSurface({
  contribution,
  active,
  resolvedTheme,
  surface,
  onOpenView,
}: {
  contribution: WorkspaceContribution
  active: boolean
  resolvedTheme: ResolvedTheme
  surface: 'activity' | 'view'
  onOpenView: (id: string | null) => void
}) {
  const { state } = contribution
  const frame = useRef<HTMLIFrameElement>(null)
  const nonce = useRef(crypto.randomUUID())
  const activeCalls = useRef<number[]>([])
  const subscribedTopics = useRef(new Set<string>())
  const [url, setUrl] = useState('')
  const [loaderFailure, setLoaderFailure] = useState('')
  const [ready, setReady] = useState(false)
  const [bridgeError, setBridgeError] = useState('')
  const followsTheme = contribution.integrations.includes('theme.followHost')
  const providesSidebar = contribution.integrations.includes('workspace.sidebar')

  const sendToPlugin = useCallback((message: Record<string, unknown>) => {
    frame.current?.contentWindow?.postMessage({
      protocol: BRIDGE_PROTOCOL,
      bridgeVersion: UI_BRIDGE_VERSION,
      pluginId: state.manifest.id,
      contributionId: contribution.contributionId,
      nonce: nonce.current,
      ...message,
    }, '*')
  }, [contribution.contributionId, state.manifest.id])

  useEffect(() => {
    let live = true
    setUrl('')
    setLoaderFailure('')
    setReady(false)
    void pluginApi.uiUrl(state.manifest.id)
      .then((next) => { if (live) setUrl(next) })
      .catch((cause) => { if (live) setLoaderFailure(errorText(cause)) })
    return () => { live = false }
  }, [state.manifest.id])

  useEffect(() => {
    const onMessage = (event: MessageEvent<unknown>) => {
      const frameWindow = frame.current?.contentWindow
      if (!frameWindow || event.source !== frameWindow || event.origin !== 'null') return
      const message = event.data as Partial<PluginUiMessage> | null
      if (
        !message
        || message.protocol !== BRIDGE_PROTOCOL
        || message.bridgeVersion !== UI_BRIDGE_VERSION
        || message.pluginId !== state.manifest.id
        || message.contributionId !== contribution.contributionId
        || message.nonce !== nonce.current
      ) return

      if (message.type === 'ready') {
        setReady(true)
        setBridgeError('')
        sendToPlugin({ type: 'surface_lifecycle', state: active ? 'active' : 'inactive' })
        return
      }
      if (message.type === 'open_sidebar' && typeof message.viewId === 'string' && providesSidebar) {
        const view = state.manifest.ui?.contributions.find((item) => item.id === message.viewId)
        if (view?.kind === 'view' && view.location === 'workspace.sidebar') {
          onOpenView(`${state.manifest.id}/${view.id}`)
        }
        return
      }
      if (message.type === 'publish' && typeof message.topic === 'string') {
        if (!/^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*$/.test(message.topic)) return
        let serialized: string
        try {
          serialized = JSON.stringify(message.payload ?? null)
        } catch {
          return
        }
        if (serialized.length > MAX_PLUGIN_UI_EVENT_BYTES || new TextEncoder().encode(serialized).byteLength > MAX_PLUGIN_UI_EVENT_BYTES) return
        window.dispatchEvent(new CustomEvent(PLUGIN_UI_EVENT, {
          detail: {
            pluginId: state.manifest.id,
            sourceContributionId: contribution.contributionId,
            topic: message.topic,
            payload: message.payload,
          },
        }))
        return
      }
      if (message.type === 'subscribe' && typeof message.topic === 'string') {
        if (!/^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*$/.test(message.topic)) return
        if (message.topic === 'host.theme' && followsTheme) {
          subscribedTopics.current.add(message.topic)
          sendToPlugin({ type: 'host_context', context: { theme: { resolved: resolvedTheme } } })
        } else if (message.topic === 'host.lifecycle') {
          subscribedTopics.current.add(message.topic)
          sendToPlugin({ type: 'surface_lifecycle', state: active ? 'active' : 'inactive' })
        } else if (!message.topic.startsWith('host.')) {
          subscribedTopics.current.add(message.topic)
        }
        return
      }
      if (message.type === 'cancel' && typeof message.requestId === 'string') {
        void pluginApi.cancel(state.manifest.id, message.requestId).catch(() => undefined)
        return
      }
      if (message.type !== 'call' || typeof message.requestId !== 'string' || typeof message.method !== 'string') return
      if (message.requestId.length < 1 || message.requestId.length > 128 || !/^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*$/.test(message.method)) {
        sendToPlugin({ type: 'error', requestId: message.requestId, error: { code: 'INVALID_REQUEST', message: 'Invalid plugin call.' } })
        return
      }
      const now = Date.now()
      activeCalls.current = activeCalls.current.filter((timestamp) => now - timestamp < 1_000)
      if (activeCalls.current.length >= MAX_CALLS_PER_SECOND) {
        sendToPlugin({ type: 'error', requestId: message.requestId, error: { code: 'RESOURCE_LIMIT', message: 'Plugin UI call limit exceeded.' } })
        return
      }
      activeCalls.current.push(now)
      void pluginApi.callWithId<unknown>(state.manifest.id, message.method, message.params ?? {}, message.requestId).promise
        .then((result) => sendToPlugin({ type: 'result', requestId: message.requestId, result }))
        .catch((cause) => sendToPlugin({ type: 'error', requestId: message.requestId, error: toErrorValue(cause) }))
    }
    window.addEventListener('message', onMessage)
    return () => window.removeEventListener('message', onMessage)
  }, [active, contribution.contributionId, followsTheme, onOpenView, providesSidebar, resolvedTheme, sendToPlugin, state.manifest.id])

  useEffect(() => {
    if (!ready) return
    sendToPlugin({ type: 'surface_lifecycle', state: active ? 'active' : 'inactive' })
  }, [active, ready, sendToPlugin])

  useEffect(() => {
    if (ready && followsTheme && subscribedTopics.current.has('host.theme')) {
      sendToPlugin({ type: 'host_context', context: { theme: { resolved: resolvedTheme } } })
    }
  }, [followsTheme, ready, resolvedTheme, sendToPlugin])

  useEffect(() => {
    let unlisten: (() => void) | undefined
    void listen<PluginEventPayload>('plugin:event', ({ payload }) => {
      if (payload.pluginId === state.manifest.id && subscribedTopics.current.has(payload.topic)) {
        sendToPlugin({ type: 'event', event: payload })
      }
    }).then((stop) => { unlisten = stop })
    return () => unlisten?.()
  }, [sendToPlugin, state.manifest.id])

  useEffect(() => {
    const onPluginUiEvent = (event: Event) => {
      const detail = (event as CustomEvent<{
        pluginId: string
        sourceContributionId: string
        topic: string
        payload: unknown
      }>).detail
      if (
        detail.pluginId === state.manifest.id
        && detail.sourceContributionId !== contribution.contributionId
        && subscribedTopics.current.has(detail.topic)
      ) {
        sendToPlugin({
          type: 'event',
          event: { pluginId: detail.pluginId, topic: detail.topic, requestId: null, payload: detail.payload },
        })
      }
    }
    window.addEventListener(PLUGIN_UI_EVENT, onPluginUiEvent)
    return () => window.removeEventListener(PLUGIN_UI_EVENT, onPluginUiEvent)
  }, [contribution.contributionId, sendToPlugin, state.manifest.id])

  const initializeFrame = () => {
    setReady(false)
    setBridgeError('')
    subscribedTopics.current.clear()
    frame.current?.contentWindow?.postMessage({
      protocol: BRIDGE_PROTOCOL,
      bridgeVersion: UI_BRIDGE_VERSION,
      pluginId: state.manifest.id,
      contributionId: contribution.contributionId,
      nonce: nonce.current,
      type: 'host_init',
      context: { lifecycle: active ? 'active' : 'inactive', surface },
    }, '*')
  }

  if (loaderFailure) return <p role="alert" className="rounded-xl bg-glass px-5 py-4 text-sm text-[var(--app-danger)]">{t('pluginPage.uiPending')} {loaderFailure}</p>
  if (!url) return <p role="status" className="rounded-xl bg-glass px-5 py-4 text-sm text-ink-muted">{t('pluginPage.uiPending')}</p>

  return (
    <div className="plugin-surface-frame" hidden={!active} aria-hidden={!active} data-active={active}>
      {!ready && <p role="status" className="border-b border-glass-line px-4 py-2 text-xs text-ink-faint">{t('pluginPage.bridgeStarting')}</p>}
      {bridgeError && <p role="alert" className="px-4 py-2 text-xs text-[var(--app-danger)]">{bridgeError}</p>}
      <iframe
        ref={frame}
        title={contribution.title}
        src={surfaceUrl(url, surface, contribution.contributionId)}
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
        onLoad={initializeFrame}
        onError={() => setBridgeError(t('pluginPage.frameFailed'))}
        className="min-h-[24rem] w-full flex-1 border-0 bg-transparent"
      />
    </div>
  )
}

function surfaceUrl(source: string, surface: 'activity' | 'view', contributionId: string) {
  const hashIndex = source.indexOf('#')
  const base = hashIndex < 0 ? source : source.slice(0, hashIndex)
  const hash = hashIndex < 0 ? '' : source.slice(hashIndex)
  const separator = base.includes('?') ? '&' : '?'
  return `${base}${separator}wonderlandSurface=${surface}&wonderlandContribution=${encodeURIComponent(contributionId)}${hash}`
}

function toErrorValue(cause: unknown) {
  if (typeof cause === 'object' && cause !== null && 'code' in cause && 'message' in cause) {
    return { code: String((cause as { code: unknown }).code), message: String((cause as { message: unknown }).message) }
  }
  return { code: 'INTERNAL', message: errorText(cause) }
}

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message: unknown }).message)
  }
  return cause instanceof Error ? cause.message : String(cause)
}
