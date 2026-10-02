import assert from 'node:assert/strict'
import { test } from 'node:test'
import { isRemotelyShareable } from '../src/plugins/remoteAccess.ts'

test('only explicitly opted-in running plugins with context support can be shared', () => {
  const state = { enabled: true, runtime: 'running', manifest: { ui: {}, remoteAccess: true, backend: { supportsServiceContext: true } } }
  assert.equal(isRemotelyShareable(state), true)
  for (const remoteAccess of [undefined, null, false]) {
    assert.equal(isRemotelyShareable({ ...state, manifest: { ...state.manifest, remoteAccess } }), false)
  }
  assert.equal(isRemotelyShareable({ ...state, manifest: { ...state.manifest, backend: {} } }), false)
  assert.equal(isRemotelyShareable({ ...state, manifest: { ...state.manifest, ui: null } }), false)
  assert.equal(isRemotelyShareable({ ...state, enabled: false }), false)
  assert.equal(isRemotelyShareable({ ...state, runtime: 'stopped' }), false)
})
