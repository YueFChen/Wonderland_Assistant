import assert from 'node:assert/strict'
import { afterEach, test } from 'node:test'
import { connectWeb, disconnectWeb, invoke, webRequest, webDownload } from '../src/core/transport.ts'

import { rememberedConnection, rememberConnection } from '../src/core/connectionMemory.ts'

const storage = new Map<string, string>()
let storageDisabled = false
const testWindow = Object.assign(new EventTarget(), { localStorage: {
  getItem: (key: string) => { if (storageDisabled) throw new Error('Storage blocked'); return storage.get(key) ?? null },
  setItem: (key: string, value: string) => { if (storageDisabled) throw new Error('Storage blocked'); storage.set(key, value) },
  removeItem: (key: string) => { storage.delete(key) },
} })
Object.defineProperty(globalThis, 'window', { value: testWindow, configurable: true })
const originalFetch = globalThis.fetch
afterEach(() => { storageDisabled = false; disconnectWeb(); storage.clear(); globalThis.fetch = originalFetch })

test('browser cannot forward desktop management commands', async () => {
  globalThis.fetch = () => { throw new Error('Must not send a request') }
  await assert.rejects(invoke('plugins_install'), /桌面 Core/)
  await assert.rejects(invoke('account_snapshot'), /桌面 Core/)
  await assert.rejects(invoke('web_access_start'), /桌面 Core/)
})

test('sends the key only as a bearer header and omits browser cookies', async () => {
  const calls: { url: string; options: RequestInit }[] = []
  globalThis.fetch = async (url, options) => {
    calls.push({ url: String(url), options: options! })
    return Response.json({ version: 'test', cursor: 7 })
  }
  await connectWeb(' temporary-key ')
  await invoke('plugin_call', { pluginId: 'example', requestId: 'id-1', method: 'load', params: {} })
  assert.equal(calls[1].url, '/api/call')
  const headers = calls[1].options.headers as Record<string, string>
  assert.equal(headers.Authorization, 'Bearer temporary-key')
  assert.equal(headers['Content-Type'], 'application/json')
  assert.match(headers['X-Wonderland-Client'], /^[a-f0-9]{64}$/)
  assert.equal(headers['X-Wonderland-Client'], (calls[0].options.headers as Record<string, string>)['X-Wonderland-Client'])
  assert.equal(calls[1].options.credentials, 'omit')
  assert.equal(calls[1].options.cache, 'no-store')
  assert.equal(JSON.parse(String(calls[1].options.body)).method, 'load')
})

test('an expired key disconnects the browser', async () => {
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0 })
  await connectWeb('old-key')
  let disconnected = false
  let reason = ''
  window.addEventListener('wonderland:web-disconnected', (event) => { disconnected = true; reason = (event as CustomEvent<string>).detail }, { once: true })
  globalThis.fetch = async () => new Response('', { status: 401 })
  await assert.rejects(webRequest('plugins'), /密钥已失效/)
  assert.equal(disconnected, true)
  assert.match(reason, /密钥.*失效/)
  assert.equal(rememberedConnection(), null)
})

test('a late unauthorized response cannot revoke a newer connection', async () => {
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0 })
  await connectWeb('old-key')
  let finish!: (response: Response) => void
  globalThis.fetch = () => new Promise((resolve) => { finish = resolve })
  const old = webRequest('plugins')
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 1 })
  await connectWeb('new-key')
  finish(new Response('', { status: 401 }))
  await assert.rejects(old, /密钥已失效/)
  globalThis.fetch = async (_url, options) => {
    assert.equal((options!.headers as Record<string, string>).Authorization, 'Bearer new-key')
    return Response.json([])
  }
  await webRequest('plugins')
})

for (const outcome of ['success', 'failure']) {
  test(`a late connection ${outcome} cannot replace or disconnect a newer login`, async () => {
    let finish!: (response: Response) => void
    globalThis.fetch = () => new Promise((resolve) => { finish = resolve })
    const first = connectWeb('old-key')
    globalThis.fetch = async () => Response.json({ version: 'new', cursor: 10 })
    assert.equal((await connectWeb('new-key')).version, 'new')
    finish(outcome === 'success' ? Response.json({ version: 'old', cursor: 0 }) : new Response('', { status: 401 }))
    await assert.rejects(first)
    globalThis.fetch = async (_url, options) => {
      assert.equal((options!.headers as Record<string, string>).Authorization, 'Bearer new-key')
      return Response.json([])
    }
    await webRequest('plugins')
  })
}

