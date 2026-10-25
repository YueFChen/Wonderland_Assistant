import { useId, useLayoutEffect, useRef, useState, type PointerEvent, type ReactNode, type RefObject } from 'react'
import { createPortal } from 'react-dom'
import './drawer.css'

export interface DrawerProps {
  open: boolean
  title: ReactNode
  onClose: () => void
  children: ReactNode
  footer?: ReactNode
  size?: 'sm' | 'md' | 'lg' | 'xl'
  dismissible?: boolean
  closeLabel?: string
  initialFocus?: RefObject<HTMLElement | null>
  bodyClassName?: string
}

const widths = { sm: 360, md: 440, lg: 560, xl: 720 }
const openDrawers: HTMLElement[] = []

/** Frame-local, non-modal sidebar for settings, editing and details. */
export function Drawer({ open, title, onClose, children, footer, size = 'md', dismissible = true, closeLabel = '关闭', initialFocus, bodyClassName = '' }: DrawerProps) {
  const element = useRef<HTMLElement>(null)
  const close = useRef({ onClose, dismissible, initialFocus })
  close.current = { onClose, dismissible, initialFocus }
  const [width, setWidth] = useState(widths[size])
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth)
  const resizing = useRef<{ x: number; width: number } | null>(null)
  const lastFocused = useRef<HTMLElement | null>(null)
  const titleId = useId()
  const maximum = Math.max(0, Math.min(720, viewportWidth - 16))
  const minimum = Math.min(280, maximum)
  const clamp = (value: number) => Math.max(minimum, Math.min(maximum, Math.round(value)))
  const visibleWidth = clamp(width)

  useLayoutEffect(() => {
    const panel = element.current
    if (!open || !panel) return
    const resize = () => setViewportWidth(window.innerWidth)
    resize()
    window.addEventListener('resize', resize)
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null
    openDrawers.push(panel)
    ;(close.current.initialFocus?.current ?? panel.querySelector<HTMLElement>('[data-drawer-autofocus]:not(:disabled)') ?? panel.querySelector<HTMLElement>('.app-drawer-close:not(:disabled)') ?? panel).focus()
    const escape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.isComposing || event.defaultPrevented || openDrawers.at(-1) !== panel || document.querySelector('dialog[open], [role="dialog"][aria-modal="false"]')) return
      event.preventDefault()
      event.stopPropagation()
      if (close.current.dismissible) close.current.onClose()
    }
    document.addEventListener('keydown', escape, true)
    return () => {
      const restore = panel.contains(document.activeElement) || document.activeElement === document.body
      const index = openDrawers.indexOf(panel)
      if (index >= 0) openDrawers.splice(index, 1)
      document.removeEventListener('keydown', escape, true)
      window.removeEventListener('resize', resize)
      resizing.current = null
      if (restore && previous?.isConnected && !document.querySelector('dialog[open]')) previous.focus()
    }
  }, [open])

  useLayoutEffect(() => {
    const target = lastFocused.current
    if (open && dismissible && document.activeElement === document.body && target?.isConnected && !target.matches(':disabled') && !document.querySelector('dialog[open]')) target.focus()
  }, [open, dismissible])

  if (!open) return null
  const endResize = (event: PointerEvent<HTMLDivElement>) => {
    resizing.current = null
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId)
  }
  return createPortal(
    <aside ref={element} className="app-drawer" style={{ width: visibleWidth }} aria-labelledby={titleId} tabIndex={-1} onFocusCapture={event => { if (event.target instanceof HTMLElement) lastFocused.current = event.target }} onKeyDown={event => { if (event.key !== 'Escape') event.stopPropagation() }}>
      <div className="app-drawer-resizer" role="separator" tabIndex={0} aria-label="调整侧栏宽度" aria-orientation="vertical" aria-valuemin={minimum} aria-valuemax={maximum} aria-valuenow={visibleWidth}
        onPointerDown={event => { if (event.button !== 0) return; event.preventDefault(); resizing.current = { x: event.clientX, width: visibleWidth }; event.currentTarget.setPointerCapture(event.pointerId) }}
        onPointerMove={event => { if (resizing.current) setWidth(clamp(resizing.current.width + resizing.current.x - event.clientX)) }}
        onPointerUp={endResize} onPointerCancel={endResize} onLostPointerCapture={() => { resizing.current = null }}
        onKeyDown={event => { if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return; event.preventDefault(); setWidth(event.key === 'Home' ? minimum : event.key === 'End' ? maximum : clamp(visibleWidth + (event.key === 'ArrowLeft' ? 20 : -20))) }} />
      <header className="app-drawer-header">
        <h2 id={titleId}>{title}</h2>
        <button type="button" className="app-drawer-close" disabled={!dismissible} aria-label={closeLabel} onClick={onClose}>
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" /></svg>
        </button>
      </header>
      <div className={`app-drawer-body ${bodyClassName}`}>{children}</div>
      {footer && <footer className="app-drawer-footer">{footer}</footer>}
    </aside>, document.body,
  )
}
