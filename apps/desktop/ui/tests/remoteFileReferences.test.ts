import assert from 'node:assert/strict'
import { test } from 'node:test'
import { RemoteFileReferences } from '../src/mobile/remoteFileReferences.ts'

test('Core leases release device files after completion, cancellation, or partial reads', () => {
  const references = new RemoteFileReferences()
  const file = new File(['contents'], 'data.bin')
  references.register('first', file)
  references.confirmed('first', () => {})
  references.register('second', file)
  references.confirmed('second', () => {})
  references.retain(new Set(['first']), references.snapshotRevision())
  assert.equal(references.get('first')?.file, file)
  assert.equal(references.get('second'), undefined)
  references.retain(new Set(), references.snapshotRevision())
  assert.equal(references.get('first'), undefined)
})

test('an older snapshot cannot release a pending reply or a newly confirmed file', () => {
  const references = new RemoteFileReferences()
  const file = new File(['contents'], 'data.bin')
  const beforeChoice = references.snapshotRevision()
  references.register('first', file)
  references.retain(new Set(), beforeChoice)
  assert.equal(references.get('first')?.file, file)
  const whileReplying = references.snapshotRevision()
  references.retain(new Set(), whileReplying)
  assert.equal(references.get('first')?.file, file)
  references.confirmed('first', () => {})
  references.retain(new Set(), whileReplying)
  assert.equal(references.get('first')?.file, file)
  references.retain(new Set(), references.snapshotRevision())
  assert.equal(references.get('first'), undefined)
})

test('failed replies and disconnect cleanup discard their references immediately', () => {
  const references = new RemoteFileReferences()
  const file = new File(['contents'], 'data.bin')
  references.register('failed', file)
  references.delete('failed')
  assert.equal(references.get('failed'), undefined)
  references.register('live', file)
  references.clear()
  assert.equal(references.get('live'), undefined)
})

test('a lease release before POST confirmation triggers a fresh synchronization', () => {
  const references = new RemoteFileReferences()
  const file = new File(['contents'], 'data.bin')
  references.register('first', file)
  references.retain(new Set(), references.snapshotRevision())
  assert.equal(references.get('first')?.file, file)
  let refreshes = 0
  references.confirmed('first', () => {
    refreshes++
    references.retain(new Set(), references.snapshotRevision())
  })
  assert.equal(refreshes, 1)
  assert.equal(references.get('first'), undefined)
})
