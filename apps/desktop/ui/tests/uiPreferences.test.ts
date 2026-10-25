import assert from 'node:assert/strict'
import test from 'node:test'
import { runUiPreferenceMethod, UiPreferenceError } from '../src/plugins/uiPreferences.ts'

function device() {
  const values = new Map<string, string>()
  return { values, store: {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value) },
    removeItem: (key: string) => { values.delete(key) },
  } }
}

test('one device shares plugin preferences across surfaces while namespaces remain separate', () => {
  const first = device()
  const second = device()
  runUiPreferenceMethod('translator', 'core.ui.preferences.set', { key: 'layout', value: { collapsed: true } }, first.store)
  assert.deepEqual(runUiPreferenceMethod('translator', 'core.ui.preferences.get', { key: 'layout' }, first.store), { collapsed: true })
  assert.equal(runUiPreferenceMethod('editor', 'core.ui.preferences.get', { key: 'layout' }, first.store), null)
  assert.equal(runUiPreferenceMethod('translator', 'core.ui.preferences.get', { key: 'layout' }, second.store), null)
  runUiPreferenceMethod('translator', 'core.ui.preferences.delete', { key: 'layout' }, first.store)
  assert.equal(first.values.size, 0)
})

test('preference calls cannot select another plugin or access host storage', () => {
  const { store, values } = device()
  values.set('wonderland:web-connection', 'private host connection')
  assert.throws(() => runUiPreferenceMethod('translator', 'core.ui.preferences.get', { key: 'layout', pluginId: 'editor' }, store), { code: 'INVALID_INPUT' })
  assert.equal(runUiPreferenceMethod('translator', 'core.ui.preferences.get', { key: 'wonderland:web-connection' }, store), null)
  assert.throws(() => runUiPreferenceMethod('translator', 'core.ui.preferences.enumerate', {}, store), { code: 'HOST_METHOD_UNSUPPORTED' })
})

test('invalid JSON and storage failures cannot be reported as a successful save', () => {
  const { store, values } = device()
  for (const value of [undefined, Number.NaN, () => {}, [undefined]]) {
    assert.throws(() => runUiPreferenceMethod('editor', 'core.ui.preferences.set', { key: 'layout', value }, store), { code: 'INVALID_INPUT' })
  }
  values.set('wonderland:plugin-ui-preferences:editor:layout', '{broken')
  assert.throws(() => runUiPreferenceMethod('editor', 'core.ui.preferences.get', { key: 'layout' }, store), { code: 'UI_STORAGE_INVALID' })
  const blocked = { ...store, setItem: () => { throw new Error('quota or denied') } }
  assert.throws(() => runUiPreferenceMethod('editor', 'core.ui.preferences.set', { key: 'layout', value: false }, blocked), (error: unknown) => error instanceof UiPreferenceError && error.code === 'UI_STORAGE_UNAVAILABLE')
})
