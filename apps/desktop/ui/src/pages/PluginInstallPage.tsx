import { ArrowLeft, PackagePlus } from 'lucide-react'
import { Link, useSearchParams } from 'react-router-dom'
import { usePluginManagement } from '../plugins/management'

import { OnlinePluginCatalog } from '../components/OnlinePluginCatalog'
import { t } from '../i18n'
import { usePlugins } from '../plugins/api'
import { requestLocalInstall } from '../plugins/localInstall'
import { isWebClient } from '../core/transport'
import '../components/pluginInstall.css'

export function PluginInstallPage() {
  const { setStates } = usePlugins()
  const operation = usePluginManagement()
  const [params, setParams] = useSearchParams()
  return (
    <section className="mx-auto w-full max-w-6xl space-y-5">
      <header className="flex items-center justify-between gap-4">
        <div className="flex min-w-0 items-center gap-3">
          <Link to="/workspace?view=plugins" aria-label={t('settings.plugins.installPage.back')} title={t('settings.plugins.installPage.back')} className="grid h-8 w-8 shrink-0 place-items-center rounded-lg text-ink-muted transition hover:bg-glass-hover hover:text-ink">
            <ArrowLeft className="h-4 w-4" aria-hidden />
          </Link>
          <h1 className="app-background-copy text-xl font-bold tracking-tight text-ink">{t('settings.plugins.installPage.title')}</h1>
        </div>
        {!isWebClient && <button type="button" disabled={Boolean(operation)} className="app-button app-button-primary" onClick={() => requestLocalInstall('files')}><PackagePlus className="h-4 w-4" aria-hidden />{t('settings.plugins.installPage.localTitle')}</button>}
      </header>
      {isWebClient
        ? <p role="status" className="glass-card rounded-xl p-4 text-sm text-ink-muted">{t('settings.plugins.distribution.desktopOnly')}</p>
        : <OnlinePluginCatalog onInstalled={setStates} updatesOnly={params.get('filter') === 'updates'} onFilter={updates => setParams(updates ? {filter:'updates'} : {})} />}
    </section>
  )
}
