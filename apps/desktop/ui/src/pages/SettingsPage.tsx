import { useEffect, useRef, useState, type ReactNode } from 'react'
import type {
  LogLevel,
  LogSettings,
  ThemeMode,
  UserDataState,
} from '@wonderland/core-bindings'
import { Check, FolderOpen, HardDrive, ImagePlus, Monitor, Moon, Palette, Sun, Trash2, type LucideIcon } from 'lucide-react'

import { loggingApi } from '../logging/api'
import { useTheme, type ResolvedTheme } from '../theme/ThemeProvider'
import { userDataApi } from '../user-data/api'
import { setStartupMemoryEnabled, startupMemoryEnabled } from '../startupMemory'
import { t, type MessageKey } from '../i18n'
import { AccountManagementSection } from './AccountSection'
import { CoreUpdateCard } from '../components/CoreUpdateCard'
import { NetworkProxyCard } from '../components/NetworkProxyCard'

/** 主题档位。自定义档的控件保持深色，与后端 `ThemeMode` 的语义一致。 */
const MODES: { value: ThemeMode; labelKey: MessageKey; hintKey: MessageKey; icon: LucideIcon }[] = [
  { value: 'system', labelKey: 'settings.theme.system', hintKey: 'settings.theme.systemHint', icon: Monitor },
  { value: 'light', labelKey: 'settings.theme.light', hintKey: 'settings.theme.lightHint', icon: Sun },
  { value: 'dark', labelKey: 'settings.theme.dark', hintKey: 'settings.theme.darkHint', icon: Moon },
  { value: 'custom', labelKey: 'settings.theme.custom', hintKey: 'settings.theme.customHint', icon: Palette },
]

const IMAGE_ACCEPT = 'image/png,image/jpeg,image/webp,image/gif,image/bmp'
const FALLBACK_COLOR = '#1b1830'
/** Common solid background options. */
const PRESET_COLORS = ['#0b0b12', '#1b1830', '#2a2440', '#3b3358', '#f3f2fa', '#ffffff']

/** 日志级别；值与后端 `LogLevel` 的 snake_case 名字一致。 */
const LOG_LEVELS: { value: LogLevel; labelKey: MessageKey; hintKey: MessageKey }[] = [
  { value: 'error', labelKey: 'settings.logLevel.error', hintKey: 'settings.logLevel.errorHint' },
  { value: 'warn', labelKey: 'settings.logLevel.warn', hintKey: 'settings.logLevel.warnHint' },
  { value: 'info', labelKey: 'settings.logLevel.info', hintKey: 'settings.logLevel.infoHint' },
  { value: 'debug', labelKey: 'settings.logLevel.debug', hintKey: 'settings.logLevel.debugHint' },
  { value: 'trace', labelKey: 'settings.logLevel.trace', hintKey: 'settings.logLevel.traceHint' },
]

