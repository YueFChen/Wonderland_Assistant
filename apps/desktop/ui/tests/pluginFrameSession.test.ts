import assert from 'node:assert/strict'
import test from 'node:test'
import { PluginFrameSession } from '../src/plugins/pluginFrameSession.ts'

test('document navigation closes an opaque frame bridge until Core creates a fresh frame', () => {
  const session = new PluginFrameSession()
  const originalWindow = {}
  assert.equal(session.accepts(originalWindow), false)
  assert.equal(session.beginLoad(null), false)
  assert.equal(session.beginLoad(originalWindow), true)
  assert.equal(session.accepts(originalWindow), true)
  assert.equal(session.accepts({}), false)
  // An external form navigation and a subsequent back navigation keep the WindowProxy.
  assert.equal(session.beginLoad(originalWindow), false)
  assert.equal(session.accepts(originalWindow), false)
  assert.equal(session.beginLoad(originalWindow), false)
  const retryWindow = {}
  assert.equal(session.beginLoad(retryWindow), true)
  assert.equal(session.accepts(retryWindow), true)
  assert.equal(session.accepts(originalWindow), false)
})
