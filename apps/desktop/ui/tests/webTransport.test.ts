import assert from 'node:assert/strict'
import { afterEach, test } from 'node:test'
import { connectWeb, disconnectWeb, invoke, listen, webRequest, webDownload } from '../src/core/transport.ts'

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
afterEach(async () => {
  storageDisabled = false
  globalThis.fetch = async () => Response.json(null)
  await disconnectWeb()
  storage.clear()
  globalThis.fetch = originalFetch
})

test('browser cannot forward desktop management commands', async () => {
  globalThis.fetch = () => { throw new Error('Must not send a request') }
  await assert.rejects(invoke('plugins_install'), /桌面 Core/)
  await assert.rejects(invoke('account_snapshot'), /桌面 Core/)
  await assert.rejects(invoke('web_access_start'), /桌面 Core/)
  await assert.rejects(invoke('plugins_distribution_settings'), /桌面 Core/)
  await assert.rejects(invoke('plugins_distribution_configure', { request: { accessCode: 'test-code' } }), /桌面 Core/)
  await assert.rejects(invoke('plugins_catalog_list'), /桌面 Core/)
  await assert.rejects(invoke('plugins_catalog_install'), /桌面 Core/)
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
  globalThis.fetch = (url) => String(url).endsWith('/revoke') ? Promise.resolve(Response.json(null)) : new Promise((resolve) => { finish = resolve })
  const pending = webRequest('plugins')
  await disconnectWeb()
  finish(Response.json([{ private: 'stale data' }]))
  await assert.rejects(pending, /连接已被替换/)
})

test('explicit disconnect forgets the device and discards downloads from the old connection', async () => {
  const identities: string[] = []
  globalThis.fetch = async (url, options) => {
    if (String(url).endsWith('/status')) identities.push((options!.headers as Record<string, string>)['X-Wonderland-Client'])
    return Response.json({ version: 'test', cursor: 0 })
  }
  await connectWeb('same-key')
  await disconnectWeb()
  assert.equal(rememberedConnection(), null)
  await connectWeb('same-key')
  assert.notEqual(identities[0], identities[1])
  let finish!: (response: Response) => void
  globalThis.fetch = (url) => String(url).endsWith('/revoke') ? Promise.resolve(Response.json(null)) : new Promise((resolve) => { finish = resolve })
  const download = webDownload('interactions/test/content')
  await disconnectWeb()
  finish(Response.json({ url: `/downloads/${'a'.repeat(64)}/${'b'.repeat(64)}/${'c'.repeat(64)}/${'d'.repeat(64)}` }))
  await assert.rejects(download, /连接已被替换/)
})

test('downloads prepare a scoped native URL without fetching or aggregating the file body', async () => {
  const downloadUrl = `/downloads/${'a'.repeat(64)}/${'b'.repeat(64)}/${'c'.repeat(64)}/${'d'.repeat(64)}`
  const requests: { url: string; options: RequestInit }[] = []
  globalThis.fetch = async (url, options) => {
    const path = String(url)
    requests.push({ url: path, options: options! })
    assert.equal(path.endsWith('/content'), false)
    assert.equal(path.startsWith('/downloads/'), false)
    return Response.json(path.endsWith('/status') ? { version: 'test', cursor: 0 } : { url: downloadUrl })
  }
  await connectWeb('key')
  assert.equal(await webDownload('interactions/file/content'), downloadUrl)
  assert.equal(requests.length, 2)
  assert.equal(requests[1].url, '/api/interactions/file/prepare')
  assert.equal(requests[1].options.credentials, 'same-origin')
  assert.equal(requests[1].options.method, 'POST')
  assert.equal((requests[1].options.headers as Record<string, string>).Authorization, 'Bearer key')
})

test('a malformed or external native download URL is rejected', async () => {
  globalThis.fetch = async (url) => Response.json(String(url).endsWith('/status') ? { version: 'test', cursor: 0 } : { url: 'https://other.example/file' })
  await connectWeb('key')
  await assert.rejects(webDownload('interactions/file/content'), /下载地址无效/)
})

