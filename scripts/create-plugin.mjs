import { cp, lstat, mkdir, readFile, rename, rm } from 'node:fs/promises'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

const coreRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const pluginsRoot = path.join(coreRoot, 'plugins')
const defaultTemplateRepository = 'git@github.com:YueFChen/template_plugin.git'
const options = parseArgs(process.argv.slice(2))
const id = options.id ?? ''
const name = options.name?.trim() ?? ''
const author = options.author?.trim() ?? ''
const version = options.version?.trim() ?? '0.1.0'
const description = options.description?.trim() ?? ''
const semanticVersion = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/

if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id)) {
  throw new Error('--id must be a lowercase plugin ID with letters, numbers, underscores, or hyphens.')
}
if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i.test(id)) {
  throw new Error('--id cannot be a reserved Windows device name.')
}
if (!name || name.length > 120) throw new Error('--name is required and must be at most 120 characters.')
if (!author || author.length > 120) throw new Error('--author is required and must be at most 120 characters.')
if (!semanticVersion.test(version)) throw new Error('--version must be a semantic version such as 0.1.0.')
if (description.length > 500) throw new Error('--description must be at most 500 characters.')
if ([name, author, description].some((value) => /[\u0000-\u001f\u007f]/.test(value))) {
  throw new Error('Name, author, and description cannot contain control characters.')
}

const destination = path.join(pluginsRoot, id)
const staging = path.join(pluginsRoot, `.create-${id}-${process.pid}`)
assertDirectChild(destination, pluginsRoot)
assertDirectChild(staging, pluginsRoot)
if (await exists(destination)) throw new Error(`Plugin destination already exists: ${destination}`)
if (await exists(staging)) throw new Error(`Temporary creation directory already exists: ${staging}`)

let sourceClone
let stagingCreated = false
let destinationCreated = false
try {
  const template = await resolveTemplate(options.template ?? process.env.WONDERLAND_PLUGIN_TEMPLATE_REPOSITORY)
  if (template.temporary) sourceClone = template.path
  await validateTemplate(template.path)

  await mkdir(pluginsRoot, { recursive: true })
  stagingCreated = true
  await cp(template.path, staging, {
    recursive: true,
    filter: (sourcePath) => shouldCopy(template.path, sourcePath),
  })

  const initArgs = [
    'scripts/init-plugin.mjs',
    '--id', id,
    '--name', name,
    '--author', author,
    '--version', version,
  ]
  if (description) initArgs.push('--description', description)
  run(process.execPath, initArgs, staging)
  runPnpm(['run', 'validate'], staging)
  run('git', ['init', '--initial-branch=main'], staging)

  await rename(staging, destination)
  stagingCreated = false
  destinationCreated = true
  runPnpm(['install', '--frozen-lockfile'], destination)
  runPnpm(['run', 'validate'], destination)
  destinationCreated = false
  console.log(`Created independent plugin repository: ${path.relative(coreRoot, destination)}`)
  console.log('Next: pnpm --dir plugins/' + id + ' run debug:ui')
} finally {
  if (sourceClone) await removeOwnedTemporary(sourceClone, '.template-source-')
  if (stagingCreated) await removeOwnedTemporary(staging, `.create-${id}-`)
  if (destinationCreated) {
    assertDirectChild(destination, pluginsRoot)
    await rm(destination, { recursive: true, force: true })
  }
}

async function resolveTemplate(specifier) {
  if (!specifier) {
    const localTemplate = path.join(pluginsRoot, 'template_plugin')
    if (await exists(localTemplate)) return { path: localTemplate, temporary: false }
    specifier = defaultTemplateRepository
  }

  if (!isGitUrl(specifier)) {
    const localPath = path.resolve(coreRoot, specifier)
    if (!(await exists(localPath))) throw new Error(`Template path does not exist: ${localPath}`)
    return { path: localPath, temporary: false }
  }

  const clonePath = path.join(pluginsRoot, `.template-source-${process.pid}`)
  assertDirectChild(clonePath, pluginsRoot)
  if (await exists(clonePath)) throw new Error(`Temporary template clone already exists: ${clonePath}`)
  await mkdir(pluginsRoot, { recursive: true })
  try {
    run('git', ['clone', '--depth', '1', specifier, clonePath], coreRoot)
  } catch (error) {
    if (await exists(clonePath)) await removeOwnedTemporary(clonePath, '.template-source-')
    throw error
  }
  return { path: clonePath, temporary: true }
}