/** 设置页：按账号、应用行为、外观、数据与诊断分组。 */
export function SettingsPage() {
  const [userData, setUserData] = useState<UserDataState | null>(null)
  const [userDataFailure, setUserDataFailure] = useState('')
  const [restartFailure, setRestartFailure] = useState('')

  useEffect(() => {
    let live = true
    void userDataApi.get().then((next) => {
      if (live) setUserData(next)
    }).catch((cause) => {
      if (live) setUserDataFailure(errorText(cause))
    })
    return () => { live = false }
  }, [])

  const needsRestart = Boolean(userData?.pending_path)
  const restart = async () => {
    setRestartFailure('')
    try {
      await userDataApi.restart()
    } catch (cause) {
      setRestartFailure(errorText(cause))
    }
  }

  return (
    <section className="w-full">
      <header className="mb-7">
        <h1 className="text-xl font-bold text-ink">{t('settings.title')}</h1>
        <p className="mt-1 text-sm text-ink-faint">{t('settings.description')}</p>
      </header>

      <div className="space-y-7">
        <SettingsGroup title={t('settings.section.account')}>
          <AccountManagementSection />
        </SettingsGroup>

        <SettingsGroup title={t('settings.section.app')} hint={t('settings.section.appHint')}>
          <div className="grid grid-cols-1 items-stretch gap-4 md:grid-cols-2">
            <StartupMemoryCard />
            <CoreUpdateCard />
          </div>
        </SettingsGroup>

        <SettingsGroup title={t('settings.section.network')} hint={t('settings.section.networkHint')}>
          <NetworkProxyCard />
        </SettingsGroup>

        <SettingsGroup title={t('settings.section.appearance')}>
          <AppearanceCard />
        </SettingsGroup>

        <SettingsGroup title={t('settings.section.maintenance')} hint={t('settings.section.maintenanceHint')}>
          <div className="settings-layout">
            <div className="settings-card-grid">
              <UserDataCard state={userData} setState={setUserData} loadFailure={userDataFailure} />
              <LoggingCard />
            </div>
          </div>
          {needsRestart && (
            <div role="status" className="glass-card mt-4 rounded-card border border-brand-400/60 p-4 sm:p-5">
              <div className="flex flex-wrap items-center justify-between gap-4">
                <div className="min-w-0 space-y-1">
                  <p className="font-semibold text-ink">{t('settings.restart.title')}</p>
                  {userData?.pending_path && <p className="break-all text-xs text-ink-muted">{t('settings.restart.data', { path: userData.pending_path })}</p>}
                </div>
                <button type="button" onClick={() => void restart()} className="rounded-lg bg-brand-600 px-4 py-2 text-sm text-white hover:bg-brand-500">
                  {t('settings.restart.now')}
                </button>
              </div>
              {restartFailure && <p role="alert" className="mt-2 text-sm text-[var(--app-danger)]">{restartFailure}</p>}
            </div>
          )}
        </SettingsGroup>
      </div>
    </section>
  )
}

function SettingsGroup({ title, hint, children }: {
  title: string
  hint?: string
  children: ReactNode
}) {
  return (
    <section className="min-w-0">
      <div className="mb-3">
        <h2 className="text-sm font-semibold text-ink">{title}</h2>
        {hint && <p className="mt-1 text-xs leading-relaxed text-ink-faint">{hint}</p>}
      </div>
      {children}
    </section>
  )
}

function StartupMemoryCard() {
  const [enabled, setEnabled] = useState(startupMemoryEnabled)

  const changeEnabled = (next: boolean) => {
    setEnabled(next)
    setStartupMemoryEnabled(next)
  }

  return (
    <section className="glass-card flex h-full items-center rounded-card p-5">
      <label htmlFor="resume-last-work-page" className="flex w-full cursor-pointer items-center justify-between gap-4">
        <span className="min-w-0">
          <span className="block text-sm font-semibold text-ink">{t('settings.startupMemory.title')}</span>
          <span className="mt-1 block text-xs leading-relaxed text-ink-muted">{t('settings.startupMemory.description')}</span>
        </span>
        <input
          id="resume-last-work-page"
          type="checkbox"
          checked={enabled}
          onChange={(event) => changeEnabled(event.target.checked)}
          className="h-4 w-4 shrink-0 accent-[var(--app-accent)]"
        />
      </label>
    </section>
  )
}

