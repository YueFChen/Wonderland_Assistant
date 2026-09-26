import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from 'react'
import { X } from 'lucide-react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { listen } from '@tauri-apps/api/event'
import { UI_BRIDGE_PROTOCOL as BRIDGE_PROTOCOL, UI_BRIDGE_VERSION } from '@wonderland/plugin-ui-sdk'
import { validatePluginUiBridgeMessage } from '../plugins/pluginUiBridgeValidation'
import {
  completePluginUiCommand,
  queuedPluginUiCommands,
  takeQueuedPluginUiCommand,
  type CliPluginUiCommand,
} from '../plugins/cliUiCommandBus'

import { t } from '../i18n'
import { pluginApi, usePlugins, type PluginEventPayload } from '../plugins/api'
import { buildContributionRegistry, findContribution } from '../plugins/contributions'
import type { WorkspaceContribution } from '../plugins/contributions'
import { MAX_OPEN_ACTIVITY_SURFACES, readWorkspaceLayout, setActiveSidebarContribution, useWorkspaceLayout } from '../plugins/workspaceLayout'
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
  commandId?: string
  input?: unknown
  result?: unknown
  error?: { code?: string; message?: string }
}

/** Core owns activity tabs and mounts each enabled contribution in its own sandboxed frame. */
export function PluginPage() {
  const { pluginId = '', contributionId = '' } = useParams()
  const navigate = useNavigate()
  const { states, error } = usePlugins()
  const { layout, update } = useWorkspaceLayout()
  const { resolved } = useTheme()
  const registry = useMemo(() => buildContributionRegistry(states), [states])
  const activityIds = useMemo(() => new Set(registry.filter((item) => item.kind === 'activity').map((item) => item.id)), [registry])
  const tabButtons = useRef(new Map<string, HTMLButtonElement>())
  const closingRouteTabId = useRef<string | null>(null)
  const activeId = `${pluginId}/${contributionId}`
  const activeContribution = findContribution(registry, activeId)

  useEffect(() => {
    if (closingRouteTabId.current && closingRouteTabId.current !== activeId) {
      closingRouteTabId.current = null
    }
    if (activeContribution?.kind === 'view' && activeContribution.status === 'ready') {
      if (layout.activeSidebarContributionId !== activeId) setActiveSidebarContribution(activeId)
      navigate('/workspace', { replace: true })
      return
    }
    if (!states) return

    const current = readWorkspaceLayout()
    const closingCurrentTab = closingRouteTabId.current === activeId
    const openIds = current.openContributionIds.filter((id) => activityIds.has(id))
    if (activeContribution?.kind === 'activity' && !closingCurrentTab && !openIds.includes(activeId)) {
      openIds.push(activeId)
    }
    const nextOpenIds = openIds.slice(-MAX_OPEN_ACTIVITY_SURFACES)
    const routeActiveId = activeContribution?.kind === 'activity' && !closingCurrentTab
      ? activeId
      : null
    const nextActiveId = routeActiveId
      ?? (current.activeContributionId && nextOpenIds.includes(current.activeContributionId)
        ? current.activeContributionId
        : null)
    const openIdsChanged = current.openContributionIds.length !== nextOpenIds.length
      || current.openContributionIds.some((id, index) => id !== nextOpenIds[index])
    if (openIdsChanged || current.activeContributionId !== nextActiveId) {
      update((state) => ({
        ...state,
        openContributionIds: nextOpenIds,
        activeContributionId: nextActiveId,
      }))
    }

    if (!activeContribution && pluginId && contributionId) {
      navigate('/workspace', {
        replace: true,
        state: { pluginPageRecovery: { pluginId, contributionId } },
      })
    }
  }, [activeContribution?.kind, activeContribution?.status, activeId, activityIds, contributionId, layout.activeContributionId, layout.activeSidebarContributionId, layout.openContributionIds, navigate, pluginId, states, update])

  const openTabs = useMemo(() => {
    const ids = layout.openContributionIds.filter((id) => activityIds.has(id))
    if (
      activeContribution?.kind === 'activity'
      && closingRouteTabId.current !== activeId
      && !ids.includes(activeId)
    ) ids.push(activeId)
    return ids
      .slice(-MAX_OPEN_ACTIVITY_SURFACES)
      .map((id) => findContribution(registry, id))
      .filter((item): item is WorkspaceContribution => item !== undefined && item.kind === 'activity')
  }, [activeContribution?.kind, activeId, activityIds, layout.openContributionIds, registry])

  const closeTab = useCallback((id: string) => {
    const routeActivityId = activeContribution?.kind === 'activity' ? activeId : null
    const closesRouteTab = id === routeActivityId
    const before = readWorkspaceLayout().openContributionIds.filter((item) => activityIds.has(item))
    if (routeActivityId && !before.includes(routeActivityId)) before.push(routeActivityId)
    if (!before.includes(id)) return

    if (closesRouteTab) closingRouteTabId.current = id
    let nextActive: WorkspaceContribution | undefined
    let focusTabId: string | null = null
    update((state) => {
      const orderedIds = state.openContributionIds.filter((item) => activityIds.has(item))
      if (routeActivityId && !orderedIds.includes(routeActivityId)) orderedIds.push(routeActivityId)
      const visibleIds = orderedIds.slice(-MAX_OPEN_ACTIVITY_SURFACES)
      const closedIndex = visibleIds.indexOf(id)
      if (closedIndex < 0) return state

      const remainingIds = visibleIds.filter((item) => item !== id)
      const fallbackIds = [
        ...visibleIds.slice(closedIndex + 1),
        ...visibleIds.slice(0, closedIndex).reverse(),
      ].filter((item) => item !== id)
      nextActive = fallbackIds
        .map((item) => findContribution(registry, item))
        .find((item): item is WorkspaceContribution => item?.kind === 'activity')

      const routeStillOpen = routeActivityId !== null
        && routeActivityId !== id
        && remainingIds.includes(routeActivityId)
      const storedActiveStillOpen = state.activeContributionId !== null
        && state.activeContributionId !== id
        && remainingIds.includes(state.activeContributionId)
      const activeContributionId = closesRouteTab
        ? nextActive?.id ?? null
        : routeStillOpen
          ? routeActivityId
          : storedActiveStillOpen
            ? state.activeContributionId
            : nextActive?.id ?? null
      focusTabId = closesRouteTab
        ? nextActive?.id ?? null
        : fallbackIds.find((item) => remainingIds.includes(item)) ?? activeContributionId

      return {
        ...state,
        openContributionIds: remainingIds,
        activeContributionId,
      }
    })

    if (closesRouteTab) {
      navigate(nextActive?.href ?? '/workspace', { replace: true })
    }
    const nextFocusId = focusTabId
    if (nextFocusId) {
      requestAnimationFrame(() => tabButtons.current.get(nextFocusId)?.focus())
    }
  }, [activeContribution?.kind, activeId, activityIds, navigate, registry, update])

  const selectTab = useCallback((item: WorkspaceContribution) => {
    update((state) => {
      const openContributionIds = state.openContributionIds.includes(item.id)
        ? state.openContributionIds
        : [...state.openContributionIds, item.id].slice(-MAX_OPEN_ACTIVITY_SURFACES)
      return state.activeContributionId === item.id
        && openContributionIds === state.openContributionIds
        ? state
        : { ...state, openContributionIds, activeContributionId: item.id }
    })
    navigate(item.href, { replace: true })
  }, [navigate, update])

  const handleTabKeyDown = useCallback((event: ReactKeyboardEvent<HTMLButtonElement>, item: WorkspaceContribution) => {
    if (event.key === 'Delete') {
      event.preventDefault()
      closeTab(item.id)
      return
    }

    const index = openTabs.findIndex((tab) => tab.id === item.id)
    if (index < 0 || openTabs.length < 2) return

    let targetIndex: number | null = null
    if (event.key === 'ArrowRight') targetIndex = (index + 1) % openTabs.length
    if (event.key === 'ArrowLeft') targetIndex = (index - 1 + openTabs.length) % openTabs.length
    if (event.key === 'Home') targetIndex = 0
    if (event.key === 'End') targetIndex = openTabs.length - 1
    if (targetIndex === null) return

    event.preventDefault()
    const target = openTabs[targetIndex]
    selectTab(target)
    requestAnimationFrame(() => tabButtons.current.get(target.id)?.focus())
  }, [closeTab, openTabs, selectTab])

  if (error) return <p role="alert" className="text-sm text-[var(--app-danger)]">{t(error)}</p>
  if (!states) return <p role="status" className="text-sm text-ink-muted">{t('pluginPage.loading')}</p>
  if (!activeContribution) return <p className="text-sm text-ink-muted">{t('pluginPage.notInstalled')}</p>
  if (activeContribution.kind !== 'activity') {
    return activeContribution.status === 'ready'
      ? null
      : <ContributionUnavailable contribution={activeContribution} />
  }

  return (
    <section className="plugin-page-surface flex h-full min-h-[28rem] min-w-0 flex-col">
      <div className="plugin-surface-tabs" role="tablist" aria-orientation="horizontal" aria-label={t('pluginPage.tabs')}>
        {openTabs.map((item) => (
          <div key={item.id} role="presentation" className={`plugin-surface-tab${item.id === activeId ? ' is-active' : ''}`}>
            <button
              ref={(node) => {
                if (node) tabButtons.current.set(item.id, node)
                else tabButtons.current.delete(item.id)
              }}
              type="button"
              role="tab"
              aria-selected={item.id === activeId}
              tabIndex={item.id === activeId ? 0 : -1}
              onClick={() => selectTab(item)}
              onKeyDown={(event) => handleTabKeyDown(event, item)}
              title={item.title}
            >
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
          item.status !== 'ready'
            ? item.id === activeId && <ContributionUnavailable key={item.id} contribution={item} />
            : <PluginSurface
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
  const pendingCalls = useRef(new Set<string>())
  const pendingCliCommands = useRef(new Map<string, CliPluginUiCommand>())
  const sentCliCommands = useRef(new Set<string>())
  const subscribedTopics = useRef(new Set<string>())
  const handshakeTimer = useRef<number | null>(null)
  const [url, setUrl] = useState('')
  const [loaderFailure, setLoaderFailure] = useState('')
  const [ready, setReady] = useState(false)
  const [bridgeError, setBridgeError] = useState('')
  const [loadGeneration, setLoadGeneration] = useState(0)
  const [frameGeneration, setFrameGeneration] = useState(0)
  const followsTheme = contribution.integrations.includes('theme.followHost')
  const providesSidebar = contribution.integrations.includes('workspace.sidebar')

  useEffect(() => () => {
    for (const requestId of pendingCalls.current) {
      void pluginApi.cancel(state.manifest.id, requestId).catch(() => undefined)
    }
    pendingCalls.current.clear()
  }, [state.manifest.id])

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

  const deliverCliCommand = useCallback((command: CliPluginUiCommand) => {
    if (!active || !ready || sentCliCommands.current.has(command.requestId)) return
    sentCliCommands.current.add(command.requestId)
    sendToPlugin({
      type: 'command',
      requestId: command.requestId,
      commandId: command.commandId,
      input: command.input,
    })
  }, [active, ready, sendToPlugin])

  useEffect(() => {
    const receiveCommand = (event: Event) => {
      const command = (event as CustomEvent<CliPluginUiCommand>).detail
      if (!command || command.contributionId !== `${state.manifest.id}/${contribution.contributionId}` || !active) return
      const queuedCommand = takeQueuedPluginUiCommand(command.requestId)
      if (!queuedCommand) return
      pendingCliCommands.current.set(command.requestId, queuedCommand)
      deliverCliCommand(queuedCommand)
    }
    window.addEventListener('wonderland:cli-ui-command', receiveCommand)
    if (active) {
      for (const command of queuedPluginUiCommands(`${state.manifest.id}/${contribution.contributionId}`)) {
        const queuedCommand = takeQueuedPluginUiCommand(command.requestId)
        if (!queuedCommand) continue
        pendingCliCommands.current.set(command.requestId, queuedCommand)
        deliverCliCommand(queuedCommand)
      }
    }
    return () => window.removeEventListener('wonderland:cli-ui-command', receiveCommand)
  }, [active, contribution.contributionId, deliverCliCommand, state.manifest.id])

  useEffect(() => {
    if (!active || !ready) return
    for (const command of pendingCliCommands.current.values()) deliverCliCommand(command)
  }, [active, deliverCliCommand, ready])

  useEffect(() => () => {
    for (const requestId of pendingCliCommands.current.keys()) {
      completePluginUiCommand(requestId, null, {
        code: 'CONTRIBUTION_CLOSED',
        message: 'The plugin UI closed before completing the command.',
      })
    }
    pendingCliCommands.current.clear()
    sentCliCommands.current.clear()
  }, [])

  useEffect(() => {
    let live = true
    setUrl('')
    setLoaderFailure('')
    setReady(false)
    void pluginApi.uiUrl(state.manifest.id)
      .then((next) => { if (live) setUrl(next) })
      .catch((cause) => { if (live) setLoaderFailure(errorText(cause)) })
    return () => { live = false }
  }, [loadGeneration, state.manifest.id])

  useEffect(() => () => {
    if (handshakeTimer.current !== null) window.clearTimeout(handshakeTimer.current)
  }, [])

  useEffect(() => {
    const onMessage = (event: MessageEvent<unknown>) => {
      const frameWindow = frame.current?.contentWindow
      if (!frameWindow || event.source !== frameWindow || event.origin !== 'null') return
      const validation = validatePluginUiBridgeMessage(event.data, {
        protocol: BRIDGE_PROTOCOL,
        bridgeVersion: UI_BRIDGE_VERSION,
        pluginId: state.manifest.id,
        contributionId: contribution.contributionId,
        nonce: nonce.current,
      })
      if (validation === 'ignore') return
      const message = event.data as Partial<PluginUiMessage>

      if (validation === 'bridge-version-mismatch') {
        if (handshakeTimer.current !== null) {
          window.clearTimeout(handshakeTimer.current)
          handshakeTimer.current = null
        }
        setReady(false)
        setBridgeError(t('pluginPage.bridgeMismatch'))
        return
      }

      if (message.type === 'ready') {
        if (handshakeTimer.current !== null) {
          window.clearTimeout(handshakeTimer.current)
          handshakeTimer.current = null
        }
        setReady(true)
        setBridgeError('')
        sendToPlugin({ type: 'surface_lifecycle', state: active ? 'active' : 'inactive' })
        return
      }
      if (
        (message.type === 'command_result' || message.type === 'command_error')
        && typeof message.requestId === 'string'
        && pendingCliCommands.current.has(message.requestId)
      ) {
        pendingCliCommands.current.delete(message.requestId)
        sentCliCommands.current.delete(message.requestId)
        if (message.type === 'command_error') {
          completePluginUiCommand(message.requestId, null, message.error)
        } else {
          let resultBytes = 0
          try {
            resultBytes = new TextEncoder().encode(JSON.stringify(message.result ?? null)).byteLength
          } catch {
            resultBytes = Number.POSITIVE_INFINITY
          }
          if (resultBytes > 64 * 1024) {
            completePluginUiCommand(message.requestId, null, {
              code: 'RESOURCE_LIMIT',
              message: 'Plugin UI command results cannot exceed 64 KiB.',
            })
          } else {
            completePluginUiCommand(message.requestId, message.result)
          }
        }
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
      const request = pluginApi.callWithId<unknown>(state.manifest.id, message.method, message.params ?? {}, message.requestId)
      pendingCalls.current.add(request.requestId)
      void request.promise
        .then((result) => sendToPlugin({ type: 'result', requestId: message.requestId, result }))
        .catch((cause) => sendToPlugin({ type: 'error', requestId: message.requestId, error: toErrorValue(cause) }))
        .finally(() => pendingCalls.current.delete(request.requestId))
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
    if (handshakeTimer.current !== null) window.clearTimeout(handshakeTimer.current)
    handshakeTimer.current = window.setTimeout(() => {
      handshakeTimer.current = null
      setBridgeError(t('pluginPage.bridgeTimeout'))
    }, 10_000)
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

  const retry = () => {
    if (handshakeTimer.current !== null) {
      window.clearTimeout(handshakeTimer.current)
      handshakeTimer.current = null
    }
    setReady(false)
    setBridgeError('')
    if (loaderFailure) {
      setUrl('')
      setLoaderFailure('')
      setLoadGeneration((value) => value + 1)
    } else {
      setFrameGeneration((value) => value + 1)
    }
  }

  if (loaderFailure) {
    return (
      <section role="alert" className="mx-auto mt-8 w-full max-w-xl rounded-xl bg-glass px-5 py-4">
        <p className="text-sm text-[var(--app-danger)]">{t('pluginPage.frameFailed')} {loaderFailure}</p>
        <div className="mt-4 flex flex-wrap gap-2">
          <button type="button" onClick={retry} className="rounded-lg bg-brand-600 px-4 py-2 text-sm font-semibold text-white hover:bg-brand-500">{t('pluginPage.retry')}</button>
          <Link to="/workspace/settings" className="rounded-lg border border-glass-line px-4 py-2 text-sm text-ink hover:bg-glass-hover">{t('workspace.goToSettings')}</Link>
        </div>
      </section>
    )
  }
  if (!url) return <p role="status" className="rounded-xl bg-glass px-5 py-4 text-sm text-ink-muted">{t('pluginPage.uiPending')}</p>

  return (
    <div className="plugin-surface-frame" hidden={!active} aria-hidden={!active} data-active={active}>
      {!ready && <p role="status" className="border-b border-glass-line px-4 py-2 text-xs text-ink-faint">{t('pluginPage.bridgeStarting')}</p>}
      {bridgeError && (
        <div role="alert" className="flex flex-wrap items-center gap-2 border-b border-glass-line px-4 py-2 text-xs text-[var(--app-danger)]">
          <span className="flex-1">{bridgeError}</span>
          <button type="button" onClick={retry} className="rounded-md border border-glass-line px-2.5 py-1 text-ink hover:bg-glass-hover">{t('pluginPage.retry')}</button>
          <Link to="/workspace/settings" className="rounded-md border border-glass-line px-2.5 py-1 text-ink hover:bg-glass-hover">{t('workspace.goToSettings')}</Link>
        </div>
      )}
      <iframe
        ref={frame}
        key={frameGeneration}
        title={contribution.title}
        src={surfaceUrl(url, surface, contribution.contributionId)}
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
        onLoad={initializeFrame}
        onError={() => {
          if (handshakeTimer.current !== null) window.clearTimeout(handshakeTimer.current)
          handshakeTimer.current = null
          setBridgeError(t('pluginPage.frameFailed'))
        }}
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
