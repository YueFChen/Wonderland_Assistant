import assert from 'node:assert/strict'
import { test } from 'node:test'
import { connectionLink, consumeConnectionLink } from '../src/core/connectionLink.ts'

const token = 'a'.repeat(64)
test('QR links contain credentials only in the fragment, for LAN and HTTPS', () => {
  for (const address of ['http://192.168.1.23:17890/', 'https://core.example.com']) {
    const link = new URL(connectionLink(address, token))
    assert.equal(link.hash, `#connect=${token}`)
    assert.equal(link.search, '')
    assert.ok(!`${link.origin}${link.pathname}${link.search}`.includes(token))
  }
  assert.throws(() => connectionLink('javascript:alert(1)', token))
  assert.throws(() => connectionLink('https://user:pass@example.com', token))
  assert.throws(() => connectionLink('https://example.com', 'invalid'))
})

test('consumes scanned links before routing and clears even malformed credentials', () => {
  let clean = ''
  assert.equal(consumeConnectionLink(connectionLink('https://core.example.com', token), (url) => { clean = url }), token)
  assert.equal(clean, 'https://core.example.com/#/')
  assert.equal(consumeConnectionLink(clean, () => { throw new Error('Must consume once') }), null)
  assert.equal(consumeConnectionLink('https://core.example.com/#connect=bad%20key', (url) => { clean = url }), null)
  assert.equal(clean, 'https://core.example.com/#/')
  assert.equal(consumeConnectionLink('https://core.example.com/#/tool/test/main', () => { throw new Error('Keep deep link') }), null)
})
