import { Database, Image, Images, Languages, MessageCircle, MessagesSquare, PenLine, Puzzle, type LucideIcon } from 'lucide-react'

/** 插件 manifest 注册图标名；宿主只负责把名字映射到统一图标组件。 */
const ICONS: Record<string, LucideIcon> = {
  database: Database,
  image: Image,
  images: Images,
  'message-circle': MessageCircle,
  'messages-square': MessagesSquare,
  'pen-line': PenLine,
  puzzle: Puzzle,
  languages: Languages,
}

export function pluginIcon(name: string): LucideIcon {
  return ICONS[name] ?? Puzzle
}
