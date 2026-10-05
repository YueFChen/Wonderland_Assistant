import { IMPLICIT_CAPABILITIES } from '@wonderland/plugin-protocol'

/** Network services are available to all plugins; legacy declarations remain readable. */
export function requestedPermissions(capabilities: readonly string[]): string[] {
  return capabilities.filter((capability) => !IMPLICIT_CAPABILITIES.some((implicit) => implicit === capability))
}
