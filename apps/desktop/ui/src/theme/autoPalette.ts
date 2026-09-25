export interface AutoPalette {
  sourceKey: string
  mode: 'light' | 'dark'
  variables: Record<string, string>
}

const FALLBACK_HUE = 264
const SAMPLE_SIZE = 56

/** Derive a readable accent palette from the visible colors in an imported image. */
export async function paletteFromImage(sourceKey: string, dataUrl: string): Promise<AutoPalette> {
  const image = new Image()
  image.src = dataUrl
  await image.decode()

  const scale = Math.min(1, SAMPLE_SIZE / Math.max(image.naturalWidth, image.naturalHeight))
  const canvas = document.createElement('canvas')
  canvas.width = Math.max(1, Math.round(image.naturalWidth * scale))
  canvas.height = Math.max(1, Math.round(image.naturalHeight * scale))
  const context = canvas.getContext('2d', { willReadFrequently: true })
  if (!context) throw new Error('Canvas sampling is unavailable')
  context.drawImage(image, 0, 0, canvas.width, canvas.height)

  const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data
  const hueBins = Array.from({ length: 36 }, () => ({ weight: 0, x: 0, y: 0 }))
  let totalAlpha = 0
  let luminanceSum = 0
  let saturationSum = 0
  let redSum = 0
  let greenSum = 0
  let blueSum = 0

  for (let index = 0; index < pixels.length; index += 4) {
    const alpha = pixels[index + 3] / 255
    if (alpha <= 0.02) continue

    const red = pixels[index]
    const green = pixels[index + 1]
    const blue = pixels[index + 2]
    const hsl = rgbToHsl(red, green, blue)
    totalAlpha += alpha
    redSum += red * alpha
    greenSum += green * alpha
    blueSum += blue * alpha
    luminanceSum += relativeLuminance(red, green, blue) * alpha
    saturationSum += hsl.saturation * alpha

    if (hsl.saturation > 0.18 && hsl.lightness > 0.1 && hsl.lightness < 0.9) {
      const bin = Math.min(35, Math.floor(hsl.hue / 10))
      const weight = alpha * hsl.saturation ** 1.7 * (0.7 + 1 - Math.abs(hsl.lightness - 0.5))
      const radians = (hsl.hue * Math.PI) / 180
      hueBins[bin].weight += weight
      hueBins[bin].x += Math.cos(radians) * weight
      hueBins[bin].y += Math.sin(radians) * weight
    }
  }

  if (totalAlpha === 0) return paletteFromColor(sourceKey, '#1b1830')

  const dominantHue = hueBins.reduce((best, current) => current.weight > best.weight ? current : best)
  const hue = dominantHue.weight > 0
    ? normalizeHue((Math.atan2(dominantHue.y, dominantHue.x) * 180) / Math.PI)
    : FALLBACK_HUE
  return createPalette(
    sourceKey,
    { red: redSum / totalAlpha, green: greenSum / totalAlpha, blue: blueSum / totalAlpha },
    hue,
    saturationSum / totalAlpha,
    luminanceSum / totalAlpha,
  )
}

/** A user-selected solid background supplies the palette color directly. */
export function paletteFromColor(sourceKey: string, hex: string): AutoPalette {
  const color = parseHex(hex)
  if (!color) return createPalette(sourceKey, { red: 27, green: 24, blue: 48 }, FALLBACK_HUE, 0.68)
  const hsl = rgbToHsl(color.red, color.green, color.blue)
  return createPalette(
    sourceKey,
    color,
    hsl.saturation > 0.12 ? hsl.hue : FALLBACK_HUE,
    hsl.saturation,
    relativeLuminance(color.red, color.green, color.blue),
  )
}

