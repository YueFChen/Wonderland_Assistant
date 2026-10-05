/** Fragments stay in the browser: HTTP requests and proxy logs never receive the key. */
export function connectionLink(address: string, token: string): string {
  const url = new URL(address)
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || token.length > 1024 || /[\x00-\x1f\x7f]/.test(token)) {
    throw new Error('Invalid connection link')
  }
  url.hash = token ? `connect=${encodeURIComponent(token)}` : '/'
  return url.href
}

/** Call once, before mounting HashRouter; also remove malformed connection fragments. */
export function consumeConnectionLink(href: string, replace: (cleanUrl: string) => void): string | null {
  const url = new URL(href)
  if (!url.hash.startsWith('#connect=')) return null
  const encoded = url.hash.slice('#connect='.length)
  url.hash = '/'
  replace(url.href)
  try {
    const token = decodeURIComponent(encoded)
    return token.length <= 1024 && !/[\x00-\x1f\x7f]/.test(token) ? token : null
  } catch { return null }
}
