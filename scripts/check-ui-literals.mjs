/** Validate that user-facing UI text is stored in translation resources. */
import { readdir, readFile, stat } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'

const ROOT = path.resolve(import.meta.dirname, '..')

/** Default UI source roots relative to the repository root. */
const DEFAULT_TARGETS = ['apps/desktop/ui/src', 'packages/ui/src', 'plugins']

const HAN = /[\p{Script=Han}]/u
const ALLOW_MARKER = 'i18n-allow'

/** Check whether a file is a translation resource. */
function isWhitelisted(file) {
  const relative = path.relative(ROOT, file)
  if (relative.split(path.sep).includes('i18n')) return true
  const base = path.basename(file)
  if (base.endsWith('.d.ts')) return true
  if (/\.test\.tsx?$/.test(base)) return true
  return false
}

function isCandidate(file) {
  return /\.tsx?$/.test(file) && !file.includes(`${path.sep}node_modules${path.sep}`)
}

/** Collect UI source files recursively. */
async function collect(directory) {
  const entries = await readdir(directory, { withFileTypes: true })
  const files = []
  for (const entry of entries) {
    if (entry.name === 'node_modules' || entry.name === 'dist') continue
    const child = path.join(directory, entry.name)
    if (entry.isDirectory()) files.push(...(await collect(child)))
    else if (isCandidate(child) && !isWhitelisted(child)) files.push(child)
  }
  return files
}

/** Remove comments while preserving strings and source positions. */
function stripComments(source) {
  let out = ''
  let mode = 'code'
  /** Template expression nesting state. */
  const templates = []
  /** Previous significant code character. */
  let previous = ''
  let index = 0

  const inExpression = () => templates.length > 0 && templates[templates.length - 1].expression

  while (index < source.length) {
    const char = source[index]
    const next = source[index + 1]

    if (mode === 'line') {
      if (char === '\n') {
        mode = 'code'
        out += char
      } else out += ' '
      index += 1
      continue
    }

    if (mode === 'block') {
      if (char === '*' && next === '/') {
        mode = 'code'
        out += '  '
        index += 2
        continue
      }
      out += char === '\n' ? '\n' : ' '
      index += 1
      continue
    }

    if (mode === 'template') {
      if (char === '\\') {
        out += char + (next ?? '')
        index += 2
        continue
      }
      if (char === '`') {
        templates.pop()
        mode = 'code'
        out += char
        index += 1
        continue
      }
      if (char === '$' && next === '{') {
        templates[templates.length - 1].expression = true
        templates[templates.length - 1].braces = 0
        mode = 'code'
        out += '${'
        index += 2
        continue
      }
      out += char
      index += 1
      continue
    }

    // mode === 'code'
    if (char === '/' && next === '/') {
      mode = 'line'
      out += '  '
      index += 2
      continue
    }
    if (char === '/' && next === '*') {
      mode = 'block'
      out += '  '
      index += 2
      continue
    }
    if (char === '/' && canStartRegex(previous)) {
      const skipped = skipRegex(source, index)
      if (skipped > 0) {
        out += source.slice(index, index + skipped)
        index += skipped
        previous = ')'
        continue
      }
    }
    if (char === "'" || char === '"') {
      out += char
      index += 1
      while (index < source.length) {
        const inner = source[index]
        if (inner === '\\') {
          out += inner + (source[index + 1] ?? '')
          index += 2
          continue
        }
        out += inner
        index += 1
        if (inner === char) break
      }
      previous = ')'
      continue
    }
    if (char === '`') {
      templates.push({ expression: false, braces: 0 })
      mode = 'template'
      out += char
      index += 1
      previous = ')'
      continue
    }
    if (inExpression()) {
      const top = templates[templates.length - 1]
      if (char === '{') top.braces += 1
      else if (char === '}') {
        if (top.braces === 0) {
          top.expression = false
          mode = 'template'
          out += char
          index += 1
          continue
        }
        top.braces -= 1
      }
    }
    out += char
    index += 1
    if (!/\s/.test(char)) previous = char
  }

  return out
}

/** Determine whether a slash can begin a regular expression. */
function canStartRegex(previous) {
  if (previous === '') return true
  return '=([{,:;!&|?+-*%~^<>'.includes(previous)
}

/** Skip a regular expression literal and return the consumed length. */
function skipRegex(source, start) {
  let index = start + 1
  let inClass = false
  while (index < source.length) {
    const char = source[index]
    if (char === '\n') return 0
    if (char === '\\') {
      index += 2
      continue
    }
    if (char === '[') inClass = true
    else if (char === ']') inClass = false
    else if (char === '/' && !inClass) {
      index += 1
      while (index < source.length && /[a-z]/i.test(source[index])) index += 1
      return index - start
    }
    index += 1
  }
  return 0
}

/** Find Han characters outside comments. */
function findViolations(file, source) {
  const stripped = stripComments(source)
  const combined = source.split('\n')
  const cleaned = stripped.split('\n')
  const violations = []
  for (let line = 0; line < cleaned.length; line += 1) {
    if (!HAN.test(cleaned[line])) continue
    if ((combined[line] ?? '').includes(ALLOW_MARKER)) continue
    violations.push({ line: line + 1, text: (combined[line] ?? '').trim() })
  }
  return violations
}

const targets = process.argv.slice(2)
const roots = targets.length > 0 ? targets : DEFAULT_TARGETS

const files = new Set()
for (const root of roots) {
  const absolute = path.resolve(ROOT, root)
  let info
  try {
    info = await stat(absolute)
  } catch {
    // 路径写错时宁可报错也不静默通过：假成功比漏报更危险。
    console.error(`目标不存在：${root}`)
    process.exit(2)
  }
  if (info.isDirectory()) {
    for (const file of await collect(absolute)) files.add(file)
    continue
  }
  // Apply the same exclusions to explicitly supplied paths.
  if (isCandidate(absolute) && !isWhitelisted(absolute)) files.add(absolute)
}

let total = 0
for (const file of [...files].sort()) {
  const source = await readFile(file, 'utf8')
  const violations = findViolations(file, source)
  if (violations.length === 0) continue
  total += violations.length
  const relative = path.relative(ROOT, file).split(path.sep).join('/')
  for (const violation of violations) {
    console.log(`${relative}:${violation.line}: ${violation.text}`)
  }
}

if (total > 0) {
  console.error(
    `\n界面源码中有 ${total} 处中文（请将界面文案放入 i18n 资源文件）。`,
  )
  process.exit(1)
}
console.log('界面源码中未发现中文字面量。')
