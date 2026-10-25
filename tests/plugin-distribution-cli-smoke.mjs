/**
 * Isolated, local-only integration drill for the compiled distribution CLI.
 * Usage: node tests/plugin-distribution-cli-smoke.mjs [--cli PATH] [--packages PATH]
 * Build the debug wla and maintained plugin packages first. No cloud credentials,
 * real desktop IPC, user profiles, or production endpoints are used.
 * Fixtures and a sanitized report are retained under target for inspection.
 */
import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, readdir, stat, unlink, writeFile } from 'node:fs/promises';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { deflateRawSync } from 'node:zlib';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const flags = new Map();
for (let i = 2; i < process.argv.length; i += 2) {
  assert(['--cli', '--packages'].includes(process.argv[i]) && process.argv[i + 1], 'Invalid drill arguments.');
  flags.set(process.argv[i], process.argv[i + 1]);
}
const cli = path.resolve(flags.get('--cli') ?? path.join(repo, 'target/debug/wla.exe'));
const packages = path.resolve(flags.get('--packages') ?? path.join(repo, 'target/w18-maintained-plugins'));
const drillRoot = path.join(repo, `target/plugin-distribution-cli-smoke-${randomUUID()}`);
const processTemp = path.join(drillRoot, 'process-temp');
await mkdir(processTemp, { recursive: true });
await stat(cli);
const checked = [];
const invocations = [];
const fixtureCode = `synthetic-${randomUUID()}`;
const replacementCode = `synthetic-${randomUUID()}`;
const settingsName = 'plugin-distribution.json';
const privateCacheName = 'plugin-distribution-restricted-v1.json';
const publicId = 'wa_bookmarks';
const privateId = 'template_plugin';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const write = async (file, contents) => {
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, contents);
};
const jsonFile = async file => JSON.parse(await readFile(file, 'utf8'));
const exists = async file => stat(file).then(() => true, () => false);
const check = (name, assertions) => { assertions(); checked.push(name); };
const assertNoCode = text => {
  assert(!text.includes(fixtureCode) && !text.includes(replacementCode), 'Access-code value appeared in an observable result.');
};

