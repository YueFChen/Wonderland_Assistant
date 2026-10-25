import { invoke, listen } from '../core/transport'
import { useSyncExternalStore } from 'react'
import { createRequestId } from '@wonderland/plugin-ui-sdk'
import type { PluginRuntimeState, PluginDependency, PluginError } from '@wonderland/plugin-protocol'
import { createPluginStateStore } from './pluginStateStore'

import type { MessageKey } from '../i18n'

export interface PluginEventPayload {
  pluginId: string
  topic: string
  requestId: string | null
  payload: unknown
}

export interface PluginCatalogRegistration {
  id: string
  name: string
  description: string
  author: string
  visibility: 'public' | 'restricted'
  updateManifestUrl: string
  signingPublicKey: string
}

export interface PluginCatalogEntry {
  id: string
  signingPublicKey: string
  name: string
  description: string
  author: string
  sourceUrl: string
  version: string
  releaseNotesUrl: string | null
  downloadUrl: string
  sha256: string
  sizeBytes: number
  hostCompatibility: {
    minCoreVersion: string
    maxCoreVersionExclusive: string
    protocol: { minVersion: string; maxVersionExclusive: string }
  }
  uiBridgeCompatibility: { minVersion: string; maxVersionExclusive: string } | null
  platform: { os: string; architecture: string; abi: string }
  dependencies: PluginDependency[]
}

export type DistributionCredentialState = 'none' | 'unchecked' | 'valid' | 'invalid' | 'expired' | 'revoked' | 'unavailable'

export interface DistributionSettingsView {
  sourceUrl: string | null
  configured: boolean
  credentialConfigured: boolean
  credentialAvailable: boolean
  credentialState: DistributionCredentialState
  expiresAt: string | null
  lastCheckedAt: string | null
  warning: string | null
}

export interface DistributionConfigureRequest {
  sourceUrl?: string
  accessCode?: string
  clearAccessCode?: boolean
}

export interface PluginCatalogSnapshot {
  sourceUrl: string | null
  generatedAt: string | null
  fetchedAt: string | null
  stale: boolean
  status: 'ready' | 'unconfigured' | 'offline' | 'rate_limited' | 'unavailable' | 'invalid_response'
  credentialState: DistributionCredentialState
  retryAfterSeconds: number | null
  message: string | null
  plugins: Array<{
    registration: PluginCatalogRegistration
    entry: PluginCatalogEntry | null
    compatible: boolean
    installedVersion: string | null
    installable: boolean
    action?: 'install' | 'update' | 'repair' | 'rollback' | 'unavailable'
    stale: boolean
    error?: PluginError | null
  }>
}

export type CatalogInstallMode = 'update' | 'repair' | 'rollback'
export interface RetainedPluginData { id: string; dataDirectory: string; hasData: boolean; hasSecrets: boolean; hasPublisherIdentity: boolean; hasLifecycle?: boolean }

export interface PluginUiLaunchInfo {
  url: string
  /** Core and the plugin manifest's declared range, newest compatible bridge first. */
  bridgeVersions: string[]
}

/** Core 只暴露稳定的插件宿主命令，不按插件业务拆分 Tauri IPC。 */
export const pluginApi = {
  states: () => invoke<PluginRuntimeState[]>('plugins_list'),
  retryScan: (pluginId: string) =>
    invoke<PluginRuntimeState[]>('plugins_retry_scan', { pluginId }),
  retryRuntime: (pluginId: string) => invoke<PluginRuntimeState[]>('plugins_retry_runtime', { pluginId }),
  catalog: () => invoke<PluginCatalogSnapshot>('plugins_catalog_list'),
  distributionSettings: () => invoke<DistributionSettingsView>('plugins_distribution_settings'),
  configureDistribution: (request: DistributionConfigureRequest) =>
    invoke<DistributionSettingsView>('plugins_distribution_configure', { request }),
  installCatalog: (pluginId: string, version: string, expectedSha256: string, approvedSourceChange: boolean, mode: CatalogInstallMode = 'update', operationId?: string, expectedSourceUrl?: string) =>
    invoke<PluginRuntimeState[]>('plugins_catalog_install', {
      request: { pluginId, version, expectedSha256, approvedSourceChange, mode, operationId, expectedSourceUrl },
    }),
  cancelCatalogInstall: (operationId: string) => invoke<boolean>('plugins_catalog_cancel', { operationId }),
  retainedData: () => invoke<RetainedPluginData[]>('plugins_retained_data'),
  cleanupData: (pluginId: string) => invoke<RetainedPluginData[]>('plugins_cleanup_data', { pluginId }),
  uninstall: (pluginId: string, removePluginData = false) =>
    invoke<PluginRuntimeState[]>('plugins_remove', { pluginId, removePluginData }),
  setEnabled: (pluginId: string, enabled: boolean) =>
    invoke<PluginRuntimeState[]>('plugins_set_enabled', { pluginId, enabled }),
  uiUrl: (pluginId: string, requestId = createRequestId(), retry = false) => invoke<PluginUiLaunchInfo>('plugins_ui_url', { pluginId, requestId, retry }),
  callWithId: <T>(pluginId: string, method: string, params: unknown = {}, requestId: string = createRequestId(), timeoutMs?: number) => {
    return {
      requestId,
      promise: invoke<T>('plugin_call', { pluginId, requestId, method, params, ...(timeoutMs === undefined ? {} : { timeoutMs }) }),
    }
  },
  call: <T>(pluginId: string, method: string, params: unknown = {}) =>
    pluginApi.callWithId<T>(pluginId, method, params).promise,
  cancel: (pluginId: string, requestId: string) =>
    invoke<void>('plugin_cancel', { pluginId, requestId }),
  subscribe: async (
    pluginId: string,
    topic: string,
    onEvent: (event: PluginEventPayload) => void,
  ) => listen<PluginEventPayload>('plugin:event', ({ payload }) => {
    if (payload.pluginId === pluginId && payload.topic === topic) onEvent(payload)
  }),
}

const pluginStates = createPluginStateStore<PluginRuntimeState>({
  read: pluginApi.states,
  listen: async (changed) => {
    const retryRead = (event: Event) => {
      if ((event as CustomEvent<string>).detail === 'connected' && pluginStates.getSnapshot().failed) changed()
    }
    window.addEventListener('wonderland:web-connection', retryRead)
    try {
      const stop = await listen('plugins:changed', changed)
      return () => { stop(); window.removeEventListener('wonderland:web-connection', retryRead) }
    } catch (error) {
      window.removeEventListener('wonderland:web-connection', retryRead)
      throw error
    }
  },
  onReset: (reset) => {
    window.addEventListener('wonderland:web-disconnected', reset)
    return () => window.removeEventListener('wonderland:web-disconnected', reset)
  },
})

export function usePlugins() {
  const { states, failed } = useSyncExternalStore(pluginStates.subscribe, pluginStates.getSnapshot)
  const error: MessageKey | null = failed ? 'plugins.stateReadFailed' : null
  return { states, error, setStates: pluginStates.setStates, refresh: pluginStates.refresh }
}

export function pluginState(
  states: PluginRuntimeState[] | null,
  id: string,
): PluginRuntimeState | null {
  return states?.find((state) => state.manifest.id === id) ?? null
}
