import { useEffect, useRef } from 'react'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'
import { useLocation, useNavigate } from 'react-router-dom'
import { getVersion } from '@tauri-apps/api/app'
import type { ThemeSettings } from '@wonderland/core-bindings'

import { buildContributionRegistry } from '../plugins/contributions'
import { pluginApi } from '../plugins/api'
import {
  MAX_OPEN_ACTIVITY_SURFACES,
  readWorkspaceLayout,
  writeWorkspaceLayout,
  type WorkspaceLayoutState,
} from '../plugins/workspaceLayout'
import { requestPluginUiCommand, CliPluginUiCommandError } from '../plugins/cliUiCommandBus'
import { useTheme } from '../theme/ThemeProvider'
import { themeApi } from '../theme/api'

interface CliUiRequest {
  requestId: string
  args: string[]
}

interface CliUiError {
  code: string
  message: string
  details?: unknown
}

class CliUiFailure extends Error {
  constructor(readonly code: string, message: string) {
    super(message)
  }
}

/** Routes local CLI UI commands through the same React state used by the desktop interface. */
export function CliUiBridge() {
  const navigate = useNavigate()
  const location = useLocation()
  const theme = useTheme()
  const route = `${location.pathname}${location.search}`
  const themeSettings = theme.settings
  const contextRef = useRef({
    navigate,
    route,
    themeSettings,
    setTheme: theme.setTheme,
  })
  contextRef.current = { navigate, route, themeSettings, setTheme: theme.setTheme }

  useEffect(() => {
    let unlisten: (() => void) | undefined
    let live = true
    void listen<CliUiRequest>('cli:ui-request', ({ payload }) => {
      void handleRequest(payload, {
        ...contextRef.current,
      }).then(
        (result) => invoke('cli_ui_ack', { requestId: payload.requestId, result, error: null }),
        (cause: unknown) => {
          const error: CliUiError = cause instanceof CliUiFailure || cause instanceof CliPluginUiCommandError
            ? { code: cause.code, message: cause.message }
            : { code: 'UI_COMMAND_FAILED', message: errorText(cause) }
          return invoke('cli_ui_ack', { requestId: payload.requestId, result: null, error })
        },
      ).catch(() => undefined)
    }).then((stop) => {
      if (live) {
        unlisten = stop
        void invoke('cli_ui_ready').catch(() => undefined)
      }
      else stop()
    })
    return () => {
      live = false
      unlisten?.()
    }
  }, [])

  return null
}

