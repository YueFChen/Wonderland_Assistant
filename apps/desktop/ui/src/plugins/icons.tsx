import { useEffect, useState } from 'react'
import {
  Database,
  Image,
  Images,
  Languages,
  MessageCircle,
  MessagesSquare,
  PenLine,
  Puzzle,
  Radar,
  type LucideIcon,
} from 'lucide-react'

/** Plugin manifests may keep using the legacy Lucide key or point at a packaged icon asset. */
const ICONS: Record<string, LucideIcon> = {
  database: Database,
  image: Image,
  images: Images,
  'message-circle': MessageCircle,
  'messages-square': MessagesSquare,
  'pen-line': PenLine,
  puzzle: Puzzle,
  languages: Languages,
  radar: Radar,
}

interface PluginIconProps {
  icon: string
  pluginId?: string
  className?: string
  'aria-hidden'?: boolean
}

function packagedIconPath(icon: string): string | null {
  const path = icon.startsWith('asset:') ? icon.slice('asset:'.length) : ''
  return /^ui\/icons\/(?!.*(?:^|\/)\.{1,2}(?:\/|$))[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)*\.(?:svg|png|webp)$/.test(path)
    ? path
    : null
}

function pluginAssetUrl(pluginId: string, path: string): string {
  // Core currently packages desktop plugins for Windows. Keep the non-Windows
  // custom-scheme form aligned with PluginManager::plugin_ui_launch_info.
  const origin = navigator.userAgent.includes('Windows')
    ? 'http://plugin-asset.localhost'
    : 'plugin-asset://localhost'
  return `${origin}/${pluginId}/${path}`
}

export function PluginIcon({ icon, pluginId, className, 'aria-hidden': ariaHidden = true }: PluginIconProps) {
  const path = pluginId ? packagedIconPath(icon) : null
  const src = path ? pluginAssetUrl(pluginId!, path) : null
  const [assetFailed, setAssetFailed] = useState(false)

  useEffect(() => setAssetFailed(false), [src])

  if (src && !assetFailed) {
    return <img src={src} className={className} alt="" aria-hidden={ariaHidden} onError={() => setAssetFailed(true)} />
  }

  const Icon = ICONS[icon] ?? Puzzle
  return <Icon className={className} aria-hidden={ariaHidden} />
}
