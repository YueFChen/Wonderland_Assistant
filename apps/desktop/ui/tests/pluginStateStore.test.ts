import assert from 'node:assert/strict'
import { test } from 'node:test'
import { createPluginStateStore } from '../src/plugins/pluginStateStore.ts'

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0))

test('a failed subscription is visible and reconnects without remounting consumers', async () => {
  let listeners = 0
  let value = 'running'
  let changed: (() => void) | undefined
  const store = createPluginStateStore({
    read: async () => [value],
    listen: async (callback) => {
      if (++listeners === 1) throw new Error('temporary registration failure')
      changed = callback
      return () => {}
    },
  }, [10])
  const stop = store.subscribe(() => {})
  try {
    await flush()
    assert.deepEqual(store.getSnapshot(), { states: ['running'], failed: true })
    value = 'stopped'
    for (let attempt = 0; attempt < 100 && !changed; attempt++) await flush()
    await flush()
    assert.equal(listeners, 2)
    assert.deepEqual(store.getSnapshot(), { states: ['stopped'], failed: false })
    value = 'failed'
    changed!()
    await flush()
    assert.deepEqual(store.getSnapshot().states, ['failed'])
  } finally { stop(); await flush() }
})

test('manual refresh retries a failed listener immediately and disposal cancels retries', async () => {
  let listeners = 0
  const store = createPluginStateStore({
    read: async () => [],
    listen: async () => { listeners++; throw new Error('offline') },
  }, [30000])
  const stop = store.subscribe(() => {})
  await flush()
  store.refresh()
  await flush()
  assert.equal(listeners, 2)
  stop()
  await flush()
  store.refresh()
  await flush()
  assert.equal(listeners, 2)
})

test('multiple consumers and a StrictMode probe share one read and listener', async () => {
  let reads = 0
  let listeners = 0
  let stopped = 0
  const store = createPluginStateStore({
    read: async () => { reads++; return ['running'] },
    listen: async () => { listeners++; return () => { stopped++ } },
  })
  const first = store.subscribe(() => {})
  first()
  const second = store.subscribe(() => {})
  const third = store.subscribe(() => {})
  await flush()
  assert.deepEqual([reads, listeners], [1, 1])
  assert.deepEqual(store.getSnapshot(), { states: ['running'], failed: false })
  second()
  await flush()
  assert.equal(stopped, 0)
  third()
  await flush()
  assert.equal(stopped, 1)
})

test('a slow refresh cannot overwrite a completed plugin mutation', async () => {
  let finish!: (states: string[]) => void
  const store = createPluginStateStore<string>({
    read: () => new Promise((resolve) => { finish = resolve }),
    listen: async () => () => {},
  })
  const stop = store.subscribe(() => {})
  await flush()
  store.setStates(['disabled'])
  finish(['running'])
  await flush()
  assert.deepEqual(store.getSnapshot(), { states: ['disabled'], failed: false })
  stop()
})

test('bursts during a refresh coalesce into one follow-up read', async () => {
  const finishes: ((states: string[]) => void)[] = []
  let changed!: () => void
  const store = createPluginStateStore<string>({
    read: () => new Promise((resolve) => finishes.push(resolve)),
    listen: async (callback) => { changed = callback; return () => {} },
  })
  const stop = store.subscribe(() => {})
  await flush()
  for (let i = 0; i < 20; i++) changed()
  assert.equal(finishes.length, 1)
  finishes[0](['starting'])
  await flush()
  assert.equal(finishes.length, 2)
  finishes[1](['running'])
  await flush()
  assert.equal(finishes.length, 2)
  assert.deepEqual(store.getSnapshot().states, ['running'])
  stop()
})

test('disconnect clears the shared snapshot and rejects old session data', async () => {
  let finish!: (states: string[]) => void
  const store = createPluginStateStore<string>({
    read: () => new Promise((resolve) => { finish = resolve }),
    listen: async () => () => {},
  })
  const stop = store.subscribe(() => {})
  await flush()
  store.setStates(['old-host'])
  store.reset()
  finish(['late-old-host'])
  await flush()
  assert.deepEqual(store.getSnapshot(), { states: null, failed: false })
  stop()
})

test('a listener established after the final consumer leaves is immediately released', async () => {
  let finishListening!: (stop: () => void) => void
  let released = false
  const store = createPluginStateStore<string>({
    read: async () => [],
    listen: () => new Promise((resolve) => { finishListening = resolve }),
  })
  const stop = store.subscribe(() => {})
  stop()
  await flush()
  finishListening(() => { released = true })
  await flush()
  assert.equal(released, true)
})

test('initial snapshot waits for the event subscription so startup changes cannot fall in a gap', async () => {
  let finishListening!: (stop: () => void) => void
  let reads = 0
  let current = 'starting'
  const store = createPluginStateStore<string>({
    read: async () => { reads++; return [current] },
    listen: () => new Promise((resolve) => { finishListening = resolve }),
  })
  const stop = store.subscribe(() => {})
  await flush()
  assert.equal(reads, 0)
  current = 'running'
  finishListening(() => {})
  await flush()
  assert.equal(reads, 1)
  assert.deepEqual(store.getSnapshot().states, ['running'])
  stop()
})
