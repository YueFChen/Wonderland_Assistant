import assert from 'node:assert/strict'
import { test } from 'node:test'
import { InteractionIdentity } from '../src/core/interactionIdentity.ts'

test('same tab reload restores pending-file owner without retaining any key', () => {
  const values = new Map<string, string>()
  const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value) } }
  const first = new InteractionIdentity(() => storage)
  const owner = 'a'.repeat(64)
  assert.equal(first.forSession('share-a', owner), owner)
  assert.equal(new InteractionIdentity(() => storage).forSession('share-a', 'b'.repeat(64)), owner)
  assert.deepEqual(Object.keys(JSON.parse([...values.values()][0])).sort(), ['clientId', 'sessionId'])
})

test('another tab or sharing session uses a distinct owner', () => {
  const first = new InteractionIdentity(() => undefined)
  const second = new InteractionIdentity(() => undefined)
  assert.equal(first.forSession('share-a', 'a'.repeat(64)), 'a'.repeat(64))
  assert.equal(second.forSession('share-a', 'b'.repeat(64)), 'b'.repeat(64))
  assert.equal(first.forSession('share-b', 'c'.repeat(64)), 'c'.repeat(64))
})

test('blocked session storage keeps ownership for the current page', () => {
  const identity = new InteractionIdentity(() => { throw new Error('blocked') })
  assert.equal(identity.forSession('share-a', 'a'.repeat(64)), 'a'.repeat(64))
  assert.equal(identity.forSession('share-a', 'b'.repeat(64)), 'a'.repeat(64))
})
