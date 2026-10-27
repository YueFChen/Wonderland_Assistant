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

/** Management follows the launcher order; plugins without a launcher remain manageable. */
export function orderPluginCards<T extends { manifest: { id: string } }>(
  states: readonly T[],
  activities: readonly { pluginId: string }[],
): T[] {
  const positions = new Map(activities.map((entry, index) => [entry.pluginId, index]))
  return [...states].sort((left, right) =>
    (positions.get(left.manifest.id) ?? activities.length) - (positions.get(right.manifest.id) ?? activities.length)
    || left.manifest.id.localeCompare(right.manifest.id),
  )
}
