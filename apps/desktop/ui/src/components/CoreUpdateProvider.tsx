import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { getVersion } from '@tauri-apps/api/app'
import { isTauri } from '@tauri-apps/api/core'
import { relaunch } from '@tauri-apps/plugin-process'
import { Download, X } from 'lucide-react'

import { t } from '../i18n'
import { createUpdateCheckGate, UPDATE_CHECK_INTERVAL_MS, UPDATE_WAKE_COOLDOWN_MS } from '../core/updateChecks'
import { networkApi, type CoreUpdateAvailable, type CoreUpdateProgress } from '../network/api'
import { useNotifications } from './Notifications'

const IGNORED_VERSIONS_KEY = 'wonderland.core.ignored-update-versions.v1'
const HOME_NOTICE_PREFIX = 'core-update:'

interface CoreUpdateContextValue {
  version: string
  update: CoreUpdateAvailable | null
  hasUpdate: boolean
  checking: boolean
  installing: boolean
  progress: number | null
  message: string
  checkForUpdate: (showPrompt?: boolean) => Promise<CoreUpdateAvailable | null>
  installUpdate: () => Promise<void>
  ignoreCurrentVersion: () => void
  dismissPrompt: () => void
}

const CoreUpdateContext = createContext<CoreUpdateContextValue | null>(null)

