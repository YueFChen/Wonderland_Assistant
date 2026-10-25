/** Device File references follow Core invocation leases, without an elapsed-time expiry. */
export class RemoteFileReferences {
  private references = new Map<string, { file: File, pending: boolean, revision: number }>()
  private revision = 0

  snapshotRevision() { return this.revision }
  register(handle: string, file: File) {
    this.references.set(handle, { file, pending: true, revision: ++this.revision })
  }
  confirmed(handle: string, synchronize: () => void) {
    const reference = this.references.get(handle)
    if (reference) {
      reference.pending = false; reference.revision = ++this.revision
      // A release event can precede the POST response; request a fresh lease snapshot.
      synchronize()
    }
  }
  get(handle: string) { return this.references.get(handle) }
  delete(handle: string) { this.references.delete(handle) }
  clear() { this.references.clear() }
  retain(handles: ReadonlySet<string>, snapshotRevision: number) {
    for (const [handle, reference] of this.references) {
      if (!reference.pending && reference.revision <= snapshotRevision && !handles.has(handle)) this.references.delete(handle)
    }
  }
}
