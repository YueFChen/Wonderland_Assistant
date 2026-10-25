import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import type { BackgroundAsset, ThemeMode, ThemeSettings, ThemeState } from '@wonderland/core-bindings'
import { useUiEffects } from '@wonderland/ui'
import { HOST_THEME_VARIABLES, type HostTheme } from '@wonderland/plugin-ui-sdk'

import { themeApi } from './api'
import { paletteFromColor, paletteFromImage, type AutoPalette } from './autoPalette'
import { useBackgroundContrast } from './useBackgroundContrast'
import { t } from '../i18n'

/** Resolved light or dark theme. */
export type ResolvedTheme = 'light' | 'dark'

interface ThemeContextValue {
  settings: ThemeSettings
  resolved: ResolvedTheme
  componentVariables: HostTheme['variables']
  /** 当前背景图的 data URL；档位不是自定义或未选择图片时为 null。 */
  backgroundUrl: string | null
  /** 后端主题是否已读回；此前先使用本地缓存预设网页底色。 */
  ready: boolean
  effectsEnabled: boolean
  setEffectsEnabled: (enabled: boolean) => void
  /** 背景库清单，最近导入的在前。 */
  backgrounds: BackgroundAsset[]
  /** 背景库缩略图缓存（id → data URL），按需加载。 */
  thumbnails: Map<string, string>
  /** 请求某张背景图的缩略图；已缓存或已在途时什么都不做。 */
  ensureThumbnail: (id: string) => void
  setTheme: (settings: ThemeSettings) => Promise<void>
  /** 导入一张图片到背景库并立即设为当前背景。 */
  addBackground: (file: File) => Promise<void>
  selectBackground: (id: string) => Promise<void>
  removeBackground: (id: string) => Promise<void>
}

const ThemeContext = createContext<ThemeContextValue | null>(null)

/** Default theme settings. */
const INITIAL: ThemeSettings = { mode: 'system', background: null, background_opacity: 100 }
const THEME_CACHE_KEY = 'wonderland.theme.mode'
const EFFECTS_CACHE_KEY = 'wonderland.ui.effects'

function cachedMode(): ThemeMode {
  try {
    const mode = localStorage.getItem(THEME_CACHE_KEY)
    if (mode === 'light' || mode === 'dark' || mode === 'system' || mode === 'custom') return mode
  } catch { /* Storage may be disabled. */ }
  return INITIAL.mode
}

function cacheMode(mode: ThemeMode) {
  try { localStorage.setItem(THEME_CACHE_KEY, mode) } catch { /* Backend remains authoritative. */ }
}

/** 与后端 `IMAGE_TYPES` 对应的可接受图片类型。 */
const IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/webp', 'image/gif', 'image/bmp']
/** 与后端 `MAX_BACKGROUND_BYTES` 对齐，提前给出可读提示。 */
const MAX_BACKGROUND_BYTES = 20 * 1024 * 1024

