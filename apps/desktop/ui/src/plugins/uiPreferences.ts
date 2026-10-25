export type JsonPreference = null | boolean | number | string | JsonPreference[] | { [key: string]: JsonPreference }

export class UiPreferenceError extends Error {
  readonly code: string
  constructor(code: string, message: string) { super(message); this.code = code }
}

type PreferenceStore = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>
const prefix = 'wonderland:plugin-ui-preferences:'

function validJson(value: unknown): boolean {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return true
  if (typeof value === 'number') return Number.isFinite(value)
  if (Array.isArray(value)) return value.every(validJson)
  return typeof value === 'object' && value !== null
    && Object.getPrototypeOf(value) === Object.prototype
    && Object.values(value).every(validJson)
}

/** The verified surface owner determines the namespace; parameters never select an owner. */
export function runUiPreferenceMethod(pluginId: string, method: string, params: unknown, store: PreferenceStore): unknown {
  if (!['core.ui.preferences.get', 'core.ui.preferences.set', 'core.ui.preferences.delete'].includes(method)) {
    throw new UiPreferenceError('HOST_METHOD_UNSUPPORTED', 'This Core UI preference method is not supported.')
  }
  if (typeof params !== 'object' || params === null || Array.isArray(params)) {
    throw new UiPreferenceError('INVALID_INPUT', 'UI preference parameters must be an object.')
  }
  const input = params as Record<string, unknown>
  const allowed = method.endsWith('.set') ? ['key', 'value'] : ['key']
  if (Object.keys(input).some((key) => !allowed.includes(key)) || typeof input.key !== 'string' || !input.key) {
    throw new UiPreferenceError('INVALID_INPUT', 'A UI preference key is required; the plugin namespace cannot be overridden.')
  }
  const storageKey = `${prefix}${encodeURIComponent(pluginId)}:${encodeURIComponent(input.key)}`
  let encoded: string | undefined
  if (method.endsWith('.set')) {
    try {
      if (!validJson(input.value)) throw new Error('invalid JSON value')
      encoded = JSON.stringify(input.value)
    } catch {
      throw new UiPreferenceError('INVALID_INPUT', 'UI preferences must contain valid JSON data.')
    }
  }
  try {
    if (method.endsWith('.get')) {
      const saved = store.getItem(storageKey)
      if (saved === null) return null
      try { return JSON.parse(saved) as JsonPreference }
      catch { throw new UiPreferenceError('UI_STORAGE_INVALID', 'The saved UI preference is not valid JSON.') }
    }
    if (method.endsWith('.set')) store.setItem(storageKey, encoded!)
    else store.removeItem(storageKey)
    return { ok: true }
  } catch (cause) {
    if (cause instanceof UiPreferenceError) throw cause
    throw new UiPreferenceError('UI_STORAGE_UNAVAILABLE', 'This device could not read or save the UI preference.')
  }
}
