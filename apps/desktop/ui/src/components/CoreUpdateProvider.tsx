import { createContext, lazy, Suspense, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { getVersion } from '@tauri-apps/api/app'
import { isTauri } from '@tauri-apps/api/core'
import { relaunch } from '@tauri-apps/plugin-process'
import { Download } from 'lucide-react'
import { Dialog, DialogButton } from '@wonderland/ui'

import { t } from '../i18n'
import { createUpdateCheckGate, UPDATE_CHECK_INTERVAL_MS, UPDATE_WAKE_COOLDOWN_MS } from '../core/updateChecks'
import { releaseDateLabel, releaseNotesPreview } from '../core/releaseNotes'
import { networkApi, type CoreUpdateAvailable, type CoreUpdateProgress } from '../network/api'
import { useNotifications } from './Notifications'

const IGNORED_VERSIONS_KEY = 'wonderland.core.ignored-update-versions.v1'
const HOME_NOTICE_PREFIX = 'core-update:'
const ReleaseNotes = lazy(() => import('./ReleaseNotes'))

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
  showUpdateDetails: () => void
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
  const [promptFailure, setPromptFailure] = useState('')
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
      setPromptFailure('')
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
          message: releaseNotesPreview(next.body) || t('home.updateNotice', { version: next.version }),
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
    setPromptFailure('')
    setMessage(t('settings.update.downloading'))
    try {
      await networkApi.updateInstall(update.updateId)
      setMessage(t('settings.update.installing'))
      notify(t('settings.update.installing'), { tone: 'success', durationMs: 5000 })
      await relaunch()
    } catch (cause) {
      const failure = t('settings.update.installFailed', { reason: errorText(cause) })
      setMessage(failure)
      setPromptFailure(failure)
      notify(failure, { tone: 'error', durationMs: 7000 })
      setInstalling(false)
      setProgress(null)
      gate.current.endInstall()
    }
  }, [notify, update])

  const showUpdateDetails = useCallback(() => {
    if (update) setPromptOpen(true)
  }, [update])

  const ignoreCurrentVersion = useCallback(() => {
    if (!update) return
    const ignored = new Set(readIgnoredVersions())
    ignored.add(update.version)
    try {
      localStorage.setItem(IGNORED_VERSIONS_KEY, JSON.stringify([...ignored].slice(-32)))
    } catch {
      const failure = t('settings.update.ignoreSaveFailed')
      setPromptFailure(failure)
      notify(failure, { tone: 'error' })
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
    showUpdateDetails,
  }), [version, update, hasUpdate, checking, installing, progress, message, checkForUpdate, installUpdate, ignoreCurrentVersion, showUpdateDetails])

  return (
    <CoreUpdateContext.Provider value={context}>
      {children}
      {promptOpen && update && (
        <CoreUpdateDialog
          update={update}
          currentVersion={version}
          checking={checking}
          installing={installing}
          progress={progress}
          failure={promptFailure}
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

export function CoreUpdateDialog({
  update,
  currentVersion,
  checking,
  installing,
  progress,
  failure,
  onInstall,
  onIgnore,
  onLater,
}: {
  update: CoreUpdateAvailable
  currentVersion: string
  checking: boolean
  installing: boolean
  progress: number | null
  failure: string
  onInstall: () => void
  onIgnore: () => void
  onLater: () => void
}) {
  const published = releaseDateLabel(update.date)
  const body = update.body?.trim()
  return <Dialog open title={t('update.prompt.title')} closeLabel={t('common.close')} size="lg" dismissible={!installing} onClose={onLater} footer={<>
    {failure && <p role="alert" className="app-dialog-notice w-full" data-tone="danger">{failure}</p>}
    {installing && <div className="w-full" role="status">
      <p className="text-xs text-ink-muted">{progress === null ? t('settings.update.downloading') : t('settings.update.progress', { progress })}</p>
      <div className="app-progress mt-2" role="progressbar" aria-label={t('settings.update.downloading')} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress ?? undefined} aria-busy={progress === null}><span style={progress === null ? undefined : { width: `${progress}%` }} /></div>
    </div>}
    <DialogButton className="mr-auto" variant="quiet" disabled={installing} onClick={onIgnore}>{t('update.prompt.ignore')}</DialogButton>
    <DialogButton data-dialog-autofocus disabled={installing} onClick={onLater}>{t('update.prompt.later')}</DialogButton>
    <DialogButton variant="primary" disabled={checking || installing} onClick={onInstall}><Download className="h-3.5 w-3.5" aria-hidden />{t('update.prompt.install')}</DialogButton>
  </>}>
    <p className="app-dialog-description">{t('update.prompt.versions', { current: currentVersion || '—', next: update.version })}</p>
    {published && <p className="mt-2 text-xs text-ink-faint"><time dateTime={update.date ?? undefined}>{t('settings.update.published', { date: published })}</time></p>}
    <section className="mt-5 border-t border-glass-line pt-5" aria-label={t('settings.update.releaseNotes')}>
      <h3 className="mb-3 text-xs font-semibold text-ink-faint">{t('settings.update.releaseNotes')}</h3>
      {body ? <Suspense fallback={<p role="status" className="text-sm text-ink-muted">{t('settings.update.notesLoading')}</p>}>
        <ReleaseNotes body={body} />
      </Suspense> : <p className="text-sm text-ink-muted">{t('settings.update.notesEmpty')}</p>}
    </section>
  </Dialog>
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
