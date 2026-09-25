import { ArrowRight, Sparkles } from 'lucide-react'
import { Link } from 'react-router-dom'

import { t } from '../i18n'

/** Product landing page. */
export function HomePage() {
  return (
    <section className="relative mx-auto flex min-h-[min(72vh,760px)] w-full max-w-6xl flex-col items-center justify-center overflow-hidden px-6 text-center">
      <div className="pointer-events-none absolute left-1/2 top-1/2 -z-10 h-[32rem] w-[32rem] -translate-x-1/2 -translate-y-1/2 rounded-full bg-brand-500/10 blur-[100px]" />
      <div className="mb-7 grid h-16 w-16 place-items-center rounded-[1.35rem] border border-glass-line bg-glass shadow-xl shadow-brand-950/10">
        <Sparkles className="h-7 w-7 text-brand-400" strokeWidth={1.6} aria-hidden />
      </div>
      <p className="mb-4 text-xs font-semibold uppercase tracking-[0.28em] text-brand-400">Wonderland Studio</p>
      <h1 className="brand-text text-5xl font-bold tracking-tight md:text-7xl">Wonderland Assistant</h1>
      <p className="mt-5 max-w-xl text-base leading-7 tracking-wide text-ink-muted md:text-lg">
        {t('home.tagline')}
      </p>
      <Link
        to="/workspace"
        className="mt-10 inline-flex items-center gap-3 rounded-xl bg-brand-600 px-6 py-3.5 text-sm font-semibold text-white shadow-lg shadow-brand-900/20 transition hover:-translate-y-0.5 hover:bg-brand-500 focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-brand-400"
      >
        {t('home.enterWorkspace')}
        <ArrowRight className="h-4 w-4" aria-hidden />
      </Link>
      <p className="mt-14 text-xs tracking-wide text-ink-faint">Wonderland Assistant · Apache-2.0</p>
    </section>
  )
}
