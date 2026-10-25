import { invoke as nativeInvoke, isTauri } from '@tauri-apps/api/core'
import { listen as nativeListen } from '@tauri-apps/api/event'
import { forgetConnection, rememberConnection, rememberedConnection } from './connectionMemory.ts'
import { dispatchRemoteNotifications, type RemoteNotificationBatch } from './remoteNotifications.ts'
import { scheduleUiDeadline, validateUiTimeout } from '../plugins/uiDeadline.ts'
import { InteractionIdentity } from './interactionIdentity.ts'

export const isWebClient = !isTauri()
let accessToken = ''
let clientId = ''
let sessionId: string | undefined
let connected = false
let cursor: number | undefined
let generation = 0
let timer: ReturnType<typeof setTimeout> | undefined
let needsResync = false
const listeners = new Map<string, Set<(event: { payload: unknown }) => void>>()
const requests = new Set<AbortController>()
const interactionIdentity = new InteractionIdentity(() => window.sessionStorage)
// Retain the old authenticated owner until the server confirms revocation.
// These records are page-local and are never exposed to plugin frames or storage.
interface DownloadRevocation { token: string; clientId: string; attempt?: Promise<boolean> }
const pendingRevocations = new Map<string, DownloadRevocation>()

function revokePendingDownloads(owner?: string): Promise<boolean> {
  const record = owner === undefined ? undefined : pendingRevocations.get(owner)
  const records = owner === undefined ? [...pendingRevocations.values()] : record ? [record] : []
  const attempts = records.map((record) => {
    record.attempt ??= (async () => {
      try {
        const response = await fetch('/api/downloads/revoke', {
          method: 'POST',
          headers: { ...keyHeaders(record.token), 'X-Wonderland-Client': record.clientId, 'Content-Type': 'application/json' },
          body: '{}', credentials: 'same-origin', cache: 'no-store',
        })
        if (!response.ok) return false
        pendingRevocations.delete(record.clientId)
        return true
      } catch { return false }
    })().finally(() => { record.attempt = undefined })
    return record.attempt
  })
  return Promise.all(attempts).then((results) => results.every(Boolean))
}

export interface WebRequestOptions {
  downloadSession?: boolean
  timeoutMs?: number
  signal?: AbortSignal
}

function keyHeaders(token: string): Record<string, string> {
  if (!token) return {}
  if (/^[\x20-\x7e]+$/.test(token)) return { Authorization: `Bearer ${token}` }
  const bytes = new TextEncoder().encode(token)
  return { 'X-Wonderland-Key': btoa(String.fromCharCode(...bytes)) }
}

