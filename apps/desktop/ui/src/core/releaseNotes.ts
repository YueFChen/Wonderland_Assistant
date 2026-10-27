const releaseDateFormat = new Intl.DateTimeFormat('zh-CN', { dateStyle: 'long' })

/** A readable text excerpt for cards and notifications; full Markdown is rendered on demand. */
export function releaseNotesPreview(body: string | null | undefined, limit = 260): string {
  const text = (body ?? '')
    .replace(/^[ \t]*(```|~~~)[^\n]*\n?/gm, '')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/<[^>]*>/g, '')
    .replace(/^[ \t]*\|?[ \t:|-]+\|[ \t]*$/gm, '')
    .replace(/^[ \t]*\|(.+)\|[ \t]*$/gm, (_match, row: string) => row.split('|').map(cell => cell.trim()).join(' · '))
    .replace(/^\s{0,3}(?:#{1,6}\s+|>\s?|[-*+]\s+|\d+[.)]\s+)/gm, '')
    .replace(/^\s*\[[ xX]\]\s+/gm, '')
    .replace(/\*\*([^*]+)\*\*|__([^_]+)__|~~([^~]+)~~|`([^`]+)`|\*([^*]+)\*/g, (_match, ...groups: unknown[]) => String(groups.slice(0, 5).find(value => value !== undefined) ?? ''))
    .replace(/\s+/g, ' ')
    .trim()
  const characters = Array.from(text)
  return characters.length > limit ? `${characters.slice(0, limit).join('').trimEnd()}…` : text
}

export function releaseDateLabel(value: string | null): string | null {
  if (!value) return null
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? null : releaseDateFormat.format(date)
}

/** Release text can link to web documentation, without navigating the privileged app itself. */
export function releaseNotesUrl(value: string): string | undefined {
  try {
    const url = new URL(value)
    if ((url.protocol === 'https:' || url.protocol === 'http:') && !url.username && !url.password) return url.href
  } catch { /* Relative paths and non-web schemes are displayed as text. */ }
  return undefined
}
