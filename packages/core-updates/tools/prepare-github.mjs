#!/usr/bin/env node
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { checkReleaseTag } from '../../../scripts/check-core-release.mjs'
import { UPDATE_ORIGIN, validateManifest } from '../src/manifest.mjs'
import { prepareRelease, readUpdateResponse } from './publish.mjs'

export function githubManifest(source, repository, notes) {
  validateManifest(source)
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) throw new Error('Invalid GitHub repository.')
  if (!source.platforms['windows-x86_64-nsis'] || !source.platforms['windows-x86_64-msi']) throw new Error('Both NSIS and MSI updates are required.')
  const manifest = structuredClone(source)
  manifest.notes = notes
  for (const entry of Object.values(manifest.platforms)) {
    const name = new URL(entry.url).pathname.split('/').at(-1)
    entry.url = `https://github.com/${repository}/releases/download/v${source.version}/${name}`
  }
  return manifest
}

async function prepareGithub(directory, repository, tag) {
  checkReleaseTag(tag)
  const response = await readUpdateResponse(`${UPDATE_ORIGIN}/stable/latest.json`)
  if (response.status !== 200) throw new Error(`Cloudflare stable index returned HTTP ${response.status}.`)
  const source = validateManifest(await response.json())
  if (tag !== `v${source.version}`) throw new Error('GitHub tag must match the published Cloudflare version.')
  const notes = await readFile(new URL('../../../RELEASE_NOTES.md', import.meta.url), 'utf8')
  const manifest = githubManifest(source, repository, notes)
  await mkdir(directory) // A partial or previous delivery must be inspected before retrying.
  const urls = new Set(Object.values(source.platforms).map(entry => entry.url))
  for (const url of urls) {
    const name = new URL(url).pathname.split('/').at(-1)
    for (const suffix of ['', '.sig']) {
      const asset = await readUpdateResponse(url + suffix)
      if (asset.status !== 200) throw new Error(`Cloudflare asset returned HTTP ${asset.status}: ${name}${suffix}`)
      await writeFile(path.join(directory, name + suffix), Buffer.from(await asset.arrayBuffer()))
    }
  }
  const { files } = await prepareRelease(directory, source)
  const latest = Buffer.from(JSON.stringify(manifest, null, 2) + '\n')
  await writeFile(path.join(directory, 'latest.json'), latest)
  const checksums = [...files].map(([name, { bytes }]) => [name, bytes])
  checksums.push(['latest.json', latest])
  await writeFile(path.join(directory, 'SHA256SUMS.txt'), checksums.map(([name, bytes]) =>
    `${createHash('sha256').update(bytes).digest('hex')}  ${name}`).join('\n') + '\n')
  console.log(`Verified original Cloudflare Core ${source.version} installers for GitHub: ${directory}`)
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.argv.length !== 5) throw new Error('Usage: node tools/prepare-github.mjs <new-output-directory> <owner/repository> <vX.Y.Z>')
    await prepareGithub(path.resolve(process.argv[2]), process.argv[3], process.argv[4])
  } catch (error) { console.error(error.message); process.exitCode = 1 }
}
