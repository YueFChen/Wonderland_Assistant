import { after, before, test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import os from 'node:os'
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { Miniflare } from 'miniflare'
import { releaseStatus, assertReleaseAllowed, readPolicy, checkReleaseTag } from '../../../scripts/check-core-release.mjs'
import { prepareRelease } from '../tools/publish.mjs'
import { verifyInstallerSignature } from '../tools/signature.mjs'
import { UPDATE_ORIGIN, validateManifest } from '../src/manifest.mjs'

let live, held, bucket
const file = 'Wonderland.Assistant_0.2.0_x64-setup.exe'
const installer = Buffer.from('signed-installer-fixture')
const { publicKey, privateKey } = generateKeyPairSync('ed25519')
const keyId = Buffer.from('12345678')
const encodedKey = Buffer.concat([Buffer.from('Ed'), keyId, publicKey.export({ type: 'spki', format: 'der' }).subarray(-32)])
const tauriPublicKey = Buffer.from(`untrusted comment: fixture\n${encodedKey.toString('base64')}\n`).toString('base64')
const detached = sign(null, createHash('blake2b512').update(installer).digest(), privateKey)
const trusted = 'timestamp:1\tfile:fixture'
const globalSignature = sign(null, Buffer.concat([detached, Buffer.from(trusted)]), privateKey)
const signature = Buffer.from(`untrusted comment: fixture\n${Buffer.concat([Buffer.from('ED'), keyId, detached]).toString('base64')}\ntrusted comment: ${trusted}\n${globalSignature.toString('base64')}\n`).toString('base64')
const valid = () => ({
  version: '0.2.0', notes: 'Core update', pub_date: '2026-10-25T00:00:00Z',
  platforms: { 'windows-x86_64': { url: `${UPDATE_ORIGIN}/releases/0.2.0/${file}`, signature } },
})

before(async () => {
  const modules = Object.fromEntries(await Promise.all(['worker.mjs', 'manifest.mjs'].map(async name =>
    [name, { type: 'esm', contents: await readFile(new URL(`../src/${name}`, import.meta.url), 'utf8') }]
  )))
  const options = enabled => ({ workers: [{ config: {
    name: 'core-updates-test', compatibilityDate: '2026-10-06',
    manifest: { mainModule: 'worker.mjs', modules },
    env: {
      UPDATE_ORIGIN: { type: 'text', value: UPDATE_ORIGIN },
      RELEASES_ENABLED: { type: 'text', value: String(enabled) },
      RELEASES: { type: 'r2', name: 'core-updates-test' },
    },
  } }] })
  live = new Miniflare(options(true))
  held = new Miniflare(options(false))
  bucket = await live.getR2Bucket('RELEASES')
})
after(async () => { await Promise.all([live?.dispose(), held?.dispose()]) })

test('held channel returns 204 and hides installers even if a pointer and files exist', async () => {
  const stored = await held.getR2Bucket('RELEASES')
  await stored.put('channels/stable/latest.json', JSON.stringify(valid()))
  await stored.put(`releases/0.2.0/${file}`, 'candidate')
  const response = await held.dispatchFetch(`${UPDATE_ORIGIN}/stable/latest.json`)
  assert.equal(response.status, 204)
  assert.equal(await response.text(), '')
  assert.equal(response.headers.get('cache-control'), 'no-store')
  assert.equal((await held.dispatchFetch(`${UPDATE_ORIGIN}/releases/0.2.0/${file}`)).status, 404)
  assert.equal((await (await held.dispatchFetch(`${UPDATE_ORIGIN}/v1/service`)).json()).releasesEnabled, false)
})

test('channel rejects unknown hosts, queries, methods and unrecognized paths', async () => {
  for (const url of ['https://unexpected.example/stable/latest.json', `${UPDATE_ORIGIN}/stable/latest.json?preview=true`, `${UPDATE_ORIGIN}/.env`]) {
    assert.equal((await live.dispatchFetch(url)).status, 404)
  }
  const response = await live.dispatchFetch(`${UPDATE_ORIGIN}/stable/latest.json`, { method: 'PUT', body: '{}' })
  assert.equal(response.status, 405)
  assert.equal(response.headers.get('allow'), 'GET, HEAD')
})

test('enabled empty channel also returns 204 without falling back to GitHub', async () => {
  assert.equal((await live.dispatchFetch(`${UPDATE_ORIGIN}/stable/latest.json`)).status, 204)
})

test('valid signed manifest keeps its signature and Cloudflare URL', async () => {
  await bucket.put('channels/stable/latest.json', JSON.stringify(valid()))
  const response = await live.dispatchFetch(`${UPDATE_ORIGIN}/stable/latest.json`)
  assert.equal(response.status, 200)
  assert.deepEqual(await response.json(), valid())
  const head = await live.dispatchFetch(`${UPDATE_ORIGIN}/stable/latest.json`, { method: 'HEAD' })
  assert.equal(head.status, 200)
  assert.equal(await head.text(), '')
})

test('corrupt, oversized and external manifests fail closed', async () => {
  const external = valid()
  external.platforms['windows-x86_64'].url = 'https://github.com/candidate.exe'
  for (const source of ['{invalid', JSON.stringify(external), 'x'.repeat(128 * 1024 + 1)]) {
    await bucket.put('channels/stable/latest.json', source)
    assert.equal((await live.dispatchFetch(`${UPDATE_ORIGIN}/stable/latest.json`)).status, 503)
  }
})

test('private R2 streams installers and supports HEAD and byte ranges', async () => {
  await bucket.put(`releases/0.2.0/${file}`, '0123456789')
  const url = `${UPDATE_ORIGIN}/releases/0.2.0/${file}`
  const full = await live.dispatchFetch(url)
  assert.equal(full.status, 200)
  assert.equal(await full.text(), '0123456789')
  assert.equal(full.headers.get('content-length'), '10')
  const head = await live.dispatchFetch(url, { method: 'HEAD' })
  assert.equal(await head.text(), '')
  assert.equal(head.headers.get('content-length'), '10')
  for (const [range, body, contentRange] of [['bytes=2-5', '2345', 'bytes 2-5/10'], ['bytes=-3', '789', 'bytes 7-9/10']]) {
    const partial = await live.dispatchFetch(url, { headers: { Range: range } })
    assert.equal(partial.status, 206)
    assert.equal(await partial.text(), body)
    assert.equal(partial.headers.get('content-range'), contentRange)
    assert.equal(partial.headers.get('content-length'), String(body.length))
  }
  assert.equal((await live.dispatchFetch(`${UPDATE_ORIGIN}/releases/0.2.0/absent.exe`)).status, 404)
  for (const range of ['bytes=99-', 'bytes=5-2', 'bytes=-0', 'bytes=-', 'bytes=0-1,3-4', 'unknown=1-2']) {
    const invalid = await live.dispatchFetch(url, { headers: { Range: range } })
    assert.equal(invalid.status, 416)
    assert.equal(invalid.headers.get('content-range'), 'bytes */10')
  }
})

test('manifest validation rejects missing signatures and version/path mismatches', () => {
  for (const change of [value => { value.platforms['windows-x86_64'].signature = '' }, value => { value.version = '0.2.1' }, value => { value.pub_date = 'invalid' }]) {
    const manifest = valid()
    change(manifest)
    assert.throws(() => validateManifest(manifest))
  }
})

test('release policy requires an explicit opening and evidence for every required plugin', () => {
  const policy = { schemaVersion: 1, allowRelease: false, reason: 'pending plugins', requiredPlugins: ['one', 'two'], integratedPlugins: {} }
  assert.throws(() => assertReleaseAllowed(policy), /Release held/)
  policy.allowRelease = true
  policy.integratedPlugins.one = { version: '0.1.0', evidence: 'signed stable acceptance receipt' }
  assert.deepEqual(releaseStatus(policy).pending, ['two'])
  assert.throws(() => assertReleaseAllowed(policy), /two/)
  policy.integratedPlugins.two = { version: '0.1.0', evidence: 'signed stable acceptance receipt' }
  assert.doesNotThrow(() => assertReleaseAllowed(policy))
  policy.requiredPlugins.push('two')
  assert.throws(() => releaseStatus(policy), /Invalid/)
})

test('source versions and CF endpoint match independently from the GitHub release hold', async () => {
  const config = JSON.parse(await readFile(new URL('../../../apps/desktop/src-tauri/tauri.conf.json', import.meta.url), 'utf8'))
  assert.doesNotThrow(() => checkReleaseTag(`v${config.version}`))
  assert.throws(() => checkReleaseTag('invalid-tag'), /versions must match/)
  assert.equal(typeof releaseStatus(readPolicy()).allowed, 'boolean')
  assert.deepEqual(config.plugins.updater.endpoints, [`${UPDATE_ORIGIN}/stable/latest.json`])
})

test('publisher accepts verified CF installers independently from GitHub and preserves signatures', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'core-publication-test-'))
  try {
    await writeFile(path.join(directory, file), installer)
    await writeFile(path.join(directory, `${file}.sig`), `${signature}\n`)
    const source = valid()
    const prepared = await prepareRelease(directory, source, tauriPublicKey)
    assert.deepEqual(prepared.manifest, valid())
    assert.equal(prepared.files.size, 2)
    assert.match(source.platforms['windows-x86_64'].url, /^https:\/\/updates.yuef.me/)
    source.platforms['windows-x86_64'].signature = 'wrong-signature'
    await assert.rejects(prepareRelease(directory, source, tauriPublicKey), /signature mismatch/)
    source.platforms['windows-x86_64'].url = `https://github.com/other/repository/releases/download/v0.2.0/${file}`
    await assert.rejects(prepareRelease(directory, source, tauriPublicKey), /Unexpected Cloudflare/)
  } finally { await rm(directory, { recursive: true, force: true }) }
})

test('Tauri installer verification rejects modified contents, signatures and signing identities', () => {
  assert.doesNotThrow(() => verifyInstallerSignature(installer, signature, tauriPublicKey))
  assert.throws(() => verifyInstallerSignature(Buffer.from('modified'), signature, tauriPublicKey), /verification failed/)
  const changed = Buffer.from(signature, 'base64').toString('utf8').replace(trusted, 'timestamp:2')
  assert.throws(() => verifyInstallerSignature(installer, Buffer.from(changed).toString('base64'), tauriPublicKey), /verification failed/)
  const other = Buffer.from(encodedKey)
  other[2] ^= 1
  const otherKey = Buffer.from(`untrusted comment: fixture\n${other.toString('base64')}\n`).toString('base64')
  assert.throws(() => verifyInstallerSignature(installer, signature, otherKey), /signing identity/)
})
