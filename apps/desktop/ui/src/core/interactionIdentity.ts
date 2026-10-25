/** Per-tab ownership survives reconnect/reload, without storing authentication. */
export class InteractionIdentity {
  private saved?: { sessionId: string; clientId: string }
  private readonly storage: () => Pick<Storage, 'getItem' | 'setItem'> | undefined
  constructor(storage: () => Pick<Storage, 'getItem' | 'setItem'> | undefined) { this.storage = storage }

  forSession(sessionId: string | undefined, candidate: string): string {
    if (!sessionId) return candidate
    if (!this.saved) {
      try {
        const value: unknown = JSON.parse(this.storage()?.getItem('wonderland.web.interaction-owner.v1') ?? 'null')
        if (value && typeof value === 'object' && 'sessionId' in value && 'clientId' in value
          && typeof value.sessionId === 'string' && typeof value.clientId === 'string'
          && /^[a-f0-9]{64}$/.test(value.clientId)) this.saved = { sessionId: value.sessionId, clientId: value.clientId }
      } catch { /* Storage can be disabled; keep the current page's in-memory owner. */ }
    }
    const clientId = this.saved?.sessionId === sessionId ? this.saved.clientId : candidate
    this.saved = { sessionId, clientId }
    try { this.storage()?.setItem('wonderland.web.interaction-owner.v1', JSON.stringify(this.saved)) } catch { /* page-local fallback */ }
    return clientId
  }
}
