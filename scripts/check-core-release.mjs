import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

export const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

export function releaseStatus(policy) {
  if (policy.schemaVersion !== 1 || typeof policy.allowRelease !== 'boolean'
    || typeof policy.reason !== 'string' || !policy.reason.trim()
    || !Array.isArray(policy.requiredPlugins) || !policy.requiredPlugins.length
    || policy.requiredPlugins.some(id => typeof id !== 'string' || !/^[a-z][a-z0-9_]*$/.test(id))
    || new Set(policy.requiredPlugins).size !== policy.requiredPlugins.length
    || !policy.integratedPlugins || Array.isArray(policy.integratedPlugins)) throw new Error('Invalid Core release policy.')
  const pending = policy.requiredPlugins.filter(id => {
    const entry = policy.integratedPlugins[id]
    return !entry || typeof entry.version !== 'string' || !/^\d+\.\d+\.\d+$/.test(entry.version)
      || typeof entry.evidence !== 'string' || !entry.evidence.trim()
  })
  return { allowed: policy.allowRelease && pending.length === 0, reason: policy.reason, pending }
}

export function assertReleaseAllowed(policy) {
  const status = releaseStatus(policy)
  if (!status.allowed) throw new Error(`GitHub Core Release held: ${status.reason} Pending: ${status.pending.join(', ') || 'explicit release approval'}`)
}

export function readPolicy() {
  return JSON.parse(readFileSync(path.join(root, '.github/core-release-policy.json'), 'utf8'))
}

export function checkReleaseTag(tag) {
  const config = JSON.parse(readFileSync(path.join(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'))
  const workspace = readFileSync(path.join(root, 'Cargo.toml'), 'utf8').match(/\[workspace.package\][\s\S]*?version\s*=\s*"([^"]+)"/)[1]
  const npm = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8')).version
  if (tag !== `v${config.version}` || config.version !== workspace || workspace !== npm) throw new Error('Core tag, Tauri, Cargo and npm versions must match.')
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.argv[2] === '--status' && process.argv.length === 3) console.log(JSON.stringify(releaseStatus(readPolicy()), null, 2))
    else if (process.argv[2] === '--tag' && process.argv.length === 4) {
      assertReleaseAllowed(readPolicy())
      checkReleaseTag(process.argv[3])
      console.log('Core release policy and versions passed.')
    } else throw new Error('Usage: node scripts/check-core-release.mjs --status | --tag vX.Y.Z')
  } catch (error) { console.error(error.message); process.exitCode = 1 }
}
