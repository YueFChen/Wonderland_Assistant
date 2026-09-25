import { Outlet } from 'react-router-dom'

/**
 * Full-width home layout without the workspace sidebar.
 */
export function HomeLayout() {
  return (
    <div className="relative z-10 flex h-full flex-col">
      <main className="min-h-0 flex-1 overflow-hidden pt-8">
        <Outlet />
      </main>
    </div>
  )
}
