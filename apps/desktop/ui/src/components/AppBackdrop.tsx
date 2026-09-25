import { useTheme } from '../theme/ThemeProvider'

/**
 * Full-window application background layer.
 */
export function AppBackdrop() {
  const { settings, backgroundUrl } = useTheme()
  const custom = settings.mode === 'custom'
  const color = custom && settings.background?.kind === 'color' ? settings.background.hex : null
  const image = custom && settings.background?.kind === 'image' ? backgroundUrl : null

  if (image) {
    return (
      <div aria-hidden className="pointer-events-none fixed inset-0 -z-10">
        <img src={image} alt="" className="h-full w-full object-cover" />
        <div className="app-backdrop-scrim absolute inset-0" />
      </div>
    )
  }

  return (
    <div aria-hidden className="pointer-events-none fixed inset-0 -z-10">
      <div
        className="app-backdrop h-full w-full"
        style={color ? { backgroundImage: 'none', backgroundColor: color } : undefined}
      />
      {color ? <div className="app-backdrop-scrim absolute inset-0" /> : null}
    </div>
  )
}
