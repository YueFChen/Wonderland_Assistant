import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

export const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

export function checkReleaseTag(tag) {
  const config = JSON.parse(readFileSync(path.join(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'))
  const workspace = readFileSync(path.join(root, 'Cargo.toml'), 'utf8').match(/\[workspace.package\][\s\S]*?version\s*=\s*"([^"]+)"/)[1]
  const npm = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8')).version
  const desktop = JSON.parse(readFileSync(path.join(root, 'apps/desktop/package.json'), 'utf8')).version
  const ui = JSON.parse(readFileSync(path.join(root, 'apps/desktop/ui/package.json'), 'utf8')).version
  if (tag !== `v${config.version}` || [workspace, npm, desktop, ui].some(version => version !== config.version)) throw new Error('Core tag, Tauri, Cargo and npm versions must match.')
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.argv[2] === '--status' && process.argv.length === 3) {
      const { version } = JSON.parse(readFileSync(path.join(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'))
      checkReleaseTag(`v${version}`)
      console.log(JSON.stringify({ version, tag: `v${version}` }, null, 2))
    } else if (process.argv[2] === '--tag' && process.argv.length === 4) {
      checkReleaseTag(process.argv[3])
      console.log('Core release versions passed.')
    } else throw new Error('Usage: node scripts/check-core-release.mjs --status | --tag vX.Y.Z')
  } catch (error) { console.error(error.message); process.exitCode = 1 }
}
