export const UPDATE_ORIGIN = 'https://updates.yuef.me'
export const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/
export const ASSET_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,180}\.(?:exe|msi|sig)$/

export function assetPath(version, name) {
  if (!VERSION.test(version) || !ASSET_NAME.test(name)) throw new Error('Invalid Core asset path.')
  return `/releases/${version}/${name}`
}

export function validateManifest(manifest, origin = UPDATE_ORIGIN) {
  if (!manifest || !VERSION.test(manifest.version) || typeof manifest.notes !== 'string'
    || manifest.notes.length > 20_000 || typeof manifest.pub_date !== 'string'
    || !/^\d{4}-\d{2}-\d{2}T/.test(manifest.pub_date) || !Number.isFinite(Date.parse(manifest.pub_date))) {
    throw new Error('Invalid Core update manifest.')
  }
  const entries = Object.entries(manifest.platforms ?? {})
  if (!entries.length || !manifest.platforms['windows-x86_64']) throw new Error('Windows update missing.')
  for (const [platform, entry] of entries) {
    if (!/^windows-x86_64(?:-nsis|-msi)?$/.test(platform)
      || !entry || typeof entry.signature !== 'string' || !entry.signature.trim()
      || entry.signature.length > 16_384) throw new Error('Invalid update platform or signature.')
    const url = new URL(entry.url)
    const name = url.pathname.split('/').at(-1)
    if (url.origin !== origin || url.username || url.password || url.search || url.hash
      || url.pathname !== assetPath(manifest.version, name) || !/\.(exe|msi)$/.test(name)) {
      throw new Error('Core update assets must use the Core Cloudflare origin and version.')
    }
  }
  return manifest
}
