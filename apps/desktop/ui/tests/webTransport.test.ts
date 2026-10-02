import assert from 'node:assert/strict'
import { afterEach, test } from 'node:test'
import { connectWeb, disconnectWeb, invoke, webRequest, webDownload } from '../src/core/transport.ts'

Object.defineProperty(globalThis, 'window', { value: new EventTarget(), configurable: true })
const originalFetch = globalThis.fetch
afterEach(() => { disconnectWeb(); globalThis.fetch = originalFetch })

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
  window.addEventListener('wonderland:web-disconnected', () => { disconnected = true }, { once: true })
  globalThis.fetch = async () => new Response('', { status: 401 })
  await assert.rejects(webRequest('plugins'), /密钥已失效/)
  assert.equal(disconnected, true)
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

test('device identity is ephemeral and downloads from an old connection are discarded', async () => {
  const identities: string[] = []
  globalThis.fetch = async (_url, options) => {
    identities.push((options!.headers as Record<string, string>)['X-Wonderland-Client'])
    return Response.json({ version: 'test', cursor: 0 })
  }
  await connectWeb('same-key')
  await connectWeb('same-key')
  assert.notEqual(identities[0], identities[1])
  let finish!: (response: Response) => void
  globalThis.fetch = () => new Promise((resolve) => { finish = resolve })
  const download = webDownload('interactions/test/content')
  disconnectWeb()
  finish(new Response('private file bytes'))
  await assert.rejects(download, /连接已被替换/)
})