test('late successful data is discarded after the browser disconnects', async () => {
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0 })
  await connectWeb('key')
  let finish!: (response: Response) => void
  globalThis.fetch = () => new Promise((resolve) => { finish = resolve })
  const pending = webRequest('plugins')
  disconnectWeb()
  finish(Response.json([{ private: 'stale data' }]))
  await assert.rejects(pending, /连接已被替换/)
})

test('explicit disconnect forgets the device and discards downloads from the old connection', async () => {
  const identities: string[] = []
  globalThis.fetch = async (_url, options) => {
    identities.push((options!.headers as Record<string, string>)['X-Wonderland-Client'])
    return Response.json({ version: 'test', cursor: 0 })
  }
  await connectWeb('same-key')
  disconnectWeb()
  assert.equal(rememberedConnection(), null)
  await connectWeb('same-key')
  assert.notEqual(identities[0], identities[1])
  let finish!: (response: Response) => void
  globalThis.fetch = () => new Promise((resolve) => { finish = resolve })
  const download = webDownload('interactions/test/content')
  disconnectWeb()
  finish(new Response('private file bytes'))
  await assert.rejects(download, /连接已被替换/)
})


test('a remembered login restores the key while keeping page interactions independent', async () => {
  let firstIdentity = ''
  globalThis.fetch = async (_url, options) => {
    firstIdentity = (options!.headers as Record<string, string>)['X-Wonderland-Client']
    return Response.json({ version: 'test', cursor: 0, sessionId: 'session-1', keyRequired: true })
  }
  await connectWeb('current-key')
  const saved = rememberedConnection()!
  assert.equal(saved.token, 'current-key')
  globalThis.fetch = async (_url, options) => {
    assert.equal((options!.headers as Record<string, string>).Authorization, 'Bearer current-key')
    assert.notEqual((options!.headers as Record<string, string>)['X-Wonderland-Client'], firstIdentity)
    return Response.json({ version: 'test', cursor: 1, sessionId: 'session-1', keyRequired: true })
  }
  await connectWeb(rememberedConnection()!.token)
  assert.deepEqual(rememberedConnection(), saved)
})

test('temporary network failures keep the remembered login; reconnection binds it to the new service', async () => {
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0, sessionId: 'session-1' })
  await connectWeb('key')
  const saved = rememberedConnection()!
  globalThis.fetch = async () => { throw new TypeError('Failed to fetch') }
  await assert.rejects(connectWeb('key'), /Failed to fetch/)
  assert.deepEqual(rememberedConnection(), saved)
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0, sessionId: 'session-2' })
  await connectWeb('key')
  assert.equal(rememberedConnection()!.token, 'key')
  assert.equal(rememberedConnection()!.sessionId, 'session-2')
})

test('a keyless connection is remembered and continues polling events', { timeout: 2500 }, async () => {
  let resolvePoll!: () => void
  const polled = new Promise<void>((resolve) => { resolvePoll = resolve })
  globalThis.fetch = async (url, options) => {
    assert.equal((options!.headers as Record<string, string>).Authorization, undefined)
    if (String(url).startsWith('/api/events')) {
      resolvePoll()
      return Response.json({ cursor: 0, events: [], missed: false })
    }
    return Response.json({ version: 'test', cursor: 0, sessionId: 'session', keyRequired: false })
  }
  await connectWeb('')
  assert.equal(rememberedConnection()!.token, '')
  await polled
})

test('UTF-8 keys work without putting non-ASCII characters into an HTTP header', async () => {
  const key = '我的 Core #密钥'
  globalThis.fetch = async (_url, options) => {
    const headers = options!.headers as Record<string, string>
    assert.equal(headers.Authorization, undefined)
    assert.equal(Buffer.from(headers['X-Wonderland-Key'], 'base64').toString('utf8'), key)
    return Response.json({ version: 'test', cursor: 0 })
  }
  await connectWeb(key)
  assert.equal(rememberedConnection()!.token, key)
})

test('connection succeeds when the browser blocks persistent storage', async () => {
  storageDisabled = true
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0 })
  assert.equal((await connectWeb('key')).version, 'test')
  assert.equal(rememberedConnection(), null)
})

test('an older connection cannot clear a newer remembered login', async () => {
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0 })
  await connectWeb('old-key')
  const newer = { token: 'new-key', sessionId: 'new-session' }
  rememberConnection(newer)
  globalThis.fetch = async () => new Response('', { status: 401 })
  await assert.rejects(webRequest('plugins'), /密钥已失效/)
  assert.deepEqual(rememberedConnection(), newer)
})
