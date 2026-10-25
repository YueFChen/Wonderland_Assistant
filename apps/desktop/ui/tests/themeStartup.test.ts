import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { runInNewContext } from 'node:vm'

const startup = readFileSync(new URL('../index.html', import.meta.url), 'utf8').match(/<script>([\s\S]*?)<\/script>/)![1]
function firstPaint(cached: string | null, dark: boolean, storageFails = false) {
  const dataset: Record<string, string> = {}
  runInNewContext(startup, { localStorage: { getItem() { if (storageFails) throw Error('Unavailable'); return cached } }, matchMedia: () => ({ matches: dark }), document: { documentElement: { dataset } } })
  return dataset.theme
}

test('first paint follows system before settings load when cache is absent, invalid or unavailable', () => {
  for (const cached of [null, 'obsolete']) {
    assert.equal(firstPaint(cached, false), 'light')
    assert.equal(firstPaint(cached, true), 'dark')
  }
  assert.equal(firstPaint(null, false, true), 'light')
})

test('first paint preserves explicit light/dark and uses system while custom image sampling is pending', () => {
  assert.equal(firstPaint('dark', false), 'dark')
  assert.equal(firstPaint('light', true), 'light')
  assert.equal(firstPaint('system', false), 'light')
  assert.equal(firstPaint('custom', false), 'light')
})
