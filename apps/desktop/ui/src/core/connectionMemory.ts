export interface RememberedConnection {
  token: string
  sessionId?: string
}

const storageKey = 'wonderland:web-connection'

/** localStorage is scoped to this Core origin. Private browsing may disable it. */
export function rememberedConnection(): RememberedConnection | null {
  try {
    const saved = JSON.parse(window.localStorage.getItem(storageKey) ?? 'null') as RememberedConnection | null
    return saved && typeof saved.token === 'string' && saved.token.length <= 1024
      && (saved.sessionId === undefined || typeof saved.sessionId === 'string') ? saved : null
  } catch { return null }
}

export function rememberConnection(connection: RememberedConnection): void {
  try { window.localStorage.setItem(storageKey, JSON.stringify(connection)) } catch { /* Continue in memory. */ }
}

/** An older tab must not erase a newer tab's login. */
export function forgetConnection(token: string, sessionId?: string): void {
  const saved = rememberedConnection()
  if (!saved || saved.token !== token || saved.sessionId !== sessionId) return
  try { window.localStorage.removeItem(storageKey) } catch { /* Storage is unavailable. */ }
}