function createPalette(
  sourceKey: string,
  background: Rgb,
  hue: number,
  sourceSaturation: number,
  sampledLuminance = relativeLuminance(background.red, background.green, background.blue),
): AutoPalette {
  const mode = sampledLuminance >= 0.52 ? 'light' : 'dark'
  const saturation = clamp(0.58 + sourceSaturation * 0.24, 0.58, 0.82)
  const lightnesses = mode === 'dark'
    ? [0.9, 0.81, 0.71, 0.62, 0.51, 0.4]
    : [0.9, 0.76, 0.54, 0.45, 0.36, 0.27]
  const shades = lightnesses.map((lightness) => hslToRgb(hue, saturation, lightness))
  const [brand200, brand300, brand400, brand500, brand600, brand700] = shades
  const accent = mode === 'dark' ? brand400 : brand600
  const accentInk = mode === 'dark' ? brand200 : brand700
  const pageLightness = mode === 'dark' ? 0.1 : 0.96
  const pageSaturation = mode === 'dark' ? 0.24 : 0.16
  const scrim = mode === 'dark'
    ? `linear-gradient(135deg, rgb(0 0 0 / 0.48), rgb(0 0 0 / 0.30) 48%, hsl(${Math.round(hue)} ${Math.round(saturation * 100)}% 12% / 0.38))`
    : `linear-gradient(135deg, rgb(255 255 255 / 0.34), rgb(255 255 255 / 0.18) 48%, hsl(${Math.round(hue)} ${Math.round(saturation * 70)}% 94% / 0.24))`

  return {
    sourceKey,
    mode,
    variables: {
      '--color-brand-200': toHex(brand200),
      '--color-brand-300': toHex(brand300),
      '--color-brand-400': toHex(brand400),
      '--color-brand-500': toHex(brand500),
      '--color-brand-600': toHex(brand600),
      '--color-brand-700': toHex(brand700),
      '--app-accent': toHex(accent),
      '--app-accent-ink': toHex(accentInk),
      '--app-accent-soft': toRgba(accent, mode === 'dark' ? 0.2 : 0.13),
      '--app-accent-line': toRgba(accent, 0.46),
      '--app-page': toHsl(hue, pageSaturation, pageLightness),
      '--app-glow-1': toRgba(accent, mode === 'dark' ? 0.28 : 0.16),
      '--app-glow-2': toRgba(brand700, mode === 'dark' ? 0.22 : 0.12),
      '--app-glow-3': toRgba(brand500, mode === 'dark' ? 0.13 : 0.08),
      '--app-title-from': toHsl(hue, mode === 'dark' ? 0.18 : 0.28, mode === 'dark' ? 0.98 : 0.18),
      '--app-title-to': `hsl(${Math.round(hue)} ${Math.round((mode === 'dark' ? 0.24 : 0.32) * 100)}% ${mode === 'dark' ? '86%' : '30%'} / 0.72)`,
      '--app-custom-scrim': scrim,
    },
  }
}

interface Rgb {
  red: number
  green: number
  blue: number
}

function parseHex(value: string): Rgb | null {
  const match = /^#?([\da-f]{6})$/i.exec(value)
  if (!match) return null
  const numeric = Number.parseInt(match[1], 16)
  return { red: numeric >> 16, green: (numeric >> 8) & 255, blue: numeric & 255 }
}

function rgbToHsl(red: number, green: number, blue: number) {
  const r = red / 255
  const g = green / 255
  const b = blue / 255
  const max = Math.max(r, g, b)
  const min = Math.min(r, g, b)
  const difference = max - min
  const lightness = (max + min) / 2
  let hue = FALLBACK_HUE
  let saturation = 0

  if (difference > 0) {
    saturation = difference / (1 - Math.abs(2 * lightness - 1))
    if (max === r) hue = 60 * (((g - b) / difference) % 6)
    else if (max === g) hue = 60 * ((b - r) / difference + 2)
    else hue = 60 * ((r - g) / difference + 4)
  }
  return { hue: normalizeHue(hue), saturation, lightness }
}

function hslToRgb(hue: number, saturation: number, lightness: number): Rgb {
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation
  const section = normalizeHue(hue) / 60
  const secondary = chroma * (1 - Math.abs((section % 2) - 1))
  const [red, green, blue] = section < 1
    ? [chroma, secondary, 0]
    : section < 2
      ? [secondary, chroma, 0]
      : section < 3
        ? [0, chroma, secondary]
        : section < 4
          ? [0, secondary, chroma]
          : section < 5
            ? [secondary, 0, chroma]
            : [chroma, 0, secondary]
  const offset = lightness - chroma / 2
  return {
    red: Math.round((red + offset) * 255),
    green: Math.round((green + offset) * 255),
    blue: Math.round((blue + offset) * 255),
  }
}

function relativeLuminance(red: number, green: number, blue: number): number {
  const linear = [red, green, blue].map((value) => {
    const channel = value / 255
    return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  })
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]
}

function toHex({ red, green, blue }: Rgb): string {
  return `#${[red, green, blue].map((value) => Math.round(value).toString(16).padStart(2, '0')).join('')}`
}

function toRgba({ red, green, blue }: Rgb, alpha: number): string {
  return `rgba(${Math.round(red)}, ${Math.round(green)}, ${Math.round(blue)}, ${alpha})`
}

function toHsl(hue: number, saturation: number, lightness: number): string {
  return `hsl(${Math.round(hue)} ${Math.round(saturation * 100)}% ${Math.round(lightness * 100)}%)`
}

function normalizeHue(hue: number): number {
  return (hue + 360) % 360
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value))
}