test('disconnect revokes the scoped cookie using the closing identity and waits for confirmation', async () => {
  let originalClient = ''
  globalThis.fetch = async (_url, options) => {
    originalClient = (options!.headers as Record<string, string>)['X-Wonderland-Client']
    return Response.json({ version: 'test', cursor: 0 })
  }
  await connectWeb('key')
  const messages: (string | undefined)[] = []
  const observe = (event: Event) => messages.push((event as CustomEvent<string | undefined>).detail)
  window.addEventListener('wonderland:web-disconnected', observe)
  let finish!: (response: Response) => void
  globalThis.fetch = (url, options) => {
    assert.equal(String(url), '/api/downloads/revoke')
    assert.equal(options!.credentials, 'same-origin')
    assert.equal((options!.headers as Record<string, string>).Authorization, 'Bearer key')
    assert.equal((options!.headers as Record<string, string>)['X-Wonderland-Client'], originalClient)
    return new Promise((resolve) => { finish = resolve })
  }
  try {
    const closing = disconnectWeb()
    assert.equal(rememberedConnection(), null)
    assert.match(messages[0]!, /正在撤销/)
    assert.equal(messages.length, 1)
    finish(Response.json(null))
    await closing
    assert.equal(messages[1], null)
  } finally { window.removeEventListener('wonderland:web-disconnected', observe) }
})

test('failed revocation remains visible and an old confirmation cannot alter a new connection', async () => {
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0 })
  await connectWeb('key')
  const messages: (string | undefined)[] = []
  const observe = (event: Event) => messages.push((event as CustomEvent<string | undefined>).detail)
  window.addEventListener('wonderland:web-disconnected', observe)
  try {
    globalThis.fetch = async () => { throw new TypeError('offline') }
    await disconnectWeb()
    assert.match(messages.at(-1)!, /尚未确认撤销/)
    globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0 })
    await connectWeb('key')
    let finish!: (response: Response) => void
    globalThis.fetch = () => new Promise((resolve) => { finish = resolve })
    const closing = disconnectWeb()
    globalThis.fetch = async () => Response.json({ version: 'new', cursor: 0 })
    await connectWeb('new-key')
    const countAfterReconnect = messages.length
    finish(Response.json(null))
    await closing
    assert.equal(messages.length, countAfterReconnect)
    assert.equal(rememberedConnection()!.token, 'new-key')
  } finally { window.removeEventListener('wonderland:web-disconnected', observe) }
})

test('remote calls have no default deadline and only explicit deadlines abort a request', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  globalThis.fetch = async () => Response.json({ version: 'test', cursor: 0 })
  await connectWeb('key')
  let finish!: (response: Response) => void
  let signal: AbortSignal | undefined
  globalThis.fetch = (url, options) => {
    if (String(url).endsWith('/call')) {
      signal = options?.signal ?? undefined
      return new Promise((resolve) => { finish = resolve })
    }
    return Promise.resolve(Response.json({ cursor: 0, events: [] }))
  }
  const long = invoke('plugin_call', { pluginId: 'example', requestId: 'job', method: 'collect', params: {} })
  context.mock.timers.tick(300_001)
  assert.equal(signal?.aborted, false)
  finish(Response.json('finished'))
  assert.equal(await long, 'finished')
  globalThis.fetch = (_url, options) => new Promise((_resolve, reject) => {
    options?.signal?.addEventListener('abort', () => reject(options.signal?.reason), { once: true })
  })
  const timed = webRequest('call', {}, { timeoutMs: 500 })
  const rejection = assert.rejects(timed, { name: 'TimeoutError' })
  context.mock.timers.tick(500)
  await rejection
})

