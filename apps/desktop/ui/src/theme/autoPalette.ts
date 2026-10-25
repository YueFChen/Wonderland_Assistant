export interface AutoPalette {
  sourceKey: string
  mode: 'light' | 'dark'
  variables: Record<string, string>
}

const THEME_HUE = 264
const SAMPLE_SIZE = 64

interface ImagePaletteOptions {
  fallbackMode?: 'light' | 'dark'
  opacity?: number
}

/** The same centered object-cover crop rendered by AppBackdrop. */
export function imageCoverCrop(width: number, height: number, viewportWidth: number, viewportHeight: number) {
  if (![width, height, viewportWidth, viewportHeight].every(value => Number.isFinite(value) && value > 0)) {
    throw new Error('Invalid background dimensions')
  }
  const ratio = viewportWidth / viewportHeight
  const cropWidth = Math.min(width, height * ratio)
  const cropHeight = Math.min(height, width / ratio)
  return { x: (width - cropWidth) / 2, y: (height - cropHeight) / 2, width: cropWidth, height: cropHeight }
}

/** Sample the entire background once; window geometry never changes the component theme. */
export async function paletteFromImage(sourceKey: string, dataUrl: string, options: ImagePaletteOptions = {}): Promise<AutoPalette> {
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

  return paletteFromPixels(sourceKey, context.getImageData(0, 0, canvas.width, canvas.height).data, options.fallbackMode, { opacity: options.opacity })
}

/** Pure sampling entry point, including transparent pixels on the current fallback canvas. */
export function paletteFromPixels(sourceKey: string, pixels: ArrayLike<number>, fallbackMode: 'light' | 'dark' = 'dark', options: { opacity?: number } = {}): AutoPalette {
  if (!pixels.length || pixels.length % 4 !== 0) throw new Error('Invalid background pixels')
  const fallback = parseHex(fallbackMode === 'light' ? '#f3f2fa' : '#171b28')!
  const count = pixels.length / 4
  let luminanceSum = 0
  const luminances: number[] = []
  let redSum = 0
  let greenSum = 0
  let blueSum = 0

  for (let index = 0; index < pixels.length; index += 4) {
    const alpha = pixels[index + 3] / 255
    const red = pixels[index] * alpha + fallback.red * (1 - alpha)
    const green = pixels[index + 1] * alpha + fallback.green * (1 - alpha)
    const blue = pixels[index + 2] * alpha + fallback.blue * (1 - alpha)
    const luminance = relativeLuminance(red, green, blue)
    redSum += red
    greenSum += green
    blueSum += blue
    luminanceSum += luminance
    luminances.push(luminance)
  }

  luminances.sort((a, b) => a - b)
  const spread = luminances[Math.ceil((count - 1) * 0.95)] - luminances[Math.floor((count - 1) * 0.05)]
  const palette = createPalette(
    sourceKey,
    { red: redSum / count, green: greenSum / count, blue: blueSum / count },
    luminanceSum / count,
    spread,
  )
  const opacity = clamp(options.opacity ?? 1, 0, 1)
  if (opacity === 1 && Array.from({ length: count }, (_, index) => pixels[index * 4 + 3]).every(alpha => alpha === 255)) return palette

  // Keep a stable page beneath the image, then derive colors from what is actually visible.
  const page = parseHex(palette.variables['--app-page'])!
  const visible: number[] = []
  for (let index = 0; index < pixels.length; index += 4) {
    const alpha = pixels[index + 3] / 255 * opacity
    visible.push(
      pixels[index] * alpha + page.red * (1 - alpha),
      pixels[index + 1] * alpha + page.green * (1 - alpha),
      pixels[index + 2] * alpha + page.blue * (1 - alpha), 255,
    )
  }
  const composited = paletteFromPixels(sourceKey, visible, fallbackMode, { ...options, opacity: 1 })
  composited.variables['--app-page'] = palette.variables['--app-page']
  return composited
}