/** 用户数据：内核统一管理目录，并在重启后的服务装载前完成整目录迁移。 */
function UserDataCard({ state, setState, loadFailure }: {
  state: UserDataState | null
  setState: (state: UserDataState) => void
  loadFailure: string
}) {
  const [customPath, setCustomPath] = useState('')
  const [busy, setBusy] = useState(false)
  const [choosing, setChoosing] = useState(false)
  const [failure, setFailure] = useState('')

  useEffect(() => {
    if (state?.location === 'custom') setCustomPath(state.active_path)
  }, [state?.active_path, state?.location])

  const run = async (action: () => Promise<UserDataState>) => {
    setBusy(true)
    setFailure('')
    try {
      setState(await action())
    } catch (cause) {
      setFailure(errorText(cause))
    } finally {
      setBusy(false)
    }
  }

  const chooseDirectory = async () => {
    setChoosing(true)
    setFailure('')
    try {
      const path = await userDataApi.chooseDirectory()
      if (path) setCustomPath(path)
    } catch (cause) {
      setFailure(errorText(cause))
    } finally {
      setChoosing(false)
    }
  }

  return (
    <section className="glass-card rounded-card p-6">
      <SectionHead title={t('settings.userData.title')} hint={t('settings.userData.hint')} />

      {!state ? (
        <p className="mt-4 text-sm text-ink-faint">{loadFailure || t('settings.userData.loading')}</p>
      ) : (
        <div className="mt-4 space-y-4">
          <div className="flex items-start gap-3 rounded-lg border border-glass-line bg-glass-subtle p-3">
            <HardDrive className="mt-0.5 h-4 w-4 shrink-0 text-ink-muted" aria-hidden />
            <div className="min-w-0">
              <p className="text-xs text-ink-faint">{t('settings.userData.current')}</p>
              <p className="mt-1 break-all font-mono text-xs text-ink">{state.active_path}</p>
              <p className="mt-1 text-[11px] text-ink-faint">
                {state.location === 'default'
                  ? t('settings.userData.defaultBadge')
                  : t('settings.userData.customBadge')}
              </p>
            </div>
          </div>

          {state.pending_path ? (
            <div className="rounded-lg border border-brand-400/60 bg-glass px-3 py-2.5">
              <p className="text-xs text-[var(--app-accent-ink)]">
                {t('settings.userData.pending')}
              </p>
              <p className="mt-1 break-all font-mono text-xs text-ink">{state.pending_path}</p>
              <div className="mt-3 flex flex-wrap gap-2">
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void run(userDataApi.cancelMigration)}
                  className="rounded-lg border border-glass-line px-3 py-1.5 text-xs text-ink-muted transition hover:bg-glass disabled:opacity-60"
                >
                  {t('settings.userData.cancelPending')}
                </button>
              </div>
            </div>
          ) : (
            <div className="space-y-3">
              <div className="text-xs text-ink-muted">
                <label htmlFor="user-data-custom-path">{t('settings.userData.customPath')}</label>
                <div className="mt-1.5 flex gap-2">
                  <input
                    id="user-data-custom-path"
                    type="text"
                    value={customPath}
                    disabled={busy || choosing}
                    placeholder={t('settings.userData.customPlaceholder')}
                    onChange={(event) => setCustomPath(event.target.value)}
                    className="min-w-0 flex-1 rounded-lg border border-glass-line bg-glass px-3 py-2 font-mono text-xs text-ink outline-none transition placeholder:text-ink-faint focus:border-brand-400 disabled:opacity-60"
                  />
                  <button
                    type="button"
                    disabled={busy || choosing}
                    onClick={() => void chooseDirectory()}
                    className="flex shrink-0 items-center gap-1 rounded-lg border border-glass-line px-3 py-2 text-xs text-ink-muted hover:bg-glass disabled:opacity-60"
                  >
                    <FolderOpen className="h-4 w-4" aria-hidden />
                    {t('settings.userData.browse')}
                  </button>
                </div>
              </div>
              <div className="flex flex-wrap gap-2">
                <button
                  type="button"
                  disabled={busy || customPath.trim() === '' || customPath.trim() === state.active_path}
                  onClick={() => void run(() => userDataApi.migrateCustom(customPath.trim()))}
                  className="rounded-lg bg-brand-600 px-3 py-1.5 text-xs text-white transition hover:bg-brand-500 disabled:cursor-default disabled:opacity-50"
                >
                  {t('settings.userData.useCustom')}
                </button>
                <button
                  type="button"
                  disabled={busy || state.location === 'default'}
                  onClick={() => void run(userDataApi.migrateDefault)}
                  className="rounded-lg border border-glass-line px-3 py-1.5 text-xs text-ink-muted transition hover:bg-glass hover:text-ink disabled:cursor-default disabled:opacity-50"
                >
                  {t('settings.userData.useDefault')}
                </button>
              </div>
              <p className="text-[11px] leading-relaxed text-ink-faint">
                {t('settings.userData.requirements')}
              </p>
              <p className="text-[11px] leading-relaxed text-ink-faint">
                {t('settings.userData.legacyExports')}
              </p>
            </div>
          )}

          {state.last_migration_error && (
            <p role="alert" className="text-sm text-[var(--app-danger)]">
              {t('settings.userData.lastFailure', { error: state.last_migration_error })}
            </p>
          )}
          {state.last_migration_path && !state.pending_path && !state.last_migration_error && (
            <p role="status" className="text-xs text-ink-muted">{t('settings.userData.lastSuccess', { path: state.last_migration_path })}</p>
          )}
          {state.last_migration_warning && (
            <p role="status" className="rounded-lg border border-brand-400/40 bg-glass px-3 py-2 text-xs text-ink-muted">
              {t('settings.userData.cleanupWarning', { path: state.last_migration_source ?? state.active_path })}
              <span className="mt-1 block">{state.last_migration_warning}</span>
            </p>
          )}
        </div>
      )}

      {failure && (
        <p role="alert" className="mt-4 text-sm text-[var(--app-danger)]">
          {failure}
        </p>
      )}
    </section>
  )
}

