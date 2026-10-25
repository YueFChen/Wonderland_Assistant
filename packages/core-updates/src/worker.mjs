import { ASSET_NAME, VERSION, validateManifest } from './manifest.mjs'

const NO_STORE = { 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' }
const MAX_MANIFEST_BYTES = 128 * 1024

function reply(status, body = null, headers = {}) {
  return new Response(body, { status, headers: { ...NO_STORE, ...headers } })
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url)
    if (url.origin !== env.UPDATE_ORIGIN || url.search) return reply(404)
    if (!['GET', 'HEAD'].includes(request.method)) return reply(405, null, { Allow: 'GET, HEAD' })
    const head = request.method === 'HEAD'
    const enabled = env.RELEASES_ENABLED === 'true'
    if (url.pathname === '/v1/service') {
      return reply(200, head ? null : JSON.stringify({
        service: 'wonderland-core-updates', protocolVersion: 1, releasesEnabled: enabled,
      }), { 'Content-Type': 'application/json; charset=utf-8' })
    }
    if (url.pathname === '/stable/latest.json') {
      // Provisioning the channel never advertises a candidate or an old GitHub Release.
      if (!enabled) return reply(204)
      try {
        const object = await env.RELEASES.get('channels/stable/latest.json')
        if (!object) return reply(204)
        if (object.size > MAX_MANIFEST_BYTES) return reply(503)
        const manifest = validateManifest(await object.json(), env.UPDATE_ORIGIN)
        return reply(200, head ? null : JSON.stringify(manifest), { 'Content-Type': 'application/json; charset=utf-8' })
      } catch { return reply(503) }
    }
    const asset = /^\/releases\/([^/]+)\/([^/]+)$/.exec(url.pathname)
    if (!enabled || !asset || !VERSION.test(asset[1]) || !ASSET_NAME.test(asset[2])) return reply(404)
    const key = url.pathname.slice(1)
    let range, metadata
    if (!head && request.headers.has('Range')) {
      metadata = await env.RELEASES.head(key)
      if (!metadata) return reply(404)
      const match = /^bytes=(\d*)-(\d*)$/.exec(request.headers.get('Range'))
      const start = match?.[1] ? Number(match[1]) : null
      const end = match?.[2] ? Number(match[2]) : null
      const offset = start ?? Math.max(0, metadata.size - end)
      const last = start === null || end === null ? metadata.size - 1 : Math.min(end, metadata.size - 1)
      if (!match || (start === null && end === null) || !Number.isSafeInteger(offset)
        || !Number.isSafeInteger(last) || offset < 0 || last < offset || offset >= metadata.size) {
        return reply(416, null, { 'Content-Range': `bytes */${metadata.size}` })
      }
      range = { offset, length: last - offset + 1 }
    }
    const object = head ? await env.RELEASES.head(key) : await env.RELEASES.get(key, range ? { range } : undefined)
    if (!object) return reply(404)
    const headers = new Headers({
      ...NO_STORE,
      'Content-Type': 'application/octet-stream',
      'Content-Disposition': `attachment; filename="${asset[2]}"`,
      'Accept-Ranges': 'bytes',
      ETag: object.httpEtag,
      'Content-Length': String(range?.length ?? object.size),
    })
    if (range) {
      headers.set('Content-Range', `bytes ${range.offset}-${range.offset + range.length - 1}/${object.size}`)
    }
    return new Response(head ? null : object.body, { status: range ? 206 : 200, headers })
  },
}