test('one notification loop resynchronizes state and device files after a transport interruption', async () => {
  let events = 0
  const refreshed: string[] = []
  const stops = await Promise.all([
    listen('plugins:changed', () => refreshed.push('plugins')),
    listen('interactions:changed', () => refreshed.push('interactions')),
  ])
  globalThis.fetch = async (url) => {
    if (String(url).endsWith('/status')) return Response.json({ version: 'test', cursor: 1 })
    if (++events === 1) throw new Error('network offline')
    return Response.json({ cursor: 1, missed: false, events: [] })
  }
  try {
    await connectWeb('key')
    await new Promise<void>((resolve) => {
      const changed = (event: Event) => {
        if ((event as CustomEvent<string>).detail !== 'connected') return
        window.removeEventListener('wonderland:web-connection', changed)
        resolve()
      }
      window.addEventListener('wonderland:web-connection', changed)
    })
    assert.equal(events, 2)
    assert.deepEqual(refreshed, ['plugins', 'interactions'])
  } finally { stops.forEach((stop) => stop()) }
})


test('a remembered login restores the same-page file owner for the same sharing session', async () => {
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
    if (String(_url).endsWith('/status')) assert.notEqual((options!.headers as Record<string, string>)['X-Wonderland-Client'], firstIdentity)
    return Response.json({ version: 'test', cursor: 1, sessionId: 'session-1', keyRequired: true })
  }
  await connectWeb(rememberedConnection()!.token)
  assert.deepEqual(rememberedConnection(), saved)
  globalThis.fetch = async (_url, options) => {
    assert.equal((options!.headers as Record<string, string>)['X-Wonderland-Client'], firstIdentity)
    return Response.json([])
  }
  await webRequest('interactions')
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


test('a restored owner cannot receive new downloads until its old revocation is confirmed', async () => {
  let originalClient = ''
  globalThis.fetch = async (_url, options) => {
    originalClient = (options!.headers as Record<string, string>)['X-Wonderland-Client']
    return Response.json({ version: 'test', cursor: 0, sessionId: 'revocation-session' })
  }
  await connectWeb('key')
  let revocations = 0
  let confirmed = false
  globalThis.fetch = async (url, options) => {
    if (String(url).endsWith('/downloads/revoke')) {
      revocations++
      assert.equal((options!.headers as Record<string, string>)['X-Wonderland-Client'], originalClient)
      return new Response('', { status: confirmed ? 200 : 503 })
    }
    assert.equal(String(url), '/api/status')
    return Response.json({ version: 'test', cursor: 0, sessionId: 'revocation-session' })
  }
  await disconnectWeb()
  await assert.rejects(connectWeb('key'), /旧下载会话尚未确认撤销/)
  await new Promise<void>((resolve) => queueMicrotask(resolve))
  assert.equal(revocations, 2)
  assert.equal(rememberedConnection(), null)
  confirmed = true
  await connectWeb('key')
  assert.equal(revocations, 3)
  globalThis.fetch = async (_url, options) => {
    assert.equal((options!.headers as Record<string, string>)['X-Wonderland-Client'], originalClient)
    return Response.json([])
  }
  await webRequest('interactions')
})

test('reconnection retries the failed old download owner rather than substituting the new client', async () => {
  let oldClient = ''
  globalThis.fetch = async (_url, options) => {
    oldClient = (options!.headers as Record<string, string>)['X-Wonderland-Client']
    return Response.json({ version: 'test', cursor: 0 })
  }
  await connectWeb('old-key')
  globalThis.fetch = async () => { throw new TypeError('offline') }
  await disconnectWeb()
  const revocations: string[] = []
  let newClient = ''
  globalThis.fetch = async (url, options) => {
    const headers = options!.headers as Record<string, string>
    if (String(url).endsWith('/downloads/revoke')) {
      revocations.push(headers['X-Wonderland-Client'])
      assert.equal(headers.Authorization, headers['X-Wonderland-Client'] === oldClient ? 'Bearer old-key' : 'Bearer new-key')
      return Response.json(null)
    }
    newClient = headers['X-Wonderland-Client']
    return Response.json({ version: 'test', cursor: 0 })
  }
  await connectWeb('new-key')
  await new Promise<void>((resolve) => queueMicrotask(resolve))
  assert.notEqual(newClient, oldClient)
  assert.deepEqual(revocations, [oldClient])
  await disconnectWeb()
  assert.deepEqual(revocations, [oldClient, newClient])
})
