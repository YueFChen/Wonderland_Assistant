const ENABLED_KEY = 'wonderland.startup.resume-work-page.v1'
const LAST_WORK_PAGE_KEY = 'wonderland.startup.last-work-page.v1'

/** Restore the most recent workspace activity by default; users can turn this off in Settings. */
export function startupMemoryEnabled(): boolean {
  try {
    return localStorage.getItem(ENABLED_KEY) !== 'false'
  } catch {
    return true
  }
}

export function setStartupMemoryEnabled(enabled: boolean) {
  try {
    localStorage.setItem(ENABLED_KEY, String(enabled))
  } catch {
  }
}

/** Only workspace and plugin activity routes are resumable; management pages always remain transient. */
export function rememberLastWorkPage(path: string) {
  if (!isWorkPage(path)) return
  try {
    localStorage.setItem(LAST_WORK_PAGE_KEY, path)
  } catch {
  }
}

export function lastWorkPageToResume(): string | null {
  if (!startupMemoryEnabled()) return null
  try {
    const path = localStorage.getItem(LAST_WORK_PAGE_KEY)
    return path && isWorkPage(path) ? path : null
  } catch {
    return null
  }
}

function isWorkPage(path: string): boolean {
  if (path.length > 1024 || !path.startsWith('/')) return false
  const [pathname, query = ''] = path.split('?', 2)
  if (pathname === '/workspace') {
    return new URLSearchParams(query).get('view') !== 'plugins'
  }
  return /^\/workspace\/plugin\/[^/]+\/[^/]+$/.test(pathname)
}
