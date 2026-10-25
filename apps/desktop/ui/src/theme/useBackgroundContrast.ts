import { useEffect } from 'react'
import { imageCoverCrop } from './autoPalette'
import { backgroundTextTone, hexRgb } from './backgroundContrast'

interface Options {
  imageUrl: string | null
  color: string | null
  opacity: number
  pageColor: string | undefined
}

const SELECTOR = '.app-background-copy'
const COMPONENTS = '.glass-card, .workspace-rail, .ws-sidebar, .plugin-surface-tabs'
const STABLE_OVERLAYS = '.app-dialog, .app-toast, .remote-interactions, .mobile-workspace-actions'
const PROPERTIES = ['--app-local-ink', '--app-local-outline', '--app-local-stroke', '--app-local-copy-filter']

/** Contrast protection is limited to exposed labels; components share the host palette. */
export function useBackgroundContrast({ imageUrl, color, opacity, pageColor }: Options) {
  useEffect(() => {
    if (!imageUrl && !color) return
    let live = true, frame = 0, canvasWidth = 0, canvasHeight = 0
    let timer: ReturnType<typeof setTimeout> | undefined
    const targets = new Set<HTMLElement | SVGElement>()
    const canvas = document.createElement('canvas')
    const context = canvas.getContext('2d', { willReadFrequently: true })
    if (!context) return
    const image = imageUrl ? document.querySelector<HTMLImageElement>('[data-app-background-image]') : null
    const clear = (element: HTMLElement | SVGElement) => {
      for (const property of PROPERTIES) element.style.removeProperty(property)
      delete element.dataset.backgroundInk
    }
    const resizeObserver = new ResizeObserver(() => settle())
    const paint = () => {
      frame = 0
      if (!live || !targets.size || (imageUrl && (!image?.complete || !image.naturalWidth))) return
      const width = window.innerWidth, height = window.innerHeight
      if (image && (width !== canvasWidth || height !== canvasHeight)) {
        const crop = imageCoverCrop(image.naturalWidth, image.naturalHeight, width, height)
        const scale = Math.min(1, 512 / Math.max(width, height))
        canvas.width = Math.max(1, Math.round(width * scale))
        canvas.height = Math.max(1, Math.round(height * scale))
        context.clearRect(0, 0, canvas.width, canvas.height)
        context.drawImage(image, crop.x, crop.y, crop.width, crop.height, 0, 0, canvas.width, canvas.height)
        canvasWidth = width
        canvasHeight = height
      }
      const appearance = getComputedStyle(document.documentElement)
      const mode = document.documentElement.dataset.theme === 'light' ? 'light' : 'dark'
      const pageValue = appearance.getPropertyValue('--app-page').trim() || pageColor || ''
      const page = hexRgb(/^#[\da-f]{6}$/i.test(pageValue) ? pageValue : mode === 'light' ? '#f3f2fa' : '#171b28')
      const previewOpacity = parseFloat(appearance.getPropertyValue('--app-background-opacity'))
      const strength = Number.isFinite(previewOpacity) ? previewOpacity : opacity
      const pixels = image ? context.getImageData(0, 0, canvas.width, canvas.height).data : null
      const visibleArea = (element: Element) => {
        let left = 0, top = 0, right = width, bottom = height
        for (let parent = element.parentElement; parent; parent = parent.parentElement) {
          const style = getComputedStyle(parent)
          if (!/hidden|auto|scroll|clip/.test(`${style.overflowX} ${style.overflowY}`)) continue
          const box = parent.getBoundingClientRect()
          if (/hidden|auto|scroll|clip/.test(style.overflowX)) {
            left = Math.max(left, box.left + parent.clientLeft)
            right = Math.min(right, box.left + parent.clientLeft + parent.clientWidth)
          }
          if (/hidden|auto|scroll|clip/.test(style.overflowY)) {
            top = Math.max(top, box.top + parent.clientTop)
            bottom = Math.min(bottom, box.top + parent.clientTop + parent.clientHeight)
          }
        }
        return { left, top, right, bottom }
      }
      const clip = (rectangle: DOMRect, area: ReturnType<typeof visibleArea>) => ({
        left: Math.max(rectangle.left, area.left), top: Math.max(rectangle.top, area.top),
        right: Math.min(rectangle.right, area.right), bottom: Math.min(rectangle.bottom, area.bottom),
      })
      const sample = (rectangles: Pick<DOMRect, 'left' | 'top' | 'right' | 'bottom'>[], limit = 512) => {
        const samples: number[] = []
        for (const rectangle of rectangles) {
          const left = Math.max(0, rectangle.left), top = Math.max(0, rectangle.top)
          const right = Math.min(width, rectangle.right), bottom = Math.min(height, rectangle.bottom)
          if (right <= left || bottom <= top) continue
          if (color) samples.push(...hexRgb(color), 255)
          else if (pixels) {
            const x = Math.max(0, Math.floor(left / width * canvas.width))
            const y = Math.max(0, Math.floor(top / height * canvas.height))
            const endX = Math.min(canvas.width, Math.ceil(right / width * canvas.width))
            const endY = Math.min(canvas.height, Math.ceil(bottom / height * canvas.height))
            const step = Math.max(1, Math.ceil(Math.max(endX - x, endY - y) / limit))
            for (let row = y; row < endY; row += step) for (let column = x; column < endX; column += step) {
              const index = (row * canvas.width + column) * 4
              samples.push(pixels[index], pixels[index + 1], pixels[index + 2], pixels[index + 3])
            }
          }
        }
        return samples
      }
      for (const target of targets) {
        const rectangles: DOMRect[] = []
        const walker = document.createTreeWalker(target, NodeFilter.SHOW_TEXT)
        while (walker.nextNode()) {
          if (!walker.currentNode.textContent?.trim()) continue
          const range = document.createRange()
          range.selectNodeContents(walker.currentNode)
          rectangles.push(...range.getClientRects())
        }
        if (!rectangles.length) rectangles.push(target.getBoundingClientRect())
        const area = visibleArea(target)
        const samples = sample(rectangles.map(rectangle => clip(rectangle, area)))
        if (!samples.length) continue
        const tone = backgroundTextTone(samples, page, strength)
        target.style.setProperty('--app-local-ink', tone.ink)
        target.style.setProperty('--app-local-outline', tone.outline)
        target.style.setProperty('--app-local-stroke', tone.coverage < 0.95 ? '0.45px' : '0px')
        target.style.setProperty('--app-local-copy-filter', `drop-shadow(0 1px 1px ${tone.outline}99)`)
        target.dataset.backgroundInk = tone.ink === '#ffffff' ? 'light' : 'dark'
      }
    }
    function schedule() {
      if (live && !frame) frame = requestAnimationFrame(paint)
    }
    function settle() {
      clearTimeout(timer)
      timer = setTimeout(schedule, 140)
    }
    const discover = () => {
      const current = new Set([...document.querySelectorAll<HTMLElement | SVGElement>(SELECTOR)]
        .filter(element => !element.closest(`${COMPONENTS}, ${STABLE_OVERLAYS}`)))
      for (const target of targets) if (!current.has(target)) {
        clear(target)
        resizeObserver.unobserve(target)
        targets.delete(target)
      }
      for (const target of current) if (!targets.has(target)) {
        targets.add(target)
        resizeObserver.observe(target)
      }
      schedule()
    }
    const mutations = new MutationObserver(discover)
    mutations.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['class', 'hidden', 'open', 'data-collapsed'] })
    const appearanceChanges = new MutationObserver(schedule)
    appearanceChanges.observe(document.documentElement, { attributes: true, attributeFilter: ['style', 'data-theme'] })
    window.addEventListener('resize', settle)
    window.addEventListener('scroll', settle, { capture: true, passive: true })
    document.addEventListener('transitionend', settle, true)
    if (image) void image.decode().then(schedule).catch(() => undefined)
    void document.fonts.ready.then(schedule)
    discover()
    return () => {
      live = false
      cancelAnimationFrame(frame)
      clearTimeout(timer)
      mutations.disconnect()
      appearanceChanges.disconnect()
      resizeObserver.disconnect()
      window.removeEventListener('resize', settle)
      window.removeEventListener('scroll', settle, true)
      document.removeEventListener('transitionend', settle, true)
      for (const target of targets) clear(target)
    }
  }, [imageUrl, color, opacity, pageColor])
}
