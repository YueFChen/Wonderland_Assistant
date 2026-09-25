import { useState } from 'react'
import { ArrowLeft, FolderOpen, PackagePlus } from 'lucide-react'
import { Link } from 'react-router-dom'

import { OnlinePluginCatalog } from '../components/OnlinePluginCatalog'
import { t } from '../i18n'
import { pluginApi, usePlugins } from '../plugins/api'

/** Dedicated install surface for reviewed online releases and local plugin packages. */
export function PluginInstallPage() {
  const { states, setStates } = usePlugins()
  const [installing, setInstalling] = useState(false)
  const [failure, setFailure] = useState('')
  const [success, setSuccess] = useState('')

  const installLocal = async () => {
    setInstalling(true)
    setFailure('')
    setSuccess('')
    try {
      const previous = states ?? await pluginApi.states()
      const previousVersions = new Map(previous.map((state) => [state.manifest.id, state.manifest.version]))
      const next = await pluginApi.install()
      setStates(next)
      const changed = next.some((state) => previousVersions.get(state.manifest.id) !== state.manifest.version)
      if (changed) setSuccess(t('settings.plugins.installPage.localSuccess'))
    } catch (cause) {
      setFailure(t('settings.plugins.installFailed', { error: errorText(cause) }))
    } finally {
      setInstalling(false)
    }
  }

  return (
    <section className="mx-auto w-full max-w-6xl space-y-4">
      <header className="flex flex-wrap items-start gap-3">
        <Link
          to="/workspace?view=plugins"
          className="mt-1 inline-flex shrink-0 items-center gap-1.5 rounded-lg border border-glass-line bg-glass-subtle px-2.5 py-1.5 text-xs font-medium text-ink-muted transition hover:bg-glass-hover hover:text-ink"
        >
          <ArrowLeft className="h-3.5 w-3.5" aria-hidden />
          {t('settings.plugins.installPage.back')}
        </Link>
        <div className="min-w-0 flex-1">
          <h1 className="text-xl font-bold tracking-tight text-ink">{t('settings.plugins.installPage.title')}</h1>
          <p className="mt-1 text-xs leading-relaxed text-ink-faint">{t('settings.plugins.installPage.description')}</p>
        </div>
      </header>

      {failure && <p role="alert" className="rounded-lg border border-[var(--app-danger)]/25 bg-[var(--app-danger)]/5 px-3 py-2 text-xs text-[var(--app-danger)]">{failure}</p>}
      {success && <p role="status" className="rounded-lg border border-brand-500/25 bg-brand-500/5 px-3 py-2 text-xs text-brand-400">{success}</p>}

      <section className="glass-card flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-glass-line p-3.5">
        <div className="flex min-w-0 items-center gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand-600/10 text-brand-400">
            <FolderOpen className="h-4 w-4" aria-hidden />
          </span>
          <div className="min-w-0">
            <h2 className="text-sm font-semibold text-ink">{t('settings.plugins.installPage.localTitle')}</h2>
            <p className="mt-0.5 text-xs text-ink-faint">{t('settings.plugins.installPage.localDescription')}</p>
          </div>
        </div>
        <button
          type="button"
          disabled={installing}
          onClick={() => void installLocal()}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-lg border border-glass-line bg-glass-subtle px-3 py-2 text-xs font-medium text-ink transition hover:border-brand-500/30 hover:bg-glass-hover disabled:opacity-50"
        >
          <PackagePlus className="h-3.5 w-3.5" aria-hidden />
          {installing ? t('settings.plugins.installing') : t('settings.plugins.install')}
        </button>
      </section>

      <OnlinePluginCatalog onInstalled={setStates} />
    </section>
  )
}

function errorText(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    return String((cause as { message?: unknown }).message ?? cause)
  }
  return String(cause)
}
