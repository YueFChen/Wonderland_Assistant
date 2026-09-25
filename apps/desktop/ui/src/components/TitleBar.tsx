import { isTauri } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Copy, Minus, Square, X } from 'lucide-react'
import { useEffect, useState } from 'react'

import { t } from '../i18n'
import './titlebar.css'

const windowHandle = isTauri() ? getCurrentWindow() : null

/** 无边框主窗口共用的拖动区和窗口操作。交互按钮不属于拖动热区。 */
export function TitleBar() {
  const [maximized, setMaximized] = useState(false)

  useEffect(() => {
    if (!windowHandle) return
    let live = true
    const sync = () => {
      void windowHandle.isMaximized().then((value) => {
        if (live) setMaximized(value)
      }).catch(() => undefined)
    }
    sync()
    const listener = windowHandle.onResized(sync)
    return () => {
      live = false
      void listener.then((unlisten) => unlisten()).catch(() => undefined)
    }
  }, [])

  return (
    <header className="app-titlebar" data-tauri-drag-region>
      <div className="app-titlebar-drag" data-tauri-drag-region />
      <div className="app-titlebar-controls">
        <button type="button" aria-label={t('window.minimize')} title={t('window.minimize')} onClick={() => { if (windowHandle) void windowHandle.minimize() }}>
          <Minus aria-hidden />
        </button>
        <button
          type="button"
          aria-label={maximized ? t('window.restore') : t('window.maximize')}
          title={maximized ? t('window.restore') : t('window.maximize')}
          onClick={() => { if (windowHandle) void windowHandle.toggleMaximize().then(() => windowHandle.isMaximized()).then(setMaximized) }}
        >
          {maximized ? <Copy aria-hidden /> : <Square aria-hidden />}
        </button>
        <button type="button" className="app-titlebar-close" aria-label={t('window.close')} title={t('window.close')} onClick={() => { if (windowHandle) void windowHandle.close() }}>
          <X aria-hidden />
        </button>
      </div>
    </header>
  )
}
