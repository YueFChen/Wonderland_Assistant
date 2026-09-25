import { Outlet } from 'react-router-dom'

import { TitleBar } from '../components/TitleBar'
import { AppBackdrop } from '../components/AppBackdrop'

/** 全应用共用的无边框窗口骨架。 */
export function AppLayout() {
  return (
    <div className="app-window relative z-10 h-full">
      <AppBackdrop />
      <TitleBar />
      <div className="h-full min-h-0">
        <Outlet />
      </div>
    </div>
  )
}
