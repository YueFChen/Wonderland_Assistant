import { ArrowRight, Bell, Clock3, FileText, LayoutGrid, Puzzle, Settings2, X } from 'lucide-react'
import { Link, useNavigate } from 'react-router-dom'

import brandAvatar from '../assets/brand-avatar.png'
import githubMark from '../assets/github-mark.svg'
import { t } from '../i18n'
import { buildContributionRegistry, openContribution } from '../plugins/contributions'
import { PluginIcon } from '../plugins/icons'
import { usePlugins } from '../plugins/api'
import { useWorkspaceLayout } from '../plugins/workspaceLayout'
import { useNotifications } from '../components/Notifications'
import { useCoreUpdate } from '../components/CoreUpdateProvider'
import './HomePage.css'

/** Home is the branded entry point for the Wonderland plugin workspace. */
export function HomePage() {
  const navigate = useNavigate()
  const { states } = usePlugins()
  const { layout } = useWorkspaceLayout()
  const { homeNotifications, dismissHomeNotification } = useNotifications()
  const { hasUpdate, update } = useCoreUpdate()
  const registry = buildContributionRegistry(states)
  const recent = [...layout.openContributionIds]
    .reverse()
    .map((id) => registry.find((item) => item.id === id && item.kind === 'activity'))
    .filter((item): item is NonNullable<typeof item> => item !== undefined)
    .slice(0, 3)

  return (
    <section className="flex h-full min-h-0 w-full">
      <aside className="relative flex w-[32%] min-w-[19rem] max-w-[34rem] shrink-0 flex-col overflow-hidden border-r border-dashed border-glass-line px-8 py-10 md:px-10 lg:px-14">
        <div className="relative flex flex-1 flex-col justify-center">
          <div className="home-avatar mb-8 size-24 shrink-0 sm:size-28 xl:mb-10 xl:size-32 2xl:size-40">
            <img src={brandAvatar} alt="" className="h-full w-full object-contain" />
          </div>
          <h1 aria-label="Wonderland Assistant" className="app-background-copy brand-text home-wordmark max-w-[18rem] text-4xl leading-[0.98] md:text-5xl 2xl:max-w-[28rem] 2xl:text-6xl">
            {['Wonderland', 'Assistant'].map((line, index) => <span aria-hidden key={line} className={`block${index ? ' mt-1' : ''}`}>{line}</span>)}
          </h1>
          <div aria-hidden className="mt-7 flex w-36 items-center gap-2">
            <span className="h-px flex-1 border-t border-dashed border-brand-500/40" />
            <span className="h-1.5 w-1.5 rounded-full bg-brand-400/70" />
          </div>
        </div>
        <footer className="home-meta">
          <a href="https://github.com/YueFChen/Wonderland_Assistant" target="_blank" rel="noopener noreferrer" className="home-meta-link home-meta-repository app-background-copy" aria-label={t('home.repository')} title={t('home.repository')}>
            <span className="home-meta-github" style={{ maskImage: `url("${githubMark}")` }} aria-hidden />
            <span className="home-meta-repository-label">GitHub</span>
          </a>
          <span className="home-meta-credit">
            <span className="app-background-copy">© {new Date().getFullYear()}</span>
            <a href="https://github.com/YueFChen" target="_blank" rel="noopener noreferrer" className="home-meta-link app-background-copy" aria-label={t('home.author', { author: 'YueFChen' })} title={t('home.author', { author: 'YueFChen' })}>
              YueFChen
            </a>
          </span>
          <a href="https://www.apache.org/licenses/LICENSE-2.0" target="_blank" rel="noopener noreferrer" className="home-meta-link home-meta-license app-background-copy" title={t('home.license')}>
            <FileText aria-hidden />
            Apache-2.0
          </a>
        </footer>
      </aside>

      <div className="flex min-h-0 min-w-0 flex-1 overflow-y-auto px-7 py-8 md:px-10 xl:px-14 xl:py-12 2xl:px-20">
        <div className="mx-auto my-auto w-full max-w-3xl space-y-8 xl:max-w-5xl 2xl:max-w-6xl">
          {homeNotifications.length > 0 && (
            <section aria-labelledby="home-notifications-heading" className="glass-card rounded-2xl border border-brand-500/20 p-4 sm:p-5">
              <div className="mb-3 flex items-center gap-2 text-brand-500">
                <Bell className="h-4 w-4" aria-hidden />
                <h2 id="home-notifications-heading" className="text-xs font-bold uppercase tracking-[0.16em]">{t('home.notificationsTitle')}</h2>
                <span className="ml-auto text-xs text-ink-faint">{homeNotifications.length}</span>
              </div>
              <ul className="divide-y divide-glass-line">
                {homeNotifications.map((notice) => (
                  <li key={notice.id} className="flex items-start gap-3 py-3 first:pt-0 last:pb-0">
                    <span className="min-w-0 flex-1">
                      <span className="block text-sm font-semibold text-ink">{notice.title}</span>
                      <span className="mt-1 block whitespace-pre-wrap text-xs leading-5 text-ink-muted">{notice.message}</span>
                    </span>
                    {notice.href && <Link to={notice.href} className="shrink-0 pt-0.5 text-xs font-semibold text-brand-500 hover:text-brand-400">{t('home.notificationOpen')}</Link>}
                    <button type="button" onClick={() => dismissHomeNotification(notice.id)} className="app-icon-button shrink-0" aria-label={t('notifications.dismiss')}>
                      <X className="h-3.5 w-3.5" aria-hidden />
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}

          <section aria-labelledby="home-recent-heading" className="border-t border-dashed border-glass-line pt-7">
            <div className="mb-3 flex items-center justify-between gap-3">
              <h2 id="home-recent-heading" className="app-background-copy flex items-center gap-2 text-xs font-bold uppercase tracking-[0.16em] text-ink-faint">
                <Clock3 className="h-4 w-4" aria-hidden />
                {t('home.recentTitle')}
              </h2>
            </div>
            {recent.length === 0 ? (
              <div className="glass-card rounded-2xl border border-glass-line px-4 py-4">
                <p className="text-sm text-ink-muted">{t('home.recentEmpty')}</p>
              </div>
            ) : (
              <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                {recent.map((item) => {
                  return (
                    <button key={item.id} type="button" onClick={() => openContribution(registry, item.id, navigate)} className="glass-card flex min-w-0 items-center gap-3 rounded-2xl border border-glass-line p-4 text-left transition hover:border-brand-500/30 hover:bg-glass-hover">
                      <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-brand-600/10 text-brand-400">
                        <PluginIcon icon={item.icon} pluginId={item.pluginId} className="h-5 w-5" />
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-sm font-semibold text-ink">{item.title}</span>
                        {item.state.manifest.name !== item.title && <span className="mt-1 block truncate text-xs text-ink-faint">{item.state.manifest.name}</span>}
                      </span>
                      <ArrowRight className="h-4 w-4 shrink-0 text-ink-faint" aria-hidden />
                    </button>
                  )
                })}
              </div>
            )}
          </section>

          <section aria-label={t('home.shortcutsTitle')} className="grid gap-3 border-t border-dashed border-glass-line pt-7 sm:grid-cols-2 lg:grid-cols-3">
            <Link to="/workspace" className="glass-card group flex min-h-20 items-center gap-3 rounded-2xl border border-glass-line p-4 transition hover:border-brand-500/30 hover:bg-glass-hover">
              <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-brand-600/10 text-brand-400">
                <LayoutGrid className="h-5 w-5" aria-hidden />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-semibold text-ink">{t('home.workspaceTitle')}</span>
              </span>
              <ArrowRight className="h-4 w-4 shrink-0 text-ink-faint transition group-hover:translate-x-0.5 group-hover:text-brand-400" aria-hidden />
            </Link>
            <Link to="/workspace?view=plugins" className="glass-card group flex min-h-20 items-center gap-3 rounded-2xl border border-glass-line p-4 transition hover:border-brand-500/30 hover:bg-glass-hover">
              <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-brand-600/10 text-brand-400">
                <Puzzle className="h-5 w-5" aria-hidden />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-semibold text-ink">{t('home.pluginManagementTitle')}</span>
              </span>
              <ArrowRight className="h-4 w-4 shrink-0 text-ink-faint transition group-hover:translate-x-0.5 group-hover:text-brand-400" aria-hidden />
            </Link>
            <Link to="/workspace/settings" className="glass-card group flex min-h-20 items-center gap-3 rounded-2xl border border-glass-line p-4 transition hover:border-brand-500/30 hover:bg-glass-hover">
              <span className="relative grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-brand-600/10 text-brand-400">
                <Settings2 className="h-5 w-5" aria-hidden />
                {hasUpdate && <span aria-hidden className="absolute right-0 top-0 h-2.5 w-2.5 rounded-full bg-[var(--app-accent)]" />}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-semibold text-ink">{t('home.settingsTitle')}</span>
                {hasUpdate && update && <span className="mt-1 block text-xs leading-5 text-ink-muted">{t('settings.update.available', { version: update.version })}</span>}
              </span>
              <ArrowRight className="h-4 w-4 shrink-0 text-ink-faint transition group-hover:translate-x-0.5 group-hover:text-brand-400" aria-hidden />
            </Link>
          </section>
        </div>
      </div>
    </section>
  )
}
