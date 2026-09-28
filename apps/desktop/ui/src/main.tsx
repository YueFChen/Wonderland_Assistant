import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { RouterProvider } from 'react-router-dom'

import { router } from './router'
import { ThemeProvider } from './theme/ThemeProvider'
import { NotificationProvider } from './components/Notifications'
import { CoreUpdateProvider } from './components/CoreUpdateProvider'
import { CloseBehaviorProvider } from './components/CloseBehaviorProvider'
import './styles/index.css'

const container = document.getElementById('root')
if (!container) {
  throw new Error('挂载节点 #root 不存在') // i18n-allow: invariant assertion
}

createRoot(container).render(
  <StrictMode>
    <ThemeProvider>
      <NotificationProvider>
        <CoreUpdateProvider>
          <CloseBehaviorProvider>
            <RouterProvider router={router} />
          </CloseBehaviorProvider>
        </CoreUpdateProvider>
      </NotificationProvider>
    </ThemeProvider>
  </StrictMode>,
)
