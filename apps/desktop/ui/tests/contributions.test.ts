import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { PluginRuntimeState } from '@wonderland/plugin-protocol'
import { buildContributionRegistry, canOpenContribution } from '../src/plugins/contributions.ts'

function plugin(): PluginRuntimeState {
  return {
    manifest: {
      manifestVersion: 3, id: 'consumer', name: 'Consumer', version: '0.1.0',
      hostCompatibility: { minCoreVersion: '0.1.9', maxCoreVersionExclusive: '1.0.0', protocol: { minVersion: '2.0.0', maxVersionExclusive: '3.0.0' } },
      platform: { os: 'windows', architecture: 'x86_64', abi: 'msvc' },
      backend: { entry: 'backend/plugin.exe', transport: 'stdio-ndjson-v1' },
      contract: 'contract.json',
      ui: { entry: 'ui/index.html', bridgeCompatibility: { minVersion: '1.1.0', maxVersionExclusive: '2.0.0' }, integrations: [], contributions: [{ id: 'main', kind: 'activity', title: 'Consumer', defaultOrder: 0 }] },
    },
    installation: 'installed', enabled: true, runtime: 'running', lastError: null,
    installationSource: null,
  }
}

test('dependency warnings keep a running consumer activity ready and openable', () => {
  for (const issue of ['missing', 'disabled', 'version mismatch', 'missing declared method', 'cycle']) {
    const state = { ...plugin(), dependencyIssues: [issue] }
    const [entry] = buildContributionRegistry([state])
    assert.equal(entry.status, 'ready', issue)
    assert.equal(entry.state.dependencyIssues?.[0], issue)
  }
})

test('real installation and process failures still make the activity unavailable', () => {
  assert.equal(buildContributionRegistry([{ ...plugin(), runtime: 'failed' }])[0].status, 'failed')
  assert.equal(buildContributionRegistry([{ ...plugin(), installation: 'incompatible' }])[0].status, 'incompatible')
  assert.equal(buildContributionRegistry([{ ...plugin(), enabled: false }])[0].status, 'disabled')
})

test('enabled cold and starting contributions stay openable without falsifying their runtime', () => {
  for (const runtime of ['stopped', 'starting', 'running'] as const) {
    const [entry] = buildContributionRegistry([{ ...plugin(), runtime }])
    assert.equal(entry.status, 'ready')
    assert.equal(entry.state.runtime, runtime)
    assert.equal(canOpenContribution(entry), true)
  }
  const [failed] = buildContributionRegistry([{ ...plugin(), runtime: 'failed' }])
  assert.equal(failed.status, 'failed')
  assert.equal(canOpenContribution(failed), true, 'an enabled failed page can present its retry UI')
  for (const state of [{ ...plugin(), enabled: false }, { ...plugin(), installation: 'incompatible' as const }, { ...plugin(), runtime: 'stopping' as const }]) {
    assert.equal(canOpenContribution(buildContributionRegistry([state])[0]), false)
  }
})
