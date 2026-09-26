import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export type ProxyMode = 'system' | 'direct' | 'custom'

export type ProxyEnvironmentVariable = {
  name: string
  configured: boolean
}

export type WindowsSystemProxyState = {
  supported: boolean
  enabled: boolean
  serverConfigured: boolean
}

export type ProxySettingsView = {
  mode: ProxyMode
  address: string | null
  bypass: string
  credentialsConfigured: boolean
  credentialsAvailable: boolean
  credentialsMatchProxy: boolean
  windowsSystemProxy: WindowsSystemProxyState
  environmentVariables: ProxyEnvironmentVariable[]
  warning: string | null
}

export type ProxySettingsUpdate = {
  mode: ProxyMode
  address: string
  bypass: string
  username: string
  password: string
  clearCredentials: boolean
}

export type ProxyTestResult = {
  success: boolean
  route: ProxyMode
  outcome: 'connected' | 'timeout' | 'connection_failed' | 'proxy_auth_required' | 'http_error' | 'invalid_response' | 'request_failed'
  httpStatus: number | null
  durationMs: number
}

export type CoreUpdateAvailable = {
  updateId: string
  version: string
  date: string | null
  body: string | null
}

export type CoreUpdateProgress = {
  downloadedBytes: number
  totalBytes: number | null
  finished: boolean
}

export const networkApi = {
  proxyGet: () => invoke<ProxySettingsView>('network_proxy_get'),
  proxySet: (update: ProxySettingsUpdate) => invoke<ProxySettingsView>('network_proxy_set', { update }),
  proxyTest: () => invoke<ProxyTestResult>('network_proxy_test'),
  updateCheck: () => invoke<CoreUpdateAvailable | null>('core_update_check'),
  updateInstall: (updateId: string) => invoke<void>('core_update_install', { updateId }),
  onUpdateProgress: (handler: (progress: CoreUpdateProgress) => void) =>
    listen<CoreUpdateProgress>('core-update-progress', (event) => handler(event.payload)),
  onUpdateInvalidated: (handler: () => void) =>
    listen<void>('core-update-invalidated', () => handler()),
}
