import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { listen, isWebClient } from '../core/transport'
import { UI_BRIDGE_PROTOCOL as BRIDGE_PROTOCOL, UI_BRIDGE_VERSION, createRequestId } from '@wonderland/plugin-ui-sdk'
import { validatePluginUiBridgeMessage } from '../plugins/pluginUiBridgeValidation'
import { runUiPreferenceMethod } from '../plugins/uiPreferences'
import { validateUiTimeout } from '../plugins/uiDeadline'
import { PluginFrameSession } from '../plugins/pluginFrameSession'
import {
  completePluginUiCommand,
  queuedPluginUiCommands,
  takeQueuedPluginUiCommand,
  type CliPluginUiCommand,
} from '../plugins/cliUiCommandBus'

import { t } from '../i18n'
import { pluginApi, usePlugins, type PluginEventPayload, type PluginUiLaunchInfo } from '../plugins/api'
import { buildContributionRegistry, canOpenContribution, findContribution } from '../plugins/contributions'
import type { WorkspaceContribution } from '../plugins/contributions'
import { MAX_OPEN_ACTIVITY_SURFACES, readWorkspaceLayout, setActiveSidebarContribution, useWorkspaceLayout } from '../plugins/workspaceLayout'
import { closeWorkspaceActivity, reduceWorkspaceLayout } from '../plugins/workspaceActions'
import { PluginTabs } from '../components/PluginTabs'
import { pluginTabId, pluginPanelId } from '../components/pluginTabIds'
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
  timeoutMs?: number
}