async function validateTemplate(templateRoot) {
  for (const relative of [
    'README.md',
    'LICENSE',
    'NOTICE.md',
    'rust-toolchain.toml',
    'pnpm-workspace.yaml',
    'pnpm-lock.yaml',
    'package/manifest.json',
    'package/contract.json',
    'Cargo.toml',
    'Cargo.lock',
    'package.json',
    'ui/package.json',
    'ui/index.html',
    'ui/src/main.tsx',
    'scripts/init-plugin.mjs',
    'scripts/debug-plugin.mjs',
    'scripts/build-plugin.mjs',
    'scripts/validate-template.mjs',
    '.github/workflows/ci.yml',
  ]) {
    if (!(await exists(path.join(templateRoot, relative)))) {
      throw new Error(`Template repository is incomplete; missing ${relative}`)
    }
  }
  const manifest = JSON.parse(await readFile(path.join(templateRoot, 'package/manifest.json'), 'utf8'))
  if (!manifest.id || !manifest.version) throw new Error('Template manifest has no plugin identity.')
}

function shouldCopy(sourceRoot, sourcePath) {
  const relative = path.relative(sourceRoot, sourcePath)
  if (!relative) return true
  const segments = relative.split(path.sep)
  const fileName = segments.at(-1)
  if (segments.some((segment) => [
    '.git', 'node_modules', 'target', 'dist', '.pnpm-store', '.cache', '.turbo',
  ].includes(segment))) return false
  if (['.npmrc', '.netrc', '.pypirc', '.git-credentials'].includes(fileName)) return false
  if (fileName === '.env' || (fileName.startsWith('.env.') && fileName !== '.env.example')) return false
  if (segments.at(-2) === '.cargo' && fileName === 'credentials.toml') return false
  return true
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit' })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`${command} failed with exit code ${result.status ?? 'unknown'}.`)
}

function runPnpm(args, cwd) {
  if (process.platform !== 'win32') return run('pnpm', args, cwd)
  const command = process.env.ComSpec ?? 'cmd.exe'
  const commandText = ['pnpm.cmd', ...args].join(' ')
  run(command, ['/d', '/s', '/c', commandText], cwd)
}

function parseArgs(args) {
  const parsed = {}
  if (args[0] === '--') args = args.slice(1)
  for (let index = 0; index < args.length; index += 1) {
    const key = args[index]
    if (!['--id', '--name', '--author', '--version', '--description', '--template'].includes(key)) {
      throw new Error(`Unknown option: ${key}`)
    }
    const value = args[index + 1]
    if (!value || value.startsWith('--')) throw new Error(`${key} requires a value.`)
    parsed[key.slice(2)] = value
    index += 1
  }
  return parsed
}

function isGitUrl(value) {
  return /^https?:\/\//i.test(value) || /^git@[^:]+:.+\.git$/i.test(value) || /^ssh:\/\//i.test(value)
}

function assertDirectChild(candidate, parent) {
  const relative = path.relative(parent, candidate)
  if (!relative || path.isAbsolute(relative) || relative.startsWith('..') || path.dirname(relative) !== '.') {
    throw new Error(`Refusing to use a temporary or destination path outside ${parent}.`)
  }
}

async function removeOwnedTemporary(candidate, expectedPrefix) {
  assertDirectChild(candidate, pluginsRoot)
  if (!path.basename(candidate).startsWith(expectedPrefix)) {
    throw new Error(`Refusing to clean up an unrecognized temporary path: ${candidate}`)
  }
  await rm(candidate, { recursive: true, force: true })
}

async function exists(target) {
  try {
    await lstat(target)
    return true
  } catch {
    return false
  }
}
