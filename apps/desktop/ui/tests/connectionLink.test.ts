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
  assert.throws(() => connectionLink('https://example.com', 'invalid\nkey'))
})

test('consumes scanned links before routing and clears even malformed credentials', () => {
  let clean = ''
  assert.equal(consumeConnectionLink(connectionLink('https://core.example.com', token), (url) => { clean = url }), token)
  assert.equal(clean, 'https://core.example.com/#/')
  assert.equal(consumeConnectionLink(clean, () => { throw new Error('Must consume once') }), null)
  assert.equal(consumeConnectionLink('https://core.example.com/#connect=bad%key', (url) => { clean = url }), null)
  assert.equal(clean, 'https://core.example.com/#/')
  assert.equal(consumeConnectionLink('https://core.example.com/#/tool/test/main', () => { throw new Error('Keep deep link') }), null)
})

test('optional and custom keys round-trip without leaking into HTTP paths', () => {
  assert.equal(connectionLink('http://192.168.1.23:17890/', ''), 'http://192.168.1.23:17890/#/')
  const key = '我的 Core #密钥 & 100%'
  const link = connectionLink('https://core.example.com/', key)
  let cleaned = ''
  assert.equal(consumeConnectionLink(link, (url) => { cleaned = url }), key)
  assert.equal(cleaned, 'https://core.example.com/#/')
  assert.equal(new URL(link).search, '')
})
