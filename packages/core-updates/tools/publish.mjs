#!/usr/bin/env node
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'
import { checkReleaseTag } from '../../../scripts/check-core-release.mjs'
import { ASSET_NAME, UPDATE_ORIGIN, assetPath, validateManifest } from '../src/manifest.mjs'
import { verifyInstallerSignature } from './signature.mjs'

const BUCKET = 'wonderland-core-releases'
const require = createRequire(import.meta.url)
const digest = value => createHash('sha256').update(value).digest('hex')

export async function prepareRelease(directory, source, publicKey) {
  publicKey ??= JSON.parse(await readFile(new URL('../../../apps/desktop/src-tauri/tauri.conf.json', import.meta.url), 'utf8')).plugins.updater.pubkey
  const manifest = structuredClone(source)
  const files = new Map()
  for (const entry of Object.values(manifest.platforms ?? {})) {
    const url = new URL(entry.url)
    const name = decodeURIComponent(url.pathname.split('/').at(-1))
    if (url.origin !== UPDATE_ORIGIN || url.username || url.password || url.search || url.hash
      || url.pathname !== assetPath(manifest.version, name)
      || !ASSET_NAME.test(name) || !/\.(exe|msi)$/.test(name)) throw new Error('Unexpected Cloudflare updater asset URL.')
    const file = path.join(directory, name)
    const signature = (await readFile(`${file}.sig`, 'utf8')).trim()
    if (!signature || signature !== entry.signature?.trim()) throw new Error(`Updater signature mismatch: ${name}`)
    const bytes = await readFile(file)
    if (!bytes.length) throw new Error(`Empty installer: ${name}`)
    verifyInstallerSignature(bytes, signature, publicKey)
    files.set(name, { file, bytes })
    files.set(`${name}.sig`, { file: `${file}.sig`, bytes: await readFile(`${file}.sig`) })
    entry.url = `${UPDATE_ORIGIN}${assetPath(manifest.version, name)}`
  }
  validateManifest(manifest)
  return { manifest, files }
}

/** Only reads are retried. Finish reading the body before returning, including cold R2 assets. */
export async function readUpdateResponse(url, fetcher = fetch) {
  const deadline = Date.now() + (new URL(url).pathname.startsWith('/releases/') ? 300_000 : 60_000)
  let failure
  for (let attempt = 0; attempt < 3 && Date.now() < deadline; attempt += 1) {
    try {
      const response = await fetcher(url, {
        redirect: 'error', signal: AbortSignal.timeout(Math.max(1, deadline - Date.now())),
        headers: { 'Cache-Control': 'no-cache' },
      })
      if ([502, 503, 504].includes(response.status) && attempt < 2) {
        await response.body?.cancel()
        throw new Error(`Temporary Core readback HTTP ${response.status}`)
      }
      const body = response.body === null ? null : await response.arrayBuffer()
      return new Response(body, { status: response.status, statusText: response.statusText, headers: response.headers })
    } catch (error) {
      failure = error
      if (attempt < 2 && Date.now() < deadline) {
        await new Promise(resolve => setTimeout(resolve, Math.min(500 * (attempt + 1), deadline - Date.now())))
      }
    }
  }
  throw new Error('Core update HTTPS readback failed after bounded retries.', { cause: failure })
}

const get = readUpdateResponse

export async function publish(directory) {
  const source = JSON.parse(await readFile(path.join(directory, 'latest.json'), 'utf8'))
  checkReleaseTag(`v${source.version}`)
  const { manifest, files } = await prepareRelease(directory, source)
  const service = await get(`${UPDATE_ORIGIN}/v1/service`)
  if (!service.ok || (await service.json()).releasesEnabled !== true) throw new Error('Core Cloudflare channel is unavailable or temporarily disabled.')
  const previous = await get(`${UPDATE_ORIGIN}/stable/latest.json`)
  if (previous.status !== 204) {
    if (previous.status !== 200) throw new Error('Cannot read current Core channel.')
    const current = validateManifest(await previous.json())
    const compare = manifest.version.split('.').map(BigInt)
    const old = current.version.split('.').map(BigInt)
    const first = compare.findIndex((value, index) => value !== old[index])
    if (first < 0 || compare[first] < old[first]) throw new Error('Stable Core version must increase; use an explicit recovery procedure for rollback.')
  }
  const wrangler = path.join(path.dirname(require.resolve('wrangler/package.json')), 'bin/wrangler.js')
  function put(key, file, type) {
    const result = spawnSync(process.execPath, [wrangler, 'r2', 'object', 'put', `${BUCKET}/${key}`,
      '--file', file, '--content-type', type, '--remote'], {
      cwd: path.resolve(import.meta.dirname, '..'), stdio: 'inherit',
    })
    if (result.error || result.status !== 0) throw new Error(`R2 upload failed: ${key}`)
  }
  for (const [name, { file, bytes }] of files) {
    const key = assetPath(manifest.version, name).slice(1)
    const existing = await get(`${UPDATE_ORIGIN}/${key}`)
    if (existing.status === 404) put(key, file, 'application/octet-stream')
    else if (!existing.ok || digest(Buffer.from(await existing.arrayBuffer())) !== digest(bytes)) throw new Error(`Immutable Core asset conflict: ${name}`)
    const verified = await get(`${UPDATE_ORIGIN}/${key}`)
    if (!verified.ok || digest(Buffer.from(await verified.arrayBuffer())) !== digest(bytes)) throw new Error(`R2 readback failed: ${name}`)
  }
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'wonderland-core-update-'))
  try {
    const file = path.join(temporary, 'latest.json')
    await writeFile(file, JSON.stringify(manifest, null, 2) + '\n')
    // The pointer is committed last, only after all signed assets have been read back.
    put('channels/stable/latest.json', file, 'application/json')
    const published = await get(`${UPDATE_ORIGIN}/stable/latest.json`)
    if (!published.ok || JSON.stringify(await published.json()) !== JSON.stringify(manifest)) throw new Error('Core stable pointer readback failed; inspect remote state before retrying.')
    console.log(`Core ${manifest.version} verified at ${UPDATE_ORIGIN}/stable/latest.json`)
  } finally { await rm(temporary, { recursive: true, force: true }) }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.argv.length !== 3) throw new Error('Usage: node tools/publish.mjs <downloaded-release-directory>')
    await publish(path.resolve(process.argv[2]))
  } catch (error) { console.error(error.message); process.exitCode = 1 }
}