/** 外观：主题档位 + 自定义背景。 */
function AppearanceCard() {
  const { settings, backgroundUrl, thumbnails, ensureThumbnail, setTheme } = useTheme()
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState('')

  const savedImageId =
    settings.background?.kind === 'image' ? settings.background.id : null
  const customThumb = savedImageId
    ? (thumbnails.get(savedImageId) ?? backgroundUrl)
    : null

  useEffect(() => {
    if (savedImageId) ensureThumbnail(savedImageId)
  }, [savedImageId, ensureThumbnail])

  const run = async (action: () => Promise<void>) => {
    setBusy(true)
    setFailure('')
    try {
      await action()
    } catch (cause) {
      setFailure(errorText(cause))
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="glass-card rounded-card p-6">
      <SectionHead title={t('settings.appearance.title')} hint={t('settings.appearance.hint')} />

      <div
        role="radiogroup"
        aria-label={t('settings.theme.aria')}
        aria-busy={busy}
        className="mt-4 grid grid-cols-2 gap-3 xl:grid-cols-4"
      >
        {MODES.map((mode) => (
          <ThemeTile
            key={mode.value}
            icon={mode.icon}
            label={t(mode.labelKey)}
            hint={t(mode.hintKey)}
            active={settings.mode === mode.value}
            disabled={busy}
            onSelect={() => void run(() => setTheme({ ...settings, mode: mode.value }))}
          >
            <ThemePreview
              variant={mode.value}
              image={mode.value === 'custom' ? customThumb : null}
            />
          </ThemeTile>
        ))}
      </div>

      {settings.mode === 'custom' && (
        <div className="mt-5">
          <BackgroundPicker />
        </div>
      )}

      {failure && (
        <p role="alert" className="mt-4 text-sm text-[var(--app-danger)]">
          {failure}
        </p>
      )}
    </section>
  )
}

/** 档位卡片：上方是该档位的缩略示意，下方是名称与说明。 */
function ThemeTile({
  icon: Icon,
  label,
  hint,
  active,
  disabled,
  onSelect,
  children,
}: {
  icon: LucideIcon
  label: string
  hint: string
  active: boolean
  disabled: boolean
  onSelect: () => void
  children: ReactNode
}) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={active}
      disabled={disabled}
      onClick={onSelect}
      className={`overflow-hidden rounded-xl border text-left transition disabled:cursor-default disabled:opacity-60 ${
        active
          ? 'border-brand-400 bg-glass ring-1 ring-brand-400'
          : 'border-glass-line hover:bg-glass-subtle'
      }`}
    >
      <span className="block h-20 w-full border-b border-glass-line">{children}</span>
      <span className="flex items-center gap-2.5 px-3 py-2.5">
        <Icon className="h-4 w-4 shrink-0 text-ink-muted" aria-hidden />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm text-ink">{label}</span>
          <span className="mt-0.5 block truncate text-[11px] text-ink-faint">{hint}</span>
        </span>
        {active ? <Check className="h-4 w-4 shrink-0 text-brand-400" aria-hidden /> : null}
      </span>
    </button>
  )
}

