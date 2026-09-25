import { createHashRouter, Navigate } from 'react-router-dom'

import { HomeLayout } from './layouts/HomeLayout'
import { AppLayout } from './layouts/AppLayout'
import { WorkspaceLayout } from './layouts/WorkspaceLayout'
import { AccountPage } from './pages/AccountPage'
import { HomePage } from './pages/HomePage'
import { PluginPage } from './pages/PluginPage'
import { SettingsPage } from './pages/SettingsPage'
import { WorkspacePage } from './pages/WorkspacePage'

/**
 * Configure application routes and page layouts.
 */
export const router = createHashRouter([
  {
    path: '/',
    Component: AppLayout,
    children: [
      { Component: HomeLayout, children: [{ index: true, Component: HomePage }] },
      {
        path: 'workspace',
        Component: WorkspaceLayout,
        children: [
          { index: true, Component: WorkspacePage },
          { path: 'account', Component: AccountPage },
          { path: 'settings', Component: SettingsPage },
          { path: 'plugin/:pluginId/:contributionId', Component: PluginPage },
        ],
      },
      { path: '*', element: <Navigate to="/" replace /> },
    ],
  },
])
