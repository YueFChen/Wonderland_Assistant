import { scheduleUiDeadline, validateUiTimeout } from './uiDeadline.ts'

export interface CliPluginUiCommand {
  requestId: string
  contributionId: string
  commandId: string
  input: unknown
  timeoutMs?: number
}

export class CliPluginUiCommandError extends Error {
  readonly code: string
  constructor(code: string, message: string) {
    super(message)
    this.code = code
  }
}

const queued = new Map<string, CliPluginUiCommand>()
const pending = new Map<string, {
  resolve: (value: unknown) => void
  reject: (error: CliPluginUiCommandError) => void
  stopDeadline: () => void
}>()

export function requestPluginUiCommand(command: CliPluginUiCommand): Promise<unknown> {
  if (pending.has(command.requestId)) return Promise.reject(new CliPluginUiCommandError('INVALID_REQUEST', 'The UI request ID is already active.'))
  let timeoutMs: number | undefined
  try { timeoutMs = validateUiTimeout(command.timeoutMs) }
  catch { return Promise.reject(new CliPluginUiCommandError('INVALID_REQUEST', 'UI command timeoutMs must be a positive safe integer.')) }
  return new Promise((resolve, reject) => {
    const stopDeadline = scheduleUiDeadline(timeoutMs, () => {
      queued.delete(command.requestId)
      pending.delete(command.requestId)
      reject(new CliPluginUiCommandError('UI_COMMAND_TIMEOUT', 'The plugin UI did not complete the command in time.'))
      window.dispatchEvent(new CustomEvent('wonderland:cli-ui-cancel', { detail: { requestId: command.requestId } }))
    })
    pending.set(command.requestId, { resolve, reject, stopDeadline })
    queued.set(command.requestId, command)
    window.dispatchEvent(new CustomEvent('wonderland:cli-ui-command', { detail: command }))
  })
}

export function takeQueuedPluginUiCommand(requestId: string): CliPluginUiCommand | undefined {
  const command = queued.get(requestId)
  if (command) queued.delete(requestId)
  return command
}

export function queuedPluginUiCommands(contributionId: string): CliPluginUiCommand[] {
  return [...queued.values()].filter((command) => command.contributionId === contributionId)
}

export function completePluginUiCommand(
  requestId: string,
  result: unknown,
  error?: { code?: string; message?: string },
) {
  const request = pending.get(requestId)
  if (!request) return
  request.stopDeadline()
  pending.delete(requestId)
  queued.delete(requestId)
  if (error) {
    request.reject(new CliPluginUiCommandError(error.code ?? 'UI_COMMAND_FAILED', error.message ?? 'Plugin UI command failed.'))
  } else {
    request.resolve(result)
  }
}

export function cancelPluginUiCommand(requestId: string, code = 'CANCELLED', message = 'The UI command was cancelled.') {
  if (!pending.has(requestId)) return
  completePluginUiCommand(requestId, null, { code, message })
  window.dispatchEvent(new CustomEvent('wonderland:cli-ui-cancel', { detail: { requestId } }))
}

export function closePluginUiCommands() {
  for (const requestId of [...pending.keys()]) cancelPluginUiCommand(requestId, 'CONTRIBUTION_CLOSED', 'The Core UI closed before completing the command.')
}
