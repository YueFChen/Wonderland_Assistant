import { useSyncExternalStore } from 'react'
import { pluginManagement } from './managementState'
import { pluginApi } from './api'

export function usePluginManagement() {
  return useSyncExternalStore(pluginManagement.subscribe, pluginManagement.getSnapshot)
}

const phases: Record<string, string> = { checking: '核对目录与授权', downloading: '下载安装包', verifying: '校验安装包', installing: '安装插件', starting: '启动插件', finishing: '完成安装' }
export function PluginManagementActivity() {
  const operation = usePluginManagement()
  if (!operation || operation.kind !== 'catalog') return null
  const percent = operation.totalBytes ? Math.min(100, Math.round((operation.downloadedBytes ?? 0) / operation.totalBytes * 100)) : null
  return <div className="app-dialog-notice mb-3 flex flex-wrap items-center gap-3" role="status" aria-live="polite">
    <div className="min-w-0 flex-1"><p className="text-sm font-medium">{operation.label}{operation.batch ? ` · ${operation.position}/${operation.totalItems}` : ''}</p><p className="text-xs text-ink-muted">{phases[operation.phase ?? 'checking'] ?? '正在处理'}{operation.phase === 'downloading' && percent !== null ? ` · ${percent}%` : ''}{operation.cancelRequested ? operation.batch ? ' · 已请求取消后续操作' : operation.cancellable ? ' · 正在取消下载' : ' · 当前安装已提交，将完成此项' : ''}</p>{operation.phase === 'downloading' && percent !== null && <progress className="mt-2 h-1.5 w-full accent-[var(--app-accent)]" max={100} value={percent} aria-label="安装包下载进度" />}</div>
    <button type="button" className="app-button" disabled={operation.cancelRequested || !operation.cancellable && !operation.batch} onClick={() => {
      pluginManagement.requestCancel()
      if (operation.cancellable) void pluginApi.cancelCatalogInstall(operation.id).catch(() => {})
    }}>{operation.batch ? '取消后续' : '取消下载'}</button>
  </div>
}
