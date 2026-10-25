import assert from 'node:assert/strict'
import { afterEach, test } from 'node:test'
import { createPluginHostClient, UI_BRIDGE_PROTOCOL, UI_BRIDGE_VERSION, type PluginHostClient } from '../../../../packages/plugin-ui-sdk/src/index.ts'

const clients: PluginHostClient[] = []
afterEach(() => { clients.splice(0).forEach((client) => client.dispose()) })

function surface() {
  const variables = new Map<string, string>()
  const sent: Record<string, unknown>[] = []
  const opened: unknown[][] = []
  const parent = { postMessage: (message: Record<string, unknown>) => sent.push(message) }
  const frame = Object.assign(new EventTarget(), { location: { search: '?wonderlandContribution=main' }, parent,
    open: (...args: unknown[]) => { opened.push(args); return null },
  })
  Object.defineProperty(globalThis, 'window', { configurable: true, value: frame })
  Object.defineProperty(globalThis, 'document', { configurable: true, value: Object.assign(new EventTarget(), { documentElement: { style: {
    setProperty: (key: string, value: string) => variables.set(key, value), removeProperty: (key: string) => variables.delete(key),
  } } }) })
  const client = createPluginHostClient('editor')
  clients.push(client)
  let nonce = 'session-one'
  const message = (value: Record<string, unknown>, origin = 'https://core.example') => frame.dispatchEvent(Object.assign(new Event('message'), {
    source: parent, origin, data: { protocol: UI_BRIDGE_PROTOCOL, bridgeVersion: UI_BRIDGE_VERSION, pluginId: 'editor', contributionId: 'main', nonce, ...value },
  }))
  message({ type: 'host_init' })
  return { client, sent, opened, message, variables, reload: () => { nonce = 'session-two'; message({ type: 'host_init' }) } }
}

test('opt-in theme sync carries component colors and clears the custom palette on preset or unsubscribe', async () => {
  const frame = surface(), themes: unknown[] = []
  const stop = await frame.client.followHostTheme(theme => themes.push(theme))
  frame.message({ type: 'host_context', context: { theme: { resolved: 'light', variables: { '--app-ink': '#202020', '--app-sidebar-surface': '#f8f4e8', '--app-radius-panel': '99px', '--background-image': 'url(private-image)' } } } })
  assert.equal(frame.variables.get('--app-sidebar-surface'), '#f8f4e8')
  assert.equal(frame.variables.has('--app-radius-panel'), false)
  assert.equal(frame.variables.has('--background-image'), false)
  frame.message({ type: 'host_context', context: { theme: { resolved: 'dark' } } })
  assert.equal(frame.variables.size, 0)
  frame.message({ type: 'host_context', context: { theme: { resolved: 'light', variables: { '--app-ink': '#111111' } } } })
  stop()
  assert.equal(frame.variables.size, 0)
  assert.equal(themes.length, 3)
})

test('late theme listeners receive the latest sample, but an unrelated origin cannot recolor a plugin', async () => {
  const frame = surface()
  frame.message({ type: 'host_context', context: { theme: { resolved: 'light', variables: { '--app-ink': '#111111' } } } })
  assert.equal(frame.variables.size, 0)
  await frame.client.followHostTheme(() => {})
  assert.equal(frame.variables.get('--app-ink'), '#111111')
  frame.message({ type: 'host_context', context: { theme: { resolved: 'dark', variables: { '--app-ink': '#ffffff' } } } }, 'https://other.example')
  assert.equal(frame.variables.get('--app-ink'), '#111111')
})

test('preferences reuse call/result and old Core reports an unsupported host method', async () => {
  const frame = surface()
  const result = frame.client.preferences.get('layout')
  await Promise.resolve()
  const call = frame.sent.find((message) => message.type === 'call')!
  assert.equal(call.method, 'core.ui.preferences.get')
  assert.deepEqual(call.params, { key: 'layout' })
  frame.message({ type: 'error', requestId: call.requestId, error: { code: 'METHOD_NOT_FOUND' } })
  await assert.rejects(result, { code: 'HOST_METHOD_UNSUPPORTED' })
})

test('reload, cancellation and disposal settle calls without a default timeout', async () => {
  const frame = surface()
  const first = frame.client.callWithId('slow', {}, 'first')
  const reloaded = assert.rejects(first.promise, { code: 'SURFACE_RELOADED' })
  await Promise.resolve()
  frame.reload()
  await reloaded
  const second = frame.client.callWithId('slow', {}, 'second')
  const cancelled = assert.rejects(second.promise, { code: 'CANCELLED' })
  frame.client.cancel('second')
  await cancelled
  const third = frame.client.call('slow')
  const closed = assert.rejects(third, { code: 'CONTRIBUTION_CLOSED' })
  frame.client.dispose()
  await closed
})

test('a reply from another parent origin cannot finish a valid call', async () => {
  const frame = surface()
  const result = frame.client.callWithId('load', {}, 'origin-test')
  await Promise.resolve()
  frame.message({ type: 'result', requestId: 'origin-test', result: 'wrong' }, 'https://other.example')
  frame.message({ type: 'result', requestId: 'origin-test', result: 'right' })
  assert.equal(await result.promise, 'right')
})

test('cancelled UI command results cannot be delivered after a cooperative abort', async () => {
  const frame = surface()
  let finish!: (value: string) => void
  let signal: AbortSignal | undefined
  await frame.client.onCommand((command) => { signal = command.signal; return new Promise<string>((resolve) => { finish = resolve }) })
  frame.message({ type: 'command', requestId: 'command', commandId: 'export', input: {} })
  await Promise.resolve()
  frame.message({ type: 'command_cancel', requestId: 'command' })
  assert.equal(signal?.aborted, true)
  finish('late')
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve()
  assert.equal(frame.sent.some((message) => message.type === 'command_result'), false)
})

test('external links disconnect their opener and preserve the HTTP(S) contract', () => {
  const frame = surface()
  frame.client.openExternal('https://example.com/tool')
  assert.deepEqual(frame.opened[0], ['https://example.com/tool', '_blank', ',noopener,noreferrer'])
  assert.throws(() => frame.client.openExternal('javascript:alert(1)'), { code: 'INVALID_INPUT' })
})
