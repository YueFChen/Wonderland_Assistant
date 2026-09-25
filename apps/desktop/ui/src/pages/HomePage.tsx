import { useEffect, useState } from 'react'
import { ArrowRight, Clock3, Gamepad2, LayoutGrid, Play, Puzzle, Settings2 } from 'lucide-react'
import { Link, useNavigate } from 'react-router-dom'

import brandAvatar from '../assets/brand-avatar.png'
import { t } from '../i18n'
import { buildContributionRegistry, openContribution } from '../plugins/contributions'
import { pluginIcon } from '../plugins/icons'
import { usePlugins } from '../plugins/api'
import { useWorkspaceLayout } from '../plugins/workspaceLayout'
import { gameLauncherApi, type GameLauncherSnapshot } from '../home/gameLauncher'
import './HomePage.css'

/** Home is the branded entry point for the game and UGC development workspace. */
export function HomePage() {
  const navigate = useNavigate()
  const { states } = usePlugins()
  const { layout } = useWorkspaceLayout()
  const [launcher, setLauncher] = useState<GameLauncherSnapshot | null>(null)
  const [gameBusy, setGameBusy] = useState(false)
  const [launchError, setLaunchError] = useState(false)

  useEffect(() => {
    let live = true
    void gameLauncherApi.snapshot()
      .then((snapshot) => { if (live) setLauncher(snapshot) })
      .catch(() => { if (live) setLauncher({ path: null, available: false, supported: true, kind: null, source: null }) })
    return () => { live = false }
  }, [])

  const registry = buildContributionRegistry(states)
  const recent = [...layout.openContributionIds]
    .reverse()
    .map((id) => registry.find((item) => item.id === id && item.kind === 'activity'))
    .filter((item): item is NonNullable<typeof item> => item !== undefined)
    .slice(0, 3)

  const canLaunch = launcher?.supported === true && launcher.available
  const gameButtonText = launcher === null
    ? t('home.launcherLoading')
    : canLaunch ? t('home.launchGame') : t('home.chooseLauncher')

  const handleGameAction = async (action: 'launch' | 'select' = canLaunch ? 'launch' : 'select') => {
    if (!launcher?.supported || gameBusy) return
    setGameBusy(true)
    setLaunchError(false)
    try {
      if (action === 'launch' && canLaunch) {
        await gameLauncherApi.launch()
      } else {
        setLauncher(await gameLauncherApi.select())
      }
    } catch {
      setLaunchError(true)
    } finally {
      setGameBusy(false)
    }
  }

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
          <section aria-labelledby="home-game-heading" className="glass-card relative overflow-hidden rounded-3xl border border-brand-500/20 bg-brand-600/[0.06] p-6 md:p-7">
            <div className="pointer-events-none absolute -right-12 -top-20 h-56 w-56 rounded-full bg-brand-500/10 blur-3xl" />
            <div className="relative flex flex-wrap items-start justify-between gap-5">
              <div className="flex min-w-0 items-start gap-4">
                <span className="grid h-11 w-11 shrink-0 place-items-center rounded-2xl bg-brand-600/15 text-brand-400">
                  <Gamepad2 className="h-5 w-5" strokeWidth={1.7} aria-hidden />
                </span>
                <div className="min-w-0 pt-0.5">
                  <p className="text-[10px] font-bold uppercase tracking-[0.19em] text-brand-400">{t('home.gameEyebrow')}</p>
                  <h2 id="home-game-heading" className="mt-1 text-xl font-semibold text-ink">{t('home.gameTitle')}</h2>
                </div>
              </div>
              {launcher?.supported && launcher.available && launcher.path && (
                <span className="max-w-52 truncate rounded-full border border-glass-line bg-glass/70 px-3 py-1.5 text-[11px] text-ink-faint" title={launcher.path}>
                  {launcher.path.split(/[\\/]/).pop()}
                </span>
              )}
            </div>
            <div className="relative mt-6 flex flex-wrap items-center gap-3 border-t border-dashed border-glass-line pt-5">
              <button
                type="button"
                onClick={() => void handleGameAction()}
                disabled={gameBusy || launcher === null || !launcher.supported}
                className="inline-flex min-w-40 items-center justify-center gap-2 rounded-xl bg-brand-600 px-4 py-3 text-sm font-semibold text-white shadow-lg shadow-brand-950/15 transition hover:-translate-y-0.5 hover:bg-brand-500 focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-brand-400 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:translate-y-0"
              >
                {canLaunch ? <Play className="h-4 w-4 fill-current" aria-hidden /> : <Gamepad2 className="h-4 w-4" aria-hidden />}
                {gameBusy ? t('home.gameWorking') : gameButtonText}
                {!gameBusy && canLaunch && <ArrowRight className="h-4 w-4" aria-hidden />}
              </button>
              {launcher?.supported && launcher.available && (
                <button type="button" onClick={() => void handleGameAction('select')} disabled={gameBusy} className="rounded-lg px-3 py-2 text-xs font-medium text-ink-muted transition hover:bg-glass-hover hover:text-ink disabled:opacity-50">
                  {t('home.changeLauncher')}
                </button>
              )}
              <span className="text-xs text-ink-faint">
                {launcher === null
                  ? t('home.launcherLoading')
                  : !launcher.supported
                    ? t('home.launcherUnsupported')
                    : launcher.available
                      ? launcher.source === 'registry'
                        ? t('home.launcherDetected')
                        : launcher.kind === 'game'
                          ? t('home.gameReady')
                          : t('home.launcherReady')
                      : t('home.launcherUnavailable')}
              </span>
            </div>
            {launchError && <p role="alert" className="relative mt-4 text-xs text-[var(--app-danger)]">{t('home.launchFailed')}</p>}
          </section>

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
