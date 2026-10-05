import { invoke as nativeInvoke, isTauri } from '@tauri-apps/api/core'
import { listen as nativeListen } from '@tauri-apps/api/event'
import { forgetConnection, rememberConnection, rememberedConnection } from './connectionMemory.ts'

export const isWebClient = !isTauri()
let accessToken = ''
let clientId = ''
let sessionId: string | undefined
let connected = false
let cursor: number | undefined
let generation = 0
let timer: ReturnType<typeof setTimeout> | undefined
const listeners = new Map<string, Set<(event: { payload: unknown }) => void>>()

function keyHeaders(token: string): Record<string, string> {
  if (!token) return {}
  if (/^[\x20-\x7e]+$/.test(token)) return { Authorization: `Bearer ${token}` }
  const bytes = new TextEncoder().encode(token)
  return { 'X-Wonderland-Key': btoa(String.fromCharCode(...bytes)) }
}

async function webFetch(path: string, body?: unknown): Promise<Response> {
  const requestGeneration = generation
  const response = await fetch(`/api/${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { ...keyHeaders(accessToken), 'X-Wonderland-Client': clientId, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: 'omit',
    cache: 'no-store',
    signal: AbortSignal.timeout(body === undefined ? 15_000 : 300_000),
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
  return response
}

export async function webRequest<T>(path: string, body?: unknown): Promise<T> {
  const current = generation
  const data = await (await webFetch(path, body)).json() as T
  if (current !== generation) throw new Error('连接已被替换，请使用当前连接。')
  return data
}

export async function webDownload(path: string): Promise<Blob> {
  const current = generation
  const data = await (await webFetch(path)).blob()
  if (current !== generation) throw new Error('连接已被替换，请使用当前连接。')
  return data
}

export async function connectWeb(token: string): Promise<{ version: string }> {
  const saved = rememberedConnection()
  resetWeb(false)
  accessToken = token.trim()
  // getRandomValues is also available on trusted-LAN HTTP pages.
  // Remember authentication, but isolate file interactions between browser tabs.
  clientId = Array.from(crypto.getRandomValues(new Uint8Array(32)), (value) => value.toString(16).padStart(2, '0')).join('')
  sessionId = saved?.token === accessToken ? saved.sessionId : undefined
  const connectionGeneration = generation
  try {
    const status = await webRequest<{ version: string; cursor: number; sessionId?: string; keyRequired?: boolean }>('status')
    sessionId = status.sessionId
    if (status.keyRequired === false) accessToken = ''
    cursor = status.cursor
    connected = true
    rememberConnection({ token: accessToken, sessionId })
    schedulePoll()
    return status
  } catch (error) {
    if (connectionGeneration === generation) resetWeb(false)
    throw error
  }
}

export function disconnectWeb() {
  resetWeb(true)
}

function resetWeb(forget: boolean, reason?: string) {
  if (forget) forgetConnection(accessToken, sessionId)
  accessToken = ''
  clientId = ''
  sessionId = undefined
  connected = false
  cursor = undefined
  generation++
  clearTimeout(timer)
  window.dispatchEvent(new CustomEvent('wonderland:web-disconnected', { detail: reason }))
}

function schedulePoll() {
  if (!connected) return
  const current = generation
  timer = setTimeout(() => {
    void webRequest<{ cursor: number; events: unknown[]; missed: boolean }>(`events${cursor === undefined ? '' : `?after=${cursor}`}`)
      .then((batch) => {
        if (current !== generation) return
        cursor = batch.cursor
        for (const payload of batch.events) {
          for (const callback of listeners.get('plugin:event') ?? []) callback({ payload })
        }
        window.dispatchEvent(new CustomEvent('wonderland:web-connection', { detail: batch.missed ? 'missed' : 'connected' }))
      })
      .catch(() => {
        if (current === generation) window.dispatchEvent(new CustomEvent('wonderland:web-connection', { detail: 'reconnecting' }))
      })
      .finally(() => { if (current === generation) schedulePoll() })
  }, 750)
}

/** Deliberately small browser command surface; never forwards arbitrary native IPC. */
export async function invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isWebClient) return nativeInvoke<T>(command, args)
  switch (command) {
    case 'plugins_list': return webRequest<T>('plugins')
    case 'plugins_ui_url': return webRequest<T>(`launch/${encodeURIComponent(String(args.pluginId))}`)
    case 'plugin_call': return webRequest<T>('call', args)
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
