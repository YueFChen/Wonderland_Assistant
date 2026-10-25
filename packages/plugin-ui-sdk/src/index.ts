import { UI_BRIDGE_PROTOCOL, UI_BRIDGE_VERSION } from '@wonderland/plugin-protocol'
import { createRequestId } from './requestId.ts'
import { applyHostThemeVariables, type HostTheme } from './theme.ts'
export { createRequestId } from './requestId.ts'
export { HOST_THEME_VARIABLES, type HostTheme } from './theme.ts'

export { UI_BRIDGE_PROTOCOL, UI_BRIDGE_VERSION }
export type { FileResult } from '@wonderland/plugin-protocol'

/** Browser-only client for the versioned UI Host Bridge. It never imports Core React state. */

export interface PluginUiEvent {
  pluginId: string
  topic: string
  requestId: string | null
  payload: unknown
}

export interface PluginUiCommandRequest {
  requestId: string
  commandId: string
  input: unknown
  /** Cancellation is cooperative; late results are discarded by the SDK. */
  signal?: AbortSignal
}

export type JsonPreference = null | boolean | number | string | JsonPreference[] | { [key: string]: JsonPreference }

export interface PluginUiCallOptions {
  /** Omitted means no operation deadline. */
  timeoutMs?: number
  signal?: AbortSignal
}

export interface PluginUiPreferences {
  get<T extends JsonPreference = JsonPreference>(key: string): Promise<T | null>
  set(key: string, value: JsonPreference): Promise<void>
  delete(key: string): Promise<void>
}

export interface PluginHostClient {
  ready: Promise<void>
  call<T>(method: string, params?: unknown, options?: PluginUiCallOptions): Promise<T>
  callWithId<T>(method: string, params?: unknown, requestId?: string, options?: PluginUiCallOptions): { requestId: string; promise: Promise<T> }
  preferences: PluginUiPreferences
  openExternal(url: string): void
  dispose(): void
  cancel(requestId: string): void
  publish(topic: string, payload: unknown): void
  subscribe(topic: string, listener: (event: PluginUiEvent) => void): Promise<() => void>
  followHostTheme(listener: (theme: HostTheme) => void): Promise<() => void>
  onSurfaceLifecycle(listener: (state: 'active' | 'inactive') => void): Promise<() => void>
  onCommand(listener: (command: PluginUiCommandRequest) => unknown | Promise<unknown>): Promise<() => void>
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
  error?: { code?: string; message?: string; details?: unknown }
  event?: PluginUiEvent
  context?: { theme?: HostTheme }
  state?: 'active' | 'inactive'
  commandId?: string
  input?: unknown
}

interface PendingCall {
  resolve: (value: unknown) => void
  reject: (cause: Error) => void
  cleanup: () => void
}

export class PluginHostError extends Error {
  readonly code: string
  readonly details: unknown

  constructor(code: string, message: string, details: unknown = null) {
    super(message)
    this.name = 'PluginHostError'
    this.code = code
    this.details = details
    Object.setPrototypeOf(this, new.target.prototype)
  }
}

