#!/usr/bin/env node
import { copyFile, mkdir, readFile, readdir, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { UPDATE_ORIGIN, assetPath } from '../src/manifest.mjs'
import { prepareRelease } from './publish.mjs'
import { checkReleaseTag } from '../../../scripts/check-core-release.mjs'

export async function prepareBundle(bundleRoot, output, notes) {
  const version = JSON.parse(await readFile(new URL('../../../apps/desktop/src-tauri/tauri.conf.json', import.meta.url), 'utf8')).version
  checkReleaseTag(`v${version}`)
  const manifest = { version, notes, pub_date: new Date().toISOString(), platforms: {} }
  const inputs = []
  for (const [kind, suffix] of [['nsis', '.exe'], ['msi', '.msi']]) {
    const directory = path.join(bundleRoot, kind)
    const names = (await readdir(directory)).filter(name => name.endsWith(suffix))
    if (names.length !== 1) throw new Error(`Expected exactly one ${kind} installer.`)
    const name = names[0].replaceAll(' ', '.')
    const signature = (await readFile(path.join(directory, `${names[0]}.sig`), 'utf8')).trim()
    const entry = { url: `${UPDATE_ORIGIN}${assetPath(version, name)}`, signature }
    manifest.platforms[`windows-x86_64-${kind}`] = entry
    if (kind === 'nsis') manifest.platforms['windows-x86_64'] = { ...entry }
    for (const extra of ['', '.sig']) inputs.push({ source: path.join(directory, names[0] + extra), name: name + extra })
  }
  await mkdir(output) // Never overwrite an earlier local delivery.
  for (const { source, name } of inputs) await copyFile(source, path.join(output, name))
  const { manifest: verified, files } = await prepareRelease(output, manifest)
  const checksums = []
  for (const [name, { bytes }] of files) checksums.push(`${createHash('sha256').update(bytes).digest('hex')}  ${name}`)
  await writeFile(path.join(output, 'SHA256SUMS.txt'), checksums.join('\n') + '\n')
  await writeFile(path.join(output, 'latest.json'), JSON.stringify(verified, null, 2) + '\n')
  console.log(`Verified signed Core ${version} delivery: ${output}`)
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.argv.length !== 5) throw new Error('Usage: node tools/prepare.mjs <bundle-root> <new-output-directory> <release-notes-file>')
    await prepareBundle(path.resolve(process.argv[2]), path.resolve(process.argv[3]), await readFile(process.argv[4], 'utf8'))
  } catch (error) { console.error(error.message); process.exitCode = 1 }
}
