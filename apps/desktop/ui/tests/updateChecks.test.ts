import assert from 'node:assert/strict'
import { test } from 'node:test'
import { createUpdateCheckGate, UPDATE_CHECK_INTERVAL_MS, UPDATE_WAKE_COOLDOWN_MS } from '../src/core/updateChecks.ts'

test('runtime checks run hourly; wake checks are throttled and share the manual check clock', () => {
  let now = 0
  const gate = createUpdateCheckGate(() => now)
  assert.equal(gate.beginCheck(), true)
  gate.endCheck()
  now = UPDATE_WAKE_COOLDOWN_MS - 1
  assert.equal(gate.beginCheck(UPDATE_WAKE_COOLDOWN_MS), false)
  now += 1
  assert.equal(gate.beginCheck(UPDATE_WAKE_COOLDOWN_MS), true)
  gate.endCheck()
  now += UPDATE_CHECK_INTERVAL_MS - 1
  assert.equal(gate.beginCheck(UPDATE_CHECK_INTERVAL_MS), false)
  now += 1
  assert.equal(gate.beginCheck(UPDATE_CHECK_INTERVAL_MS), true)
  gate.endCheck()
  now += 1
  assert.equal(gate.beginCheck(), true, 'manual checks bypass the cooldown')
  gate.endCheck()
  assert.equal(gate.beginCheck(UPDATE_WAKE_COOLDOWN_MS), false)
})

test('checks and installations exclude each other, including after errors', () => {
  const gate = createUpdateCheckGate()
  assert.equal(gate.beginCheck(), true)
  assert.equal(gate.beginCheck(), false)
  assert.equal(gate.beginInstall(), false)
  gate.endCheck()
  assert.equal(gate.beginInstall(), true)
  assert.equal(gate.beginInstall(), false)
  assert.equal(gate.beginCheck(), false)
  gate.endInstall()
  assert.equal(gate.beginCheck(), true)
})

test('failed checks are throttled, and moving the clock backwards does not suspend checks', () => {
  let now = UPDATE_CHECK_INTERVAL_MS
  const gate = createUpdateCheckGate(() => now)
  assert.equal(gate.beginCheck(), true)
  gate.endCheck()
  assert.equal(gate.beginCheck(UPDATE_WAKE_COOLDOWN_MS), false)
  now = 0
  assert.equal(gate.beginCheck(UPDATE_CHECK_INTERVAL_MS), true)
})