async function invoke(profile, args, { input = '', origin, expectedCode, raw = false } = {}) {
  const environment = { ...process.env, TEMP: processTemp, TMP: processTemp };
  // An inherited developer injection must not alter the default-state tests.
  delete environment.WONDERLAND_DISTRIBUTION_TEST_ORIGIN;
  if (origin) environment.WONDERLAND_DISTRIBUTION_TEST_ORIGIN = origin;
  const arguments_ = profile ? ['--data-dir', profile, ...args] : args;
  assertNoCode(arguments_.join('\n'));
  const start = Date.now();
  const child = spawn(cli, arguments_, { env: environment, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.setEncoding('utf8').on('data', chunk => stdout += chunk);
  child.stderr.setEncoding('utf8').on('data', chunk => stderr += chunk);
  child.stdin.on('error', () => {});
  child.stdin.end(input);
  const timer = setTimeout(() => child.kill(), 195_000);
  const [exitCode] = await once(child, 'close');
  clearTimeout(timer);
  assertNoCode(stdout + stderr);
  let result;
  if (raw) {
    assert.equal(exitCode, 0, stderr);
    result = stdout.trim();
  } else {
    try { result = JSON.parse(exitCode === 0 ? stdout : stderr); }
    catch { throw new Error(`CLI output was not JSON (${exitCode}): ${stdout} ${stderr}`); }
    if (expectedCode) {
      assert.notEqual(exitCode, 0, 'Expected command to fail.');
      assert.equal(result.error?.code, expectedCode, JSON.stringify(result));
    } else {
      assert.equal(exitCode, 0, JSON.stringify(result));
      assert.equal(result.ok, true);
    }
  }
  invocations.push({ args, elapsedMs: Date.now() - start, exitCode, resultCode: result?.error?.code ?? null });
  return raw ? result : result.data ?? result;
}

// No external ZIP library or release-signing key is required for these fixtures.
const crcTable = Array.from({ length: 256 }, (_, i) => {
  let value = i;
  for (let bit = 0; bit < 8; bit++) value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  return value >>> 0;
});
function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = crcTable[(crc ^ byte) & 255] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}
function zip(files) {
  const locals = [], central = [];
  let offset = 0;
  for (const [relative, bytes] of [...files.entries()].sort(([a], [b]) => a.localeCompare(b))) {
    const filename = Buffer.from(relative), compressed = deflateRawSync(bytes), crc = crc32(bytes);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x800, 6);
    local.writeUInt16LE(8, 8); local.writeUInt16LE(0x21, 12); local.writeUInt32LE(crc, 14); local.writeUInt32LE(compressed.length, 18);
    local.writeUInt32LE(bytes.length, 22); local.writeUInt16LE(filename.length, 26);
    locals.push(local, filename, compressed);
    const entry = Buffer.alloc(46);
    entry.writeUInt32LE(0x02014b50, 0); entry.writeUInt16LE(20, 4); entry.writeUInt16LE(20, 6);
    entry.writeUInt16LE(0x800, 8); entry.writeUInt16LE(8, 10); entry.writeUInt16LE(0x21, 14); entry.writeUInt32LE(crc, 16);
    entry.writeUInt32LE(compressed.length, 20); entry.writeUInt32LE(bytes.length, 24);
    entry.writeUInt16LE(filename.length, 28); entry.writeUInt32LE(offset, 42);
    central.push(entry, filename);
    offset += local.length + filename.length + compressed.length;
  }
  const directory = Buffer.concat(central), end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(files.size, 8); end.writeUInt16LE(files.size, 10);
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, directory, end]);
}
async function packageFixture(id, version) {
  const files = new Map(), source = path.join(packages, id);
  async function visit(directory, prefix = '') {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const relative = prefix + entry.name;
      if (entry.isDirectory()) await visit(path.join(directory, entry.name), relative + '/');
      else if (entry.isFile() && relative !== 'checksums.json') files.set(relative, await readFile(path.join(directory, entry.name)));
      else if (!entry.isFile()) throw new Error('Package fixture contains a non-regular file.');
    }
  }
  await visit(source);
  const manifest = JSON.parse(files.get('manifest.json'));
  assert.equal(manifest.id, id);
  manifest.version = version;
  manifest.hostCompatibility.minCoreVersion = '0.2.0';
  manifest.hostCompatibility.maxCoreVersionExclusive = '0.3.0';
  files.set('manifest.json', Buffer.from(JSON.stringify(manifest, null, 2)));
  files.set('checksums.json', Buffer.from(JSON.stringify({ algorithm: 'sha256', files: [...files].map(([relative, bytes]) => ({ path: relative, sha256: sha(bytes) })) }, null, 2)));
  const directory = path.join(drillRoot, 'packages', `${id}-${version}`);
  for (const [relative, bytes] of files) await write(path.join(directory, relative), bytes);
  const bytes = zip(files);
  await write(directory + '.wplug', bytes);
  return { directory, bytes, manifest, sha256: sha(bytes) };
}

