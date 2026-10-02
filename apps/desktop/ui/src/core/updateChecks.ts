export const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000
export const UPDATE_WAKE_COOLDOWN_MS = 15 * 60 * 1000

/** Shared by manual and automatic checks so neither can race an installation. */
export function createUpdateCheckGate(now: () => number = Date.now) {
  let lastStarted: number | null = null
  let checking = false
  let installing = false
  return {
    beginCheck(minIntervalMs = 0) {
      const time = now()
      if (checking || installing) return false
      if (lastStarted !== null && time >= lastStarted && time - lastStarted < minIntervalMs) return false
      lastStarted = time
      checking = true
      return true
    },
    endCheck() { checking = false },
    beginInstall() {
      if (checking || installing) return false
      installing = true
      return true
    },
    endInstall() { installing = false },
  }
}
