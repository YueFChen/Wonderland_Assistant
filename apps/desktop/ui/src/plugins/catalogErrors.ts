import type { MessageKey } from '../i18n'

const messages: Record<string, MessageKey> = {
  INVALID_DISTRIBUTION_SOURCE: 'settings.plugins.distribution.invalidSource',
  CREDENTIAL_STORE_UNAVAILABLE: 'settings.plugins.distribution.storeUnavailable',
  DISTRIBUTION_NOT_CONFIGURED: 'settings.plugins.distribution.unconfigured',
  CATALOG_CHANGED: 'settings.plugins.distribution.catalogChanged',
  NOT_AVAILABLE: 'settings.plugins.distribution.notAvailable',
  RATE_LIMITED: 'settings.plugins.distribution.rateLimited',
  ACCESS_CODE_INVALID: 'settings.plugins.distribution.credential.invalid',
  ACCESS_CODE_EXPIRED: 'settings.plugins.distribution.credential.expired',
  ACCESS_CODE_REVOKED: 'settings.plugins.distribution.credential.revoked',
  INVALID_CATALOG: 'settings.plugins.distribution.invalidResponse',
  INVALID_UPDATE_MANIFEST: 'settings.plugins.distribution.invalidResponse',
  INVALID_UPDATE_SIGNATURE: 'settings.plugins.distribution.invalidSignature',
  PACKAGE_CHECKSUM_MISMATCH: 'settings.plugins.distribution.invalidPackage',
  CATALOG_MISMATCH: 'settings.plugins.distribution.invalidPackage',
  RESOURCE_LIMIT: 'settings.plugins.distribution.resourceLimit',
  INCOMPATIBLE_CORE: 'settings.plugins.catalog.incompatible',
  CATALOG_VERSION_NOT_NEWER: 'settings.plugins.distribution.noNewerVersion',
  CATALOG_INSTALL_MODE_MISMATCH: 'settings.plugins.distribution.installModeChanged',
  SOURCE_CHANGE_APPROVAL_REQUIRED: 'settings.plugins.distribution.sourceApproval',
  INVALID_REQUEST: 'settings.plugins.distribution.invalidRequest',
  DISTRIBUTION_UNAVAILABLE: 'settings.plugins.distribution.unavailable',
  INVALID_DISTRIBUTION_RESPONSE: 'settings.plugins.distribution.invalidResponse',
  PUBLISHER_IDENTITY_CONFLICT: 'settings.plugins.distribution.identityConflict',
  PUBLISHER_IDENTITY_CHANGED: 'settings.plugins.distribution.identityConflict',
  PUBLISHER_IDENTITY_UNAVAILABLE: 'settings.plugins.distribution.identityUnavailable',
  PUBLISHER_IDENTITY_PERSIST_FAILED: 'settings.plugins.distribution.identitySaveFailed',
  PACKAGE_SIGNATURE_REQUIRED: 'settings.plugins.distribution.packageSignatureRequired',
  INVALID_PACKAGE_SIGNATURE: 'settings.plugins.distribution.invalidPackageSignature',
  UNSUPPORTED_PACKAGE_SIGNATURE: 'settings.plugins.distribution.unsupportedPackageSignature',
  INSTALL_RECOVERY_REQUIRED: 'settings.plugins.distribution.recoveryRequired',
  PLUGIN_START_FAILED: 'settings.plugins.distribution.startFailed',
  PLUGIN_BUSY: 'settings.plugins.distribution.busy',
  LOCAL_IO: 'settings.plugins.distribution.localIo',
  CANCELLED: 'settings.plugins.distribution.cancelled',
}

/** Never echo arbitrary server/IPC messages, which can contain credentials. */
export function catalogErrorMessageKey(code: string): MessageKey {
  return Object.hasOwn(messages, code) ? messages[code] : 'settings.plugins.distribution.operationFailed'
}