/** Create one client for the current sandbox frame using its Core-injected contribution ID. */
export function createPluginHostClient(pluginId: string): PluginHostClient {
  const contributionId = new URLSearchParams(window.location.search).get('wonderlandContribution')
  if (!contributionId) throw new Error('Core did not provide a contribution ID.')

  const pending = new Map<string, PendingCall>()
  const eventListeners = new Map<string, Set<(event: PluginUiEvent) => void>>()
  const themeListeners = new Set<(theme: HostTheme) => void>()
  const lifecycleListeners = new Set<(state: 'active' | 'inactive') => void>()
  const commandListeners = new Set<(command: PluginUiCommandRequest) => unknown | Promise<unknown>>()
  const requestedTopics = new Set<string>()
  let lastTheme: HostTheme | undefined
  let nonce = ''
  let parentOrigin: string | undefined
  let disposed = false
  const commands = new Map<string, AbortController>()
  let resolveReady: (() => void) | undefined
  let rejectReady: ((reason: Error) => void) | undefined
  const ready = new Promise<void>((resolve, reject) => { resolveReady = resolve; rejectReady = reject })
  void ready.catch(() => undefined)

  const settle = (requestId: string, value: unknown, error?: Error) => {
    const request = pending.get(requestId)
    if (!request) return
    pending.delete(requestId)
    request.cleanup()
    if (error) request.reject(error)
    else request.resolve(value)
  }
  const failPending = (code: string, message: string) => {
    for (const requestId of [...pending.keys()]) settle(requestId, null, new PluginHostError(code, message))
    for (const command of commands.values()) command.abort()
    commands.clear()
  }

  const send = (message: Record<string, unknown>) => {
    if (!nonce || disposed) return
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
    if (disposed || event.source !== window.parent || (parentOrigin !== undefined && event.origin !== parentOrigin)) return
    const message = event.data as HostEnvelope | null
    if (
      !message
      || message.protocol !== UI_BRIDGE_PROTOCOL
      // Core sends candidates newest first; this SDK accepts only the exact contract it implements.
      || message.bridgeVersion !== UI_BRIDGE_VERSION
      || message.pluginId !== pluginId
      || message.contributionId !== contributionId
    ) return

    if (message.type === 'host_init' && typeof message.nonce === 'string') {
      if (nonce && nonce !== message.nonce) failPending('SURFACE_RELOADED', 'The Core UI surface was reloaded.')
      parentOrigin = event.origin
      nonce = message.nonce
      send({ type: 'ready' })
      resolveReady?.()
      resolveReady = undefined
      rejectReady = undefined
      for (const topic of requestedTopics) send({ type: 'subscribe', topic })
      return
    }
    if (!nonce || message.nonce !== nonce) return

    if (message.type === 'host_close') { dispose(); return }
    if (message.type === 'command_cancel' && typeof message.requestId === 'string') {
      commands.get(message.requestId)?.abort()
      commands.delete(message.requestId)
      return
    }

    if (message.type === 'host_context' && message.context?.theme) {
      lastTheme = message.context.theme
      if (themeListeners.size > 0) applyHostThemeVariables(document.documentElement.style, lastTheme.variables)
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
    if (
      message.type === 'command'
      && typeof message.requestId === 'string'
      && typeof message.commandId === 'string'
    ) {
      const listener = commandListeners.values().next().value
      if (!listener) {
        send({
          type: 'command_error',
          requestId: message.requestId,
          error: { code: 'UI_COMMAND_UNHANDLED', message: 'This plugin UI does not handle the declared command.' },
        })
        return
      }
      const requestId = message.requestId
      const commandNonce = nonce
      const controller = new AbortController()
      commands.set(requestId, controller)
      const reply = (result: Record<string, unknown>) => {
        if (!disposed && nonce === commandNonce && commands.get(requestId) === controller && !controller.signal.aborted) send(result)
        if (commands.get(requestId) === controller) commands.delete(requestId)
      }
      void Promise.resolve()
        .then(() => listener({
          requestId: message.requestId!,
          commandId: message.commandId!,
          input: message.input,
          signal: controller.signal,
        }))
        .then((result) => reply({ type: 'command_result', requestId, result }))
        .catch((cause: unknown) => reply({
          type: 'command_error',
          requestId,
          error: {
            code: cause instanceof PluginHostError ? cause.code : 'UI_COMMAND_FAILED',
            message: cause instanceof Error ? cause.message : String(cause),
          },
        }))
      return
    }
    if (typeof message.requestId !== 'string' || (message.type !== 'result' && message.type !== 'error')) return
    if (message.type === 'result') settle(message.requestId, message.result)
    if (message.type === 'error') {
      settle(message.requestId, null, new PluginHostError(
        message.error?.code ?? 'PLUGIN_ERROR',
        message.error?.message ?? message.error?.code ?? 'Plugin call failed.',
        message.error?.details,
      ))
    }
  }
  window.addEventListener('message', onMessage)

  const originalOpen = window.open
  const safeOpen: typeof window.open = (url, target, features) => originalOpen.call(window, url, target, `${features ?? ''},noopener,noreferrer`)
  window.open = safeOpen

  const dispose = () => {
    if (disposed) return
    for (const requestId of pending.keys()) send({ type: 'cancel', requestId })
    failPending('CONTRIBUTION_CLOSED', 'The plugin UI surface closed.')
    rejectReady?.(new PluginHostError('CONTRIBUTION_CLOSED', 'The plugin UI surface closed.'))
    disposed = true
    window.removeEventListener('message', onMessage)
    window.removeEventListener('pagehide', dispose)
    document.removeEventListener('click', protectExternalLink, true)
    if (themeListeners.size > 0) applyHostThemeVariables(document.documentElement.style, undefined)
    if (window.open === safeOpen) window.open = originalOpen
  }
  const protectExternalLink = (event: Event) => {
    const link = event.target instanceof Element ? event.target.closest('a[target]') : null
    if (link instanceof HTMLAnchorElement && link.target !== '_self') {
      link.relList.add('noopener', 'noreferrer')
    }
  }
  window.addEventListener('pagehide', dispose)
  document.addEventListener('click', protectExternalLink, true)

  const callWithId = <T>(method: string, params: unknown = {}, requestId = createRequestId(), options: PluginUiCallOptions = {}) => {
    const promise = new Promise<T>((resolve, reject) => {
      if (disposed) { reject(new PluginHostError('CONTRIBUTION_CLOSED', 'The plugin UI surface closed.')); return }
      if (pending.has(requestId)) { reject(new PluginHostError('INVALID_REQUEST', 'This request ID is already active.')); return }
      const timeoutMs = options.timeoutMs
      if (timeoutMs !== undefined && (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0)) {
        reject(new PluginHostError('INVALID_INPUT', 'timeoutMs must be a positive safe integer.')); return
      }
      let timer: ReturnType<typeof setTimeout> | undefined
      const abort = () => {
        send({ type: 'cancel', requestId })
        settle(requestId, null, new PluginHostError('CANCELLED', 'The plugin UI call was cancelled.'))
      }
      pending.set(requestId, { resolve: resolve as (value: unknown) => void, reject, cleanup: () => {
        if (timer !== undefined) clearTimeout(timer)
        options.signal?.removeEventListener('abort', abort)
      } })
      const started = Date.now()
      const deadline = () => {
        if (timeoutMs === undefined || !pending.has(requestId)) return
        const remaining = timeoutMs - (Date.now() - started)
        if (remaining > 0) { timer = setTimeout(deadline, Math.min(remaining, 2_147_483_647)); return }
        send({ type: 'cancel', requestId })
        settle(requestId, null, new PluginHostError('TIMEOUT', 'The plugin UI call exceeded its explicit deadline.'))
      }
      deadline()
      options.signal?.addEventListener('abort', abort, { once: true })
      if (options.signal?.aborted) { abort(); return }
      void ready.then(() => {
        if (!pending.has(requestId)) return
        try { send({ type: 'call', requestId, method, params, ...(timeoutMs === undefined ? {} : { timeoutMs }) }) }
        catch { settle(requestId, null, new PluginHostError('INVALID_INPUT', 'Plugin UI parameters could not be transferred.')) }
      }, (error: Error) => settle(requestId, null, error))
    })
    return { requestId, promise }
  }

  const preferenceCall = async <T>(method: string, params: unknown): Promise<T> => {
    try { return await callWithId<T>(method, params).promise }
    catch (cause) {
      if (cause instanceof PluginHostError && ['METHOD_NOT_FOUND', 'HOST_METHOD_UNSUPPORTED'].includes(cause.code)) {
        throw new PluginHostError('HOST_METHOD_UNSUPPORTED', 'This Core does not provide UI preference storage.')
      }
      throw cause
    }
  }

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
    if (lastTheme) {
      applyHostThemeVariables(document.documentElement.style, lastTheme.variables)
      listener(lastTheme)
    }
    await ready
    send({ type: 'subscribe', topic: 'host.theme' })
    return () => {
      themeListeners.delete(listener)
      if (themeListeners.size === 0) {
        requestedTopics.delete('host.theme')
        applyHostThemeVariables(document.documentElement.style, undefined)
      }
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

  const onCommand = async (listener: (command: PluginUiCommandRequest) => unknown | Promise<unknown>) => {
    commandListeners.add(listener)
    await ready
    return () => commandListeners.delete(listener)
  }

  return {
    ready,
    async call<T>(method: string, params: unknown = {}, options: PluginUiCallOptions = {}) {
      return callWithId<T>(method, params, undefined, options).promise
    },
    callWithId,
    preferences: {
      get: <T extends JsonPreference = JsonPreference>(key: string) => preferenceCall<T | null>('core.ui.preferences.get', { key }),
      async set(key: string, value: JsonPreference) { await preferenceCall('core.ui.preferences.set', { key, value }) },
      async delete(key: string) { await preferenceCall('core.ui.preferences.delete', { key }) },
    },
    openExternal(raw: string) {
      if (disposed) throw new PluginHostError('CONTRIBUTION_CLOSED', 'The plugin UI surface closed.')
      let url: URL
      try { url = new URL(raw) }
      catch { throw new PluginHostError('INVALID_INPUT', 'External links must be valid HTTP(S) addresses.') }
      if (!['http:', 'https:'].includes(url.protocol)) throw new PluginHostError('INVALID_INPUT', 'External links must use HTTP(S).')
      window.open(url.href, '_blank')
    },
    dispose,
    cancel(requestId: string) {
      send({ type: 'cancel', requestId })
      settle(requestId, null, new PluginHostError('CANCELLED', 'The plugin UI call was cancelled.'))
    },
    publish(topic: string, payload: unknown) {
      send({ type: 'publish', topic, payload })
    },
    subscribe,
    followHostTheme,
    onSurfaceLifecycle,
    onCommand,
    async openWorkspaceView(viewId: string) {
      await ready
      send({ type: 'open_sidebar', viewId })
    },
  }
}
