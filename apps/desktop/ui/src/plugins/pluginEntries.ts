interface Entry {
  id: string
  pluginId: string
  kind: 'activity' | 'view'
  defaultOrder: number
}

/** Stable launcher identity: prefer the first Activity; View-only plugins still have an entry. */
export function primaryPluginEntries<T extends Entry>(contributions: readonly T[]): T[] {
  const sorted = [...contributions].sort((a, b) => a.defaultOrder - b.defaultOrder || a.id.localeCompare(b.id))
  const primary = new Map<string, T>()
  for (const entry of sorted) {
    const current = primary.get(entry.pluginId)
    if (!current || (current.kind === 'view' && entry.kind === 'activity')) primary.set(entry.pluginId, entry)
  }
  return [...primary.values()].sort((a, b) => a.defaultOrder - b.defaultOrder || a.id.localeCompare(b.id))
}
