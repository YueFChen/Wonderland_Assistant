import { readFileSync, existsSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const files = process.argv.slice(2)
const currentDocuments = files.length ? files : [
  'README.md',
  'AGENTS.md',
  'RELEASING.md',
  'docs/CORE-CLI-COMMAND-INVENTORY.md',
  'docs/WEB-ACCESS.md',
  'packages/core-updates/README.md',
  'packages/plugin-tools/README.md',
  'plugins/README.md',
  'tests/README.md',
  'trash/README.md',
  'tests/fixtures/direct-plugin/README.md',
]
const missing = []
let checked = 0
for (const file of currentDocuments) {
  const source = path.resolve(root, file)
  const markdown = readFileSync(source, 'utf8').replace(/```[\s\S]*?```/g, '')
  const links = markdown.matchAll(/\[[^\]]*\]\((?:<([^>]+)>|([^\s)]+))(?:\s+[^)]*)?\)/g)
  for (const match of links) {
    const href = match[1] ?? match[2]
    if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith('#')) continue
    const relative = decodeURIComponent(href.split('#')[0].split('?')[0])
    if (!relative) continue
    checked += 1
    if (!existsSync(path.resolve(path.dirname(source), relative))) missing.push(`${file}: ${href}`)
  }
}
if (missing.length) {
  throw new Error(`Missing local documentation links:\n${missing.join('\n')}`)
}
console.log(`Checked ${checked} local file links in ${currentDocuments.length} current Core documents.`)
