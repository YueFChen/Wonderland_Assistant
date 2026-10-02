/** Fragments stay in the browser: HTTP requests and proxy logs never receive the key. */
export function connectionLink(address: string, token: string): string {
  const url = new URL(address)
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || !/^[a-f0-9]{64}$/.test(token)) {
    throw new Error('Invalid connection link')
  }
  url.hash = `connect=${token}`
  return url.href
}

/** Call once, before mounting HashRouter; also remove malformed connection fragments. */
export function consumeConnectionLink(href: string, replace: (cleanUrl: string) => void): string | null {
  const url = new URL(href)
  if (!url.hash.startsWith('#connect=')) return null
  const token = url.hash.slice('#connect='.length)
  url.hash = '/'
  replace(url.href)
  return /^[a-f0-9]{64}$/.test(token) ? token : null
}
