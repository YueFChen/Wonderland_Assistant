import assert from 'node:assert/strict'
import { test } from 'node:test'
import { imageCoverCrop, paletteFromColor, paletteFromImage, paletteFromPixels } from '../src/theme/autoPalette.ts'

type Rgb = number[]
function color(value: string): Rgb {
  if (value.startsWith('#')) return [1, 3, 5].map(offset => parseInt(value.slice(offset, offset + 2), 16))
  return value.match(/[\d.]+/g)!.map(Number)
}
function over(value: string, background: Rgb): Rgb {
  const rgb = color(value), alpha = rgb[3] ?? 1
  return rgb.slice(0, 3).map((channel, i) => channel * alpha + background[i] * (1 - alpha))
}
function luminance(rgb: Rgb) {
  return rgb.slice(0, 3).map(channel => channel / 255).map(channel => channel <= .04045 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4).reduce((sum, channel, i) => sum + channel * [.2126, .7152, .0722][i], 0)
}
function contrast(a: Rgb, b: Rgb) {
  const x = luminance(a), y = luminance(b)
  return (Math.max(x, y) + .05) / (Math.min(x, y) + .05)
}

test('bright and medium-light backgrounds produce light components rather than a fixed navy surface', () => {
  for (const hex of ['#ffffff', '#bdbdbd', '#f4e7c1', '#ccebea']) {
    const palette = paletteFromColor(hex, hex)
    assert.equal(palette.mode, 'light', hex)
    assert(luminance(color(palette.variables['--app-field'])) > .7)
    assert.notEqual(palette.variables['--app-sidebar-surface'], '#181f36')
  }
  assert.equal(paletteFromColor('night', '#171a26').mode, 'dark')
})

test('wallpaper tint stays restrained and the character accent remains stable across different hues', () => {
  const warm = paletteFromColor('warm', '#e6c27a').variables
  const cool = paletteFromColor('cool', '#87d9ea').variables
  for (const key of ['--app-glass', '--app-field', '--app-sidebar-surface']) {
    const a = color(warm[key]), b = color(cool[key])
    assert(a.slice(0, 3).every((channel, i) => Math.abs(channel - b[i]) <= 8), key)
  }
  assert.equal(warm['--app-accent'], '#61528f')
  assert.equal(cool['--app-accent'], '#61528f')
  assert.equal(paletteFromColor('red-night', '#20080a').variables['--app-accent'], '#b7b1f1')
  assert.equal(paletteFromColor('blue-night', '#080a20').variables['--app-accent'], '#b7b1f1')
  assert(!Object.keys(warm).some(key => /radius|font-size|time-/.test(key)))
})

test('text, selected items, primary buttons and fields retain contrast across different hues and brightnesses', () => {
  for (const hex of ['#ffffff', '#f4e7c1', '#bdbdbd', '#5084a0', '#bc6088', '#64ba70', '#101820', '#502752', '#ad4b23', '#ffff00', '#000000']) {
    const vars = paletteFromColor(hex, hex).variables
    for (const background of [[0, 0, 0], [255, 255, 255]]) {
      for (const surface of ['--app-glass', '--app-glass-subtle', '--app-glass-hover', '--app-field', '--app-dialog-surface', '--app-sidebar-surface']) {
        for (const ink of ['--app-ink', '--app-ink-muted', '--app-ink-faint']) assert(contrast(color(vars[ink]), over(vars[surface], background)) >= 4.5, `${hex} ${ink} on ${surface}`)
      }
    }
    assert(contrast(color(vars['--app-accent-ink']), color(vars['--app-accent-soft'])) >= 4.5, hex + ' selected')
    assert(contrast(color(vars['--app-primary-ink']), color(vars['--app-accent'])) >= 4.5, hex + ' primary')
    assert(contrast(color(vars['--app-field-border']), color(vars['--app-field'])) >= 3, hex + ' field border')
    for (const state of ['danger', 'success', 'warning']) assert(contrast(color(vars[`--app-${state}`]), color(vars[`--app-${state}-soft`])) >= 4.5, hex + ' ' + state)
  }
})

