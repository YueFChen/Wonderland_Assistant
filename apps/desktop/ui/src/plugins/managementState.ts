export interface ManagementOperation {
  id: string
  label: string
  kind: 'management' | 'local' | 'catalog'
  phase?: string
  downloadedBytes?: number
  totalBytes?: number
  cancellable?: boolean
  cancelRequested?: boolean
  batch?: boolean
  position?: number
  totalItems?: number
}

/** One admission point for every desktop plugin mutation, including both installers. */
export function createManagementState() {
  let operation: ManagementOperation | null = null
  let owner: symbol | null = null
  const listeners = new Set<() => void>()
  const notify = () => { for (const listener of listeners) listener() }
  return {
    getSnapshot: () => operation,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener) } },
    begin(value: ManagementOperation) {
      if (owner) return null
      const token = Symbol()
      owner = token
      operation = value
      notify()
      return {
        update(changes: Partial<ManagementOperation>) { if (owner === token && operation) { operation = { ...operation, ...changes }; notify() } },
        cancelled: () => owner === token && operation?.cancelRequested === true,
        finish() { if (owner === token) { owner = null; operation = null; notify() } },
      }
    },
    progress(value: { operationId: string; phase: string; downloadedBytes: number; totalBytes: number; cancellable: boolean }) {
      if (operation?.id !== value.operationId) return
      operation = { ...operation, phase: value.phase, downloadedBytes: value.downloadedBytes, totalBytes: value.totalBytes, cancellable: value.cancellable }
      notify()
    },
    requestCancel() { if (operation && (operation.cancellable || operation.batch)) { operation = { ...operation, cancelRequested: true }; notify() } },
  }
}

export const pluginManagement = createManagementState()
