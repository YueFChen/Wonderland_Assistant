import assert from 'node:assert/strict'
import { test } from 'node:test'
import { githubManifest } from '../tools/prepare-github.mjs'

function source() {
  const nsis = { url: 'https://updates.yuef.me/releases/0.2.1/Core.exe', signature: 'original-nsis-signature' }
  return {
    version: '0.2.1', notes: 'CF notes', pub_date: '2026-10-27T01:49:14.206Z',
    platforms: {
      'windows-x86_64': { ...nsis },
      'windows-x86_64-nsis': nsis,
      'windows-x86_64-msi': { url: 'https://updates.yuef.me/releases/0.2.1/Core.msi', signature: 'original-msi-signature' },
    },
  }
}

test('GitHub index preserves CF version, date, platform mapping and original signatures', () => {
  const original = source()
  const manifest = githubManifest(original, 'YueFChen/Wonderland_Assistant', 'Current release notes')
  assert.equal(manifest.version, original.version)
  assert.equal(manifest.pub_date, original.pub_date)
  assert.equal(manifest.notes, 'Current release notes')
  assert.deepEqual(Object.keys(manifest.platforms), Object.keys(original.platforms))
  for (const [platform, entry] of Object.entries(manifest.platforms)) {
    assert.equal(entry.signature, original.platforms[platform].signature)
    const name = platform.endsWith('-msi') ? 'Core.msi' : 'Core.exe'
    assert.equal(entry.url, `https://github.com/YueFChen/Wonderland_Assistant/releases/download/v0.2.1/${name}`)
  }
  assert.deepEqual(original, source())
})

test('GitHub preparation rejects foreign assets, incomplete bundles and invalid repositories', () => {
  const foreign = source()
  foreign.platforms['windows-x86_64'].url = 'https://example.com/Core.exe'
  assert.throws(() => githubManifest(foreign, 'owner/repo', 'notes'), /Cloudflare origin/)
  const incomplete = source()
  delete incomplete.platforms['windows-x86_64-msi']
  assert.throws(() => githubManifest(incomplete, 'owner/repo', 'notes'), /NSIS and MSI/)
  assert.throws(() => githubManifest(source(), '../owner/repo', 'notes'), /repository/)
})