test('transparent images follow the fallback canvas and ignore hidden pixel colors', () => {
  const transparent = [255, 0, 0, 0, 0, 255, 0, 0]
  assert.equal(paletteFromPixels('transparent', transparent, 'light').mode, 'light')
  assert.equal(paletteFromPixels('transparent', transparent, 'dark').mode, 'dark')
  assert.deepEqual(paletteFromPixels('x', [255, 0, 0, 0], 'light'), paletteFromPixels('x', [0, 0, 255, 0], 'light'))
})

test('busy images keep component text readable on their own surfaces', () => {
  const palette = paletteFromPixels('split', [0, 0, 0, 255, 255, 255, 255, 255])
  const vars = palette.variables
  for (const pixel of [[0, 0, 0], [255, 255, 255]]) {
    for (const surface of ['--app-glass', '--app-glass-subtle', '--app-field', '--app-sidebar-surface']) {
      assert(contrast(color(vars['--app-ink-muted']), over(vars[surface], pixel)) >= 4.5, surface)
    }
  }
})

test('exposed text sampling still matches the actual cover crop on wide and narrow windows', () => {
  assert.deepEqual(imageCoverCrop(4000, 1000, 400, 800), { x: 1750, y: 0, width: 500, height: 1000 })
  assert.deepEqual(imageCoverCrop(1000, 4000, 800, 400), { x: 0, y: 1750, width: 1000, height: 500 })
  assert.throws(() => imageCoverCrop(0, 1000, 800, 600))
})

test('global image sampling always includes the whole image, independent of viewport geometry', async () => {
  const originalImage = Object.getOwnPropertyDescriptor(globalThis, 'Image')
  const originalDocument = Object.getOwnPropertyDescriptor(globalThis, 'document')
  const calls: number[][] = []
  const canvas = { width: 0, height: 0, getContext: () => ({
    drawImage: (_image: unknown, ...args: number[]) => calls.push(args),
    getImageData: () => ({ data: new Uint8ClampedArray([0, 0, 0, 255, 255, 255, 255, 255]) }),
  }) }
  Object.defineProperty(globalThis, 'Image', { configurable: true, value: class { src = ''; naturalWidth = 4000; naturalHeight = 1000; decode() { return Promise.resolve() } } })
  Object.defineProperty(globalThis, 'document', { configurable: true, value: { createElement: () => canvas } })
  try {
    const palette = await paletteFromImage('wide', 'synthetic')
    assert.deepEqual(calls, [[0, 0, 64, 16]])
    assert.deepEqual(palette, paletteFromPixels('wide', [0, 0, 0, 255, 255, 255, 255, 255]))
  } finally {
    if (originalImage) Object.defineProperty(globalThis, 'Image', originalImage)
    else Reflect.deleteProperty(globalThis, 'Image')
    if (originalDocument) Object.defineProperty(globalThis, 'document', originalDocument)
    else Reflect.deleteProperty(globalThis, 'document')
  }
})

test('image complexity strengthens one shared panel opacity while overlays remain opaque', () => {
  const flat = paletteFromColor('flat', '#777777').variables
  const busy = paletteFromPixels('busy', [0, 0, 0, 255, 255, 255, 255, 255]).variables
  assert(color(busy['--app-glass'])[3] > color(flat['--app-glass'])[3])
  for (const vars of [flat, busy]) {
    assert.equal(color(vars['--app-glass'])[3], color(vars['--app-glass-subtle'])[3])
    assert(vars['--app-sidebar-surface'].startsWith('#'))
    assert(vars['--app-dialog-surface'].startsWith('#'))
  }
})

test('global component colors include user opacity while keeping a stable underlying page', () => {
  const pixels = [0, 0, 0, 255, 255, 255, 255, 255]
  const full = paletteFromPixels('split', pixels, 'light')
  const faded = paletteFromPixels('split', pixels, 'light', { opacity: .25 })
  assert.equal(faded.variables['--app-page'], full.variables['--app-page'])
  assert.notEqual(faded.variables['--app-glass'], full.variables['--app-glass'])
  assert.equal(faded.mode, 'light')
  assert.deepEqual(paletteFromPixels('split', pixels, 'light', { opacity: 0 }).variables, paletteFromColor('page', full.variables['--app-page']).variables)
})
