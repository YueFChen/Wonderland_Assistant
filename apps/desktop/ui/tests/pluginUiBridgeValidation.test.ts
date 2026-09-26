import assert from 'node:assert/strict'
import test from 'node:test'
import { validatePluginUiBridgeMessage } from '../src/plugins/pluginUiBridgeValidation.ts'

const expected = {
  protocol: 'wonderland-plugin-ui',
  bridgeVersion: '1.0.0',
  pluginId: 'sample_plugin',
  contributionId: 'editor',
  nonce: 'session-nonce',
}

test('accepts a ready message with the expected bridge identity and version', () => {
  assert.equal(validatePluginUiBridgeMessage({ ...expected, type: 'ready' }, expected), 'accept')
})

test('reports a bridge mismatch only for an otherwise matching ready message', () => {
  assert.equal(
    validatePluginUiBridgeMessage({ ...expected, bridgeVersion: '0.9.0', type: 'ready' }, expected),
    'bridge-version-mismatch',
  )
  assert.equal(
    validatePluginUiBridgeMessage({ ...expected, bridgeVersion: '0.9.0', type: 'publish' }, expected),
    'ignore',
  )
})

test('ignores messages from a different contribution or nonce', () => {
  assert.equal(
    validatePluginUiBridgeMessage({ ...expected, contributionId: 'other', type: 'ready' }, expected),
    'ignore',
  )
  assert.equal(
    validatePluginUiBridgeMessage({ ...expected, nonce: 'old-session', type: 'ready' }, expected),
    'ignore',
  )
})