async function webFetch(path: string, body?: unknown, options: WebRequestOptions = {}): Promise<{ response: Response; finish: () => void }> {
  const requestGeneration = generation
  const timeoutMs = validateUiTimeout(options.timeoutMs)
  const controller = new AbortController()
  requests.add(controller)
  const abort = () => controller.abort(options.signal?.reason)
  options.signal?.addEventListener('abort', abort, { once: true })
  if (options.signal?.aborted) abort()
  const stopDeadline = scheduleUiDeadline(timeoutMs, () => controller.abort(new DOMException('The explicit request deadline expired.', 'TimeoutError')))
  const finish = () => {
    stopDeadline()
    options.signal?.removeEventListener('abort', abort)
    requests.delete(controller)
  }
  try {
  const response = await fetch(`/api/${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { ...keyHeaders(accessToken), 'X-Wonderland-Client': clientId, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: options.downloadSession ? 'same-origin' : 'omit',
    cache: 'no-store',
    signal: controller.signal,
  })
  if (response.status === 401) {
    if (requestGeneration === generation) resetWeb(true, '访问密钥无效或已失效，请输入当前密钥重新连接。')
    throw new Error('连接已关闭或访问密钥已失效，请重新连接。')
  }
  if (!response.ok) {
    const error = await response.json().catch(() => ({ message: `请求失败 (${response.status})` }))
    throw Object.assign(new Error(error.message ?? `请求失败 (${response.status})`), error)
  }
  if (requestGeneration !== generation) throw new Error('连接已被替换，请使用当前连接。')
  return { response, finish }
  } catch (error) { finish(); throw error }
}

export async function webRequest<T>(path: string, body?: unknown, options: WebRequestOptions = {}): Promise<T> {
  const current = generation
  const { response, finish } = await webFetch(path, body, options)
  try {
  const data = await response.json() as T
  if (current !== generation) throw new Error('连接已被替换，请使用当前连接。')
  return data
  } finally { finish() }
}

export async function webDownload(path: string): Promise<string> {
  const prepared = await webRequest<{ url: string }>(path.replace(/\/content$/, '/prepare'), {}, { downloadSession: true })
  if (!/^\/downloads\/[a-f0-9]{64}\/[a-f0-9]{64}\/[a-f0-9]{64}\/[a-f0-9]{64}$/.test(prepared.url)) throw new Error('下载地址无效，请重新准备。')
  return prepared.url
}

export async function connectWeb(token: string): Promise<{ version: string }> {
  const saved = rememberedConnection()
  resetWeb(false)
  accessToken = token.trim()
  // getRandomValues is also available on trusted-LAN HTTP pages.
  // Each tab gets its own owner; a matching sharing session can resume that owner.
  clientId = Array.from(crypto.getRandomValues(new Uint8Array(32)), (value) => value.toString(16).padStart(2, '0')).join('')
  sessionId = saved?.token === accessToken ? saved.sessionId : undefined
  const connectionGeneration = generation
  try {
    const status = await webRequest<{ version: string; cursor: number; sessionId?: string; keyRequired?: boolean }>('status')
    const owner = interactionIdentity.forSession(status.sessionId, clientId)
    if (pendingRevocations.has(owner)) await revokePendingDownloads(owner)
    if (connectionGeneration !== generation) throw new Error('连接已被替换，请使用当前连接。')
    if (pendingRevocations.has(owner)) throw new Error('旧下载会话尚未确认撤销，请稍后重新连接，或在宿主处理遗留下载。')
    clientId = owner
    sessionId = status.sessionId
    if (status.keyRequired === false) accessToken = ''
    cursor = status.cursor
    connected = true
    rememberConnection({ token: accessToken, sessionId })
    schedulePoll()
    void revokePendingDownloads()
    return status
  } catch (error) {
    if (connectionGeneration === generation) resetWeb(false)
    throw error
  }
}

export async function disconnectWeb(): Promise<void> {
  if (clientId) pendingRevocations.set(clientId, { token: accessToken, clientId })
  const hasPending = pendingRevocations.size > 0
  resetWeb(true, hasPending ? '已断开连接，正在撤销下载会话…' : undefined)
  if (!hasPending) return
  const closingGeneration = generation
  const confirmed = await revokePendingDownloads()
  if (generation !== closingGeneration) return
  window.dispatchEvent(new CustomEvent('wonderland:web-disconnected', {
    detail: confirmed ? undefined : '已断开连接，但服务端尚未确认撤销下载会话。当前页面保留撤销记录，重新连接或再次断开会重试；关闭页面前请确认撤销，或在宿主停止共享。',
  }))
}

function resetWeb(forget: boolean, reason?: string) {
  if (forget) forgetConnection(accessToken, sessionId)
  accessToken = ''
  clientId = ''
  sessionId = undefined
  connected = false
  cursor = undefined
  needsResync = false
  generation++
  for (const request of requests) request.abort(new DOMException('The Core connection closed.', 'AbortError'))
  requests.clear()
  clearTimeout(timer)
  window.dispatchEvent(new CustomEvent('wonderland:web-disconnected', { detail: reason }))
}

function schedulePoll() {
  if (!connected) return
  const current = generation
  timer = setTimeout(() => {
    void webRequest<RemoteNotificationBatch>(`events${cursor === undefined ? '' : `?after=${cursor}`}`)
      .then((batch) => {
        if (current !== generation) return
        cursor = batch.cursor
        dispatchRemoteNotifications({ ...batch, missed: batch.missed || needsResync }, (name, payload) => {
          for (const callback of listeners.get(name) ?? []) callback({ payload })
        })
        needsResync = false
        window.dispatchEvent(new CustomEvent('wonderland:web-connection', { detail: batch.missed ? 'missed' : 'connected' }))
      })
      .catch(() => {
        if (current === generation) {
          needsResync = true
          window.dispatchEvent(new CustomEvent('wonderland:web-connection', { detail: 'reconnecting' }))
        }
      })
      .finally(() => { if (current === generation) schedulePoll() })
  }, 750)
}

/** Deliberately small browser command surface; never forwards arbitrary native IPC. */
export async function invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isWebClient) return nativeInvoke<T>(command, args)
  switch (command) {
    case 'plugins_list': return webRequest<T>('plugins')
    case 'plugins_ui_url': return webRequest<T>(`launch/${encodeURIComponent(String(args.pluginId))}?${new URLSearchParams({ requestId: String(args.requestId), retry: String(args.retry === true) })}`)
    case 'plugin_call': return webRequest<T>('call', args, { timeoutMs: args.timeoutMs as number | undefined })
    case 'plugin_cancel': return webRequest<T>('cancel', args)
    default: throw new Error('此操作需要在桌面 Core 中完成。')
  }
}

export async function listen<T>(name: string, callback: (event: { payload: T }) => void): Promise<() => void> {
  if (!isWebClient) return nativeListen<T>(name, callback)
  const handler = callback as (event: { payload: unknown }) => void
  const group = listeners.get(name) ?? new Set()
  group.add(handler)
  listeners.set(name, group)
  return () => {
    group.delete(handler)
    if (group.size === 0) listeners.delete(name)
  }
}
