/** Explicit operation deadlines may be long; browser timer overflow must not shorten them. */
export function validateUiTimeout(timeoutMs: unknown): number | undefined {
  if (timeoutMs === undefined || timeoutMs === null) return undefined
  if (typeof timeoutMs !== 'number' || !Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) {
    throw new Error('timeoutMs must be a positive safe integer.')
  }
  return timeoutMs
}

export function scheduleUiDeadline(timeoutMs: number | undefined, expire: () => void): () => void {
  if (timeoutMs === undefined) return () => {}
  const started = Date.now()
  let timer: ReturnType<typeof setTimeout> | undefined
  let cancelled = false
  const arm = () => {
    if (cancelled) return
    const remaining = timeoutMs - (Date.now() - started)
    if (remaining <= 0) { expire(); return }
    timer = setTimeout(arm, Math.min(remaining, 2_147_483_647))
  }
  arm()
  return () => { cancelled = true; if (timer !== undefined) clearTimeout(timer) }
}
