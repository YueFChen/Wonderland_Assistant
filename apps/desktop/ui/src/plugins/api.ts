import { invoke, listen } from '../core/transport'
import { useEffect, useState } from 'react'
import type { PluginRuntimeState, PluginService, PluginServiceRequirement } from '@wonderland/plugin-protocol'

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
  repositoryUrl: string
  updateManifestUrl: string
  signingPublicKey: string
}

export interface PluginCatalogEntry {
  id: string
  name: string
  description: string
  author: string
  repositoryUrl: string
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
  capabilities: string[]
  networkPublicHosts: string[]
  provides: PluginService[]
  requires: PluginServiceRequirement[]
}

export interface PluginCatalogSnapshot {
  generatedAt: string
  stale: boolean
  plugins: Array<{
    registration: PluginCatalogRegistration
    entry: PluginCatalogEntry | null
    compatible: boolean
    installedVersion: string | null
    installable: boolean
    stale: boolean
  }>
}

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
  catalog: () => invoke<PluginCatalogSnapshot>('plugins_catalog_list'),
  installCatalog: (pluginId: string, version: string, expectedSha256: string, approvedCapabilities: string[], approvedSourceChange: boolean) =>
    invoke<PluginRuntimeState[]>('plugins_catalog_install', {
      request: { pluginId, version, expectedSha256, approvedCapabilities, approvedSourceChange },
    }),
  install: () => invoke<PluginRuntimeState[]>('plugins_install'),
  uninstall: (pluginId: string, removePluginData = false) =>
    invoke<PluginRuntimeState[]>('plugins_remove', { pluginId, removePluginData }),
  setEnabled: (pluginId: string, enabled: boolean) =>
    invoke<PluginRuntimeState[]>('plugins_set_enabled', { pluginId, enabled }),
  setCapabilities: (pluginId: string, capabilities: string[]) =>
    invoke<PluginRuntimeState[]>('plugins_set_capabilities', { pluginId, capabilities }),
  uiUrl: (pluginId: string) => invoke<PluginUiLaunchInfo>('plugins_ui_url', { pluginId }),
  callWithId: <T>(pluginId: string, method: string, params: unknown = {}, requestId: string = crypto.randomUUID()) => {
    return {
      requestId,
      promise: invoke<T>('plugin_call', { pluginId, requestId, method, params }),
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

export function usePlugins() {
  const [states, setStates] = useState<PluginRuntimeState[] | null>(null)
  const [error, setError] = useState<MessageKey | null>(null)

  useEffect(() => {
    let live = true
    const refresh = () => {
      void pluginApi.states()
        .then((next) => {
          if (live) {
            setStates(next)
            setError(null)
          }
        })
        .catch(() => {
          if (live) setError('plugins.stateReadFailed')
        })
    }
    refresh()
    let unlisten: (() => void) | undefined
    void listen('plugins:changed', refresh).then((stop) => {
      if (live) unlisten = stop
      else stop()
    })
    const timer = window.setInterval(refresh, 2_000)
    return () => {
      live = false
      window.clearInterval(timer)
      unlisten?.()
    }
  }, [])

  return { states, error, setStates }
}

export function pluginState(
  states: PluginRuntimeState[] | null,
  id: string,
): PluginRuntimeState | null {
  return states?.find((state) => state.manifest.id === id) ?? null
}
