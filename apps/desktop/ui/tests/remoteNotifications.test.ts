import assert from 'node:assert/strict'
import { test } from 'node:test'
import { dispatchRemoteNotifications } from '../src/core/remoteNotifications.ts'

test('one remote batch dispatches typed plugin, state, interaction and download changes', () => {
  const dispatched: [string, unknown][] = []
  const interaction = { kind: 'interaction' as const, id: 'pick-b', rootPluginId: 'a', pluginId: 'b', state: 'ready' as const }
  const download = { ...interaction, kind: 'download' as const, id: 'download-c', pluginId: 'c', state: 'delivered' as const }
  dispatchRemoteNotifications({ cursor: 4, missed: false, events: [
    { kind: 'plugin_event', payload: { pluginId: 'a', topic: 'progress' } },
    { kind: 'plugin_state' },
    { kind: 'interaction_change', payload: interaction },
    { kind: 'interaction_change', payload: download },
  ] }, (name, payload) => dispatched.push([name, payload]))
  assert.deepEqual(dispatched, [
    ['plugin:event', { pluginId: 'a', topic: 'progress' }], ['plugins:changed', null],
    ['interactions:changed', interaction], ['interactions:changed', download],
  ])
})

test('missing notification history resynchronizes plugin and device interaction snapshots', () => {
  const dispatched: [string, unknown][] = []
  dispatchRemoteNotifications({ cursor: 300, missed: true, events: [{ kind: 'plugin_event', payload: 'new' }] },
    (name, payload) => dispatched.push([name, payload]))
  assert.deepEqual(dispatched, [['plugins:changed', null], ['interactions:changed', null], ['plugin:event', 'new']])
})
