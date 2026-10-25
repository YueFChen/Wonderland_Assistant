import { forwardRef, useId, useLayoutEffect, useRef, type ButtonHTMLAttributes, type ReactNode, type RefObject } from 'react'
import { createPortal } from 'react-dom'
import './dialog.css'

export interface DialogProps {
  open: boolean
  title: ReactNode
  onClose: () => void
  children: ReactNode
  footer?: ReactNode
  size?: 'sm' | 'md' | 'lg' | 'xl'
  placement?: 'center' | 'top'
  dismissible?: boolean
  closeOnBackdrop?: boolean
  closeLabel?: string
  initialFocus?: RefObject<HTMLElement | null>
  headerContent?: ReactNode
  bodyClassName?: string
}

/** Native modality supplies focus containment and isolates the underlying page and frames. */
export function Dialog({ open, title, onClose, children, footer, size = 'md', placement = 'center', dismissible = true, closeOnBackdrop = true, closeLabel = 'Close', initialFocus, headerContent, bodyClassName = '' }: DialogProps) {
  const element = useRef<HTMLDialogElement>(null)
  const outsidePress = useRef(false)
  const titleId = useId()
  const focusTarget = useRef(initialFocus)
  focusTarget.current = initialFocus

  useLayoutEffect(() => {
    const dialog = element.current
    if (!open || !dialog) return
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null
    dialog.showModal()
    ;(focusTarget.current?.current ?? dialog.querySelector<HTMLElement>('[data-dialog-autofocus]') ?? dialog.querySelector<HTMLElement>('.app-dialog-close'))?.focus()
    return () => {
      dialog.close()
      if (previous?.isConnected && !document.querySelector('dialog[open]')) previous.focus()
    }
  }, [open])

  useLayoutEffect(() => {
    const dialog = element.current
    if (!dialog?.open) return
    const active = document.activeElement
    // Disabling the focused submit button can make Chromium focus <body>.
    if (!dialog.contains(active) || active?.matches(':disabled')) {
      const target = dialog.querySelector<HTMLElement>('[data-dialog-autofocus]:not(:disabled), .app-dialog-close:not(:disabled)')
      ;(target ?? dialog).focus()
    }
  })

  if (!open) return null
  const outside = (x: number, y: number) => {
    const box = element.current?.getBoundingClientRect()
    return box !== undefined && (x < box.left || x > box.right || y < box.top || y > box.bottom)
  }

  return createPortal(
    <dialog ref={element} className="app-dialog" data-size={size} data-placement={placement} aria-labelledby={titleId} aria-modal="true" tabIndex={-1}
      onKeyDown={event => {
        event.stopPropagation()
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'p') event.preventDefault()
        if (event.key !== 'Tab') return
        const dialog = event.currentTarget
        const targets = [...dialog.querySelectorAll<HTMLElement>('button, a[href], input, select, textarea, summary, [tabindex], [contenteditable=true]')]
          .filter(target => target.tabIndex >= 0 && !target.matches(':disabled') && target.getClientRects().length > 0)
        const first = targets[0]
        const last = targets.at(-1)
        if (!first) { event.preventDefault(); dialog.focus(); return }
        if (event.shiftKey && (document.activeElement === first || document.activeElement === dialog)) {
          event.preventDefault(); last?.focus()
        } else if (!event.shiftKey && (document.activeElement === last || document.activeElement === dialog)) {
          event.preventDefault(); first.focus()
        }
      }}
      onCancel={event => { event.preventDefault(); if (dismissible) onClose() }}
      onPointerDown={event => { outsidePress.current = event.target === event.currentTarget && outside(event.clientX, event.clientY) }}
      onPointerUp={event => { if (outsidePress.current && dismissible && closeOnBackdrop && outside(event.clientX, event.clientY)) onClose(); outsidePress.current = false }}>
      <header className="app-dialog-header">
        <h2 id={titleId} className={headerContent ? 'app-dialog-title app-dialog-title-hidden' : 'app-dialog-title'}>{title}</h2>
        {headerContent && <div className="app-dialog-heading-content">{headerContent}</div>}
        <button type="button" className="app-dialog-close" disabled={!dismissible} aria-label={closeLabel} onClick={onClose}>
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" /></svg>
        </button>
      </header>
      <div className={`app-dialog-body ${bodyClassName}`}>{children}</div>
      {footer && <footer className="app-dialog-footer">{footer}</footer>}
    </dialog>, document.body,
  )
}

export const DialogButton = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement> & { variant?: 'primary' | 'secondary' | 'danger' | 'quiet' }>(
  function DialogButton({ variant = 'secondary', className = '', type = 'button', ...props }, ref) {
    return <button {...props} ref={ref} type={type} data-variant={variant} className={`app-button app-dialog-button ${className}`} />
  },
)