async function handleRequest(
  request: CliUiRequest,
  context: {
    navigate: ReturnType<typeof useNavigate>
    route: string
    themeSettings: ThemeSettings
    setTheme: (settings: ThemeSettings) => Promise<void>
  },
): Promise<unknown> {
  const [group, command, ...args] = request.args
  const states = await pluginApi.states().catch(() => null)
  const registry = buildContributionRegistry(states ?? [])
  const layout = readWorkspaceLayout()

  if (group === 'app' && (command === 'status' || command === 'open')) {
    if (command === 'status' && args.length !== 0) throw new CliUiFailure('INVALID_REQUEST', '`app status` takes no options.')
    if (command === 'open') {
      const targetIndex = args.indexOf('--target')
      if (targetIndex >= 0) {
        if (targetIndex !== 0 || args.length !== 2) throw new CliUiFailure('INVALID_REQUEST', '`app open` accepts only one `--target <route>`.')
        const target = args[targetIndex + 1]
        if (!target) throw new CliUiFailure('INVALID_REQUEST', '--target requires a route.')
        context.navigate(resolveRoute(target, registry))
        await nextFrame()
      } else if (args.length !== 0) {
        throw new CliUiFailure('INVALID_REQUEST', 'Unknown `app open` option.')
      }
    }
    return {
      coreVersion: await getVersion(),
      desktopRunning: true,
      currentRoute: window.location.hash.slice(1) || '/',
      pluginManagerAvailable: states !== null,
      pluginCount: states?.length ?? null,
      runningPluginCount: states?.filter((state) => state.runtime === 'running').length ?? null,
      workspace: readWorkspaceLayout(),
      theme: context.themeSettings,
    }
  }

  if (group !== 'ui') throw new CliUiFailure('INVALID_REQUEST', 'Unsupported desktop UI command.')
  if (command === 'state' && args.length === 0) {
    return {
      route: context.route,
      workspace: layout,
      theme: context.themeSettings,
      openActivities: layout.openContributionIds,
      activeActivity: layout.activeContributionId,
      activeSidebarView: layout.activeSidebarContributionId,
    }
  }
  if (command === 'navigate' && args.length === 1) {
    const target = resolveRoute(args[0], registry)
    context.navigate(target)
    await nextFrame()
    return { route: window.location.hash.slice(1) || '/', applied: true }
  }

  if (command === 'activity') {
    const [action, id] = args
    const activities = registry.filter((item) => item.kind === 'activity')
    if (action === 'list' && args.length === 1) {
      return {
        activities: activities.map(({ id: activityId, title, status, href }) => ({ id: activityId, title, status, href })),
        open: layout.openContributionIds,
        active: layout.activeContributionId,
      }
    }
    if (typeof id !== 'string') throw new CliUiFailure('INVALID_REQUEST', 'An Activity ID is required.')
    const contribution = activities.find((item) => item.id === id)
    if (!contribution) throw new CliUiFailure('CONTRIBUTION_NOT_FOUND', `Activity '${id}' is not registered.`)
    if (action === 'open' || action === 'activate') {
      if (args.length !== 2) throw new CliUiFailure('INVALID_REQUEST', `Activity ${action} takes one contribution ID.`)
      if (contribution.status !== 'ready') throw new CliUiFailure('CONTRIBUTION_UNAVAILABLE', `Activity '${id}' is not ready.`)
      updateLayout((current) => ({
        ...current,
        openContributionIds: [...current.openContributionIds.filter((item) => item !== id), id].slice(-MAX_OPEN_ACTIVITY_SURFACES),
        activeContributionId: id,
      }))
      context.navigate(contribution.href)
      await nextFrame()
      return { opened: id, route: window.location.hash.slice(1) }
    }
    if (action === 'close') {
      if (args.length !== 2) throw new CliUiFailure('INVALID_REQUEST', 'Activity close takes one contribution ID.')
      const next = layout.openContributionIds.filter((item) => item !== id)
      const active = layout.activeContributionId === id ? next.at(-1) ?? null : layout.activeContributionId
      updateLayout((current) => ({ ...current, openContributionIds: next, activeContributionId: active }))
      const nextContribution = activities.find((item) => item.id === active)
      context.navigate(nextContribution?.href ?? '/workspace')
      await nextFrame()
      return { closed: id, activeActivity: active }
    }
    throw new CliUiFailure('INVALID_REQUEST', 'Expected list, open, activate, or close.')
  }

  if (command === 'command') {
    const [action, contributionId, commandId, ...options] = args
    if (action !== 'run' || !contributionId || !commandId) {
      throw new CliUiFailure('INVALID_REQUEST', 'Expected `ui command run <plugin>/<contribution> <command-id> --input-json <json> [--yes]`.')
    }
    const contribution = registry.find((item) => item.id === contributionId)
    if (!contribution || contribution.status !== 'ready') {
      throw new CliUiFailure('CONTRIBUTION_UNAVAILABLE', `Contribution '${contributionId}' is not ready.`)
    }
    const declaration = contribution.state.manifest.ui?.contributions
      .find((item) => item.id === contribution.contributionId)
    const declaredCommand = (declaration?.commands ?? []).find((item) => item.id === commandId)
    if (!declaredCommand) throw new CliUiFailure('UI_COMMAND_NOT_FOUND', `Command '${commandId}' is not declared by '${contributionId}'.`)
    let inputJson: string | undefined
    let confirmed = false
    for (let index = 0; index < options.length; index += 1) {
      if (options[index] === '--yes') {
        if (confirmed) throw new CliUiFailure('INVALID_REQUEST', '--yes may be specified once.')
        confirmed = true
      }
      else if (options[index] === '--input-json') {
        if (inputJson !== undefined) throw new CliUiFailure('INVALID_REQUEST', '--input-json may be specified once.')
        inputJson = options[index + 1]
        if (inputJson === undefined) throw new CliUiFailure('INVALID_REQUEST', '--input-json requires a JSON value.')
        index += 1
      } else {
        throw new CliUiFailure('INVALID_REQUEST', `Unknown UI command option '${options[index]}'.`)
      }
    }
    if (inputJson === undefined) throw new CliUiFailure('INVALID_REQUEST', '--input-json is required.')
    if (declaredCommand.effect === 'mutating' && !confirmed) {
      throw new CliUiFailure('CONFIRMATION_REQUIRED', 'This plugin UI command changes content. Pass --yes to confirm.')
    }
    let input: unknown
    try {
      input = JSON.parse(inputJson)
    } catch {
      throw new CliUiFailure('INVALID_REQUEST', '--input-json must contain valid JSON.')
    }
    if (contribution.kind === 'view') {
      updateLayout((current) => ({ ...current, activeSidebarContributionId: contributionId }))
      context.navigate('/workspace')
    } else {
      updateLayout((current) => ({
        ...current,
        openContributionIds: [...current.openContributionIds.filter((item) => item !== contributionId), contributionId].slice(-MAX_OPEN_ACTIVITY_SURFACES),
        activeContributionId: contributionId,
      }))
      context.navigate(contribution.href)
    }
    await nextFrame()
    return requestPluginUiCommand({
      requestId: request.requestId,
      contributionId,
      commandId,
      input,
    })
  }

  if (command === 'sidebar') {
    const [action, id] = args
    if (action === 'state' && args.length === 1) {
      return { collapsed: layout.collapsed, active: layout.activeSidebarContributionId }
    }
    if (action === 'collapse' || action === 'expand') {
      if (args.length !== 1) throw new CliUiFailure('INVALID_REQUEST', `Sidebar ${action} takes no contribution ID.`)
      updateLayout((current) => ({ ...current, collapsed: action === 'collapse' }))
      await nextFrame()
      return { collapsed: action === 'collapse' }
    }
    if (action === 'close' && args.length === 1) {
      updateLayout((current) => ({ ...current, activeSidebarContributionId: null }))
      context.navigate('/workspace')
      await nextFrame()
      return { active: null }
    }
    if (action === 'open' && id) {
      if (args.length !== 2) throw new CliUiFailure('INVALID_REQUEST', 'Sidebar open takes one contribution ID.')
      const view = registry.find((item) => item.id === id && item.kind === 'view')
      if (!view) throw new CliUiFailure('CONTRIBUTION_NOT_FOUND', `Sidebar View '${id}' is not registered.`)
      if (view.status !== 'ready') throw new CliUiFailure('CONTRIBUTION_UNAVAILABLE', `Sidebar View '${id}' is not ready.`)
      updateLayout((current) => ({ ...current, activeSidebarContributionId: id }))
      context.navigate('/workspace')
      await nextFrame()
      return { active: id }
    }
    throw new CliUiFailure('INVALID_REQUEST', 'Expected state, collapse, expand, open, or close.')
  }

  if (command === 'layout') {
    const result = applyLayoutCommand(args, registry.map((item) => item.id))
    await nextFrame()
    return result
  }
  if (command === 'theme') {
    if (args.length === 1 && args[0] === 'get') return { settings: context.themeSettings }
    if (args[0] === 'set') {
      const settings = parseThemeOptions(args.slice(1), context.themeSettings)
      if (settings.background?.kind === 'image') {
        const backgrounds = await themeApi.backgrounds()
        const backgroundId = settings.background.id
        if (!backgrounds.some((background) => background.id === backgroundId)) {
          throw new CliUiFailure('BACKGROUND_NOT_FOUND', 'The selected theme background is not in the Core library.')
        }
      }
      await context.setTheme(settings)
      await nextFrame()
      return { settings, applied: true }
    }
    throw new CliUiFailure('INVALID_REQUEST', 'Expected `ui theme get` or `ui theme set`.')
  }
  throw new CliUiFailure('INVALID_REQUEST', 'Unknown or malformed UI command.')
}

