import assert from 'node:assert/strict'
import { test } from 'node:test'
import { readUpdateResponse } from '../tools/publish.mjs'

test('readback recovers transient HTTP and interrupted bodies without retrying writes', async () => {
  const requests = []
  const result = await readUpdateResponse('https://updates.yuef.me/releases/0.2.1/installer.exe', async (url, options) => {
    requests.push({ url, options })
    if (requests.length === 1) return new Response('', { status: 503 })
    if (requests.length === 2) return new Response(new ReadableStream({
      start(controller) { controller.error(new Error('connection reset during body')) },
    }))
    return new Response('complete-installer', { headers: { 'content-type': 'application/octet-stream' } })
  })
  assert.equal(await result.text(), 'complete-installer')
  assert.equal(result.headers.get('content-type'), 'application/octet-stream')
  assert.equal(requests.length, 3)
  assert.ok(requests.every(({ options }) => options.method === undefined && options.redirect === 'error'))
})

test('absence and disabled channels retain their meaning; persistent failures stop', async () => {
  for (const status of [204, 404]) {
    let calls = 0
    const result = await readUpdateResponse('https://updates.yuef.me/stable/latest.json', async () => {
      calls += 1
      return new Response(null, { status })
    })
    assert.equal(result.status, status)
    assert.equal(calls, 1)
  }
  let calls = 0
  await assert.rejects(readUpdateResponse('https://updates.yuef.me/stable/latest.json', async () => {
    calls += 1
    throw new Error('connection failed')
  }), /bounded retries/)
  assert.equal(calls, 3)
})
