export type Rgb = readonly [number, number, number]

export function hexRgb(hex: string): Rgb {
  if (!/^#[\da-f]{6}$/i.test(hex)) throw new Error('Invalid background color')
  return [parseInt(hex.slice(1, 3), 16), parseInt(hex.slice(3, 5), 16), parseInt(hex.slice(5, 7), 16)]
}

function luminance(rgb: Rgb) {
  const channels = rgb.map(value => {
    const channel = value / 255
    return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  })
  return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722
}

/** Compare candidates against the pixels actually beneath a label, including image opacity. */
export function backgroundTextTone(pixels: ArrayLike<number>, page: Rgb, opacity = 1) {
  if (!pixels.length || pixels.length % 4 !== 0) throw new Error('Invalid background pixels')
  const light = '#ffffff', dark = '#000000'
  const candidates = [light, dark].map(ink => ({ ink, luminance: luminance(hexRgb(ink)), passing: 0, contrast: 0 }))
  const strength = Math.min(1, Math.max(0, opacity))
  for (let index = 0; index < pixels.length; index += 4) {
    const alpha = pixels[index + 3] / 255 * strength
    const background = luminance([
      pixels[index] * alpha + page[0] * (1 - alpha),
      pixels[index + 1] * alpha + page[1] * (1 - alpha),
      pixels[index + 2] * alpha + page[2] * (1 - alpha),
    ])
    for (const candidate of candidates) {
      const contrast = (Math.max(background, candidate.luminance) + 0.05) / (Math.min(background, candidate.luminance) + 0.05)
      candidate.passing += Number(contrast >= 4.5)
      candidate.contrast += contrast
    }
  }
  candidates.sort((a, b) => b.passing - a.passing || b.contrast - a.contrast)
  const selected = candidates[0], coverage = selected.passing / (pixels.length / 4)
  return { ink: selected.ink, outline: selected.ink === light ? dark : light, coverage }
}