const report = { cli, packagesPrerequisite: packages, drillRoot, checked, invocations, fixturePackages: [], limitation: 'Loopback protocol and CLI validation only; Cloudflare deployment, edge cache and production channel cutover are outside this drill. Maintained plugin package directories must already exist at packagesPrerequisite. Plugin fixtures are disabled, so backend startup is not covered here.' };
let server;
try {
  const expectedVersion = (await readFile(path.join(repo, 'Cargo.toml'), 'utf8')).match(/^version = "([^"]+)"/m)[1];
  assert.equal(await invoke(null, ['--version'], { raw: true }), `wla ${expectedVersion}`);
  report.cliSha256 = sha(await readFile(cli));
  const profile = path.join(drillRoot, 'configuration');
  let state = await invoke(profile, ['plugins', 'distribution', 'status']);
  check('new instance defaults to the official stable origin without a network request', () => {
    assert.equal(state.configured, true);
    assert.equal(state.sourceUrl, 'https://plugins-stable.yuef.me');
    assert.equal(state.credentialConfigured, false);
  });
  await invoke(profile, ['plugins', 'distribution', 'clear-source', '--yes']);
  state = await invoke(profile, ['plugins', 'distribution', 'status']);
  check('explicit source clear survives process restart with no legacy fallback', () => {
    assert.equal(state.configured, false);
    assert.equal(state.sourceUrl, null);
  });
  const unconfigured = await invoke(profile, ['plugins', 'catalog']);
  assert.equal(unconfigured.status, 'unconfigured'); assert.deepEqual(unconfigured.plugins, []);
  await invoke(profile, ['plugins', 'install-online', publicId, '--yes'], { expectedCode: 'DISTRIBUTION_NOT_CONFIGURED' });
  await invoke(profile, ['plugins', 'distribution', 'source', 'https://distribution.example.test', '--yes']);
  const settingsPath = path.join(profile, settingsName);
  const beforeBadInput = sha(await readFile(settingsPath));
  for (const input of ['', fixtureCode + '\nInjected: true', 'has whitespace', 'x'.repeat(4097), Buffer.from([0xff])]) {
    await invoke(profile, ['plugins', 'distribution', 'set-code', '--stdin', '--yes'], { input, expectedCode: 'INVALID_REQUEST' });
    assert.equal(sha(await readFile(settingsPath)), beforeBadInput);
  }
  await invoke(profile, ['plugins', 'distribution', 'set-code', 'forbidden-position-value', '--yes'], { expectedCode: 'INVALID_REQUEST' });
  await invoke(profile, ['plugins', 'distribution', 'set-code', '--stdin'], { input: fixtureCode, expectedCode: 'CONFIRMATION_REQUIRED' });
  await invoke(profile, ['plugins', 'distribution', 'source', 'http://distribution.example.test', '--yes'], { expectedCode: 'INVALID_DISTRIBUTION_SOURCE' });
  assert.equal(sha(await readFile(settingsPath)), beforeBadInput);
  checked.push('bounded stdin, EOF, control bytes, mutation confirmation and source validation preserve settings');
  state = await invoke(profile, ['plugins', 'distribution', 'set-code', '--stdin', '--yes'], { input: fixtureCode + '\r\n' });
  assert.equal(state.credentialState, 'unchecked'); assert.equal(state.credentialConfigured, true);
  assertNoCode(await readFile(settingsPath, 'utf8'));
  assert((await jsonFile(settingsPath)).encryptedAccessCode);
  checked.push('CRLF stdin code is protected at rest and absent from safe views');
  state = await invoke(profile, ['plugins', 'distribution', 'source', 'https://new-distribution.example.test', '--yes']);
  assert.equal(state.credentialConfigured, false);
  await invoke(profile, ['plugins', 'distribution', 'set-code', '--stdin', '--yes'], { input: replacementCode });
  state = await invoke(profile, ['plugins', 'distribution', 'clear-code', '--yes']);
  assert.equal(state.credentialState, 'none');
  state = await invoke(profile, ['plugins', 'distribution', 'clear-source', '--yes']);
  assert.equal(state.configured, false);
  checked.push('source replacement, clear-code and clear-source reset credentials');

  // Each fake desktop endpoint is located in the child-only TEMP directory.
  async function ipcFixture(capable) {
    const ipcProfile = path.join(drillRoot, capable ? 'ipc-supported' : 'ipc-legacy');
    await mkdir(ipcProfile, { recursive: true });
    const endpointPath = path.join(processTemp, 'wonderland-assistant-cli-v1.json');
    let captured = null, receivedBytes = 0;
    const listener = net.createServer(socket => {
      let data = '';
      socket.on('data', bytes => { receivedBytes += bytes.length; data += bytes; });
      socket.on('end', () => {
        if (!data) return socket.end();
        captured = JSON.parse(data);
        socket.end(JSON.stringify({ protocolVersion: 1, requestId: captured.requestId, ok: true, data: { configured: true, credentialConfigured: true, credentialState: 'unchecked' } }) + '\n');
      });
    });
    listener.listen(0, '127.0.0.1'); await once(listener, 'listening');
    const endpoint = { protocolVersion: 1, port: listener.address().port, token: 'synthetic-ipc-token', dataDir: ipcProfile, processId: process.pid };
    if (capable) endpoint.sensitivePayloadVersion = 1;
    await write(endpointPath, JSON.stringify(endpoint));
    try {
      const result = await invoke(ipcProfile, ['plugins', 'distribution', 'set-code', '--stdin', '--yes'], { input: fixtureCode, ...(capable ? {} : { expectedCode: 'IPC_PROTOCOL_MISMATCH' }) });
      if (capable) {
        assert.equal(result.credentialConfigured, true);
        assert.deepEqual(captured.args, ['plugins', 'distribution', 'set-code', '--stdin', '--yes']);
        assert.equal(captured.sensitivePayload?.kind, 'distributionAccessCode');
        assert(captured.sensitivePayload.value === fixtureCode, 'Closed IPC payload did not match stdin fixture.');
      } else {
        assert.equal(receivedBytes, 0); assert.equal(captured, null);
        assert.deepEqual(await jsonFile(endpointPath), endpoint);
      }
      assert.equal(await exists(path.join(ipcProfile, settingsName)), false, 'IPC failure must not start headless mutation.');
    } finally {
      await unlink(endpointPath); await new Promise(resolve => listener.close(resolve));
    }
  }
  await ipcFixture(false); await ipcFixture(true);
  checked.push('legacy IPC receives no secret or fallback; capable IPC uses the closed sensitive payload');

  const fixtures = new Map();
  for (const [id, version] of [[publicId, '0.1.0'], [publicId, '0.1.1'], [privateId, '0.1.1']]) {
    const fixture = await packageFixture(id, version); fixtures.set(`${id}@${version}`, fixture);
    report.fixturePackages.push({ id, version, sha256: fixture.sha256, sizeBytes: fixture.bytes.length });
  }
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  const publicHex = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('hex');
  const network = { mode: 'ready', revoked: false, expired: false, scope: true, version: '0.1.0', badSignature: false, badPackage: false, denyPrivateUpdate: false, requests: [] };
  let origin;
  const authorized = request => [fixtureCode, replacementCode].includes(request.headers.authorization?.replace(/^Bearer /, '')) && !network.revoked && !network.expired;
  server = http.createServer((request, response) => {
    const pathname = request.url.split('?')[0], hasCredential = Boolean(request.headers.authorization);
    network.requests.push({ path: pathname, authenticated: hasCredential });
    response.setHeader('Cache-Control', 'no-store');
    const json = (status, value) => { response.writeHead(status, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(value)); };
    const failure = (status, code) => json(status, { error: { code } });
    if (network.mode === 'offline') return failure(503, 'DISTRIBUTION_UNAVAILABLE');
    if (network.mode === 'limited') { response.setHeader('Retry-After', '3'); return failure(429, 'RATE_LIMITED'); }
    if (hasCredential && !authorized(request)) return failure(401, network.revoked ? 'ACCESS_CODE_REVOKED' : network.expired ? 'ACCESS_CODE_EXPIRED' : 'ACCESS_CODE_INVALID');
    if (pathname === '/v1/access') return json(200, { schemaVersion: 1, state: authorized(request) ? 'valid' : 'none', expiresAt: null });
    const visible = [publicId, ...(authorized(request) && network.scope ? [privateId] : [])];
    if (pathname === '/v1/catalog') return json(200, { schemaVersion: 1, generatedAt: new Date().toISOString(), plugins: visible.map(id => {
      const fixture = fixtures.get(`${id}@${id === publicId ? network.version : '0.1.1'}`);
      return { id, name: fixture.manifest.name, description: fixture.manifest.description, author: 'local test', visibility: id === publicId ? 'public' : 'restricted', updateManifestUrl: `${origin}/v1/plugins/${id}/update`, signingPublicKey: publicHex };
    }) });
    const update = pathname.match(/^\/v1\/plugins\/([^/]+)\/update$/);
    if (update) {
      const id = update[1];
      if (!visible.includes(id) || (id === privateId && network.denyPrivateUpdate)) return failure(404, 'NOT_AVAILABLE');
      const version = id === publicId ? network.version : '0.1.1', fixture = fixtures.get(`${id}@${version}`);
      const payload = JSON.stringify({ schemaVersion: 1, id, version, releaseNotesUrl: null, downloadUrl: `${origin}/v1/plugins/${id}/versions/${version}/package`, sha256: fixture.sha256, sizeBytes: fixture.bytes.length, hostCompatibility: fixture.manifest.hostCompatibility, uiBridgeCompatibility: fixture.manifest.ui.bridgeCompatibility, platform: fixture.manifest.platform, dependencies: fixture.manifest.dependencies ?? [] });
      const signature = network.badSignature && id === publicId ? '0'.repeat(128) : sign(null, Buffer.from(payload), privateKey).toString('hex');
      return json(200, { schemaVersion: 1, payload, signature });
    }
    const download = pathname.match(/^\/v1\/plugins\/([^/]+)\/versions\/([^/]+)\/package$/);
    if (download) {
      const fixture = fixtures.get(`${download[1]}@${download[2]}`);
      if (!visible.includes(download[1]) || !fixture) return failure(404, 'NOT_AVAILABLE');
      const bytes = network.badPackage ? Buffer.from(fixture.bytes) : fixture.bytes;
      if (network.badPackage) bytes[bytes.length - 1] ^= 1;
      response.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': bytes.length });
      return response.end(bytes);
    }
    failure(404, 'NOT_AVAILABLE');
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  origin = `http://127.0.0.1:${server.address().port}`;
  const onlineProfile = path.join(drillRoot, 'online');
  await write(path.join(onlineProfile, 'plugin-manager.json'), JSON.stringify({ schemaVersion: 4, enabledPlugins: { [publicId]: false, [privateId]: false }, activeVersions: {}, installationSources: {} }));
  const online = (args, options = {}) => invoke(onlineProfile, args, { origin, ...options });
  const ids = snapshot => snapshot.plugins.map(item => item.registration.id).sort();
  let snapshot = await online(['plugins', 'catalog']);
  assert.equal(snapshot.status, 'ready'); assert.deepEqual(ids(snapshot), [publicId]);
  assert(snapshot.plugins[0].installable);
  await online(['plugins', 'distribution', 'set-code', '--stdin', '--yes'], { input: fixtureCode });
  snapshot = await online(['plugins', 'catalog']);
  assert.equal(snapshot.credentialState, 'valid'); assert.deepEqual(ids(snapshot), [privateId, publicId].sort());
  assert(await exists(path.join(onlineProfile, privateCacheName)));
  checked.push('anonymous public catalog and valid-code restricted catalog use signed manifests');
  network.mode = 'offline';
  snapshot = await online(['plugins', 'catalog']);
  assert.equal(snapshot.status, 'offline'); assert(snapshot.stale); assert.deepEqual(ids(snapshot), [privateId, publicId].sort());
  assert(snapshot.plugins.every(item => item.stale && !item.installable));
  await online(['plugins', 'install-online', privateId, '--yes'], { expectedCode: 'DISTRIBUTION_UNAVAILABLE' });
  network.mode = 'limited';
  snapshot = await online(['plugins', 'catalog']);
  assert.equal(snapshot.status, 'rate_limited'); assert.equal(snapshot.retryAfterSeconds, 3);
  assert(snapshot.plugins.every(item => !item.installable));
  const limitedInstall = await online(['plugins', 'install-online', privateId, '--yes'], { expectedCode: 'RATE_LIMITED' });
  assert.equal(limitedInstall.error.details.retryAfterSeconds, 3);
  checked.push('offline and 429 snapshots retain introduction only and never permit cached installation');
  network.mode = 'ready'; network.scope = false;
  snapshot = await online(['plugins', 'catalog']);
  assert.deepEqual(ids(snapshot), [publicId]);
  assert.equal(await exists(path.join(onlineProfile, privateCacheName)), false);
  network.scope = true;
  snapshot = await online(['plugins', 'catalog']); assert(ids(snapshot).includes(privateId));
  network.denyPrivateUpdate = true;
  snapshot = await online(['plugins', 'catalog']);
  assert(!ids(snapshot).includes(privateId));
  assert.equal(await exists(path.join(onlineProfile, privateCacheName)), false);
  network.denyPrivateUpdate = false;
  await online(['plugins', 'catalog']); network.revoked = true;
  snapshot = await online(['plugins', 'catalog']);
  assert.equal(snapshot.credentialState, 'revoked'); assert.deepEqual(ids(snapshot), [publicId]);
  assert.equal(await exists(path.join(onlineProfile, privateCacheName)), false);
  network.mode = 'offline'; snapshot = await online(['plugins', 'catalog']);
  assert.deepEqual(ids(snapshot), [publicId]);
  checked.push('scope shrink, item denial and revocation clear restricted snapshots across process restarts');
  network.mode = 'ready'; network.revoked = false;
  await online(['plugins', 'catalog']);
  network.expired = true;
  snapshot = await online(['plugins', 'catalog']);
  assert.equal(snapshot.credentialState, 'expired'); assert.deepEqual(ids(snapshot), [publicId]);
  assert.equal(await exists(path.join(onlineProfile, privateCacheName)), false);
  network.mode = 'offline'; snapshot = await online(['plugins', 'catalog']);
  assert.deepEqual(ids(snapshot), [publicId]);
  network.mode = 'ready'; network.expired = false;
  await online(['plugins', 'catalog']);
  checked.push('expired credentials remove restricted introductions and only live revalidation restores scope');
  await online(['plugins', 'distribution', 'set-code', '--stdin', '--yes'], { input: replacementCode });
  assert.equal(await exists(path.join(onlineProfile, privateCacheName)), false);
  network.mode = 'offline'; snapshot = await online(['plugins', 'catalog']);
  assert.deepEqual(ids(snapshot), [publicId]);
  network.mode = 'ready'; await online(['plugins', 'catalog']);
  await online(['plugins', 'distribution', 'clear-code', '--yes']);
  assert.equal(await exists(path.join(onlineProfile, privateCacheName)), false);
  snapshot = await online(['plugins', 'catalog']); assert.deepEqual(ids(snapshot), [publicId]);
  checked.push('changing and clearing a code cannot restore another code identity snapshot');

  const original = fixtures.get(`${publicId}@0.1.0`);
  await online(['plugins', 'install', original.directory, '--yes']);
  // Reproduce the existing catalog provenance without contacting the legacy URL.
  const managerPath = path.join(onlineProfile, 'plugin-manager.json');
  const legacyOrigin = 'https://github.com/example/legacy-catalog';
  const preferences = await jsonFile(managerPath);
  preferences.installationSources[publicId] = { kind: 'catalog', origin: legacyOrigin, author: 'local test' };
  await write(managerPath, JSON.stringify(preferences, null, 2));
  const legacy = (await online(['plugins', 'list'])).plugins.find(item => item.manifest.id === publicId);
  assert.equal(legacy.installationSource.kind, 'catalog');
  assert.equal(legacy.installationSource.origin, legacyOrigin);
  const markerPaths = ['plugin-data/wa_bookmarks/opaque.bin', 'plugin-secrets/wa_bookmarks/opaque.dpapi', 'account/opaque.bin', 'settings.json', 'theme/fixture.bin'];
  for (const relative of markerPaths) await write(path.join(onlineProfile, relative), Buffer.from(relative === 'settings.json' ? '{}' : `Synthetic retained bytes for ${relative}`));
  const beforeHashes = await Promise.all(markerPaths.map(async relative => [relative, sha(await readFile(path.join(onlineProfile, relative)))]));
  network.version = '0.1.1';
  await online(['plugins', 'update', publicId, '--yes'], { expectedCode: 'SOURCE_CHANGE_APPROVAL_REQUIRED' });
  let installed = (await online(['plugins', 'list'])).plugins.find(item => item.manifest.id === publicId);
  assert.equal(installed.manifest.version, '0.1.0');
  assert.equal(installed.installationSource.origin, legacyOrigin);
  await online(['plugins', 'update', publicId, '--yes', '--approve-source-change']);
  installed = (await online(['plugins', 'list'])).plugins.find(item => item.manifest.id === publicId);
  assert.equal(installed.manifest.version, '0.1.1');
  assert.equal(installed.enabled, false); assert.equal(installed.installationSource.kind, 'catalog');
  assert.equal(installed.installationSource.origin, origin);
  for (const [relative, before] of beforeHashes) assert.equal(sha(await readFile(path.join(onlineProfile, relative))), before, `User-defined bytes changed: ${relative}`);
  await online(['plugins', 'update', publicId, '--yes'], { expectedCode: 'CATALOG_VERSION_NOT_NEWER' });
  await online(['plugins', 'update', 'not_installed', '--yes'], { expectedCode: 'NOT_INSTALLED' });
  checked.push('legacy GitHub catalog provenance requires new-source confirmation and preserves opaque data');
  await online(['plugins', 'install-online', privateId, '--yes'], { expectedCode: 'NOT_AVAILABLE' });
  await online(['plugins', 'distribution', 'set-code', '--stdin', '--yes'], { input: fixtureCode });
  network.badPackage = true;
  const badPackageResult = await online(['plugins', 'install-online', privateId, '--yes'], { expectedCode: 'PACKAGE_CHECKSUM_MISMATCH' });
  assert(badPackageResult.error);
  network.badPackage = false;
  await online(['plugins', 'install-online', privateId, '--yes']);
  installed = (await online(['plugins', 'list'])).plugins.find(item => item.manifest.id === privateId);
  assert.equal(installed.manifest.version, '0.1.1'); assert.equal(installed.enabled, false);
  checked.push('restricted install rechecks current code and refuses altered package bytes');
  network.badSignature = true;
  snapshot = await online(['plugins', 'catalog']);
  assert.equal(snapshot.status, 'invalid_response');
  const badItem = snapshot.plugins.find(item => item.registration.id === publicId);
  assert.equal(badItem.error.code, 'INVALID_UPDATE_SIGNATURE'); assert.equal(badItem.entry, null);
  await online(['plugins', 'install-online', publicId, '--yes'], { expectedCode: 'INVALID_UPDATE_SIGNATURE' });
  checked.push('bad signature produces explicit invalid metadata rather than an older usable manifest');
  report.httpRequests = network.requests;
  for (const directory of [profile, onlineProfile]) {
    async function scan(directory_) {
      for (const entry of await readdir(directory_, { withFileTypes: true })) {
        const filename = path.join(directory_, entry.name);
        assertNoCode(filename);
        if (entry.isDirectory()) await scan(filename);
        else if (entry.isFile() && (filename.endsWith('.json') || filename.endsWith('.log'))) assertNoCode(await readFile(filename, 'utf8'));
      }
    }
    await scan(directory);
  }
  checked.push('settings, catalogs, logs and persisted paths do not expose synthetic access codes');
  report.ok = true;
} catch (failure) {
  report.ok = false; report.failure = String(failure?.stack ?? failure);
  assertNoCode(report.failure);
  process.exitCode = 1;
} finally {
  if (server) await new Promise(resolve => server.close(resolve));
  const reportPath = path.join(drillRoot, 'report.json');
  await write(reportPath, JSON.stringify(report, null, 2));
  process.stdout.write(JSON.stringify({ ok: report.ok, checks: checked.length, reportPath, failure: report.failure ?? null }, null, 2) + '\n');
}
