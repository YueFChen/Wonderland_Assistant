// Run against a debug desktop already started with an isolated WONDERLAND_DEV_DATA_DIR.
import assert from 'node:assert/strict'
import {spawn} from 'node:child_process'
import {createServer} from 'node:http'
import {copyFile, mkdir, writeFile} from 'node:fs/promises'
import path from 'node:path'
import {fileURLToPath} from 'node:url'
import {finishPackage} from '../packages/plugin-tools/pack.mjs'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
const option = name => args[args.indexOf(name) + 1]
const dataDir = args.includes('--data-dir') ? path.resolve(option('--data-dir')) : ''
const cli = args.includes('--cli') ? path.resolve(option('--cli')) : path.join(root, 'target/debug/wla.exe')
if (!dataDir || !['target', 'artifacts/reports'].some(folder => {
  const relative = path.relative(path.join(root, folder), dataDir)
  return relative && !relative.startsWith('..') && !path.isAbsolute(relative)
})) throw new Error('Choose an isolated --data-dir below this checkout target/ or artifacts/reports/.')

async function call(...command) {
  const child = spawn(cli, ['--data-dir', dataDir, ...command], {windowsHide: true})
  const timeout = setTimeout(() => child.kill(), 50_000)
  let stdout = '', stderr = ''
  child.stdout.setEncoding('utf8').on('data', text => {stdout += text})
  child.stderr.setEncoding('utf8').on('data', text => {stderr += text})
  try {
    const code = await new Promise((resolve, reject) => {child.once('error', reject); child.once('close', resolve)})
    if (code !== 0) throw new Error(stderr || `CLI exit ${code}`)
    const result = JSON.parse(stdout)
    assert.equal(result.ok, true, stdout)
    return result.data
  } finally {clearTimeout(timeout)}
}

const status = await call('app', 'status')
assert.equal(status.desktopRunning, true, 'Start the isolated debug desktop before this check.')
const plugins = await call('plugins', 'list')
assert.ok(!plugins.plugins.some(plugin => plugin.manifest.id === 'a'), 'Fixture ID a must be unused.')
const stage = path.join(root, 'target/browser-session-probe/stage')
await mkdir(path.join(stage, 'backend'), {recursive: true})
await copyFile(path.join(root, 'target/debug/wonderland-direct-plugin-fixture.exe'), path.join(stage, 'backend/probe.exe'))
await copyFile(path.join(root, 'tests/fixtures/direct-plugin/contract.json'), path.join(stage, 'contract.json'))
await writeFile(path.join(stage, 'manifest.json'), JSON.stringify({
  manifestVersion: 3, id: 'a', name: 'Browser session probe', version: '0.0.0', author: 'Tests',
  hostCompatibility: {minCoreVersion: '0.2.0', maxCoreVersionExclusive: '0.3.0', protocol: {minVersion: '1.0.0', maxVersionExclusive: '2.0.0'}},
  platform: {os: 'windows', architecture: 'x86_64', abi: 'msvc'}, contract: 'contract.json',
  backend: {entry: 'backend/probe.exe', transport: 'stdio-ndjson-v1', supportsServiceContext: true, activation: 'on_demand'},
}))
const archive = path.join(root, 'target/browser-session-probe/a-0.0.0.wplug')
await finishPackage({root, stage, archivePath: archive, signing: 'none'})

const receipts = new Set()
const page = script => `<!doctype html><title>Browser session regression</title><script>${script}</script>`
const pages = {
  '/': page(`addEventListener('message', event => {
    if (event.origin !== location.origin) return;
    fetch('/receipt/' + event.data);
    if (event.data === 'direct') {
      const child = window.open('about:blank', 'blank-probe');
      if (child) child.location.href = '/blank';
    }
  }); window.open('/direct', 'direct-probe');`),
  '/direct': page(`document.cookie = 'popup_direct=1; path=/';
    if (window.opener) window.opener.postMessage('direct', location.origin);`),
  '/blank': page(`document.cookie = 'popup_blank=1; path=/';
    if (window.opener) window.opener.postMessage('blank', location.origin);
    window.open('/nested', 'nested-probe');`),
  '/nested': page(`document.cookie = 'popup_nested=1; path=/';
    if (window.opener && window.opener.opener) window.opener.opener.postMessage('nested', location.origin);`),
}
const server = createServer((request, response) => {
  if (request.url.startsWith('/receipt/')) receipts.add(request.url.slice('/receipt/'.length))
  response.writeHead(200, {'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store'})
  response.end(pages[request.url] ?? 'ok')
})
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
let installed = false
let receipt
try {
  await call('plugins', 'install', archive, '--yes')
  installed = true
  const result = await call('plugins', 'call', 'a', 'browser_session_probe', '--input-json', JSON.stringify({url: `http://127.0.0.1:${server.address().port}/`}), '--yes')
  assert.equal(result.result.sharedCookieCount, 3)
  const deadline = Date.now() + 2000
  while (receipts.size < 3 && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20))
  assert.deepEqual([...receipts].sort(), ['blank', 'direct', 'nested'])
  receipt = {ok: true, sharedCookies: 3, openerCallbacks: [...receipts].sort(), sessionClosed: true}
} finally {
  try {
    // WebView2 may retain profile locks until the desktop exits. Preserve this isolated fixture
    // directory rather than reporting a successful popup check as a data-removal failure.
    if (installed) await call('plugins', 'uninstall', 'a', '--yes')
  } finally {
    server.closeAllConnections()
    await new Promise(resolve => server.close(resolve))
  }
}
console.log(JSON.stringify(receipt))