function resolveRoute(target: string, registry: ReturnType<typeof buildContributionRegistry>): string {
  const routes: Record<string, string> = {
    home: '/',
    workspace: '/workspace',
    plugins: '/workspace?view=plugins',
    settings: '/workspace/settings',
    'plugin-install': '/workspace/plugins/install',
  }
  if (routes[target]) return routes[target]
  if (target.startsWith('plugin:')) {
    const id = target.slice('plugin:'.length)
    const contribution = registry.find((item) => item.id === id)
    if (contribution) return contribution.href
  }
  throw new CliUiFailure('INVALID_REQUEST', `Unknown Core route '${target}'.`)
}

function applyLayoutCommand(args: string[], contributionIds: string[]) {
  const [action, id, ...options] = args
  const current = readWorkspaceLayout()
  if (action === 'get' && args.length === 1) return current
  if (action === 'reset' && args.length === 1) {
    writeWorkspaceLayout(emptyLayout())
    return readWorkspaceLayout()
  }
  if (!id || !contributionIds.includes(id)) {
    throw new CliUiFailure('CONTRIBUTION_NOT_FOUND', `Contribution '${id ?? ''}' is not registered.`)
  }
  let next: WorkspaceLayoutState
  switch (action) {
    case 'pin':
      if (options.length !== 0) throw new CliUiFailure('INVALID_REQUEST', 'Pin takes one contribution ID.')
      next = { ...current, pinnedIds: unique([...current.pinnedIds, id]), hiddenIds: current.hiddenIds.filter((item) => item !== id) }
      break
    case 'unpin':
      if (options.length !== 0) throw new CliUiFailure('INVALID_REQUEST', 'Unpin takes one contribution ID.')
      next = { ...current, pinnedIds: current.pinnedIds.filter((item) => item !== id) }
      break
    case 'hide':
      if (options.length !== 0) throw new CliUiFailure('INVALID_REQUEST', 'Hide takes one contribution ID.')
      next = { ...current, hiddenIds: unique([...current.hiddenIds, id]), pinnedIds: current.pinnedIds.filter((item) => item !== id) }
      break
    case 'show':
      if (options.length !== 0) throw new CliUiFailure('INVALID_REQUEST', 'Show takes one contribution ID.')
      next = { ...current, hiddenIds: current.hiddenIds.filter((item) => item !== id) }
      break
    case 'move': {
      const beforeIndex = options.indexOf('--before')
      const before = options[beforeIndex + 1]
      if (beforeIndex !== 0 || options.length !== 2 || !before || !contributionIds.includes(before)) {
        throw new CliUiFailure('INVALID_REQUEST', '`move` requires --before <registered-contribution>.')
      }
      const ids = current.pinnedIds.includes(id) ? [...current.orderIds] : [...current.activityOrderIds]
      const ordered = ids.filter((item) => item !== id)
      const index = ordered.indexOf(before)
      ordered.splice(index < 0 ? ordered.length : index, 0, id)
      next = current.pinnedIds.includes(id)
        ? { ...current, orderIds: ordered }
        : { ...current, activityOrderIds: ordered }
      break
    }
    default:
      throw new CliUiFailure('INVALID_REQUEST', 'Expected get, reset, pin, unpin, hide, show, or move.')
  }
  writeWorkspaceLayout(next)
  return readWorkspaceLayout()
}

