export interface PluginStateSnapshot<T> {
  states: T[] | null
  failed: boolean
}

export interface PluginStateSource<T> {
  read: () => Promise<T[]>
  listen: (changed: () => void) => Promise<() => void>
  onReset?: (reset: () => void) => () => void
}

/** One snapshot and subscription shared by every mounted plugin-state consumer. */
export function createPluginStateStore<T>(source: PluginStateSource<T>, retryDelays = [250, 1000, 3000, 10000, 30000]) {
  let snapshot: PluginStateSnapshot<T> = { states: null, failed: false }
  const consumers = new Set<() => void>()
  let running = false
  let generation = 0
  let revision = 0
  let request: Promise<void> | null = null
  let refreshAgain = false
  let stopListening: (() => void) | undefined
  let stopReset: (() => void) | undefined
  let listenerFailed = false
  let connecting = false
  let retryAttempt = 0
  let retryTimer: ReturnType<typeof setTimeout> | undefined

  const publish = (next: PluginStateSnapshot<T>) => {
    snapshot = next
    for (const consumer of consumers) consumer()
  }

  const setStates = (states: T[]) => {
    revision++
    publish({ states, failed: listenerFailed })
  }

  const reset = () => {
    revision++
    publish({ states: null, failed: false })
  }

  const read = () => {
    if (!running) return
    if (request) {
      refreshAgain = true
      return
    }
    const currentGeneration = generation
    const currentRevision = revision
    const pending = Promise.resolve()
      .then(source.read)
      .then((states) => {
        if (running && currentGeneration === generation && currentRevision === revision) {
          publish({ states, failed: listenerFailed })
        }
      })
      .catch(() => {
        if (running && currentGeneration === generation && currentRevision === revision) {
          publish({ ...snapshot, failed: true })
        }
      })
      .finally(() => {
        if (request !== pending) return
        request = null
        if (refreshAgain && running) {
          refreshAgain = false
          read()
        }
      })
    request = pending
  }

  const connect = () => {
    if (!running || connecting || stopListening) return
    if (retryTimer) clearTimeout(retryTimer)
    retryTimer = undefined
    connecting = true
    const currentGeneration = generation
    void Promise.resolve().then(() => source.listen(() => {
      if (running && currentGeneration === generation) read()
    }))
    .then((stop) => {
      if (currentGeneration !== generation) { stop(); return }
      connecting = false
      if (running) {
        stopListening = stop
        listenerFailed = false
        retryAttempt = 0
        // Re-read after registration to close changes missed during reconnection.
        read()
      } else stop()
    }).catch(() => {
      if (!running || currentGeneration !== generation) return
      connecting = false
      listenerFailed = true
      publish({ ...snapshot, failed: true })
      read()
      const delay = retryDelays[Math.min(retryAttempt++, retryDelays.length - 1)] ?? 30000
      retryTimer = setTimeout(connect, delay)
    })
  }

  const refresh = () => {
    if (listenerFailed) connect()
    read()
  }

  const start = () => {
    running = true
    generation++
    connecting = false
    listenerFailed = false
    retryAttempt = 0
    connect()
    stopReset = source.onReset?.(reset)
  }

  const stop = () => {
    running = false
    generation++
    if (retryTimer) clearTimeout(retryTimer)
    retryTimer = undefined
    connecting = false
    stopListening?.()
    stopReset?.()
    stopListening = stopReset = undefined
    request = null
    refreshAgain = false
  }

  return {
    getSnapshot: () => snapshot,
    setStates,
    reset,
    refresh,
    subscribe(consumer: () => void) {
      consumers.add(consumer)
      if (!running) start()
      return () => {
        consumers.delete(consumer)
        // React StrictMode may subscribe again immediately after releasing its probe.
        queueMicrotask(() => { if (consumers.size === 0 && running) stop() })
      }
    },
  }
}
