import { useCallback, useEffect, useState, type ReactNode } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { isTauri } from '@tauri-apps/api/core'
import { Power, X } from 'lucide-react'

import { t } from '../i18n'
import { coreApi } from '../core/api'
import { readCloseBehavior, writeCloseBehavior, type CloseBehavior } from '../closeBehavior'
import { useNotifications } from './Notifications'

export function CloseBehaviorProvider({ children }: { children: ReactNode }) {
  const { notify } = useNotifications()
  const [dialogOpen, setDialogOpen] = useState(false)
  const [rememberChoice, setRememberChoice] = useState(false)
  const [busy, setBusy] = useState(false)

  const hideToTray = useCallback(async () => {
    try {
      await getCurrentWindow().hide()
    } catch {
      notify(t('closeChoice.hideFailed'), { tone: 'error' })
    }
  }, [notify])

  useEffect(() => {
    if (!isTauri()) return
    let live = true
    let unlisten: (() => void) | undefined
    void coreApi.onCloseRequested(() => {
      const behavior = readCloseBehavior()
      if (behavior === 'tray') {
        void hideToTray()
        return
      }
      if (behavior === 'exit') {
        void coreApi.exit()
        return
      }
      setRememberChoice(false)
      setDialogOpen(true)
    }).then((dispose) => {
      if (live) unlisten = dispose
      else dispose()
    })
    return () => {
      live = false
      unlisten?.()
    }
  }, [hideToTray])

  const choose = async (choice: Exclude<CloseBehavior, 'ask'>) => {
    setBusy(true)
    if (rememberChoice) {
      try {
        writeCloseBehavior(choice)
      } catch {
        // The explicit choice still applies to this close when storage is unavailable.
      }
    }
    setDialogOpen(false)
    try {
      if (choice === 'tray') await hideToTray()
      else await coreApi.exit()
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      {children}
      {dialogOpen && (
        <div className="fixed inset-0 z-[160] grid place-items-center bg-slate-950/45 p-5 backdrop-blur-sm" role="presentation">
          <section role="dialog" aria-modal="true" aria-labelledby="close-choice-title" className="glass-card w-full max-w-lg rounded-2xl border border-glass-line p-6 shadow-2xl">
            <div className="flex items-start justify-between gap-4">
              <div>
                <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-brand-500">Wonderland Assistant</p>
                <h2 id="close-choice-title" className="mt-1 text-lg font-semibold text-ink">{t('closeChoice.title')}</h2>
              </div>
              <button type="button" disabled={busy} onClick={() => setDialogOpen(false)} className="rounded-lg p-2 text-ink-faint hover:bg-glass-hover hover:text-ink disabled:opacity-50" aria-label={t('common.cancel')}>
                <X className="h-4 w-4" aria-hidden />
              </button>
            </div>
            <p className="mt-3 text-sm leading-6 text-ink-muted">{t('closeChoice.description')}</p>
            <label className="mt-5 flex items-center gap-2 text-xs text-ink-muted">
              <input type="checkbox" checked={rememberChoice} onChange={(event) => setRememberChoice(event.currentTarget.checked)} className="accent-brand-500" />
              {t('closeChoice.remember')}
            </label>
            <div className="mt-6 flex flex-wrap justify-end gap-2">
              <button type="button" disabled={busy} onClick={() => setDialogOpen(false)} className="rounded-lg border border-glass-line px-4 py-2 text-sm text-ink hover:bg-glass-hover disabled:opacity-50">{t('common.cancel')}</button>
              <button type="button" disabled={busy} onClick={() => void choose('exit')} className="inline-flex items-center gap-2 rounded-lg border border-[var(--app-danger)]/35 px-4 py-2 text-sm text-[var(--app-danger)] hover:bg-[var(--app-danger)]/10 disabled:opacity-50">
                <Power className="h-4 w-4" aria-hidden />{t('closeChoice.exit')}
              </button>
              <button type="button" disabled={busy} onClick={() => void choose('tray')} className="rounded-lg bg-brand-600 px-4 py-2 text-sm font-semibold text-white hover:bg-brand-500 disabled:opacity-50">{t('closeChoice.tray')}</button>
            </div>
          </section>
        </div>
      )}
    </>
  )
}