/** Render a theme preview using fixed colors. */
function ThemePreview({ variant, image }: { variant: ThemeMode; image: string | null }) {
  if (variant === 'custom') {
    return image ? (
      <span className="block h-full w-full">
        <img src={image} alt="" className="h-full w-full object-cover" />
      </span>
    ) : (
      <span className="flex h-full w-full items-center justify-center bg-glass-subtle">
        <ImagePlus className="h-5 w-5 text-ink-faint" aria-hidden />
      </span>
    )
  }

  const background: Record<ResolvedTheme | 'system', string> = {
    light: 'linear-gradient(135deg, #ffffff, #e9e7f6)',
    dark: 'linear-gradient(135deg, #262145, #0b0b12)',
    system: 'linear-gradient(115deg, #ffffff 0 50%, #1b1830 50% 100%)',
  }
  const panel = 'rgb(128 128 145 / 0.22)'
  const line = 'rgb(128 128 145 / 0.45)'

  return (
    <span className="relative block h-full w-full" style={{ backgroundImage: background[variant] }}>
      <span
        className="absolute inset-x-2.5 top-2.5 bottom-2.5 rounded-md"
        style={{ backgroundColor: panel }}
      />
      <span
        className="absolute top-4 left-4 h-1.5 w-10 rounded-full"
        style={{ backgroundColor: line }}
      />
      <span
        className="absolute top-6.5 left-4 h-1.5 w-16 rounded-full"
        style={{ backgroundColor: panel }}
      />
    </span>
  )
}

