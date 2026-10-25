import assert from 'node:assert/strict'
import { test } from 'node:test'
import { backgroundTextTone, hexRgb } from '../src/theme/backgroundContrast.ts'

test('exposed copy chooses the higher contrast ink for its local pixels', () => {
  const paper = hexRgb('#f3f2fa')
  assert.equal(backgroundTextTone([10, 12, 18, 255], paper).ink, '#ffffff')
  assert.equal(backgroundTextTone([250, 245, 235, 255], paper).ink, '#000000')
  assert.equal(backgroundTextTone([119, 119, 119, 255], paper).ink, '#000000')
})

test('transparent pixels and reduced image opacity are evaluated on the visible canvas', () => {
  const paper = hexRgb('#ffffff')
  assert.equal(backgroundTextTone([0, 0, 0, 0], paper).ink, '#000000')
  assert.equal(backgroundTextTone([0, 0, 0, 255], paper, 0.25).ink, '#000000')
  assert.equal(backgroundTextTone([255, 255, 255, 255], hexRgb('#111319'), 0).ink, '#ffffff')
})

test('a small highlight does not overturn the ink for a predominantly dark glyph area', () => {
  const pixels = [...Array.from({ length: 9 }, () => [10, 12, 18, 255]).flat(), 255, 255, 255, 255]
  const tone = backgroundTextTone(pixels, hexRgb('#ffffff'))
  assert.equal(tone.ink, '#ffffff')
  assert.equal(tone.coverage, 0.9)
  assert.equal(tone.outline, '#000000')
})
