import { ArrowRight, Clock3, LayoutGrid, Puzzle, Settings2 } from 'lucide-react'
import { Link, useNavigate } from 'react-router-dom'

import brandAvatar from '../assets/brand-avatar.png'
import { t } from '../i18n'
import { buildContributionRegistry, openContribution } from '../plugins/contributions'
import { pluginIcon } from '../plugins/icons'
import { usePlugins } from '../plugins/api'
import { useWorkspaceLayout } from '../plugins/workspaceLayout'
import './HomePage.css'

/** Home is the branded entry point for the Wonderland plugin workspace. */
export function HomePage() {
  const navigate = useNavigate()
  const { states } = usePlugins()
  const { layout } = useWorkspaceLayout()
  const registry = buildContributionRegistry(states)
  const recent = [...layout.openContributionIds]
    .reverse()
    .map((id) => registry.find((item) => item.id === id && item.kind === 'activity'))
    .filter((item): item is NonNullable<typeof item> => item !== undefined)
    .slice(0, 3)

  return (
    <section className="flex h-full min-h-0 w-full">
      <aside className="relative flex w-[32%] min-w-[19rem] max-w-[34rem] shrink-0 flex-col overflow-hidden border-r border-dashed border-glass-line px-8 py-10 md:px-10 lg:px-14">
        <div className="pointer-events-none absolute -left-32 top-1/4 h-80 w-80 rounded-full bg-brand-500/10 blur-[100px]" />
        <div className="relative flex flex-1 flex-col justify-center">
          <div className="mb-8 grid size-24 shrink-0 place-items-center overflow-hidden rounded-[1.6rem] border border-glass-line bg-glass shadow-xl shadow-brand-950/20 sm:size-28 xl:mb-10 xl:size-32 2xl:size-40 2xl:rounded-[2.4rem]">
            <img src={brandAvatar} alt="" className="h-full w-full object-cover" />
          </div>
          <p className="mb-4 text-[11px] font-semibold uppercase tracking-[0.25em] text-brand-400">
            Wonderland Studio
          </p>
          <h1 className="brand-text home-wordmark max-w-[18rem] text-4xl leading-[0.98] text-ink md:text-5xl 2xl:max-w-[28rem] 2xl:text-6xl">
            <span className="block">Wonderland</span>
            <span className="mt-1 block">Assistant</span>
          </h1>
          <div aria-hidden className="mt-7 flex w-36 items-center gap-2">
            <span className="h-px flex-1 border-t border-dashed border-brand-500/40" />
            <span className="h-1.5 w-1.5 rounded-full bg-brand-400/70" />
          </div>
        </div>
        <p className="relative mt-8 text-[11px] tracking-wide text-ink-faint">
          © Wonderland Assistant · Apache-2.0 · Yuef Chen
        </p>
      </aside>

      <div className="flex min-h-0 min-w-0 flex-1 overflow-y-auto px-7 py-8 md:px-10 xl:px-14 xl:py-12 2xl:px-20">
        <div className="mx-auto my-auto w-full max-w-3xl space-y-8 xl:max-w-5xl 2xl:max-w-6xl">
          <section aria-labelledby="home-recent-heading" className="border-t border-dashed border-glass-line pt-7">
            <div className="mb-3 flex items-center justify-between gap-3">
              <h2 id="home-recent-heading" className="flex items-center gap-2 text-xs font-bold uppercase tracking-[0.16em] text-ink-faint">
                <Clock3 className="h-4 w-4" aria-hidden />
                {t('home.recentTitle')}
              </h2>
              {recent.length > 0 && <span className="text-[11px] text-ink-faint">{recent.length}</span>}
            </div>
            {recent.length === 0 ? (
              <div className="glass-card flex flex-wrap items-center justify-between gap-4 rounded-2xl border border-glass-line px-5 py-5">
                <p className="max-w-md text-sm leading-6 text-ink-muted">{t('home.recentEmpty')}</p>
                <Link to="/workspace" className="inline-flex shrink-0 items-center gap-1 text-xs font-semibold text-brand-400 transition hover:text-brand-300">
                  {t('home.enterWorkspace')}<ArrowRight className="h-3.5 w-3.5" aria-hidden />
                </Link>
              </div>
            ) : (
              <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                {recent.map((item) => {
                  const Icon = pluginIcon(item.icon)
                  return (
                    <button key={item.id} type="button" onClick={() => openContribution(registry, item.id, navigate)} className="glass-card flex min-w-0 items-center gap-3 rounded-2xl border border-glass-line p-4 text-left transition hover:-translate-y-0.5 hover:border-brand-500/30 hover:bg-glass-hover">
                      <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-brand-600/10 text-brand-400">
                        <Icon className="h-5 w-5" aria-hidden />
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-sm font-semibold text-ink">{item.title}</span>
                        <span className="mt-1 block truncate text-[11px] text-ink-faint">{item.state.manifest.name}</span>
                      </span>
                      <ArrowRight className="h-4 w-4 shrink-0 text-ink-faint" aria-hidden />
                    </button>
                  )
                })}
              </div>
            )}
          </section>

          <section aria-label={t('home.shortcutsTitle')} className="grid gap-3 border-t border-dashed border-glass-line pt-7 sm:grid-cols-2 lg:grid-cols-3">
            <Link to="/workspace" className="glass-card group flex min-h-32 items-start gap-4 rounded-2xl border border-glass-line p-5 transition hover:-translate-y-0.5 hover:border-brand-500/30 hover:bg-glass-hover">
              <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-brand-600/10 text-brand-400">
                <LayoutGrid className="h-5 w-5" aria-hidden />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-semibold text-ink">{t('home.workspaceTitle')}</span>
                <span className="mt-1.5 block text-xs leading-5 text-ink-muted">{t('home.workspaceDescription')}</span>
              </span>
              <ArrowRight className="mt-1 h-4 w-4 shrink-0 text-ink-faint transition group-hover:translate-x-0.5 group-hover:text-brand-400" aria-hidden />
            </Link>
            <Link to="/workspace?view=plugins" className="glass-card group flex min-h-32 items-start gap-4 rounded-2xl border border-glass-line p-5 transition hover:-translate-y-0.5 hover:border-brand-500/30 hover:bg-glass-hover">
              <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-brand-600/10 text-brand-400">
                <Puzzle className="h-5 w-5" aria-hidden />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-semibold text-ink">{t('home.pluginManagementTitle')}</span>
                <span className="mt-1.5 block text-xs leading-5 text-ink-muted">{t('home.pluginManagementDescription')}</span>
              </span>
              <ArrowRight className="mt-1 h-4 w-4 shrink-0 text-ink-faint transition group-hover:translate-x-0.5 group-hover:text-brand-400" aria-hidden />
            </Link>
            <Link to="/workspace/settings" className="glass-card group flex min-h-32 items-start gap-4 rounded-2xl border border-glass-line p-5 transition hover:-translate-y-0.5 hover:border-brand-500/30 hover:bg-glass-hover">
              <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-brand-600/10 text-brand-400">
                <Settings2 className="h-5 w-5" aria-hidden />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-semibold text-ink">{t('home.settingsTitle')}</span>
                <span className="mt-1.5 block text-xs leading-5 text-ink-muted">{t('home.settingsDescription')}</span>
              </span>
              <ArrowRight className="mt-1 h-4 w-4 shrink-0 text-ink-faint transition group-hover:translate-x-0.5 group-hover:text-brand-400" aria-hidden />
            </Link>
          </section>
        </div>
      </div>
    </section>
  )
}
