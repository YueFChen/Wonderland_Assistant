import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { AlertCircle, CheckCircle2, Info, TriangleAlert, X } from 'lucide-react'

import { t } from '../i18n'

const HOME_NOTICE_KEY = 'wonderland.home.notifications.v1'
const HOME_NOTICE_EVENT = 'wonderland:home-notifications-change'
const MAX_HOME_NOTICES = 8

export type NotificationTone = 'info' | 'success' | 'warning' | 'error'

export interface HomeNotification {
  id: string
  title: string
  message: string
  href?: string
  createdAt: number
}

interface ToastNotification {
  id: string
  message: string
  tone: NotificationTone
  durationMs: number
}

interface NotificationContextValue {
  homeNotifications: HomeNotification[]
  notify: (message: string, options?: { tone?: NotificationTone; durationMs?: number }) => void
  publishHomeNotification: (notification: Omit<HomeNotification, 'createdAt'>) => void
  dismissHomeNotification: (id: string) => void
  clearHomeNotifications: (prefix: string) => void
}

const NotificationContext = createContext<NotificationContextValue | null>(null)

export function NotificationProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastNotification[]>([])
  const [homeNotifications, setHomeNotifications] = useState(readHomeNotifications)
  const nextId = useRef(0)

  const dismissToast = useCallback((id: string) => {
    setToasts((current) => current.filter((item) => item.id !== id))
  }, [])

  const notify = useCallback((message: string, options: { tone?: NotificationTone; durationMs?: number } = {}) => {
    const id = `${Date.now()}-${nextId.current++}`
    setToasts((current) => [...current, {
      id,
      message,
      tone: options.tone ?? 'info',
      durationMs: options.durationMs ?? 4200,
    }].slice(-4))
  }, [])

  const publishHomeNotification = useCallback((notification: Omit<HomeNotification, 'createdAt'>) => {
    setHomeNotifications((current) => {
      const next = [{ ...notification, createdAt: Date.now() }, ...current.filter((item) => item.id !== notification.id)]
        .slice(0, MAX_HOME_NOTICES)
      saveHomeNotifications(next)
      return next
    })
  }, [])

  const dismissHomeNotification = useCallback((id: string) => {
    setHomeNotifications((current) => {
      const next = current.filter((item) => item.id !== id)
      saveHomeNotifications(next)
      return next
    })
  }, [])

  const clearHomeNotifications = useCallback((prefix: string) => {
    setHomeNotifications((current) => {
      const next = current.filter((item) => !item.id.startsWith(prefix))
      saveHomeNotifications(next)
      return next
    })
  }, [])

  useEffect(() => {
    const refresh = () => setHomeNotifications(readHomeNotifications())
    window.addEventListener(HOME_NOTICE_EVENT, refresh)
    window.addEventListener('storage', refresh)
    return () => {
      window.removeEventListener(HOME_NOTICE_EVENT, refresh)
      window.removeEventListener('storage', refresh)
    }
  }, [])

  const context = useMemo(() => ({
    homeNotifications,
    notify,
    publishHomeNotification,
    dismissHomeNotification,
    clearHomeNotifications,
  }), [homeNotifications, notify, publishHomeNotification, dismissHomeNotification, clearHomeNotifications])

  return (
    <NotificationContext.Provider value={context}>
      {children}
      <div className="pointer-events-none fixed right-5 top-14 z-[150] flex w-[min(26rem,calc(100vw-2.5rem))] flex-col gap-2" aria-live="polite" aria-relevant="additions text">
        {toasts.map((toast) => <ToastItem key={toast.id} toast={toast} onDismiss={dismissToast} />)}
      </div>
    </NotificationContext.Provider>
  )
}

export function useNotifications() {
  const value = useContext(NotificationContext)
  if (!value) throw new Error('useNotifications must be used inside NotificationProvider')
  return value
}

function ToastItem({ toast, onDismiss }: { toast: ToastNotification; onDismiss: (id: string) => void }) {
  useEffect(() => {
    const timer = window.setTimeout(() => onDismiss(toast.id), toast.durationMs)
    return () => window.clearTimeout(timer)
  }, [onDismiss, toast.durationMs, toast.id])

  const icon = toast.tone === 'success'
    ? <CheckCircle2 className="h-4 w-4 shrink-0" aria-hidden />
    : toast.tone === 'warning'
      ? <TriangleAlert className="h-4 w-4 shrink-0" aria-hidden />
      : toast.tone === 'error'
        ? <AlertCircle className="h-4 w-4 shrink-0" aria-hidden />
        : <Info className="h-4 w-4 shrink-0" aria-hidden />

  const color = toast.tone === 'error'
    ? 'border-[var(--app-danger)]/35 text-[var(--app-danger)]'
    : toast.tone === 'success'
      ? 'border-emerald-500/30 text-emerald-700 dark:text-emerald-300'
      : toast.tone === 'warning'
        ? 'border-amber-500/35 text-amber-800 dark:text-amber-300'
        : 'border-brand-500/25 text-brand-500'

  return (
    <div role={toast.tone === 'error' ? 'alert' : 'status'} className={`toast-enter pointer-events-auto flex items-start gap-3 rounded-xl border bg-[color-mix(in_srgb,var(--app-glass)_96%,var(--app-ink)_4%)] px-4 py-3 text-sm shadow-xl backdrop-blur-xl ${color}`}>
      {icon}
      <p className="min-w-0 flex-1 whitespace-pre-wrap break-words leading-5 text-ink">{toast.message}</p>
      <button type="button" onClick={() => onDismiss(toast.id)} className="-mr-1 -mt-1 rounded-md p-1 text-ink-faint hover:bg-glass-hover hover:text-ink" aria-label={t('notifications.dismiss')}>
        <X className="h-3.5 w-3.5" aria-hidden />
      </button>
    </div>
  )
}

function readHomeNotifications(): HomeNotification[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(HOME_NOTICE_KEY) ?? '[]')
    if (!Array.isArray(value)) return []
    return value.filter(isHomeNotification).slice(0, MAX_HOME_NOTICES)
  } catch {
    return []
  }
}

function saveHomeNotifications(notifications: HomeNotification[]) {
  try {
    localStorage.setItem(HOME_NOTICE_KEY, JSON.stringify(notifications))
    window.dispatchEvent(new Event(HOME_NOTICE_EVENT))
  } catch {
    // Keep the in-memory notice if browser storage is unavailable.
  }
}

function isHomeNotification(value: unknown): value is HomeNotification {
  if (typeof value !== 'object' || value === null) return false
  const item = value as Record<string, unknown>
  return typeof item.id === 'string'
    && typeof item.title === 'string'
    && typeof item.message === 'string'
    && typeof item.createdAt === 'number'
    && (item.href === undefined || typeof item.href === 'string')
}
