import assert from 'node:assert/strict'
import { test } from 'node:test'
import { createRequestId } from '../../../../packages/plugin-ui-sdk/src/requestId.ts'

test('LAN HTTP identifiers use secure random bytes without randomUUID', () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'crypto')
  Object.defineProperty(globalThis, 'crypto', { configurable: true, value: { getRandomValues: (bytes: Uint8Array) => { bytes.fill(42); return bytes } } })
  try { assert.equal(createRequestId(), '2a'.repeat(16)) }
  finally { if (original) Object.defineProperty(globalThis, 'crypto', original) }
})
