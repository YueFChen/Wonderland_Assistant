import { useEffect, useLayoutEffect, useRef, useState, type DragEvent, type KeyboardEvent, type RefObject } from 'react'
import { Plus, X } from 'lucide-react'
import { t } from '../i18n'
import type { WorkspaceContribution } from '../plugins/contributions'
import { PluginIcon } from '../plugins/icons'
import { pluginTabId, pluginPanelId } from './pluginTabIds'

const TAB_DRAG = 'application/x-wonderland-tab'

interface Props {
  tabs: readonly WorkspaceContribution[]
  activeId: string
  buttons: RefObject<Map<string, HTMLButtonElement>>
  onSelect: (item: WorkspaceContribution) => void
  onClose: (id: string) => void
  onReorder: (id: string, targetId: string) => void
}

/** Browser-style chrome; reordering tabs never reorders the mounted plugin frames. */
export function PluginTabs({ tabs, activeId, buttons, onSelect, onClose, onReorder }: Props) {
  const list = useRef<HTMLDivElement>(null)
  const [dragged, setDragged] = useState<string | null>(null)
  const dragSource = useRef<string | null>(null)
  const [target, setTarget] = useState<string | null>(null)

  useLayoutEffect(() => {
    buttons.current.get(activeId)?.scrollIntoView({ block: 'nearest', inline: 'nearest' })
  }, [activeId, buttons])

  useEffect(() => {
    const node = list.current
    if (!node) return
    const wheel = (event: WheelEvent) => {
      if (event.ctrlKey || Math.abs(event.deltaX) >= Math.abs(event.deltaY) || node.scrollWidth <= node.clientWidth) return
      event.preventDefault()
      const unit = event.deltaMode === 1 ? 20 : event.deltaMode === 2 ? node.clientWidth : 1
      node.scrollLeft += event.deltaY * unit
    }
    node.addEventListener('wheel', wheel, { passive: false })
    return () => node.removeEventListener('wheel', wheel)
  }, [])

  const keyDown = (event: KeyboardEvent<HTMLButtonElement>, item: WorkspaceContribution) => {
    if (event.key === 'Delete') { event.preventDefault(); onClose(item.id); return }
    const index = tabs.findIndex(tab => tab.id === item.id)
    if (index < 0) return
    if (event.ctrlKey && event.shiftKey && (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
      event.preventDefault()
      const neighbor = tabs[index + (event.key === 'ArrowLeft' ? -1 : 1)]
      if (neighbor) {
        onReorder(item.id, neighbor.id)
        requestAnimationFrame(() => buttons.current.get(item.id)?.scrollIntoView({ block: 'nearest', inline: 'nearest' }))
      }
      return
    }
    let next: number | undefined
    if (event.key === 'ArrowRight') next = (index + 1) % tabs.length
    if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length
    if (event.key === 'Home') next = 0
    if (event.key === 'End') next = tabs.length - 1
    if (next === undefined) return
    event.preventDefault()
    onSelect(tabs[next])
    requestAnimationFrame(() => buttons.current.get(tabs[next].id)?.focus())
  }
  const hoverTab = (event: DragEvent, id: string) => {
    if (dragSource.current && dragSource.current !== id) {
      event.preventDefault()
      event.dataTransfer.dropEffect = 'move'
      setTarget(id)
    }
  }
  return <div className="plugin-surface-tabs" data-tauri-drag-region>
    <div ref={list} className="plugin-tab-list" role="tablist" aria-orientation="horizontal" aria-label={t('pluginPage.tabs')} data-tauri-drag-region>
      {tabs.map((item, index) => <div key={item.id} role="presentation" className={`plugin-surface-tab${item.id === activeId ? ' is-active' : ''}${dragged === item.id ? ' is-dragging' : ''}`}
        data-drop-side={target === item.id && dragged ? tabs.findIndex(tab => tab.id === dragged) < index ? 'end' : 'start' : undefined}
        draggable={tabs.length > 1}
        onDragStart={event => { dragSource.current = item.id; setDragged(item.id); event.dataTransfer.effectAllowed = 'move'; event.dataTransfer.setData(TAB_DRAG, item.id) }}
        onDragEnter={event => hoverTab(event, item.id)}
        onDragOver={event => hoverTab(event, item.id)}
        onDragLeave={event => {
          if (!(event.relatedTarget instanceof Node) || !event.currentTarget.contains(event.relatedTarget)) setTarget(null)
        }}
        onDragEnd={() => { dragSource.current = null; setDragged(null); setTarget(null) }}
        onDrop={event => {
          const source = event.dataTransfer.getData(TAB_DRAG)
          if (!tabs.some(tab => tab.id === source)) return
          event.preventDefault(); event.stopPropagation(); onReorder(source, item.id); dragSource.current = null; setDragged(null); setTarget(null)
        }}
        onPointerDown={event => { if (event.button === 1) event.preventDefault() }}
        onAuxClick={event => { if (event.button === 1) { event.preventDefault(); onClose(item.id) } }}>
        <button ref={node => { if (node) buttons.current.set(item.id, node); else buttons.current.delete(item.id) }}
          type="button" role="tab" id={pluginTabId(item.id)} aria-controls={pluginPanelId(item.id)} aria-selected={item.id === activeId}
          tabIndex={item.id === activeId ? 0 : -1} onClick={() => onSelect(item)} onKeyDown={event => keyDown(event, item)} title={item.title}>
          <PluginIcon icon={item.icon} pluginId={item.pluginId} className="plugin-tab-icon" />
          <span className="plugin-tab-title">{item.title}</span>
        </button>
        <button type="button" className="plugin-surface-tab-close" tabIndex={-1} aria-label={t('pluginPage.closeTab', { title: item.title })} title={t('pluginPage.closeTab', { title: item.title })} onClick={() => onClose(item.id)}><X aria-hidden /></button>
      </div>)}
    </div>
    <div className="plugin-tab-tools">
      <button type="button" className="plugin-tab-tool" aria-label={t('pluginPage.openTool')} title={t('pluginPage.openTool')} onClick={() => window.dispatchEvent(new Event('wonderland:open-command-palette'))}><Plus aria-hidden /></button>
    </div>
  </div>
}
