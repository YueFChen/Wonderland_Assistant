import assert from 'node:assert/strict'
import { test } from 'node:test'
import { isRemotelyShareable, remoteDependencySummary } from '../src/plugins/remoteAccess.ts'

test('explicit opt-in and valid metadata allow cold plugins to be shared without activation', () => {
  const state = { enabled: true, runtime: 'running', manifest: { ui: {}, remoteAccess: true, backend: { supportsServiceContext: true } } }
  assert.equal(isRemotelyShareable(state), true)
  for (const remoteAccess of [undefined, null, false]) {
    assert.equal(isRemotelyShareable({ ...state, manifest: { ...state.manifest, remoteAccess } }), false)
  }
  assert.equal(isRemotelyShareable({ ...state, manifest: { ...state.manifest, backend: {} } }), false)
  assert.equal(isRemotelyShareable({ ...state, manifest: { ...state.manifest, ui: null } }), false)
  assert.equal(isRemotelyShareable({ ...state, enabled: false }), false)
  assert.equal(isRemotelyShareable({ ...state, runtime: 'stopped' }), true)
  assert.equal(isRemotelyShareable({ ...state, runtime: 'failed' }), true)
  assert.equal(isRemotelyShareable({ ...state, installation: 'invalid' }), false)
  assert.equal(isRemotelyShareable({ ...state, installation: 'incompatible' }), false)
})

test('root shares declared transitive methods while providers keep independent entries hidden', () => {
  const plugin = (id: string, target?: string) => ({
    enabled: true, runtime: 'running',
    manifest: { id, name: id.toUpperCase(), ui: {}, remoteAccess: id === 'a', backend: { supportsServiceContext: true },
      dependencies: target ? [{ pluginId: target, methods: [`${target}.run`] }] : [] },
  })
  const plugins = [plugin('a', 'b'), plugin('b', 'c'), plugin('c', 'a')]
  assert.deepEqual(remoteDependencySummary(plugins, 'a').map(({ pluginId, methods }) => ({ pluginId, methods })),
    [{ pluginId: 'b', methods: ['b.run'] }, { pluginId: 'c', methods: ['c.run'] }])
  assert.equal(isRemotelyShareable(plugins[1]), false)
  assert.equal(isRemotelyShareable(plugins[2]), false)
})

test('sharing diagnoses disabled providers without treating normal cold state as failure', () => {
  const plugins = [
    { enabled: true, runtime: 'running', manifest: { id: 'a', name: 'A', backend: { supportsServiceContext: true }, dependencies: [
      { pluginId: 'missing', methods: ['run'] }, { pluginId: 'stopped', methods: ['run'] }, { pluginId: 'legacy', methods: ['run'] },
    ] } },
    { enabled: false, runtime: 'stopped', dependencyIssues: ['版本期望不符。'], manifest: { id: 'stopped', name: 'Stopped', backend: { supportsServiceContext: true } } },
    { enabled: true, runtime: 'running', manifest: { id: 'legacy', name: 'Legacy', backend: {} } },
  ]
  const summary = remoteDependencySummary(plugins, 'a')
  assert.deepEqual(summary[0].issues, ['依赖尚未安装。'])
  assert.deepEqual(summary[1].issues, ['版本期望不符。', '依赖已停用。'])
  assert.deepEqual(summary[2].issues, ['依赖不支持传递当前设备上下文。'])
})
