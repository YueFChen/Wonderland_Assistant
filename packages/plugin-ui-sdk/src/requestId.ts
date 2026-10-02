/** Secure identifiers also work on LAN HTTP, where crypto.randomUUID is unavailable. */
export function createRequestId(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID()
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (value) => value.toString(16).padStart(2, '0')).join('')
}
