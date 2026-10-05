import assert from 'node:assert/strict'
import { test } from 'node:test'
import { requestedPermissions } from '../src/plugins/capabilities.ts'

test('legacy network declarations do not require approval while credential and account access does', () => {
  const capabilities = ['network.public', 'network.model', 'secrets.plugin', 'account.authed_get', 'browser.open']
  assert.deepEqual(requestedPermissions(capabilities), ['secrets.plugin', 'account.authed_get', 'browser.open'])
  assert.deepEqual(capabilities.slice(0, 2), ['network.public', 'network.model'])
})
