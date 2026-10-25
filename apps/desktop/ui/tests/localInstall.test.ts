import assert from 'node:assert/strict'
import test from 'node:test'
import { chooseInstallCandidate, filesFromDrop, initialInstallSelection, installSignatureDescription, type DropEntry } from '../src/plugins/localInstallSelection.ts'
import type { InstallCandidate } from '../src/plugins/localInstall.ts'

test('conflicting IDs require an explicit unique choice; invalid packages stay excluded', () => {
  const items = [
    { key: 'a1', manifest: { id: 'a' }, error: null }, { key: 'a2', manifest: { id: 'a' }, error: null },
    { key: 'b', manifest: { id: 'b' }, error: null }, { key: 'bad', manifest: { id: 'c' }, error: { message: 'invalid' } },
  ] as InstallCandidate[]
  const selected = initialInstallSelection(items)
  assert.deepEqual([...selected], ['b'])
  const first = chooseInstallCandidate(items, selected, 'a1')
  assert.deepEqual([...first], ['b', 'a1'])
  const second = chooseInstallCandidate(items, first, 'a2')
  assert.deepEqual([...second], ['b', 'a2'])
  assert.deepEqual([...chooseInstallCandidate(items, second, 'bad')], ['b', 'a2'])
  assert.deepEqual([...chooseInstallCandidate(items, second, 'a2')], ['b'])
})

test('drop directories read all entry batches, only first-level packages, and leave business files out', async () => {
  const entry = (name: string): DropEntry => ({ name, isFile: true, isDirectory: false, file: ok => ok(new File(['test'], name)), createReader: () => { throw Error('Do not recurse into files') } })
  const nested: DropEntry = { name: 'nested', isFile: false, isDirectory: true, file: () => { throw Error('Do not read directories as files') }, createReader: () => { throw Error('Do not recurse') } }
  const batches = [[entry('one.wplug'), entry('picture.png'), nested], [entry('two.WPLUG')], []]
  const directory: DropEntry = { ...nested, name: 'packages', createReader: () => ({ readEntries: ok => ok(batches.shift()!) }) }
  const result = await filesFromDrop([directory], [])
  assert.deepEqual(result.map(item => item.name), ['packages/one.wplug', 'packages/two.WPLUG'])
  assert.deepEqual((await filesFromDrop([], [new File(['image'], 'image.png'), new File(['package'], 'three.wplug')])).map(item => item.name), ['three.wplug'])
})

test('local preview distinguishes unsigned, unconfirmed, pinned, and development identities', () => {
  const item = { signaturePublicKey: null, existing: null } as InstallCandidate
  assert.match(installSignatureDescription(item), /未签名/)
  item.signaturePublicKey = 'a'.repeat(64)
  assert.match(installSignatureDescription(item), /尚未经目录确认/)
  item.existing = { signature: { expectedPublicKey: item.signaturePublicKey } } as NonNullable<InstallCandidate['existing']>
  assert.match(installSignatureDescription(item), /匹配已记录发布公钥/)
  item.signaturePublicKey = 'b'.repeat(64)
  assert.match(installSignatureDescription(item), /开发覆盖保留原公钥/)
})
