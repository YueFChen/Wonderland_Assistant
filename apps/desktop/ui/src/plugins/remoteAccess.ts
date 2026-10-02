/** Remote opt-in is independent of SDK support. Old or unreviewed plugins stay local. */
export function isRemotelyShareable(item: {
  enabled: boolean
  runtime: string
  manifest: { ui?: unknown; remoteAccess?: boolean | null; backend: { supportsServiceContext?: boolean | null } }
}): boolean {
  return item.enabled && item.runtime === 'running' && Boolean(item.manifest.ui)
    && item.manifest.remoteAccess === true && item.manifest.backend.supportsServiceContext === true
}
