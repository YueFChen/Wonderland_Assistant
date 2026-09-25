import { createHashRouter, Navigate, redirect } from 'react-router-dom'

import { HomeLayout } from './layouts/HomeLayout'
import { AppLayout } from './layouts/AppLayout'
import { WorkspaceLayout } from './layouts/WorkspaceLayout'
import { HomePage } from './pages/HomePage'
import { PluginPage } from './pages/PluginPage'
import { PluginInstallPage } from './pages/PluginInstallPage'
import { SettingsPage } from './pages/SettingsPage'
import { WorkspacePage } from './pages/WorkspacePage'
import { lastWorkPageToResume } from './startupMemory'

let startupResumePending = isInitialHomeRoute()

/**
 * Configure application routes and page layouts.
 */
export const router = createHashRouter([
  {
    path: '/',
    Component: AppLayout,
    children: [
      {
        Component: HomeLayout,
        children: [{ index: true, loader: restoreLastWorkPage, Component: HomePage }],
      },
      {
        path: 'workspace',
        Component: WorkspaceLayout,
        children: [
          { index: true, Component: WorkspacePage },
          { path: 'account', element: <Navigate to="/workspace/settings" replace /> },
          { path: 'settings', Component: SettingsPage },
          { path: 'plugins/install', Component: PluginInstallPage },
          { path: 'plugin/:pluginId/:contributionId', Component: PluginPage },
        ],
      },
      { path: '*', element: <Navigate to="/" replace /> },
    ],
  },
])

function restoreLastWorkPage() {
  if (!startupResumePending) return null
  startupResumePending = false
  const path = lastWorkPageToResume()
  return path ? redirect(path) : null
}

function isInitialHomeRoute(): boolean {
  const route = window.location.hash.startsWith('#')
    ? window.location.hash.slice(1)
    : window.location.hash
  return route === '' || route === '/'
}