export function CoreUpdateProvider({ children }: { children: ReactNode }) {
  const { notify, publishHomeNotification, clearHomeNotifications, dismissHomeNotification } = useNotifications()
  const [version, setVersion] = useState('')
  const [update, setUpdate] = useState<CoreUpdateAvailable | null>(null)
  const [checking, setChecking] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [progress, setProgress] = useState<number | null>(null)
  const [message, setMessage] = useState('')
  const [promptOpen, setPromptOpen] = useState(false)
  const [hasUpdate, setHasUpdate] = useState(false)
  const startupCheckStarted = useRef(false)
  const gate = useRef(createUpdateCheckGate())
  const announcedVersions = useRef(new Set<string>())
  const generation = useRef(0)

  const checkForUpdate = useCallback(async (showPrompt = false, minIntervalMs = 0) => {
    if (!isTauri() || !gate.current.beginCheck(minIntervalMs)) return null
    const requestGeneration = generation.current
    setChecking(true)
    try {
      const next = await networkApi.updateCheck()
      if (requestGeneration !== generation.current) return null
      setUpdate(next)
      if (!next) {
        setHasUpdate(false)
        clearHomeNotifications(HOME_NOTICE_PREFIX)
        setPromptOpen(false)
        setMessage(t('settings.update.current'))
        return null
      }

      const noticeId = `${HOME_NOTICE_PREFIX}${next.version}`
      if (readIgnoredVersions().includes(next.version)) {
        setHasUpdate(false)
        clearHomeNotifications(HOME_NOTICE_PREFIX)
        setPromptOpen(false)
        setMessage(t('settings.update.ignored', { version: next.version }))
        return next
      }

      setHasUpdate(true)
      setMessage(t('settings.update.available', { version: next.version }))
      // Closing a notice is respected for the rest of this application session.
      if (!announcedVersions.current.has(next.version)) {
        announcedVersions.current.add(next.version)
        clearHomeNotifications(HOME_NOTICE_PREFIX)
        publishHomeNotification({
          id: noticeId,
          title: t('settings.update.available', { version: next.version }),
          message: next.body?.trim().slice(0, 260) || t('home.updateNotice', { version: next.version }),
          href: '/workspace/settings',
        })
      }
      if (showPrompt) setPromptOpen(true)
      return next
    } catch (cause) {
      if (requestGeneration !== generation.current) return null
      // A transient check failure must not discard a previously discovered update.
      const failure = t('settings.update.checkFailed', { reason: errorText(cause) })
      setMessage(failure)
      if (showPrompt) notify(failure, { tone: 'warning', durationMs: 6000 })
      return null
    } finally {
      setChecking(false)
      gate.current.endCheck()
    }
  }, [clearHomeNotifications, notify, publishHomeNotification])

  const installUpdate = useCallback(async () => {
    if (!update || !gate.current.beginInstall()) return
    setInstalling(true)
    setProgress(null)
    setMessage(t('settings.update.downloading'))
    try {
      await networkApi.updateInstall(update.updateId)
      setMessage(t('settings.update.installing'))
      notify(t('settings.update.installing'), { tone: 'success', durationMs: 5000 })
      await relaunch()
    } catch (cause) {
      const failure = t('settings.update.installFailed', { reason: errorText(cause) })
      setMessage(failure)
      notify(failure, { tone: 'error', durationMs: 7000 })
      setInstalling(false)
      setProgress(null)
      gate.current.endInstall()
    }
  }, [notify, update])

  const ignoreCurrentVersion = useCallback(() => {
    if (!update) return
    const ignored = new Set(readIgnoredVersions())
    ignored.add(update.version)
    try {
      localStorage.setItem(IGNORED_VERSIONS_KEY, JSON.stringify([...ignored].slice(-32)))
    } catch {
      notify(t('settings.update.ignoreSaveFailed'), { tone: 'error' })
      return
    }
    dismissHomeNotification(`${HOME_NOTICE_PREFIX}${update.version}`)
    setHasUpdate(false)
    setPromptOpen(false)
    setMessage(t('settings.update.ignored', { version: update.version }))
  }, [dismissHomeNotification, notify, update])

  useEffect(() => {
    if (!isTauri()) return
    let live = true
    void getVersion().then((next) => { if (live) setVersion(next) }).catch(() => undefined)
    let stopProgress: (() => void) | undefined
    let stopInvalidated: (() => void) | undefined
    void networkApi.onUpdateProgress((event: CoreUpdateProgress) => {
      if (!live) return
      if (event.finished) {
        setProgress(null)
        setMessage(t('settings.update.installing'))
      } else if (event.totalBytes && event.totalBytes > 0) {
        setProgress(Math.min(100, Math.round(event.downloadedBytes * 100 / event.totalBytes)))
      } else {
        setProgress(null)
      }
    }).then((dispose) => { if (live) stopProgress = dispose; else dispose() })
    void networkApi.onUpdateInvalidated(() => {
      if (!live) return
      generation.current += 1
      setUpdate(null)
      setHasUpdate(false)
      setPromptOpen(false)
      clearHomeNotifications(HOME_NOTICE_PREFIX)
      const notice = t('settings.update.proxyChanged')
      setMessage(notice)
      notify(notice, { tone: 'info' })
    }).then((dispose) => { if (live) stopInvalidated = dispose; else dispose() })

    if (!startupCheckStarted.current) {
      startupCheckStarted.current = true
      void checkForUpdate(true)
    }
    return () => {
      live = false
      stopProgress?.()
      stopInvalidated?.()
    }
  }, [checkForUpdate, clearHomeNotifications, notify])

  useEffect(() => {
    if (!isTauri()) return
    const check = (minIntervalMs: number) => {
      if (navigator.onLine) void checkForUpdate(false, minIntervalMs)
    }
    const wake = () => check(UPDATE_WAKE_COOLDOWN_MS)
    const onVisible = () => { if (document.visibilityState === 'visible') wake() }
    // A short tick keeps manual checks from shifting the next check by another hour.
    const timer = window.setInterval(() => check(UPDATE_CHECK_INTERVAL_MS), 60_000)
    window.addEventListener('focus', wake)
    window.addEventListener('online', wake)
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      window.clearInterval(timer)
      window.removeEventListener('focus', wake)
      window.removeEventListener('online', wake)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [checkForUpdate])

  const context = useMemo(() => ({
    version,
    update,
    hasUpdate,
    checking,
    installing,
    progress,
    message,
    checkForUpdate,
    installUpdate,
    ignoreCurrentVersion,
    dismissPrompt: () => setPromptOpen(false),
  }), [version, update, hasUpdate, checking, installing, progress, message, checkForUpdate, installUpdate, ignoreCurrentVersion])

  return (
    <CoreUpdateContext.Provider value={context}>
      {children}
      {promptOpen && update && (
        <CoreUpdateDialog
          update={update}
          currentVersion={version}
          installing={installing}
          progress={progress}
          onInstall={() => void installUpdate()}
          onIgnore={ignoreCurrentVersion}
          onLater={() => setPromptOpen(false)}
        />
      )}
    </CoreUpdateContext.Provider>
  )
}

export function useCoreUpdate() {
  const value = useContext(CoreUpdateContext)
  if (!value) throw new Error('useCoreUpdate must be used inside CoreUpdateProvider')
  return value
}

function CoreUpdateDialog({
  update,
  currentVersion,
  installing,
  progress,
  onInstall,
  onIgnore,
  onLater,
}: {
  update: CoreUpdateAvailable
  currentVersion: string
  installing: boolean
  progress: number | null
  onInstall: () => void
  onIgnore: () => void
  onLater: () => void
}) {
  useEffect(() => {
    if (installing) return
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onLater()
    }
    window.addEventListener('keydown', closeOnEscape)
    return () => window.removeEventListener('keydown', closeOnEscape)
  }, [installing, onLater])

  return (
    <div className="fixed inset-0 z-[140] grid place-items-center bg-slate-950/45 p-5 backdrop-blur-sm" role="presentation">
      <section role="dialog" aria-modal="true" aria-labelledby="core-update-title" className="glass-card w-full max-w-xl rounded-2xl border border-glass-line p-6 shadow-2xl">
        <div className="flex items-start justify-between gap-4">
          <div>
            <p className="text-[11px] font-bold uppercase tracking-[0.16em] text-brand-400">Wonderland Assistant</p>
            <h2 id="core-update-title" className="mt-2 text-xl font-bold text-ink">{t('update.prompt.title')}</h2>
          </div>
          {!installing && <button type="button" onClick={onLater} className="rounded-lg p-2 text-ink-faint hover:bg-glass-hover hover:text-ink" aria-label={t('update.prompt.later')}><X className="h-4 w-4" aria-hidden /></button>}
        </div>
        <p className="mt-3 text-sm text-ink-muted">{t('update.prompt.versions', { current: currentVersion || '—', next: update.version })}</p>
        {update.date && <p className="mt-1 text-xs text-ink-faint">{update.date}</p>}
        {update.body && <p className="mt-4 max-h-56 overflow-y-auto whitespace-pre-wrap rounded-xl border border-glass-line bg-glass/60 p-4 text-sm leading-6 text-ink-muted">{update.body}</p>}
        {installing && (
          <div className="mt-5" role="status">
            <div className="flex justify-between text-xs text-ink-muted"><span>{progress === null ? t('settings.update.downloading') : t('settings.update.progress', { progress })}</span>{progress !== null && <span>{progress}%</span>}</div>
            <div className="mt-2 h-2 overflow-hidden rounded-full bg-glass-line"><div className="h-full bg-brand-500 transition-[width]" style={{ width: `${progress ?? 18}%` }} /></div>
          </div>
        )}
        <div className="mt-6 flex flex-wrap justify-end gap-2">
          <button type="button" disabled={installing} onClick={onIgnore} className="rounded-lg border border-glass-line px-4 py-2 text-sm text-ink-muted hover:bg-glass-hover disabled:opacity-50">{t('update.prompt.ignore')}</button>
          <button type="button" disabled={installing} onClick={onLater} className="rounded-lg border border-glass-line px-4 py-2 text-sm text-ink hover:bg-glass-hover disabled:opacity-50">{t('update.prompt.later')}</button>
          <button type="button" disabled={installing} onClick={onInstall} className="inline-flex items-center gap-2 rounded-lg bg-brand-600 px-4 py-2 text-sm font-semibold text-white hover:bg-brand-500 disabled:opacity-50"><Download className="h-4 w-4" aria-hidden />{t('update.prompt.install')}</button>
        </div>
      </section>
    </div>
  )
}

function readIgnoredVersions(): string[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(IGNORED_VERSIONS_KEY) ?? '[]')
    return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string' && item.length <= 64) : []
  } catch {
    return []
  }
}

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message: unknown }).message)
  }
  return cause instanceof Error ? cause.message : String(cause)
}
