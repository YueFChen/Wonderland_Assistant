import assert from 'node:assert/strict'
import { test } from 'node:test'
import { primaryPluginEntries } from '../src/plugins/pluginEntries.ts'

test('one entry per plugin, preferring primary activity over auxiliary views', () => {
  const entries = [
    { pluginId: 'editor', id: 'editor/images', kind: 'view' as const, defaultOrder: 0 },
    { pluginId: 'editor', id: 'editor/second', kind: 'activity' as const, defaultOrder: 20 },
    { pluginId: 'editor', id: 'editor/main', kind: 'activity' as const, defaultOrder: 10 },
    { pluginId: 'viewer', id: 'viewer/view', kind: 'view' as const, defaultOrder: 30 },
  ]
  assert.deepEqual(primaryPluginEntries(entries).map((item) => item.id), ['editor/main', 'viewer/view'])
  assert.deepEqual(primaryPluginEntries([...entries].reverse()).map((item) => item.id), ['editor/main', 'viewer/view'])
  assert.equal(entries[0].id, 'editor/images')
  assert.deepEqual(primaryPluginEntries([]), [])
})
