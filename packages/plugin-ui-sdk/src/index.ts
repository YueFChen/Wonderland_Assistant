/** Browser-only client for the versioned UI Host Bridge. It never imports Core React state. */
export const UI_BRIDGE_PROTOCOL = 'wonderland-plugin-ui'
export const UI_BRIDGE_VERSION = '1.0.0'

export interface PluginUiEvent {
  pluginId: string
  topic: string
  requestId: string | null
  payload: unknown
}

export interface HostTheme {
  resolved: 'light' | 'dark'
}

/** Read-only payload returned by the Core `account.read` capability. */
export interface AccountSnapshot {
  status: 'logged_out' | 'logging_in' | 'logged_in'
  current_account_key: string | null
  accounts: Array<{
    account_key: string
    game_roles: Array<{
      uid: string
      region: string
      region_name: string
      nickname: string
      level: number
    }>
  }>
  last_login_failure: 'window_closed' | 'timeout' | 'cancelled' | 'failed' | null
}

export interface PluginHostClient {
  ready: Promise<void>
  call<T>(method: string, params?: unknown): Promise<T>
  callWithId<T>(method: string, params?: unknown, requestId?: string): { requestId: string; promise: Promise<T> }
  cancel(requestId: string): void
  publish(topic: string, payload: unknown): void
  subscribe(topic: string, listener: (event: PluginUiEvent) => void): Promise<() => void>
  followHostTheme(listener: (theme: HostTheme) => void): Promise<() => void>
  onSurfaceLifecycle(listener: (state: 'active' | 'inactive') => void): Promise<() => void>
  openWorkspaceView(viewId: string): Promise<void>
}

interface HostEnvelope {
  protocol?: string
  bridgeVersion?: string
  pluginId?: string
  contributionId?: string
  nonce?: string
  type?: string
  requestId?: string
  result?: unknown
  error?: { code?: string; message?: string }
  event?: PluginUiEvent
  context?: { theme?: HostTheme }
  state?: 'active' | 'inactive'
}

interface PendingCall {
  resolve: (value: unknown) => void
  reject: (cause: Error) => void
}

/** Create one client for the current sandbox frame using its Core-injected contribution ID. */
export function createPluginHostClient(pluginId: string): PluginHostClient {
  const contributionId = new URLSearchParams(window.location.search).get('wonderlandContribution')
  if (!contributionId) throw new Error('Core did not provide a contribution ID.')

  const pending = new Map<string, PendingCall>()
  const eventListeners = new Map<string, Set<(event: PluginUiEvent) => void>>()
  const themeListeners = new Set<(theme: HostTheme) => void>()
  const lifecycleListeners = new Set<(state: 'active' | 'inactive') => void>()
  const requestedTopics = new Set<string>()
  let nonce = ''
  let resolveReady: (() => void) | undefined
  const ready = new Promise<void>((resolve) => { resolveReady = resolve })

  const send = (message: Record<string, unknown>) => {
    if (!nonce) return
    window.parent.postMessage({
      protocol: UI_BRIDGE_PROTOCOL,
      bridgeVersion: UI_BRIDGE_VERSION,
      pluginId,
      contributionId,
      nonce,
      ...message,
    }, '*')
  }

  const onMessage = (event: MessageEvent<unknown>) => {
    if (event.source !== window.parent) return
    const message = event.data as HostEnvelope | null
    if (
      !message
      || message.protocol !== UI_BRIDGE_PROTOCOL
      || message.bridgeVersion !== UI_BRIDGE_VERSION
      || message.pluginId !== pluginId
      || message.contributionId !== contributionId
    ) return

    if (message.type === 'host_init' && typeof message.nonce === 'string') {
      nonce = message.nonce
      send({ type: 'ready' })
      resolveReady?.()
      resolveReady = undefined
      for (const topic of requestedTopics) send({ type: 'subscribe', topic })
      return
    }
    if (!nonce || message.nonce !== nonce) return

    if (message.type === 'host_context' && message.context?.theme) {
      for (const listener of themeListeners) listener(message.context.theme)
      return
    }
    if (message.type === 'surface_lifecycle' && message.state) {
      for (const listener of lifecycleListeners) listener(message.state)
      return
    }
    if (message.type === 'event' && message.event) {
      for (const listener of eventListeners.get(message.event.topic) ?? []) listener(message.event)
      return
    }
    if (typeof message.requestId !== 'string') return
    const request = pending.get(message.requestId)
    if (!request) return
    pending.delete(message.requestId)
    if (message.type === 'result') request.resolve(message.result)
    if (message.type === 'error') request.reject(new Error(message.error?.message ?? message.error?.code ?? 'Plugin call failed.'))
  }
  window.addEventListener('message', onMessage)

  const subscribe = async (topic: string, listener: (event: PluginUiEvent) => void) => {
    const listeners = eventListeners.get(topic) ?? new Set()
    listeners.add(listener)
    eventListeners.set(topic, listeners)
    requestedTopics.add(topic)
    await ready
    send({ type: 'subscribe', topic })
    return () => {
      listeners.delete(listener)
      if (listeners.size === 0) {
        eventListeners.delete(topic)
        requestedTopics.delete(topic)
      }
    }
  }

  const followHostTheme = async (listener: (theme: HostTheme) => void) => {
    themeListeners.add(listener)
    requestedTopics.add('host.theme')
    await ready
    send({ type: 'subscribe', topic: 'host.theme' })
    return () => {
      themeListeners.delete(listener)
      if (themeListeners.size === 0) requestedTopics.delete('host.theme')
    }
  }

  const onSurfaceLifecycle = async (listener: (state: 'active' | 'inactive') => void) => {
    lifecycleListeners.add(listener)
    requestedTopics.add('host.lifecycle')
    await ready
    send({ type: 'subscribe', topic: 'host.lifecycle' })
    return () => {
      lifecycleListeners.delete(listener)
      if (lifecycleListeners.size === 0) requestedTopics.delete('host.lifecycle')
    }
  }

  return {
    ready,
    async call<T>(method: string, params: unknown = {}) {
      return this.callWithId<T>(method, params).promise
    },
    callWithId<T>(method: string, params: unknown = {}, requestId = crypto.randomUUID()) {
      const promise = (async () => {
        await ready
        const result = new Promise<T>((resolve, reject) => {
          pending.set(requestId, { resolve: resolve as (value: unknown) => void, reject })
        })
        send({ type: 'call', requestId, method, params })
        return result
      })()
      return { requestId, promise }
    },
    cancel(requestId: string) {
      send({ type: 'cancel', requestId })
    },
    publish(topic: string, payload: unknown) {
      send({ type: 'publish', topic, payload })
    },
    subscribe,
    followHostTheme,
    onSurfaceLifecycle,
    async openWorkspaceView(viewId: string) {
      await ready
      send({ type: 'open_sidebar', viewId })
    },
  }
}
