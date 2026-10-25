/** Remote opt-in is independent of SDK support. Old or unreviewed plugins stay local. */
export function isRemotelyShareable(item: {
  enabled: boolean
  runtime: string
  installation?: string
  manifest: { ui?: unknown; remoteAccess?: boolean | null; backend: { supportsServiceContext?: boolean | null } }
}): boolean {
  return item.enabled && (item.installation === undefined || item.installation === 'installed') && Boolean(item.manifest.ui)
    && item.manifest.remoteAccess === true && item.manifest.backend.supportsServiceContext === true
}

interface DependencyPlugin {
  manifest: { id: string; name: string; backend: { supportsServiceContext?: boolean | null }; dependencies?: { pluginId: string; methods: string[] }[] }
  enabled: boolean
  runtime: string
  installation?: string
  dependencyIssues?: string[]
}

export interface RemoteDependencySummary { pluginId: string; name: string; methods: string[]; issues: string[] }

/** Summarize delegated providers; opt-in controls direct roots, never dependency delegation. */
export function remoteDependencySummary(plugins: DependencyPlugin[], rootId: string): RemoteDependencySummary[] {
  const visited = new Set<string>([rootId])
  const summaries = new Map<string, RemoteDependencySummary>()
  const pending = [rootId]
  while (pending.length) {
    const id = pending.pop()
    const consumer = plugins.find((plugin) => plugin.manifest.id === id)
    for (const dependency of consumer?.manifest.dependencies ?? []) {
      if (dependency.pluginId === rootId) continue
      const provider = plugins.find((plugin) => plugin.manifest.id === dependency.pluginId)
      let summary = summaries.get(dependency.pluginId)
      if (!summary) {
        const issues = provider ? [...provider.dependencyIssues ?? []] : ['依赖尚未安装。']
        if (provider && !provider.enabled) issues.push('依赖已停用。')
        if (provider && provider.installation !== undefined && provider.installation !== 'installed') issues.push('依赖包不可用。')
        if (provider?.runtime === 'failed') issues.push('依赖启动失败。')
        if (provider && provider.manifest.backend.supportsServiceContext !== true) issues.push('依赖不支持传递当前设备上下文。')
        summary = { pluginId: dependency.pluginId, name: provider?.manifest.name ?? dependency.pluginId, methods: [], issues }
        summaries.set(dependency.pluginId, summary)
      }
      summary.methods = [...new Set([...summary.methods, ...dependency.methods])]
      if (dependency.methods.length && provider?.enabled && (provider.installation === undefined || provider.installation === 'installed')
        && provider.manifest.backend.supportsServiceContext === true && !visited.has(dependency.pluginId)) {
        visited.add(dependency.pluginId)
        pending.push(dependency.pluginId)
      }
    }
  }
  return [...summaries.values()]
}
