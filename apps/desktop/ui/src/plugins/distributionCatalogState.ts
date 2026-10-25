import type { DistributionConfigureRequest, DistributionSettingsView, PluginCatalogSnapshot } from './api.ts'

interface DistributionSource {
  settings: () => Promise<DistributionSettingsView>
  catalog: () => Promise<PluginCatalogSnapshot>
  configure: (request: DistributionConfigureRequest) => Promise<DistributionSettingsView>
}

interface CatalogState {
  settings: DistributionSettingsView | null
  catalog: PluginCatalogSnapshot | null
  loading: boolean
  saving: boolean
  errorCode: string | null
}

/** UI requests have their own ordering so old IPC responses cannot restore a replaced code's view. */
export function createDistributionCatalogState(source: DistributionSource) {
  let state: CatalogState = { settings: null, catalog: null, loading: false, saving: false, errorCode: null }
  let revision = 0
  const subscribers = new Set<() => void>()
  const publish = (changes: Partial<CatalogState>) => {
    state = { ...state, ...changes }
    for (const subscriber of subscribers) subscriber()
  }
  const refresh = async () => {
    if (state.saving) return
    const request = ++revision
    publish({ loading: true, errorCode: null })
    try {
      if (!state.settings) {
        const settings = await source.settings()
        if (request !== revision) return
        publish({ settings })
      }
      const catalog = await source.catalog()
      if (request !== revision) return
      // Read after catalog authorization has updated the safe credential status.
      const settings = await source.settings()
      if (request === revision) {
        // The known expiry may pass between the two reads, or CLI may replace the source.
        const visible = catalog.sourceUrl !== settings.sourceUrl ? null : {
          ...catalog,
          credentialState: settings.credentialState,
          plugins: ['none', 'invalid', 'expired', 'revoked'].includes(settings.credentialState)
            ? catalog.plugins.filter((item) => item.registration.visibility === 'public')
            : catalog.plugins,
        }
        publish({ settings, catalog: visible })
      }
    } catch (error) {
      if (request === revision) publish({ catalog: unavailableSnapshot(state.catalog), errorCode: safeErrorCode(error) })
    } finally {
      if (request === revision) publish({ loading: false })
    }
  }
  const configure = async (request: DistributionConfigureRequest) => {
    if (state.saving) return
    const current = ++revision
    const previous = state
    // Discard visible restricted data while the old credential is being replaced.
    publish({ catalog: null, saving: true, loading: false, errorCode: null })
    try {
      const settings = await source.configure(request)
      if (current !== revision) return
      publish({ settings, saving: false })
      await refresh()
    } catch (error) {
      if (current === revision) publish({
        settings: previous.settings,
        catalog: previous.catalog,
        saving: false,
        errorCode: safeErrorCode(error),
      })
    }
  }
  return {
    getSnapshot: () => state,
    subscribe(listener: () => void) { subscribers.add(listener); return () => { subscribers.delete(listener) } },
    refresh,
    configure,
    reset() { revision++; publish({ settings: null, catalog: null, loading: false, saving: false, errorCode: null }) },
    invalidate() { revision++; },
  }
}

export function canInstallCatalogItem(catalog: PluginCatalogSnapshot | null, item: PluginCatalogSnapshot['plugins'][number]) {
  return catalog?.status === 'ready' && !catalog.stale && !item.stale && item.compatible && item.installable && item.entry !== null
    && (item.registration.visibility === 'public' || catalog.credentialState === 'valid')
}

function unavailableSnapshot(catalog: PluginCatalogSnapshot | null): PluginCatalogSnapshot | null {
  return catalog && { ...catalog, status: 'unavailable', stale: true, plugins: catalog.plugins.map((item) => ({ ...item, installable: false })) }
}

/** Error objects are not presentation text: never retain arbitrary IPC messages alongside credentials. */
function safeErrorCode(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'code' in error && typeof error.code === 'string' && /^[A-Z_]{1,64}$/.test(error.code)) return error.code
  return 'DISTRIBUTION_UNAVAILABLE'
}