function parseThemeOptions(args: string[], current: ThemeSettings): ThemeSettings {
  let mode = current.mode
  let opacity = current.background_opacity
  let background = current.background ?? null
  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index]
    const value = args[index + 1]
    if (flag === '--mode' && value && ['system', 'light', 'dark', 'custom'].includes(value)) {
      if (args.indexOf('--mode') !== index) throw new CliUiFailure('INVALID_REQUEST', '--mode may be specified once.')
      mode = value as ThemeSettings['mode']
      index += 1
    } else if (flag === '--opacity' && value && /^\d{1,3}$/.test(value)) {
      if (args.indexOf('--opacity') !== index) throw new CliUiFailure('INVALID_REQUEST', '--opacity may be specified once.')
      opacity = Number(value)
      if (opacity > 100) throw new CliUiFailure('INVALID_REQUEST', '--opacity must be between 0 and 100.')
      index += 1
    } else if (flag === '--background' && value) {
      if (args.indexOf('--background') !== index) throw new CliUiFailure('INVALID_REQUEST', '--background may be specified once.')
      background = { kind: 'image', id: value }
      index += 1
    } else {
      throw new CliUiFailure('INVALID_REQUEST', `Unknown or incomplete theme option '${flag}'.`)
    }
  }
  return { mode, background, background_opacity: opacity }
}

function updateLayout(change: (current: WorkspaceLayoutState) => WorkspaceLayoutState) {
  writeWorkspaceLayout(change(readWorkspaceLayout()))
}

function emptyLayout(): WorkspaceLayoutState {
  return {
    version: 1,
    collapsed: false,
    pinnedIds: [],
    hiddenIds: [],
    orderIds: [],
    activityOrderIds: [],
    openContributionIds: [],
    activeContributionId: null,
    activeSidebarContributionId: null,
  }
}

function unique(values: string[]) {
  return [...new Set(values)]
}

function nextFrame() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
}

function errorText(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause)
}
