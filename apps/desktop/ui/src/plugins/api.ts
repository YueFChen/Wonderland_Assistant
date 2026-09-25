import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { useEffect, useState } from 'react'
import type { PluginRuntimeState } from '@wonderland/plugin-protocol'

import type { MessageKey } from '../i18n'

export interface PluginEventPayload {
  pluginId: string
  topic: string
  requestId: string | null
  payload: unknown
}

/** Core 只暴露稳定的插件宿主命令，不按插件业务拆分 Tauri IPC。 */
export const pluginApi = {
  states: () => invoke<PluginRuntimeState[]>('plugins_list'),
  install: () => invoke<PluginRuntimeState[]>('plugins_install'),
  remove: (pluginId: string, removePluginData = false) =>
    invoke<PluginRuntimeState[]>('plugins_remove', { pluginId, removePluginData }),
  setEnabled: (pluginId: string, enabled: boolean) =>
    invoke<PluginRuntimeState[]>('plugins_set_enabled', { pluginId, enabled }),
  setCapabilities: (pluginId: string, capabilities: string[]) =>
    invoke<PluginRuntimeState[]>('plugins_set_capabilities', { pluginId, capabilities }),
  uiUrl: (pluginId: string) => invoke<string>('plugins_ui_url', { pluginId }),
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
    const timer = window.setInterval(refresh, 2_000)
    return () => {
      live = false
      window.clearInterval(timer)
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
