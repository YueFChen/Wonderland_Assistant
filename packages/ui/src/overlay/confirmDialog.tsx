import { createRef, type ReactNode } from 'react'
import { createRoot } from 'react-dom/client'
import { Dialog, DialogButton } from './Dialog.tsx'

export interface ConfirmDialogOptions {
  title: string
  message: ReactNode
  confirmLabel?: string
  cancelLabel?: string
  closeLabel?: string
  danger?: boolean
}

/** Shared native confirmation for handlers outside the React component tree. */
export function confirmDialog({ title, message, confirmLabel = '确认', cancelLabel = '取消', closeLabel = '关闭', danger = false }: ConfirmDialogOptions): Promise<boolean> {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  const cancel = createRef<HTMLButtonElement>()
  return new Promise(resolve => {
    let settled = false
    const finish = (accepted: boolean) => {
      if (settled) return
      settled = true
      queueMicrotask(() => { root.unmount(); container.remove(); resolve(accepted) })
    }
    root.render(<Dialog open title={title} onClose={() => finish(false)} closeLabel={closeLabel} initialFocus={cancel} size="sm"
      footer={<>
        <DialogButton ref={cancel} onClick={() => finish(false)}>{cancelLabel}</DialogButton>
        <DialogButton variant={danger ? 'danger' : 'primary'} onClick={() => finish(true)}>{confirmLabel}</DialogButton>
      </>}>
      {typeof message === 'string' ? <p>{message}</p> : message}
    </Dialog>)
  })
}
