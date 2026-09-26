export interface CliPluginUiCommand {
  requestId: string
  contributionId: string
  commandId: string
  input: unknown
}

export class CliPluginUiCommandError extends Error {
  constructor(readonly code: string, message: string) {
    super(message)
  }
}

const queued = new Map<string, CliPluginUiCommand>()
const pending = new Map<string, {
  resolve: (value: unknown) => void
  reject: (error: CliPluginUiCommandError) => void
  timer: number
}>()

export function requestPluginUiCommand(command: CliPluginUiCommand): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => {
      queued.delete(command.requestId)
      pending.delete(command.requestId)
      reject(new CliPluginUiCommandError('UI_COMMAND_TIMEOUT', 'The plugin UI did not complete the command in time.'))
    }, 45_000)
    pending.set(command.requestId, { resolve, reject, timer })
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
  window.clearTimeout(request.timer)
  pending.delete(requestId)
  queued.delete(requestId)
  if (error) {
    request.reject(new CliPluginUiCommandError(error.code ?? 'UI_COMMAND_FAILED', error.message ?? 'Plugin UI command failed.'))
  } else {
    request.resolve(result)
  }
}
