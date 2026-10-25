import assert from 'node:assert/strict'
import { test } from 'node:test'
import { canInstallCatalogItem, createDistributionCatalogState } from '../src/plugins/distributionCatalogState.ts'
import type { DistributionSettingsView, PluginCatalogSnapshot } from '../src/plugins/api.ts'

const settings: DistributionSettingsView = {
  sourceUrl: 'https://plugins.example.com', configured: true, credentialConfigured: true,
  credentialAvailable: true, credentialState: 'valid', expiresAt: null, lastCheckedAt: null, warning: null,
}

function catalog(id = 'restricted-plugin'): PluginCatalogSnapshot {
  return {
    sourceUrl: settings.sourceUrl, generatedAt: '2026-10-13T00:00:00Z', fetchedAt: '2026-10-13T00:00:00Z',
    stale: false, status: 'ready', credentialState: 'valid', retryAfterSeconds: null, message: null,
    plugins: [{
      registration: { id, name: id, description: '', author: 'Example', visibility: 'restricted', updateManifestUrl: '', signingPublicKey: '' },
      entry: {
        id, name: id, description: '', author: 'Example', sourceUrl: settings.sourceUrl!, version: '1.0.0',
        releaseNotesUrl: null, downloadUrl: '', sha256: '', sizeBytes: 1,
        hostCompatibility: { minCoreVersion: '0.1.0', maxCoreVersionExclusive: '1.0.0', protocol: { minVersion: '3.0.0', maxVersionExclusive: '4.0.0' } },
        uiBridgeCompatibility: null, platform: { os: 'windows', architecture: 'x86_64', abi: 'msvc' }, dependencies: [],
      },
      compatible: true, installedVersion: null, installable: true, stale: false,
    }],
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0))

test('replacing a code clears the old view immediately and rejects its late catalog result', async () => {
  const old = deferred<PluginCatalogSnapshot>()
  const save = deferred<DistributionSettingsView>()
  let reads = 0
  const store = createDistributionCatalogState({
    settings: async () => settings,
    catalog: async () => ++reads === 1 ? catalog('before') : reads === 2 ? old.promise : catalog('new-code'),
    configure: () => save.promise,
  })
  await store.refresh()
  const slow = store.refresh()
  const replacing = store.configure({ accessCode: 'test-new-code' })
  assert.equal(store.getSnapshot().catalog, null)
  assert.equal(store.getSnapshot().saving, true)
  save.resolve(settings)
  await replacing
  old.resolve(catalog('late-old-code'))
  await slow
  assert.equal(store.getSnapshot().catalog?.plugins[0].registration.id, 'new-code')
  assert.equal(JSON.stringify(store.getSnapshot()).includes('test-new-code'), false)
})

test('a configuration event during its own command response performs one current reload', async () => {
  const save = deferred<DistributionSettingsView>()
  let reads = 0
  let currentSettings = settings
  const store = createDistributionCatalogState({
    settings: async () => currentSettings,
    catalog: async () => {
      reads++
      const snapshot = catalog('current')
      snapshot.credentialState = currentSettings.credentialState
      if (!currentSettings.credentialConfigured) snapshot.plugins[0].registration.visibility = 'public'
      return snapshot
    },
    configure: () => save.promise,
  })
  await store.refresh()
  const pending = store.configure({ clearAccessCode: true })
  currentSettings = { ...settings, credentialState: 'none', credentialConfigured: false }
  store.reset()
  await store.refresh()
  save.resolve(currentSettings)
  await pending
  assert.equal(reads, 2)
  assert.equal(store.getSnapshot().loading, false)
  assert.equal(store.getSnapshot().saving, false)
  assert.equal(store.getSnapshot().catalog?.plugins[0].registration.id, 'current')
  assert.equal(store.getSnapshot().catalog?.plugins[0].registration.visibility, 'public')
  assert.equal(store.getSnapshot().settings?.credentialConfigured, false)
})

test('failed credential saving keeps the previous settings and hides raw errors', async () => {
  const store = createDistributionCatalogState({
    settings: async () => settings,
    catalog: async () => catalog(),
    configure: async () => { throw { code: 'CREDENTIAL_STORE_UNAVAILABLE', message: 'unsafe echoed-code' } },
  })
  await store.refresh()
  await store.configure({ accessCode: 'echoed-code' })
  assert.deepEqual(store.getSnapshot().settings, settings)
  assert.equal(store.getSnapshot().catalog?.plugins.length, 1)
  assert.equal(store.getSnapshot().errorCode, 'CREDENTIAL_STORE_UNAVAILABLE')
  assert.equal(JSON.stringify(store.getSnapshot()).includes('echoed-code'), false)
  assert.equal(canInstallCatalogItem(store.getSnapshot().catalog, store.getSnapshot().catalog!.plugins[0]), true)
})

test('a newer restricted-scope result wins over a slow earlier refresh', async () => {
  const old = deferred<PluginCatalogSnapshot>()
  let reads = 0
  const store = createDistributionCatalogState({
    settings: async () => settings,
    catalog: async () => ++reads === 1 ? old.promise : { ...catalog(), plugins: [] },
    configure: async () => settings,
  })
  const first = store.refresh()
  await flush()
  await store.refresh()
  old.resolve(catalog('removed-by-later-sync'))
  await first
  assert.deepEqual(store.getSnapshot().catalog?.plugins, [])
})

test('rejected refresh retains display only while public items remain installable after invalid-code fallback', async () => {
  let failed = false
  const store = createDistributionCatalogState({
    settings: async () => settings,
    catalog: async () => { if (failed) throw new Error('transport failure'); return catalog() },
    configure: async () => settings,
  })
  await store.refresh()
  failed = true
  await store.refresh()
  const offline = store.getSnapshot().catalog!
  assert.equal(offline.plugins.length, 1)
  assert.equal(canInstallCatalogItem(offline, offline.plugins[0]), false)
  const fallback = catalog()
  fallback.credentialState = 'revoked'
  fallback.plugins[0].registration.visibility = 'public'
  assert.equal(canInstallCatalogItem(fallback, fallback.plugins[0]), true)
  fallback.plugins[0].registration.visibility = 'restricted'
  assert.equal(canInstallCatalogItem(fallback, fallback.plugins[0]), false)
})

test('offline, throttled, or invalid snapshots and stale individual releases cannot enable installation', () => {
  for (const status of ['offline', 'rate_limited', 'unavailable', 'invalid_response', 'unconfigured'] as const) {
    const snapshot = catalog()
    snapshot.status = status
    assert.equal(canInstallCatalogItem(snapshot, snapshot.plugins[0]), false, status)
  }
  const snapshot = catalog()
  assert.equal(canInstallCatalogItem(snapshot, snapshot.plugins[0]), true)
  snapshot.plugins[0].stale = true
  assert.equal(canInstallCatalogItem(snapshot, snapshot.plugins[0]), false)
})

test('a known expiry observed after catalog reading removes restricted introductions immediately', async () => {
  let reads = 0
  const store = createDistributionCatalogState({
    settings: async () => ++reads === 1 ? settings : { ...settings, credentialState: 'expired' },
    catalog: async () => catalog(),
    configure: async () => settings,
  })
  await store.refresh()
  assert.equal(store.getSnapshot().catalog?.credentialState, 'expired')
  assert.deepEqual(store.getSnapshot().catalog?.plugins, [])
})
