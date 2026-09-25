import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from 'react'
import { createPortal } from 'react-dom'
import { t } from '../i18n/index.ts'

/** Panel metadata displayed by the host. */
interface Registration {
  id: string
  title: string
  onClose: () => void
}

interface SidebarValue {
  panels: Registration[]
  /** Active panel ID. */
  activeId: string | null
  setActiveId: (id: string) => void
  width: number
  setWidth: (width: number) => void
  /** 面板内容的落点，由 `SidebarHost` 提供。 */
  body: HTMLElement | null
  setBody: (node: HTMLElement | null) => void
  register: (panel: Registration) => void
  unregister: (id: string) => void
}

const SidebarContext = createContext<SidebarValue | null>(null)

export const SIDEBAR_MIN_WIDTH = 280
export const SIDEBAR_MAX_WIDTH = 720
const DEFAULT_WIDTH = 360

/**
 * Shared right sidebar for registered panels.
 */
export function SidebarProvider({ children }: { children: ReactNode }) {
  const [panels, setPanels] = useState<Registration[]>([])
  const [preferredId, setPreferredId] = useState<string | null>(null)
  const [width, setWidth] = useState(DEFAULT_WIDTH)
  const [body, setBody] = useState<HTMLElement | null>(null)

  const register = useCallback((panel: Registration) => {
    setPanels((current) => {
      const index = current.findIndex((item) => item.id === panel.id)
      if (index < 0) return [...current, panel]
      if (current[index].title === panel.title) return current
      const next = [...current]
      next[index] = panel
      return next
    })
  }, [])

  const unregister = useCallback((id: string) => {
    setPanels((current) =>
      current.some((item) => item.id === id)
        ? current.filter((item) => item.id !== id)
        : current,
    )
  }, [])

  const active = panels.find((panel) => panel.id === preferredId) ?? panels[0] ?? null
  const activeId = active?.id ?? null

  const value = useMemo(
    () => ({
      panels,
      activeId,
      setActiveId: setPreferredId,
      width,
      setWidth,
      body,
      setBody,
      register,
      unregister,
    }),
    [panels, activeId, width, body, register, unregister],
  )
  return <SidebarContext.Provider value={value}>{children}</SidebarContext.Provider>
}

export function useSidebar(): SidebarValue {
  const value = useContext(SidebarContext)
  if (!value) throw new Error('侧栏必须在 <SidebarProvider> 内使用') // i18n-allow: invariant assertion
  return value
}

interface PanelOptions {
  /** 面板标识：同一个 id 重复声明视为更新，不会叠出两个。 */
  id: string
  title: string
  /** 是否显示；由声明方（页面）决定。 */
  open: boolean
  /** 标题栏关闭按钮的动作：声明方据此把 `open` 置 false。 */
  onClose: () => void
}

/**
 * Register panel metadata and visibility state.
 */
export function useSidebarPanel({ id, title, open, onClose }: PanelOptions) {
  const { register, unregister } = useSidebar()
  const close = useRef(onClose)
  close.current = onClose
  const stableClose = useCallback(() => close.current(), [])

  useEffect(() => {
    if (!open) return
    register({ id, title, onClose: stableClose })
    return () => unregister(id)
  }, [id, title, open, stableClose, register, unregister])
}

export interface SidebarPanelProps extends PanelOptions {
  children: ReactNode
}

/**
 * Register a panel and render its content in the host sidebar.
 */
export function SidebarPanel({ children, ...options }: SidebarPanelProps) {
  const { body, panels, activeId } = useSidebar()
  useSidebarPanel(options)

  if (!options.open || !body) return null
  const visible = panels.length <= 1 || activeId === options.id
  return createPortal(
    <div className="ws-sidebar-pane" hidden={!visible}>
      {children}
    </div>,
    body,
  )
}

/** 宿主侧的容器：没有面板注册时完全不占位。 */
export function SidebarHost() {
  const { panels, activeId, setActiveId, width, setWidth, setBody } = useSidebar()
  const active = panels.find((panel) => panel.id === activeId) ?? null
  const resizing = useRef(false)

  const startResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault()
    resizing.current = true
    event.currentTarget.setPointerCapture(event.pointerId)
  }
  const moveResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!resizing.current) return
    setWidth(clampWidth(window.innerWidth - event.clientX))
  }
  const endResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    resizing.current = false
    event.currentTarget.releasePointerCapture(event.pointerId)
  }

  if (!active) return null

  return (
    <>
    <div className="ws-sidebar-scrim" onClick={active.onClose} aria-hidden />
    <aside className="ws-sidebar" style={{ width }} aria-label={t('sidebar.aria')}>
      <div
        className="ws-sidebar-resizer"
        role="separator"
        aria-orientation="vertical"
        aria-label={t('sidebar.resize')}
        onPointerDown={startResize}
        onPointerMove={moveResize}
        onPointerUp={endResize}
        onPointerCancel={endResize}
      />

      <header className="ws-sidebar-head">
        {panels.length > 1 ? (
          <div className="ws-sidebar-tabs" role="tablist">
            {panels.map((panel) => (
              <button
                key={panel.id}
                type="button"
                role="tab"
                className="ws-sidebar-tab"
                aria-selected={panel.id === active.id}
                onClick={() => setActiveId(panel.id)}
              >
                {panel.title}
              </button>
            ))}
          </div>
        ) : (
          <span className="ws-sidebar-title" title={active.title}>
            {active.title}
          </span>
        )}
        <button
          type="button"
          className="ws-sidebar-close"
          title={t('sidebar.close', { title: active.title })}
          aria-label={t('sidebar.close', { title: active.title })}
          onClick={active.onClose}
        >
          ×
        </button>
      </header>

      <div className="ws-sidebar-body" ref={setBody} />
    </aside>
    </>
  )
}

function clampWidth(width: number): number {
  return Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, Math.round(width)))
}
