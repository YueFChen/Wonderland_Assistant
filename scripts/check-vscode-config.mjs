import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')

function parseJsonc(relativePath) {
  const source = readFileSync(resolve(root, relativePath), 'utf8')
  let output = ''
  let inString = false
  let escaped = false
  let lineComment = false
  let blockComment = false

  for (let index = 0; index < source.length; index += 1) {
    const char = source[index]
    const next = source[index + 1]

    if (lineComment) {
      if (char === '\n') {
        lineComment = false
        output += char
      } else {
        output += ' '
      }
      continue
    }
    if (blockComment) {
      if (char === '*' && next === '/') {
        output += '  '
        index += 1
        blockComment = false
      } else {
        output += char === '\n' ? '\n' : ' '
      }
      continue
    }
    if (inString) {
      output += char
      if (escaped) escaped = false
      else if (char === '\\') escaped = true
      else if (char === '"') inString = false
      continue
    }
    if (char === '"') {
      inString = true
      output += char
    } else if (char === '/' && next === '/') {
      lineComment = true
      output += '  '
      index += 1
    } else if (char === '/' && next === '*') {
      blockComment = true
      output += '  '
      index += 1
    } else {
      output += char
    }
  }

  if (blockComment) {
    throw new Error(`${relativePath} contains an unterminated block comment`)
  }

  // JSONC permits trailing commas; strip them after comments while preserving quoted values.
  let normalized = ''
  inString = false
  escaped = false
  for (let index = 0; index < output.length; index += 1) {
    const char = output[index]
    if (inString) {
      normalized += char
      if (escaped) escaped = false
      else if (char === '\\') escaped = true
      else if (char === '"') inString = false
      continue
    }
    if (char === '"') {
      inString = true
      normalized += char
      continue
    }
    if (char === ',') {
      let next = index + 1
      while (/\s/.test(output[next] ?? '')) next += 1
      if (output[next] === '}' || output[next] === ']') continue
    }
    normalized += char
  }

  try {
    return JSON.parse(normalized)
  } catch (error) {
    throw new Error(`${relativePath} is not valid JSONC: ${error.message}`)
  }
}

const tasksConfig = parseJsonc('.vscode/tasks.json')
const launchConfig = parseJsonc('.vscode/launch.json')
if (!Array.isArray(tasksConfig.tasks)) {
  throw new Error('.vscode/tasks.json must define a tasks array')
}
if (!Array.isArray(launchConfig.configurations)) {
  throw new Error('.vscode/launch.json must define a configurations array')
}
const tasks = tasksConfig.tasks ?? []
const labels = new Set()

for (const task of tasks) {
  if (!task.label) throw new Error('Every VS Code task must have a label')
  if (labels.has(task.label)) throw new Error(`Duplicate VS Code task label: ${task.label}`)
  labels.add(task.label)
}

for (const task of tasks) {
  const dependencies = Array.isArray(task.dependsOn) ? task.dependsOn : [task.dependsOn]
  for (const dependency of dependencies.filter(Boolean)) {
    const label = typeof dependency === 'string' ? dependency : dependency.task
    if (label && !labels.has(label)) {
      throw new Error(`Task “${task.label}” depends on unknown task “${label}”`)
    }
  }
}

const visiting = new Set()
const visited = new Set()
function visit(label) {
  if (visiting.has(label)) throw new Error(`Task dependency cycle includes “${label}”`)
  if (visited.has(label)) return
  visiting.add(label)
  const task = tasks.find((candidate) => candidate.label === label)
  const dependencies = Array.isArray(task.dependsOn) ? task.dependsOn : [task.dependsOn]
  for (const dependency of dependencies.filter(Boolean)) {
    const child = typeof dependency === 'string' ? dependency : dependency.task
    if (child) visit(child)
  }
  visiting.delete(label)
  visited.add(label)
}
for (const label of labels) visit(label)

const configurationNames = new Set()
for (const configuration of launchConfig.configurations ?? []) {
  if (!configuration.name || configurationNames.has(configuration.name)) {
    throw new Error(`Missing or duplicate launch configuration name: ${configuration.name ?? '(missing)'}`)
  }
  configurationNames.add(configuration.name)
  if (!configuration.type || !configuration.request) {
    throw new Error(`Launch configuration “${configuration.name}” needs type and request`)
  }
  if (configuration.type === 'node-terminal') {
    if (typeof configuration.command !== 'string' || !configuration.command.trim()) {
      throw new Error(`Terminal launch configuration “${configuration.name}” needs a command string`)
    }
    if (configuration.args !== undefined) {
      throw new Error(`Terminal launch configuration “${configuration.name}” must put arguments in command, not args`)
    }
  }
  if (configuration.preLaunchTask && !labels.has(configuration.preLaunchTask)) {
    throw new Error(`Launch configuration “${configuration.name}” references unknown preLaunchTask “${configuration.preLaunchTask}”`)
  }
}

console.log(`VS Code config valid: ${tasks.length} tasks, ${configurationNames.size} launch configurations.`)
