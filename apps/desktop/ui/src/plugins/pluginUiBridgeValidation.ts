export interface PluginUiBridgeIdentity {
  protocol: string
  bridgeVersion: string
  pluginId: string
  contributionId: string
  nonce: string
}

export type PluginUiBridgeValidation = 'accept' | 'bridge-version-mismatch' | 'ignore'

/** Validates bridge identity; negotiable versions apply only to the initial ready response. */
export function validatePluginUiBridgeMessage(
  value: unknown,
  expected: PluginUiBridgeIdentity,
  negotiableBridgeVersions: readonly string[] = [],
): PluginUiBridgeValidation {
  if (typeof value !== 'object' || value === null) return 'ignore'
  const message = value as Partial<PluginUiBridgeIdentity> & { type?: unknown }
  if (
    message.protocol !== expected.protocol
    || message.pluginId !== expected.pluginId
    || message.contributionId !== expected.contributionId
    || message.nonce !== expected.nonce
  ) return 'ignore'

  if (message.bridgeVersion !== expected.bridgeVersion) {
    if (message.type !== 'ready') return 'ignore'
    return negotiableBridgeVersions.includes(message.bridgeVersion ?? '')
      ? 'accept'
      : 'bridge-version-mismatch'
  }
  return 'accept'
}
