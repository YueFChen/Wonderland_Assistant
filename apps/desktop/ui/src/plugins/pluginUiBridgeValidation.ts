export interface PluginUiBridgeIdentity {
  protocol: string
  bridgeVersion: string
  pluginId: string
  contributionId: string
  nonce: string
}

export type PluginUiBridgeValidation = 'accept' | 'bridge-version-mismatch' | 'ignore'

/** Validates the identity and version fields shared by all plugin UI bridge messages. */
export function validatePluginUiBridgeMessage(
  value: unknown,
  expected: PluginUiBridgeIdentity,
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
    return message.type === 'ready' ? 'bridge-version-mismatch' : 'ignore'
  }
  return 'accept'
}
