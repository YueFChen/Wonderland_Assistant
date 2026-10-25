import assert from 'node:assert/strict'
import { test } from 'node:test'
import { closeWorkspaceActivity, emptyWorkspaceLayout, reduceWorkspaceLayout } from '../src/plugins/workspaceActions.ts'

function opened(ids: string[], active: string) {
  return { ...emptyWorkspaceLayout(), openContributionIds: ids, activeContributionId: active }
}

test('active close picks the right neighbor, then the nearest left neighbor', () => {
  const ids = ['a/main', 'b/main', 'c/main', 'd/main']
  const middle = closeWorkspaceActivity(opened(ids, 'b/main'), 'b/main', 'b/main', ids)
  assert.equal(middle.layout.activeContributionId, 'c/main')
  assert.equal(middle.routeChanged, true)
  assert.equal(middle.focusId, 'c/main')
  const last = closeWorkspaceActivity(opened(ids, 'd/main'), 'd/main', 'd/main', ids)
  assert.equal(last.layout.activeContributionId, 'c/main')
})

test('closing an inactive tab keeps the current activity and route', () => {
  const ids = ['a/main', 'b/main', 'c/main']
  const result = closeWorkspaceActivity(opened(ids, 'b/main'), 'a/main', 'b/main', ids)
  assert.deepEqual(result.layout.openContributionIds, ['b/main', 'c/main'])
  assert.equal(result.layout.activeContributionId, 'b/main')
  assert.equal(result.routeChanged, false)
  assert.equal(result.focusId, 'b/main')
})

test('close accounts for a current route not yet recorded in persisted tabs', () => {
  const result = closeWorkspaceActivity(opened(['a/main'], 'a/main'), 'b/main', 'b/main', ['a/main', 'b/main'])
  assert.equal(result.closed, true)
  assert.equal(result.layout.activeContributionId, 'a/main')
  assert.deepEqual(result.layout.openContributionIds, ['a/main'])
})

test('opening and selecting use one ordering rule and retain at most eight surfaces', () => {
  let layout = emptyWorkspaceLayout()
  for (let i = 0; i < 10; i++) layout = reduceWorkspaceLayout(layout, { type: 'openActivity', id: `p${i}/main` })
  assert.equal(layout.openContributionIds.length, 8)
  assert.equal(layout.openContributionIds[0], 'p2/main')
  const before = [...layout.openContributionIds]
  layout = reduceWorkspaceLayout(layout, { type: 'openActivity', id: 'p3/main' })
  assert.deepEqual(layout.openContributionIds, before)
  assert.equal(layout.activeContributionId, 'p3/main')
})

test('route synchronization removes obsolete IDs and does not resurrect a removed plugin', () => {
  const layout = opened(['gone/main', 'a/main'], 'gone/main')
  const next = reduceWorkspaceLayout(layout, { type: 'syncActivities', availableIds: ['a/main'], activeId: 'gone/main' })
  assert.deepEqual(next.openContributionIds, ['a/main'])
  assert.equal(next.activeContributionId, null)
})

test('explicit CLI pin/hide actions and GUI toggles apply the same invariant', () => {
  const initial = { ...emptyWorkspaceLayout(), hiddenIds: ['a/main'] }
  const toggle = reduceWorkspaceLayout(initial, { type: 'pin', id: 'a/main' })
  const explicit = reduceWorkspaceLayout(initial, { type: 'pin', id: 'a/main', value: true })
  assert.deepEqual(toggle, explicit)
  assert.deepEqual(explicit.hiddenIds, [])
  const hidden = reduceWorkspaceLayout(explicit, { type: 'hide', id: 'a/main', value: true })
  assert.deepEqual(hidden.pinnedIds, [])
  assert.deepEqual(hidden.hiddenIds, ['a/main'])
})

test('pinned ordering skips historical unpinned entries rather than moving into an invisible slot', () => {
  const current = { ...emptyWorkspaceLayout(), pinnedIds: ['a/main', 'b/main', 'c/main'], orderIds: ['a/main', 'old/main', 'b/main', 'c/main'] }
  const moved = reduceWorkspaceLayout(current, { type: 'move', id: 'b/main', direction: -1 })
  assert.deepEqual(moved.orderIds.filter(id => moved.pinnedIds.includes(id)), ['b/main', 'a/main', 'c/main'])
  assert(moved.orderIds.includes('old/main'))
  assert.equal(reduceWorkspaceLayout(moved, { type: 'move', id: 'b/main', direction: -1 }), moved)
})

test('reordering open tabs keeps the active activity, membership and launcher/sidebar order unchanged', () => {
  const current = { ...opened(['a/main', 'b/main', 'c/main'], 'b/main'), pinnedIds: ['c/main'], activityOrderIds: ['c/main', 'a/main', 'b/main'] }
  const moved = reduceWorkspaceLayout(current, { type: 'reorderTabs', id: 'a/main', targetId: 'c/main' })
  assert.deepEqual(moved.openContributionIds, ['b/main', 'c/main', 'a/main'])
  assert.equal(moved.activeContributionId, 'b/main')
  assert.deepEqual(moved.pinnedIds, current.pinnedIds)
  assert.deepEqual(moved.activityOrderIds, current.activityOrderIds)
  assert.equal(reduceWorkspaceLayout(current, { type: 'reorderTabs', id: 'gone/main', targetId: 'b/main' }), current)
  assert.equal(reduceWorkspaceLayout(current, { type: 'reorderTabs', id: 'a/main', targetId: 'missing/main' }), current)
})
