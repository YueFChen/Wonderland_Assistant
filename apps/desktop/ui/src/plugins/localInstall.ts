import type { PluginError, PluginManifest, PluginRuntimeState } from '@wonderland/plugin-protocol'
import { invoke } from '../core/transport'

export interface InstallCandidate {
  key: string; source: string; manifest: PluginManifest | null; signaturePublicKey: string | null
  existing: PluginRuntimeState | null; error: PluginError | null
}
export interface InstallPreview { batchId: string; items: InstallCandidate[] }
export interface InstallResult { key: string; status: string; runtime: string | null; error: PluginError | null }
export type InstallMode = 'files' | 'directory' | 'development'
export const INSTALL_REQUEST = 'wonderland:install-local'
export const localInstall = {
  reset: () => invoke<void>('plugins_install_reset'),
  prepare: (mode: InstallMode | 'uploads', uploadIds?: string[]) => invoke<InstallPreview | null>('plugins_install_prepare', { mode, uploadIds }),
  run: (batchId: string, selected: string[]) => invoke<{ items: InstallResult[]; states: PluginRuntimeState[] }>('plugins_install_run', { batchId, selected }),
  discard: (batchId?: string, uploadIds?: string[]) => invoke<void>('plugins_install_discard', { batchId, uploadIds }),
  uploadOpen: (name: string, size: number) => invoke<string>('plugins_install_upload_open', { name, size }),
  uploadWrite: (uploadId: string, offset: number, contentBase64: string) => invoke<void>('plugins_install_upload_write', { uploadId, offset, contentBase64 }),
}
export function requestLocalInstall(mode: InstallMode) { window.dispatchEvent(new CustomEvent(INSTALL_REQUEST, { detail: mode })) }
export {installSignatureDescription,initialInstallSelection,chooseInstallCandidate,filesFromDrop,type DropEntry} from './localInstallSelection'