/** 自定义背景：图片库与纯色两条路径。 */
function BackgroundPicker() {
  const {
    settings,
    setTheme,
    backgrounds,
    thumbnails,
    ensureThumbnail,
    addBackground,
    selectBackground,
    removeBackground,
  } = useTheme()
  const [tab, setTab] = useState<'image' | 'color'>(
    settings.background?.kind === 'color' ? 'color' : 'image',
  )
  const [busy, setBusy] = useState('')
  const [dragOver, setDragOver] = useState(false)
  const [pendingDelete, setPendingDelete] = useState('')
  const [failure, setFailure] = useState('')
  const [opacity, setOpacity] = useState(settings.background_opacity)
  const opacityRef = useRef(settings.background_opacity)
  const savingOpacity = useRef(false)

  useEffect(() => {
    opacityRef.current = settings.background_opacity
    setOpacity(settings.background_opacity)
  }, [settings.background_opacity])

  useEffect(() => {
    for (const asset of backgrounds) ensureThumbnail(asset.id)
  }, [backgrounds, ensureThumbnail])

  const selectedId = settings.background?.kind === 'image' ? settings.background.id : null
  const color = settings.background?.kind === 'color' ? settings.background.hex : FALLBACK_COLOR

  const run = async (key: string, action: () => Promise<void>) => {
    setBusy(key)
    setFailure('')
    try {
      await action()
    } catch (cause) {
      setFailure(errorText(cause))
    } finally {
      setBusy('')
    }
  }

  const importFile = (file: File) => void run('add', () => addBackground(file))
  const previewOpacity = (next: number) => {
    opacityRef.current = next
    setOpacity(next)
    document.documentElement.style.setProperty('--app-background-opacity', String(next / 100))
  }
  const saveOpacity = async () => {
    const next = opacityRef.current
    if (savingOpacity.current || busy !== '' || next === settings.background_opacity) return
    savingOpacity.current = true
    setBusy('opacity')
    setFailure('')
    try {
      await setTheme({ ...settings, mode: 'custom', background_opacity: next })
    } catch (cause) {
      previewOpacity(settings.background_opacity)
      setFailure(errorText(cause))
    } finally {
      savingOpacity.current = false
      setBusy('')
    }
  }

  return (
    <div className="rounded-card border border-glass-line bg-glass-subtle p-4">
      <div className="flex flex-wrap items-center gap-2">
        <TabButton
          label={t('settings.background.imageTab')}
          active={tab === 'image'}
          onClick={() => setTab('image')}
        />
        <TabButton
          label={t('settings.background.colorTab')}
          active={tab === 'color'}
          onClick={() => setTab('color')}
        />
        <span className="ml-auto text-[11px] text-ink-faint">
          {t('settings.background.hint')}
        </span>
      </div>

      {tab === 'image' ? (
        <div className="mt-4 space-y-4">
          <label
            onDragOver={(event) => {
              event.preventDefault()
              setDragOver(true)
            }}
            onDragLeave={() => setDragOver(false)}
            onDrop={(event) => {
              event.preventDefault()
              setDragOver(false)
              const file = event.dataTransfer.files?.[0]
              if (file) importFile(file)
            }}
            className={`flex cursor-pointer items-center justify-center gap-2 rounded-lg border border-dashed px-4 py-6 text-sm transition ${
              dragOver ? 'border-brand-400 bg-glass' : 'border-glass-line hover:bg-glass'
            } ${busy === 'add' ? 'cursor-default opacity-60' : ''}`}
          >
            <input
              type="file"
              accept={IMAGE_ACCEPT}
              className="hidden"
              disabled={busy === 'add'}
              onChange={(event) => {
                const file = event.target.files?.[0]
                event.target.value = ''
                if (file) importFile(file)
              }}
            />
            <ImagePlus className="h-4 w-4 shrink-0 text-ink-muted" aria-hidden />
            <span className="text-ink-muted">
              {busy === 'add'
                ? t('settings.background.importing')
                : dragOver
                  ? t('settings.background.dropToImport')
                  : t('settings.background.chooseOrDrop')}
            </span>
          </label>
          <p className="text-[11px] text-ink-faint">
            {t('settings.background.limits')}
          </p>

          {backgrounds.length > 0 && (
            <ul className="grid grid-cols-2 gap-3 sm:grid-cols-3 xl:grid-cols-4">
              {backgrounds.map((asset) => {
                const active = asset.id === selectedId
                const url = thumbnails.get(asset.id)
                return (
                  <li
                    key={asset.id}
                    className={`group overflow-hidden rounded-lg border transition ${
                      active ? 'border-brand-400' : 'border-glass-line hover:border-glass-line-strong'
                    }`}
                  >
                    <button
                      type="button"
                      disabled={busy !== ''}
                      onClick={() => void run(asset.id, () => selectBackground(asset.id))}
                      title={asset.name}
                      className="relative block w-full disabled:cursor-default"
                    >
                      <span className="block aspect-video w-full bg-glass">
                        {url ? (
                          <img src={url} alt="" className="h-full w-full object-cover" />
                        ) : (
                          <span className="block h-full w-full animate-pulse bg-glass-hover" />
                        )}
                      </span>
                      {active ? (
                        <span className="absolute top-2 left-2 inline-flex items-center gap-1 rounded-full bg-brand-600/85 px-2 py-0.5 text-[10px] text-white">
                          <Check className="h-3 w-3" aria-hidden />
                          {t('common.current')}
                        </span>
                      ) : null}
                    </button>
                    <div className="flex items-center gap-2 px-2.5 py-2">
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-xs text-ink" title={asset.name}>
                          {asset.name}
                        </span>
                        <span className="block text-[10px] text-ink-faint">
                          {formatBytes(asset.size)}
                        </span>
                      </span>
                      {pendingDelete === asset.id ? (
                        <span className="flex shrink-0 items-center gap-1">
                          <button
                            type="button"
                            disabled={busy !== ''}
                            className="rounded border border-[var(--app-danger-line)] bg-[var(--app-danger-soft)] px-1.5 py-0.5 text-[10px] text-[var(--app-danger)] disabled:opacity-60"
                            onClick={() =>
                              void run(asset.id, async () => {
                                await removeBackground(asset.id)
                                setPendingDelete('')
                              })
                            }
                          >
                            {t('common.delete')}
                          </button>
                          <button
                            type="button"
                            className="rounded px-1.5 py-0.5 text-[10px] text-ink-muted transition hover:text-ink"
                            onClick={() => setPendingDelete('')}
                          >
                            {t('common.cancel')}
                          </button>
                        </span>
                      ) : (
                        <button
                          type="button"
                          aria-label={t('settings.background.removeAria', { name: asset.name })}
                          className="shrink-0 rounded p-1 text-ink-faint opacity-0 transition group-hover:opacity-100 hover:text-[var(--app-danger)] focus-visible:opacity-100"
                          onClick={() => setPendingDelete(asset.id)}
                        >
                          <Trash2 className="h-3.5 w-3.5" aria-hidden />
                        </button>
                      )}
                    </div>
                  </li>
                )
              })}
            </ul>
          )}
        </div>
      ) : (
        <div className="mt-4 flex flex-wrap items-center gap-3">
          <input
            type="color"
            value={color}
            aria-label={t('settings.background.colorAria')}
            className="h-9 w-14 cursor-pointer rounded-lg border border-glass-line bg-transparent"
            onChange={(event) =>
              void run('color', () =>
                setTheme({
                  mode: 'custom',
                  background: { kind: 'color', hex: event.target.value },
                  background_opacity: settings.background_opacity,
                }),
              )
            }
          />
          <span className="font-mono text-xs text-ink-muted">{color}</span>
          <span className="flex items-center gap-1.5">
            {PRESET_COLORS.map((preset) => (
              <button
                key={preset}
                type="button"
                aria-label={t('settings.background.usePreset', { color: preset })}
                title={preset}
                className={`h-5 w-5 rounded-full border transition hover:scale-110 ${
                  preset.toLowerCase() === color.toLowerCase()
                    ? 'border-brand-400 ring-1 ring-brand-400'
                    : 'border-glass-line-strong'
                }`}
                style={{ backgroundColor: preset }}
                onClick={() =>
                  void run('color', () =>
                    setTheme({
                      mode: 'custom',
                      background: { kind: 'color', hex: preset },
                      background_opacity: settings.background_opacity,
                    }),
                  )
                }
              />
            ))}
          </span>
        </div>
      )}

      <div className="mt-4 rounded-lg border border-glass-line bg-glass-subtle px-3 py-3">
        <div className="flex items-center justify-between gap-3">
          <label htmlFor="custom-background-opacity" className="text-xs font-medium text-ink-muted">
            {t('settings.background.opacity')}
          </label>
          <output htmlFor="custom-background-opacity" className="min-w-10 text-right font-mono text-xs text-ink-faint">
            {opacity}%
          </output>
        </div>
        <input
          id="custom-background-opacity"
          type="range"
          min={0}
          max={100}
          step={1}
          value={opacity}
          disabled={busy !== ''}
          aria-label={t('settings.background.opacity')}
          className="mt-3 w-full accent-brand-600 disabled:opacity-50"
          onChange={(event) => previewOpacity(Number(event.target.value))}
          onPointerUp={() => void saveOpacity()}
          onKeyUp={() => void saveOpacity()}
          onBlur={() => void saveOpacity()}
        />
        <p className="mt-1.5 text-[11px] text-ink-faint">{t('settings.background.opacityHint')}</p>
      </div>

      {failure && (
        <p role="alert" className="mt-4 text-sm text-[var(--app-danger)]">
          {failure}
        </p>
      )}
    </div>
  )
}