/** A user-selected solid background supplies the palette color directly. */
export function paletteFromColor(sourceKey: string, hex: string, opacity = 1): AutoPalette {
  const color = parseHex(hex)
  if (!color) return paletteFromColor(sourceKey, '#1b1830', opacity)
  return paletteFromPixels(sourceKey, [color.red, color.green, color.blue, 255], 'dark', { opacity })
}

function createPalette(
  sourceKey: string,
  background: Rgb,
  sampledLuminance: number,
  spread: number,
): AutoPalette {
  const hue = THEME_HUE
  const darkInk = hslToRgb(hue, 0.12, 0.12), lightInk = hslToRgb(hue, 0.12, 0.96)
  const mode = (sampledLuminance + 0.05) / (luminance(darkInk) + 0.05)
    >= (luminance(lightInk) + 0.05) / (sampledLuminance + 0.05) ? 'light' : 'dark'
  const dark = mode === 'dark'
  const source = rgbToHsl(background.red, background.green, background.blue)
  const tint = hslToRgb(source.hue, Math.min(0.12, source.saturation), dark ? 0.13 : 0.97)
  const page = parseHex(dark ? '#0b0b12' : '#f3f2fa')!
  // Only a restrained neutral tint comes from the wallpaper; character colors stay shared.
  const glass = mix(parseHex(dark ? '#181f36' : '#f7f6fc')!, tint, 0.16)
  const subtle = mix(glass, page, 0.25)
  const hover = mix(glass, dark ? lightInk : darkInk, 0.07)
  const field = mix(glass, dark ? page : { red: 255, green: 255, blue: 255 }, 0.65)
  const dialog = mix(glass, field, 0.4)
  // One opacity for all ordinary panels, strengthened globally for mixed-light images.
  const glassOpacity = Math.round((0.90 + clamp(spread / 0.5, 0, 1) * 0.06) * 100) / 100
  const subtleOpacity = glassOpacity
  const hoverOpacity = Math.min(1, glassOpacity + 0.04)
  const satin = hslToRgb(hue, 0.12, 0.96)
  const satinOpacity = dark ? 0.025 : 0.25
  // Include black/white pixels, satin highlights and nested surfaces, not just the mean image.
  const surfaces = [glass, subtle, hover, field, dialog]
  for (const backdrop of [{ red: 0, green: 0, blue: 0 }, { red: 255, green: 255, blue: 255 }]) {
    for (const surface of [glass, subtle]) {
      const composited = mix(backdrop, surface, Math.min(glassOpacity, subtleOpacity))
      surfaces.push(composited, mix(composited, satin, satinOpacity))
    }
  }
  const ink = readable(hue, 0.10, dark ? 0.96 : 0.12, surfaces, mode)
  const muted = readable(hue, 0.10, dark ? 0.74 : 0.38, surfaces, mode)
  const accent = parseHex(dark ? '#b7b1f1' : '#61528f')!
  const accentSoft = mix(glass, accent, dark ? 0.16 : 0.10)
  const accentInk = readable(hue, 0.44, dark ? 0.82 : 0.31, [...surfaces, accentSoft], mode)
  const border = readable(hue, 0.10, dark ? 0.56 : 0.48, [...surfaces, field], mode, 3)
  const primaryInk = contrast(accent, darkInk) > contrast(accent, lightInk) ? darkInk : lightInk
  const state = (stateHue: number) => {
    const color = readable(stateHue, 0.46, dark ? 0.76 : 0.34, surfaces, mode)
    return [toHex(color), toHex(mix(glass, color, 0.10))]
  }
  const [danger, dangerSoft] = state(344), [success, successSoft] = state(158), [warning, warningSoft] = state(42)

  return {
    sourceKey,
    mode,
    variables: {
      '--app-ink': toHex(ink),
      '--app-ink-muted': toHex(muted),
      '--app-ink-faint': toHex(muted),
      '--app-ink-rgb': toChannels(ink),
      '--app-surface-rgb': toChannels(glass),
      '--app-glass': toRgba(glass, glassOpacity),
      '--app-glass-subtle': toRgba(subtle, subtleOpacity),
      '--app-glass-hover': toRgba(hover, hoverOpacity),
      '--app-glass-line': toHex(mix(glass, ink, 0.24)),
      '--app-glass-line-strong': toHex(border),
      '--app-field': toHex(field),
      '--app-field-border': toHex(border),
      '--app-dialog-surface': toHex(dialog),
      '--app-dialog-backdrop': toRgba(page, dark ? 0.72 : 0.42),
      '--app-dialog-shadow': `0 24px 64px ${toRgba(darkInk, dark ? 0.38 : 0.16)}`,
      // Overlay panels must not reveal the page text underneath them.
      '--app-sidebar-surface': toHex(glass),
      '--app-sidebar-shadow': `-16px 0 40px ${toRgba(darkInk, dark ? 0.26 : 0.12)}`,
      '--app-sidebar-scrim': toRgba(page, dark ? 0.26 : 0.18),
      '--app-accent': toHex(accent),
      '--app-accent-ink': toHex(accentInk),
      '--app-accent-soft': toHex(accentSoft),
      '--app-accent-line': toHex(accent),
      '--app-primary-ink': toHex(primaryInk),
      '--app-ornament': toHex(readable(44, 0.30, dark ? 0.68 : 0.34, surfaces, mode, 3)),
      '--app-satin': toRgba(lightInk, dark ? 0.025 : 0.25),
      '--app-sheen': toRgba(ink, 0.10),
      '--app-danger': danger, '--app-danger-soft': dangerSoft, '--app-danger-line': danger,
      '--app-success': success, '--app-success-soft': successSoft, '--app-success-line': success,
      '--app-warning': warning, '--app-warning-soft': warningSoft, '--app-warning-line': warning,
      '--app-page': toHex(page),
      '--app-glow-1': toRgba(accent, mode === 'dark' ? 0.28 : 0.16),
      '--app-glow-2': toRgba(accent, dark ? 0.16 : 0.10),
      '--app-glow-3': toRgba(accent, dark ? 0.10 : 0.06),
      '--app-title-from': toHex(ink),
      '--app-title-to': toHex(muted),
      // Protect exposed labels locally; never recolor the full user background.
      '--app-background-copy-filter': `drop-shadow(0 1px 1px ${toRgba(page, 0.85)})`,
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
  let hue = THEME_HUE
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
  return `rgba(${Math.round(red)}, ${Math.round(green)}, ${Math.round(blue)}, ${Math.round(alpha * 1000) / 1000})`
}

function toChannels(color: Rgb): string {
  return [color.red, color.green, color.blue].map(Math.round).join(' ')
}

function mix(a: Rgb, b: Rgb, amount: number): Rgb {
  return { red: a.red * (1 - amount) + b.red * amount, green: a.green * (1 - amount) + b.green * amount, blue: a.blue * (1 - amount) + b.blue * amount }
}

function luminance(color: Rgb) { return relativeLuminance(color.red, color.green, color.blue) }
function contrast(a: Rgb, b: Rgb) {
  const x = luminance(a), y = luminance(b)
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05)
}

function readable(hue: number, saturation: number, lightness: number, surfaces: Rgb[], mode: 'light' | 'dark', minimum = 4.5): Rgb {
  for (let step = 0; step <= 100; step++) {
    const candidate = hslToRgb(hue, saturation, clamp(lightness + step * (mode === 'dark' ? 0.01 : -0.01), 0, 1))
    if (surfaces.every(surface => contrast(candidate, surface) >= minimum)) return candidate
  }
  return hslToRgb(hue, 0, mode === 'dark' ? 1 : 0)
}

function normalizeHue(hue: number): number {
  return (hue + 360) % 360
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value))
}