/** Core owns activity tabs and mounts each enabled contribution in its own sandboxed frame. */
export function PluginPage() {
  const { pluginId = '', contributionId = '' } = useParams()
  const navigate = useNavigate()
  const { states, error, setStates } = usePlugins()
  const { layout, update } = useWorkspaceLayout()
  const { resolved, componentVariables } = useTheme()
  const [enablingPluginId, setEnablingPluginId] = useState<string | null>(null)
  const [enableFailure, setEnableFailure] = useState('')
  const [mountedIds, setMountedIds] = useState<Set<string>>(() => new Set())
  const registry = useMemo(() => buildContributionRegistry(states), [states])
  const activityIds = useMemo(() => new Set(registry.filter((item) => item.kind === 'activity').map((item) => item.id)), [registry])
  const tabButtons = useRef(new Map<string, HTMLButtonElement>())
  const closingRouteTabId = useRef<string | null>(null)
  const activeId = `${pluginId}/${contributionId}`
  const activeContribution = findContribution(registry, activeId)

  const enablePlugin = useCallback(async (targetPluginId: string) => {
    setEnablingPluginId(targetPluginId)
    setEnableFailure('')
    try {
      setStates(await pluginApi.setEnabled(targetPluginId, true))
    } catch (cause) {
      setEnableFailure(errorText(cause))
    } finally {
      setEnablingPluginId(null)
    }
  }, [setStates])

  useEffect(() => {
    if (closingRouteTabId.current && closingRouteTabId.current !== activeId) {
      closingRouteTabId.current = null
    }
    if (activeContribution?.kind === 'view' && canOpenContribution(activeContribution)) {
      if (layout.activeSidebarContributionId !== activeId) setActiveSidebarContribution(activeId)
      navigate('/workspace', { replace: true })
      return
    }
    if (!states) return

    const closingCurrentTab = closingRouteTabId.current === activeId
    const routeActiveId = activeContribution?.kind === 'activity' && !closingCurrentTab
      ? activeId
      : null
    const current = readWorkspaceLayout()
    const next = reduceWorkspaceLayout(current, { type: 'syncActivities', availableIds: [...activityIds], activeId: routeActiveId })
    if (next !== current) update(() => next)

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

  useEffect(() => {
    const open = new Set(openTabs.map(item => item.id))
    setMountedIds(previous => {
      const next = new Set([...previous].filter(id => open.has(id)))
      if (open.has(activeId)) next.add(activeId)
      return next.size === previous.size && [...next].every(id => previous.has(id)) ? previous : next
    })
  }, [activeId, openTabs])

  const closeTab = useCallback((id: string) => {
    const routeActivityId = activeContribution?.kind === 'activity' ? activeId : null
    const result = closeWorkspaceActivity(readWorkspaceLayout(), id, routeActivityId, [...activityIds])
    if (!result.closed) return
    if (result.routeChanged) closingRouteTabId.current = id
    update(() => result.layout)
    if (result.routeChanged) {
      const nextActive = result.layout.activeContributionId ? findContribution(registry, result.layout.activeContributionId) : undefined
      navigate(nextActive?.href ?? '/workspace', {
        replace: true,
        state: nextActive ? null : { focusWorkspaceHeading: true },
      })
    }
    const nextFocusId = result.focusId
    if (nextFocusId) {
      requestAnimationFrame(() => tabButtons.current.get(nextFocusId)?.focus({ preventScroll: !result.routeChanged }))
    }
  }, [activeContribution?.kind, activeId, activityIds, navigate, registry, update])

  const selectTab = useCallback((item: WorkspaceContribution) => {
    update((state) => reduceWorkspaceLayout(state, { type: 'openActivity', id: item.id }))
    navigate(item.href, { replace: true })
  }, [navigate, update])

  const reorderTabs = useCallback((id: string, targetId: string) => {
    update(current => reduceWorkspaceLayout(current, { type: 'reorderTabs', id, targetId }))
  }, [update])
  // Keep frame DOM order independent of the visible tab order: moving an iframe can reload it.
  const surfaces = [...openTabs].sort((left, right) => left.id.localeCompare(right.id))

  if (error) return <p role="alert" className="text-sm text-[var(--app-danger)]">{t(error)}</p>
  if (!states) return <p role="status" className="text-sm text-ink-muted">{t('pluginPage.loading')}</p>
  if (!activeContribution) return <p className="text-sm text-ink-muted">{t('pluginPage.notInstalled')}</p>
  if (activeContribution.kind !== 'activity') {
    return canOpenContribution(activeContribution)
      ? null
      : <ContributionUnavailable
        contribution={activeContribution}
        onEnable={() => void enablePlugin(activeContribution.pluginId)}
        enabling={enablingPluginId === activeContribution.pluginId}
        enableFailure={enableFailure}
      />
  }

  return (
    <section className="plugin-page-surface flex h-full min-h-[28rem] min-w-0 flex-col">
      <PluginTabs tabs={openTabs} activeId={activeId} buttons={tabButtons} onSelect={selectTab} onClose={closeTab} onReorder={reorderTabs} />
      <div className="relative min-h-0 flex-1">
        {surfaces.map((item) => (
          <div key={item.id} className="plugin-tab-panel" role="tabpanel" id={pluginPanelId(item.id)} aria-labelledby={pluginTabId(item.id)} hidden={item.id !== activeId} tabIndex={0}>
          {!canOpenContribution(item)
            ? item.id === activeId && <ContributionUnavailable
              key={item.id}
              contribution={item}
              onEnable={() => void enablePlugin(item.pluginId)}
              enabling={enablingPluginId === item.pluginId}
              enableFailure={enableFailure}
            />
            : (item.id === activeId || mountedIds.has(item.id)) && <PluginSurface
              key={item.id}
              contribution={item}
              active={item.id === activeId}
              resolvedTheme={resolved}
              themeVariables={componentVariables}
              surface="activity"
              onOpenView={setActiveSidebarContribution}
            />}
          </div>
        ))}
      </div>
    </section>
  )
}

function ContributionUnavailable({ contribution, onEnable, enabling, enableFailure }: {
  contribution: WorkspaceContribution
  onEnable: () => void
  enabling: boolean
  enableFailure: string
}) {
  const message = contribution.status === 'invalid'
    ? contribution.state.lastError?.message ?? t('pluginPage.invalid')
    : contribution.status === 'incompatible'
      ? t('pluginPage.incompatible')
      : contribution.status === 'disabled'
        ? t('pluginPage.disabled')
      : contribution.status === 'failed'
          ? t('pluginPage.failed', {
            error: contribution.state.lastError?.message
              ?? contribution.state.dependencyIssues?.join(' ')
              ?? '',
          })
          : t(`settings.plugins.runtime.${contribution.state.runtime}`)
  return (
    <section className="glass-card mx-auto mt-10 w-full max-w-xl rounded-2xl p-7">
      {contribution.state.manifest.name !== contribution.title && <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-ink-faint">{contribution.state.manifest.name}</p>}
      <h1 className="text-xl font-bold text-ink">{contribution.title}</h1>
      <p role={contribution.status === 'failed' || contribution.status === 'invalid' || contribution.status === 'incompatible' ? 'alert' : 'status'} className="mt-3 text-sm leading-6 text-ink-muted">{message}</p>
      {contribution.status === 'disabled' ? (
        <>
          <button type="button" disabled={enabling} onClick={onEnable} className="app-button app-button-primary mt-5 inline-flex">
            {enabling ? t('pluginPage.enabling') : t('pluginPage.enable')}
          </button>
          {enableFailure && <p role="alert" className="mt-3 text-sm text-[var(--app-danger)]">{t('pluginPage.enableFailed', { error: enableFailure })}</p>}
        </>
      ) : (
        <Link to="/workspace/settings" className="app-button app-button-primary mt-5">{t('workspace.goToSettings')}</Link>
      )}
    </section>
  )
}

export function PluginSurface({
  contribution,
  active,
  resolvedTheme,
  themeVariables,
  surface,
  onOpenView,
}: {
  contribution: WorkspaceContribution
  active: boolean
  resolvedTheme: ResolvedTheme
  themeVariables?: Record<string, string>
  surface: 'activity' | 'view'
  onOpenView: (id: string | null) => void
}) {
  const { state } = contribution
  const frame = useRef<HTMLIFrameElement>(null)
  const lastFrameWindow = useRef<Window | null>(null)
  const hasInitializedFrame = useRef(false)
  const frameSession = useRef(new PluginFrameSession())
  const nonce = useRef(createRequestId())
  const activeCalls = useRef<number[]>([])
  const pendingCalls = useRef(new Map<string, string>())
  const pendingCliCommands = useRef(new Map<string, CliPluginUiCommand>())
  const sentCliCommands = useRef(new Set<string>())
  const subscribedTopics = useRef(new Set<string>())
  const handshakeTimer = useRef<number | null>(null)
  const negotiatedBridgeVersion = useRef<string | null>(null)
  const [launchInfo, setLaunchInfo] = useState<PluginUiLaunchInfo | null>(null)
  const url = launchInfo?.url ?? ''
  const [loaderFailure, setLoaderFailure] = useState('')
  const [backendRetrying, setBackendRetrying] = useState(false)
  const [backendFailure, setBackendFailure] = useState('')
  const backendRetryRequest = useRef<string | null>(null)
  const [ready, setReady] = useState(false)
  const [bridgeError, setBridgeError] = useState('')
  const [loadGeneration, setLoadGeneration] = useState(0)
  const [frameGeneration, setFrameGeneration] = useState(0)
  const followsTheme = contribution.integrations.includes('theme.followHost')
  const providesSidebar = contribution.integrations.includes('workspace.sidebar')

  useEffect(() => () => {
    if (backendRetryRequest.current) void pluginApi.cancel(state.manifest.id, backendRetryRequest.current).catch(() => undefined)
    const surfaceWindow = frame.current?.contentWindow ?? lastFrameWindow.current
    for (const requestId of pendingCalls.current.keys()) if (frameSession.current.accepts(surfaceWindow)) surfaceWindow?.postMessage({
      protocol: BRIDGE_PROTOCOL, bridgeVersion: negotiatedBridgeVersion.current ?? UI_BRIDGE_VERSION,
      pluginId: state.manifest.id, contributionId: contribution.contributionId, nonce: nonce.current,
      type: 'error', requestId, error: { code: 'CONTRIBUTION_CLOSED', message: 'The Core UI surface closed.' },
    }, '*')
    if (frameSession.current.accepts(surfaceWindow)) surfaceWindow?.postMessage({
      protocol: BRIDGE_PROTOCOL, bridgeVersion: negotiatedBridgeVersion.current ?? UI_BRIDGE_VERSION,
      pluginId: state.manifest.id, contributionId: contribution.contributionId, nonce: nonce.current, type: 'host_close',
    }, '*')
    for (const requestId of pendingCalls.current.keys()) {
      void pluginApi.cancel(state.manifest.id, requestId).catch(() => undefined)
    }
    pendingCalls.current.clear()
  }, [contribution.contributionId, state.manifest.id])

  const sendToPlugin = useCallback((message: Record<string, unknown>) => {
    if (!frameSession.current.accepts(frame.current?.contentWindow ?? null)) return
    frame.current?.contentWindow?.postMessage({
      protocol: BRIDGE_PROTOCOL,
      bridgeVersion: negotiatedBridgeVersion.current ?? launchInfo?.bridgeVersions[0] ?? UI_BRIDGE_VERSION,
      pluginId: state.manifest.id,
      contributionId: contribution.contributionId,
      nonce: nonce.current,
      ...message,
    }, '*')
  }, [contribution.contributionId, launchInfo, state.manifest.id])

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
    const cancelCommand = (event: Event) => {
      const requestId = (event as CustomEvent<{ requestId: string }>).detail?.requestId
      if (!requestId || !pendingCliCommands.current.has(requestId)) return
      pendingCliCommands.current.delete(requestId)
      sentCliCommands.current.delete(requestId)
      sendToPlugin({ type: 'command_cancel', requestId })
    }
    window.addEventListener('wonderland:cli-ui-command', receiveCommand)
    window.addEventListener('wonderland:cli-ui-cancel', cancelCommand)
    if (active) {
      for (const command of queuedPluginUiCommands(`${state.manifest.id}/${contribution.contributionId}`)) {
        const queuedCommand = takeQueuedPluginUiCommand(command.requestId)
        if (!queuedCommand) continue
        pendingCliCommands.current.set(command.requestId, queuedCommand)
        deliverCliCommand(queuedCommand)
      }
    }
    return () => {
      window.removeEventListener('wonderland:cli-ui-command', receiveCommand)
      window.removeEventListener('wonderland:cli-ui-cancel', cancelCommand)
    }
  }, [active, contribution.contributionId, deliverCliCommand, sendToPlugin, state.manifest.id])

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
    const failure = bridgeError || loaderFailure
    if (!failure) return
    const ids = new Set([
      ...pendingCliCommands.current.keys(),
      ...queuedPluginUiCommands(`${state.manifest.id}/${contribution.contributionId}`).map((item) => item.requestId),
    ])
    for (const requestId of ids) {
      sendToPlugin({ type: 'command_cancel', requestId })
      completePluginUiCommand(requestId, null, { code: 'UI_BRIDGE_FAILED', message: failure })
    }
    pendingCliCommands.current.clear()
    sentCliCommands.current.clear()
  }, [bridgeError, contribution.contributionId, loaderFailure, sendToPlugin, state.manifest.id])

  useEffect(() => {
    let live = true
    setLaunchInfo(null)
    setLoaderFailure('')
    setReady(false)
    const requestId = createRequestId()
    void pluginApi.uiUrl(state.manifest.id, requestId, loadGeneration > 0)
      .then((next) => { if (live) setLaunchInfo(next) })
      .catch((cause) => { if (live) setLoaderFailure(errorText(cause)) })
    return () => { live = false; void pluginApi.cancel(state.manifest.id, requestId).catch(() => undefined) }
  }, [loadGeneration, state.manifest.id, state.manifest.version, state.lifecycle?.packageRevision])

  useEffect(() => () => {
    if (handshakeTimer.current !== null) window.clearTimeout(handshakeTimer.current)
  }, [])

  useEffect(() => {
    const onMessage = (event: MessageEvent<unknown>) => {
      const frameWindow = frame.current?.contentWindow
      if (!frameWindow || !frameSession.current.accepts(frameWindow) || event.source !== frameWindow || event.origin !== 'null') return
      const validation = validatePluginUiBridgeMessage(event.data, {
        protocol: BRIDGE_PROTOCOL,
        bridgeVersion: negotiatedBridgeVersion.current ?? launchInfo?.bridgeVersions[0] ?? UI_BRIDGE_VERSION,
        pluginId: state.manifest.id,
        contributionId: contribution.contributionId,
        nonce: nonce.current,
      }, negotiatedBridgeVersion.current ? [] : launchInfo?.bridgeVersions ?? [])
      if (validation === 'ignore') return
      const message = event.data as Partial<PluginUiMessage>

      if (validation === 'bridge-version-mismatch') {
        if (
          message.type === 'ready'
          && negotiatedBridgeVersion.current !== null
          && typeof message.bridgeVersion === 'string'
          && launchInfo?.bridgeVersions.includes(message.bridgeVersion)
        ) {
          // A multi-version plugin may answer more than one init candidate; keep the first choice.
          return
        }
        if (handshakeTimer.current !== null) {
          window.clearTimeout(handshakeTimer.current)
          handshakeTimer.current = null
        }
        setReady(false)
        setBridgeError(t('pluginPage.bridgeMismatch'))
        return
      }

      if (message.type === 'ready') {
        if (
          typeof message.bridgeVersion !== 'string'
          || !launchInfo?.bridgeVersions.includes(message.bridgeVersion)
          || (negotiatedBridgeVersion.current !== null && negotiatedBridgeVersion.current !== message.bridgeVersion)
        ) {
          if (handshakeTimer.current !== null) {
            window.clearTimeout(handshakeTimer.current)
            handshakeTimer.current = null
          }
          setReady(false)
          setBridgeError(t('pluginPage.bridgeMismatch'))
          return
        }
        negotiatedBridgeVersion.current = message.bridgeVersion
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
          sendToPlugin({ type: 'host_context', context: { theme: { resolved: resolvedTheme, variables: themeVariables } } })
        } else if (message.topic === 'host.lifecycle') {
          subscribedTopics.current.add(message.topic)
          sendToPlugin({ type: 'surface_lifecycle', state: active ? 'active' : 'inactive' })
        } else if (!message.topic.startsWith('host.')) {
          subscribedTopics.current.add(message.topic)
        }
        return
      }
      if (message.type === 'cancel' && typeof message.requestId === 'string') {
        if (pendingCalls.current.get(message.requestId) === nonce.current) {
          void pluginApi.cancel(state.manifest.id, message.requestId).catch(() => undefined)
        }
        return
      }
      if (message.type !== 'call' || typeof message.requestId !== 'string' || typeof message.method !== 'string') return
      if (message.requestId.length < 1 || message.requestId.length > 128 || !/^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*$/.test(message.method)) {
        sendToPlugin({ type: 'error', requestId: message.requestId, error: { code: 'INVALID_REQUEST', message: 'Invalid plugin call.' } })
        return
      }
      if (pendingCalls.current.has(message.requestId)) {
        sendToPlugin({ type: 'error', requestId: message.requestId, error: { code: 'INVALID_REQUEST', message: 'Request ID is already active in this surface.' } })
        return
      }
      const now = Date.now()
      activeCalls.current = activeCalls.current.filter((timestamp) => now - timestamp < 1_000)
      if (activeCalls.current.length >= MAX_CALLS_PER_SECOND) {
        sendToPlugin({ type: 'error', requestId: message.requestId, error: { code: 'RESOURCE_LIMIT', message: 'Plugin UI call limit exceeded.' } })
        return
      }
      activeCalls.current.push(now)
      let timeoutMs: number | undefined
      try { timeoutMs = validateUiTimeout(message.timeoutMs) }
      catch {
        sendToPlugin({ type: 'error', requestId: message.requestId, error: { code: 'INVALID_INPUT', message: 'timeoutMs must be a positive safe integer.' } })
        return
      }
      if (message.method.startsWith('core.ui.preferences.')) {
        try {
          const result = runUiPreferenceMethod(state.manifest.id, message.method, message.params ?? {}, window.localStorage)
          sendToPlugin({ type: 'result', requestId: message.requestId, result })
        } catch (cause) {
          sendToPlugin({ type: 'error', requestId: message.requestId, error: cause instanceof Error && 'code' in cause
            ? { code: String(cause.code), message: cause.message }
            : { code: 'UI_STORAGE_UNAVAILABLE', message: 'This device could not read or save the UI preference.' } })
        }
        return
      }
      const request = pluginApi.callWithId<unknown>(state.manifest.id, message.method, message.params ?? {}, message.requestId, timeoutMs)
      const callNonce = nonce.current
      const callWindow = frameWindow
      const currentFrame = () => nonce.current === callNonce && frame.current?.contentWindow === callWindow
      pendingCalls.current.set(request.requestId, callNonce)
      void request.promise
        .then((result) => { if (currentFrame()) sendToPlugin({ type: 'result', requestId: message.requestId, result }) })
        .catch((cause) => { if (currentFrame()) sendToPlugin({ type: 'error', requestId: message.requestId, error: toErrorValue(cause) }) })
        .finally(() => { if (pendingCalls.current.get(request.requestId) === callNonce) pendingCalls.current.delete(request.requestId) })
    }
    window.addEventListener('message', onMessage)
    return () => window.removeEventListener('message', onMessage)
  }, [active, contribution.contributionId, followsTheme, launchInfo, onOpenView, providesSidebar, resolvedTheme, themeVariables, sendToPlugin, state.manifest.id])

  useEffect(() => {
    if (!ready) return
    sendToPlugin({ type: 'surface_lifecycle', state: active ? 'active' : 'inactive' })
  }, [active, ready, sendToPlugin])

  useEffect(() => {
    if (ready && followsTheme && subscribedTopics.current.has('host.theme')) {
      sendToPlugin({ type: 'host_context', context: { theme: { resolved: resolvedTheme, variables: themeVariables } } })
    }
  }, [followsTheme, ready, resolvedTheme, themeVariables, sendToPlugin])

  useEffect(() => {
    let live = true
    let unlisten: (() => void) | undefined
    void listen<PluginEventPayload>('plugin:event', ({ payload }) => {
      if (payload.pluginId === state.manifest.id && subscribedTopics.current.has(payload.topic)) {
        sendToPlugin({ type: 'event', event: payload })
      }
    }).then((stop) => { if (live) unlisten = stop; else stop() })
    return () => { live = false; unlisten?.() }
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
    lastFrameWindow.current = frame.current?.contentWindow ?? null
    const firstDocument = frameSession.current.beginLoad(lastFrameWindow.current)
    for (const requestId of pendingCalls.current.keys()) {
      void pluginApi.cancel(state.manifest.id, requestId).catch(() => undefined)
    }
    pendingCalls.current.clear()
    if (hasInitializedFrame.current) {
      for (const requestId of pendingCliCommands.current.keys()) completePluginUiCommand(requestId, null, {
        code: 'SURFACE_RELOADED', message: 'The plugin UI surface reloaded before completing the command.',
      })
      pendingCliCommands.current.clear()
      sentCliCommands.current.clear()
    }
    hasInitializedFrame.current = true
    setReady(false)
    setBridgeError('')
    negotiatedBridgeVersion.current = null
    // A fresh nonce binds each iframe load to its own handshake session.
    nonce.current = createRequestId()
    subscribedTopics.current.clear()
    if (handshakeTimer.current !== null) window.clearTimeout(handshakeTimer.current)
    handshakeTimer.current = null
    // Forms may navigate this frame. Its WindowProxy survives navigation, so a new nonce
    // must never grant the next document access to the plugin's backend or preferences.
    if (!firstDocument) {
      setBridgeError(t('pluginPage.frameNavigated'))
      return
    }
    const bridgeVersions = launchInfo?.bridgeVersions ?? []
    if (bridgeVersions.length === 0) {
      setBridgeError(t('pluginPage.bridgeMismatch'))
      return
    }
    handshakeTimer.current = window.setTimeout(() => {
      handshakeTimer.current = null
      setBridgeError(t('pluginPage.bridgeTimeout'))
    }, 10_000)
    // Newer SDKs accept the first version; older SDKs ignore it and accept their own version.
    // All candidates come from Core's supported set intersected with the plugin's declared range.
    for (const bridgeVersion of bridgeVersions) {
      frame.current?.contentWindow?.postMessage({
        protocol: BRIDGE_PROTOCOL,
        bridgeVersion,
        pluginId: state.manifest.id,
        contributionId: contribution.contributionId,
        nonce: nonce.current,
        type: 'host_init',
        context: { lifecycle: active ? 'active' : 'inactive', surface },
      }, '*')
    }
  }

  const retryBackend = async () => {
    const requestId = createRequestId()
    backendRetryRequest.current = requestId
    setBackendRetrying(true)
    setBackendFailure('')
    try { await pluginApi.uiUrl(state.manifest.id, requestId, true) }
    catch (cause) { setBackendFailure(errorText(cause)) }
    finally { backendRetryRequest.current = null; setBackendRetrying(false) }
  }

  const retry = () => {
    if (handshakeTimer.current !== null) {
      window.clearTimeout(handshakeTimer.current)
      handshakeTimer.current = null
    }
    setReady(false)
    setBridgeError('')
    if (loaderFailure) {
      setLaunchInfo(null)
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
          <button type="button" onClick={retry} className="app-button app-button-primary">{t('pluginPage.retry')}</button>
          {isWebClient ? <p className="text-xs text-ink-muted">{t('mobile.checkHost')}</p> : <Link to="/workspace/settings" className="rounded-lg border border-glass-line px-4 py-2 text-sm text-ink hover:bg-glass-hover">{t('workspace.goToSettings')}</Link>}
        </div>
      </section>
    )
  }
  if (!url) return <p role="status" className="rounded-xl bg-glass px-5 py-4 text-sm text-ink-muted">{t('pluginPage.uiPending')}</p>

  return (
    <div className="plugin-surface-frame" hidden={!active} aria-hidden={!active} data-active={active}>
      {state.runtime === 'failed' && <div role="alert" className="flex items-center gap-2 border-b border-glass-line px-4 py-2 text-xs text-[var(--app-danger)]">
        <span className="flex-1">{backendFailure || state.lastError?.message || t('pluginPage.frameFailed')}</span>
        <button type="button" disabled={backendRetrying} className="app-button" onClick={() => void retryBackend()}>{t('pluginPage.retry')}</button>
      </div>}
      {!ready && <p role="status" className="border-b border-glass-line px-4 py-2 text-xs text-ink-faint">{t('pluginPage.bridgeStarting')}</p>}
      {bridgeError && (
        <div role="alert" className="flex flex-wrap items-center gap-2 border-b border-glass-line px-4 py-2 text-xs text-[var(--app-danger)]">
          <span className="flex-1">{bridgeError}</span>
          <button type="button" onClick={retry} className="app-button">{t('pluginPage.retry')}</button>
          {isWebClient ? <span className="text-ink-muted">{t('mobile.checkHost')}</span> : <Link to="/workspace/settings" className="rounded-md border border-glass-line px-2.5 py-1 text-ink hover:bg-glass-hover">{t('workspace.goToSettings')}</Link>}
        </div>
      )}
      <iframe
        ref={frame}
        key={frameGeneration}
        title={contribution.title}
        src={surfaceUrl(url, surface, contribution.contributionId)}
        sandbox="allow-scripts allow-forms allow-downloads allow-popups allow-popups-to-escape-sandbox"
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
  return `${base}${separator}wonderlandSurface=${surface}&wonderlandContribution=${encodeURIComponent(contributionId)}${isWebClient ? '&wonderlandClient=web' : ''}${hash}`
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