function TabButton({
  label,
  active,
  onClick,
}: {
  label: string
  active: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={`rounded-lg border px-3 py-1.5 text-xs transition ${
        active
          ? 'border-brand-400 bg-glass text-ink'
          : 'border-glass-line text-ink-muted hover:bg-glass hover:text-ink'
      }`}
    >
      {label}
    </button>
  )
}

/** 日志：级别下拉 + 打开日志目录。 */
function LoggingCard() {
  const [settings, setSettings] = useState<LogSettings | null>(null)
  const [directory, setDirectory] = useState('')
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState('')

  useEffect(() => {
    let live = true
    void loggingApi
      .get()
      .then((next) => {
        if (live) setSettings(next)
      })
      .catch((cause) => {
        if (live) setFailure(errorText(cause))
      })
    void loggingApi.dir().then((path) => {
      if (live) setDirectory(path)
    }).catch((cause) => {
      if (live) setFailure(errorText(cause))
    })
    return () => {
      live = false
    }
  }, [])

  const changeLevel = async (level: LogLevel) => {
    setBusy(true)
    setFailure('')
    try {
      setSettings(await loggingApi.setLevel(level))
    } catch (cause) {
      setFailure(errorText(cause))
    } finally {
      setBusy(false)
    }
  }

  const reveal = async () => {
    setFailure('')
    try {
      await loggingApi.revealDir()
    } catch (cause) {
      setFailure(errorText(cause))
    }
  }

  const level = LOG_LEVELS.find((item) => item.value === settings?.level)
  const hint = level ? t(level.hintKey) : ''

  return (
    <section className="glass-card rounded-card p-6">
      <SectionHead
        title={t('settings.logging.title')}
        hint={t('settings.logging.hint')}
      />

      {!settings ? (
        <p className="mt-4 text-sm text-ink-faint">{t('settings.logging.loading')}</p>
      ) : (
        <div className="mt-4 space-y-4">
          <div className="flex flex-wrap items-center gap-3">
            <label className="flex items-center gap-2 text-sm text-ink-muted">
              {t('settings.logging.level')}
              <select
                value={settings.level}
                disabled={busy}
                onChange={(event) => void changeLevel(event.target.value as LogLevel)}
                className="rounded-lg border border-[var(--app-field-border)] bg-[var(--app-field)] px-2.5 py-1.5 text-sm text-ink transition disabled:cursor-default disabled:opacity-60"
              >
                {LOG_LEVELS.map((item) => (
                  <option key={item.value} value={item.value}>
                    {t(item.labelKey)}
                  </option>
                ))}
              </select>
            </label>
            <span className="text-xs text-ink-faint">{hint}</span>
            <button
              type="button"
              onClick={() => void reveal()}
              className="ml-auto rounded-lg border border-glass-line px-3 py-1.5 text-xs text-ink-muted transition hover:bg-glass hover:text-ink"
            >
              {t('settings.logging.reveal')}
            </button>
          </div>
          {directory && (
            <div className="rounded-lg border border-glass-line bg-glass-subtle p-3">
              <p className="text-xs text-ink-faint">{t('settings.logging.directory')}</p>
              <p className="mt-1 break-all font-mono text-xs text-ink">{directory}</p>
            </div>
          )}
          <p className="text-xs leading-relaxed text-ink-faint">{t('settings.logging.retention')}</p>
        </div>
      )}

      {failure && (
        <p role="alert" className="mt-4 text-sm text-[var(--app-danger)]">
          {failure}
        </p>
      )}
    </section>
  )
}

function SectionHead({ title, hint }: { title: string; hint: string }) {
  return (
    <div className="max-w-2xl">
      <h2 className="text-base font-semibold text-ink">{title}</h2>
      <p className="mt-1 text-xs leading-relaxed text-ink-faint">{hint}</p>
    </div>
  )
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

/** IPC 失败可能是后端的 `ErrorPayload`（对象）或前端抛出的 `Error`。 */
function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message: unknown }).message)
  }
  return cause instanceof Error ? cause.message : String(cause)
}
