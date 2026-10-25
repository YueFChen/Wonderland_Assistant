import { useCallback, useEffect, useState, type ReactNode } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { isTauri } from '@tauri-apps/api/core'
import { Power } from 'lucide-react'
import { Dialog, DialogButton } from '@wonderland/ui'

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
      <Dialog open={dialogOpen} title={t('closeChoice.title')} closeLabel={t('common.close')} size="sm" dismissible={!busy} onClose={() => setDialogOpen(false)} footer={<>
        <DialogButton data-dialog-autofocus disabled={busy} onClick={() => setDialogOpen(false)}>{t('common.cancel')}</DialogButton>
        <DialogButton variant="danger" disabled={busy} onClick={() => void choose('exit')}><Power className="h-3.5 w-3.5" aria-hidden />{t('closeChoice.exit')}</DialogButton>
        <DialogButton variant="primary" disabled={busy} onClick={() => void choose('tray')}>{t('closeChoice.tray')}</DialogButton>
      </>}>
        <p className="app-dialog-description">{t('closeChoice.description')}</p>
        <label className="mt-4 flex items-center gap-2 text-xs text-ink-muted"><input type="checkbox" checked={rememberChoice} onChange={event => setRememberChoice(event.currentTarget.checked)} className="accent-[var(--app-accent)]" />{t('closeChoice.remember')}</label>
      </Dialog>
    </>
  )
}