/** Applies the selected theme and manages the background library cache. */
export function ThemeProvider({ children }: { children: ReactNode }) {
  const [effectsEnabled, updateEffectsEnabled] = useState(() => {
    try { return localStorage.getItem(EFFECTS_CACHE_KEY) !== 'off' } catch { return true }
  })
  const setEffectsEnabled = useCallback((enabled: boolean) => {
    updateEffectsEnabled(enabled)
    try { localStorage.setItem(EFFECTS_CACHE_KEY, enabled ? 'standard' : 'off') } catch { /* Device preference can remain in memory. */ }
  }, [])
  useUiEffects(effectsEnabled)
  const appliedBackgroundVariables = useRef<string[]>([])
  const [state, setState] = useState<ThemeState>(() => ({ settings: { ...INITIAL, mode: cachedMode() }, background_url: null }))
  const [backgrounds, setBackgrounds] = useState<BackgroundAsset[]>([])
  const [customPalette, setCustomPalette] = useState<AutoPalette | null>(null)
  const [ready, setReady] = useState(false)
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia('(prefers-color-scheme: dark)').matches,
  )
  const thumbnailsRef = useRef(new Map<string, string>())
  const pendingThumbs = useRef(new Set<string>())
  const [thumbVersion, setThumbVersion] = useState(0)

  useEffect(() => {
    const query = window.matchMedia('(prefers-color-scheme: dark)')
    const onChange = (event: MediaQueryListEvent) => setSystemDark(event.matches)
    query.addEventListener('change', onChange)
    return () => query.removeEventListener('change', onChange)
  }, [])

  useEffect(() => {
    let live = true
    void themeApi.get()
      .then((next) => {
        if (!live) return
        setState(next)
        cacheMode(next.settings.mode)
      })
      .catch(() => undefined)
      .finally(() => {
        if (live) setReady(true)
      })
    void themeApi.backgrounds().then((assets) => {
      if (live) setBackgrounds(assets)
    }).catch(() => undefined)
    return () => {
      live = false
    }
  }, [])

  const { settings, background_url: backgroundUrl } = state
  const customSourceKey = settings.background?.kind === 'image'
    ? `image:${settings.background.id}`
    : settings.background?.kind === 'color'
      ? `color:${settings.background.hex}`
      : 'none'
  const activeCustomPalette = customPalette?.sourceKey === customSourceKey ? customPalette : null
  // Keep the previous palette while the next background is decoding, avoiding a dark flash.
  const effectivePalette = settings.mode === 'custom' ? activeCustomPalette ?? customPalette : null

  useEffect(() => {
    if (settings.mode !== 'custom') {
      setCustomPalette(null)
      return
    }
    if (settings.background?.kind === 'image') {
      if (!backgroundUrl) {
        setCustomPalette(null)
        return
      }
      let live = true
      void paletteFromImage(customSourceKey, backgroundUrl, { fallbackMode: systemDark ? 'dark' : 'light', opacity: settings.background_opacity / 100 })
        .then((palette) => { if (live) setCustomPalette(palette) })
        .catch(() => {
          if (live) setCustomPalette(paletteFromColor(customSourceKey, systemDark ? '#171b28' : '#f3f2fa'))
        })
      return () => { live = false }
    }
    setCustomPalette(paletteFromColor(
      customSourceKey,
      settings.background?.kind === 'color' ? settings.background.hex : systemDark ? '#171b28' : '#f3f2fa',
      settings.background_opacity / 100,
    ))
  }, [settings.mode, customSourceKey, backgroundUrl, settings.background_opacity, systemDark])

  const resolved: ResolvedTheme =
    settings.mode === 'light'
      ? 'light'
      : settings.mode === 'system' && !systemDark
        ? 'light'
        : settings.mode === 'custom'
          ? effectivePalette?.mode ?? (systemDark ? 'dark' : 'light')
          : 'dark'

  useLayoutEffect(() => {
    const root = document.documentElement
    root.dataset.theme = resolved
    root.style.setProperty(
      '--app-background-opacity',
      String(Math.min(100, Math.max(0, settings.background_opacity)) / 100),
    )
    for (const property of appliedBackgroundVariables.current) {
      root.style.removeProperty(property)
    }
    appliedBackgroundVariables.current = []
    if (effectivePalette) {
      for (const [property, value] of Object.entries(effectivePalette.variables)) {
        root.style.setProperty(property, value)
        appliedBackgroundVariables.current.push(property)
      }
    }
    return () => {
      for (const property of appliedBackgroundVariables.current) root.style.removeProperty(property)
      appliedBackgroundVariables.current = []
    }
  }, [resolved, settings.background_opacity, effectivePalette])

  useBackgroundContrast({
    imageUrl: ready && settings.mode === 'custom' && settings.background?.kind === 'image' ? backgroundUrl : null,
    color: ready && settings.mode === 'custom' && settings.background?.kind === 'color' ? settings.background.hex : null,
    opacity: settings.background_opacity / 100,
    pageColor: effectivePalette?.variables['--app-page'],
  })

  const componentVariables = useMemo<HostTheme['variables']>(() => effectivePalette
    ? Object.fromEntries(HOST_THEME_VARIABLES.map(property => [property, effectivePalette.variables[property]]).filter(([, value]) => value !== undefined))
    : undefined, [effectivePalette])

  useEffect(() => {
    if (settings.mode !== 'custom' || settings.background?.kind !== 'image') return
    const id = settings.background.id
    if (!backgroundUrl || thumbnailsRef.current.get(id) === backgroundUrl) return
    thumbnailsRef.current.set(id, backgroundUrl)
    setThumbVersion((version) => version + 1)
  }, [settings, backgroundUrl])

  const ensureThumbnail = useCallback((id: string) => {
    if (thumbnailsRef.current.has(id) || pendingThumbs.current.has(id)) return
    pendingThumbs.current.add(id)
    void themeApi
      .backgroundUrl(id)
      .then((url) => {
        if (!url) return
        thumbnailsRef.current.set(id, url)
        setThumbVersion((version) => version + 1)
      })
      .catch(() => undefined)
      .finally(() => pendingThumbs.current.delete(id))
  }, [])

  const refreshBackgrounds = useCallback(async () => {
    setBackgrounds(await themeApi.backgrounds())
  }, [])

  const setTheme = useCallback(async (next: ThemeSettings) => {
    const saved = await themeApi.set(next)
    setState(saved)
    cacheMode(saved.settings.mode)
  }, [])

  const addBackground = useCallback(
    async (file: File) => {
      const { dataUrl, name } = await readImage(file)
      const saved = await themeApi.addBackground(dataUrl, name)
      setState(saved)
      cacheMode(saved.settings.mode)
      await refreshBackgrounds()
    },
    [refreshBackgrounds],
  )

  const selectBackground = useCallback(async (id: string) => {
    const saved = await themeApi.selectBackground(id)
    setState(saved)
    cacheMode(saved.settings.mode)
  }, [])

  const removeBackground = useCallback(
    async (id: string) => {
      const saved = await themeApi.removeBackground(id)
      setState(saved)
      cacheMode(saved.settings.mode)
      thumbnailsRef.current.delete(id)
      await refreshBackgrounds()
    },
    [refreshBackgrounds],
  )

  const thumbnails = useMemo(() => new Map(thumbnailsRef.current), [thumbVersion])

  const value = useMemo(
    () => ({
      settings,
      resolved,
      componentVariables,
      backgroundUrl,
      ready,
      effectsEnabled,
      setEffectsEnabled,
      backgrounds,
      thumbnails,
      ensureThumbnail,
      setTheme,
      addBackground,
      selectBackground,
      removeBackground,
    }),
    [
      settings,
      resolved,
      componentVariables,
      backgroundUrl,
      ready,
      effectsEnabled,
      setEffectsEnabled,
      backgrounds,
      thumbnails,
      ensureThumbnail,
      setTheme,
      addBackground,
      selectBackground,
      removeBackground,
    ],
  )

  return <ThemeContext.Provider value={value}>{ready ? children : null}</ThemeContext.Provider>
}

export function useTheme(): ThemeContextValue {
  const value = useContext(ThemeContext)
  if (!value) {
    throw new Error('useTheme 必须在 ThemeProvider 内使用') // i18n-allow: invariant assertion
  }
  return value
}

/** 读出 data URL；类型与大小在这里先拦一道，后端还会复核。 */
function readImage(file: File): Promise<{ dataUrl: string; name: string }> {
  if (!IMAGE_TYPES.includes(file.type)) {
    return Promise.reject(new Error(t('theme.unsupportedImage')))
  }
  if (file.size > MAX_BACKGROUND_BYTES) {
    return Promise.reject(new Error(t('theme.imageTooLarge')))
  }
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve({ dataUrl: String(reader.result), name: file.name })
    reader.onerror = () => reject(new Error(t('theme.imageReadFailed')))
    reader.readAsDataURL(file)
  })
}
