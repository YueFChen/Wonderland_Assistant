import assert from 'node:assert/strict'
import { afterEach, test } from 'node:test'
import { requestPluginUiCommand, completePluginUiCommand, cancelPluginUiCommand, closePluginUiCommands, queuedPluginUiCommands } from '../src/plugins/cliUiCommandBus.ts'

const events = new EventTarget()
Object.defineProperty(globalThis, 'window', { configurable: true, value: events })
afterEach(() => closePluginUiCommands())
const command = (requestId: string, timeoutMs?: number) => ({ requestId, contributionId: 'editor/main', commandId: 'export', input: {}, timeoutMs })

test('a default UI command survives the old 45 second limit', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  const result = requestPluginUiCommand(command('long'))
  context.mock.timers.tick(60_000)
  assert.equal(queuedPluginUiCommands('editor/main').length, 1)
  completePluginUiCommand('long', 'finished')
  assert.equal(await result, 'finished')
})

test('explicit deadlines and cancellation settle the pending command and remove its queue item', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  const timed = requestPluginUiCommand(command('timed', 500))
  const rejected = assert.rejects(timed, { code: 'UI_COMMAND_TIMEOUT' })
  context.mock.timers.tick(500)
  await rejected
  assert.equal(queuedPluginUiCommands('editor/main').length, 0)
  const cancelled = requestPluginUiCommand(command('cancelled'))
  const cancelledResult = assert.rejects(cancelled, { code: 'CANCELLED' })
  cancelPluginUiCommand('cancelled')
  await cancelledResult
})

test('long explicit deadlines do not overflow the browser timer', async (context) => {
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  const result = requestPluginUiCommand(command('long-explicit', 3_000_000_000))
  context.mock.timers.tick(2_147_483_647)
  assert.equal(queuedPluginUiCommands('editor/main').length, 1)
  completePluginUiCommand('long-explicit', null)
  assert.equal(await result, null)
})
