import type { CatalogInstallMode, PluginCatalogSnapshot } from './api.ts'
type Item = PluginCatalogSnapshot['plugins'][number]
export function catalogAction(item: Item): 'install' | 'update' | 'repair' | 'rollback' | 'unavailable' {
  return item.action ?? (!item.entry ? 'unavailable' : !item.installedVersion ? 'install' : item.installedVersion === item.entry.version ? 'repair' : 'update')
}
export function catalogInstallMode(item: Item): CatalogInstallMode {
  const action = catalogAction(item)
  return action === 'repair' || action === 'rollback' ? action : 'update'
}
export const catalogActionLabels = { install: '安装', update: '更新', repair: '修复安装', rollback: '回退安装', unavailable: '暂不可用' }
