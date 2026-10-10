'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { createHumanDownloads } = require('../browser-human-downloads.cjs');
const { createStandardDownloadHistory } = require('../standard-download-history.cjs');
const { openStandardNavigationHistorySource } = require('../standard-navigation-history.cjs');
const owner = { workerEnvironmentId: 'stock-smoke', formationPin: 'b'.repeat(64) };
function source(hostRoot, purpose) {
  const payloadBytes = Buffer.from(JSON.stringify({ schema_version: 1, source_id: '22222222-2222-4222-8222-222222222222',
    owner_interface: 'system_browser_smoke', collection: 'explicit_door_selection', query_policy: {
      model_read_enabled: true, request_timeout_ms: 1000, max_queries: 4, max_query_bytes: 4096, max_results: 100, max_result_bytes: 65536 } }));
  return { hostRoot, purpose, sourceRef: { id: 'stock-smoke-history', content: Array(32).fill(2) }, payloadBytes,
    payloadDigest: crypto.createHash('sha256').update(payloadBytes).digest('hex'), revalidate: async () => {} };
}
async function restore(root) {
  const downloads = createHumanDownloads({ hostRoot: root, electron: { session: {} }, historyBackend: createStandardDownloadHistory(), emit() {} });
  await downloads.initialize(); await downloads.ready(owner);
  const snapshot = downloads.snapshot(owner);
  assert.deepEqual(snapshot.downloads.map(row => row.status).sort(), ['canceled', 'complete']);
  assert.ok(snapshot.downloads.every(row => !row.canResume && !row.canCancel));
  const history = await openStandardNavigationHistorySource(source(root, 'read'));
  const visits = await history.search({ queries: ['Fixture report'] }); assert.equal(visits.items.length, 1);
  history.close(); await downloads.stop();
  process.stdout.write(JSON.stringify({ freshProcessRestore: true, downloadCount: snapshot.downloads.length, visitCount: visits.items.length }));
}
async function smoke() {
  const electron = require('electron'), { app, WebContentsView, session } = electron;
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-stock-history-smoke-')));
  fs.mkdirSync(path.join(root, 'profile'), { mode: 0o700 }); app.setPath('userData', path.join(root, 'profile'));
  const timer = setTimeout(() => { process.stderr.write('Stock Electron smoke timeout\n'); app.exit(1); }, 20000);
  let history, downloads, server, view;
  try {
    await app.whenReady();
    const bytes = Buffer.from('real stock Electron download\n');
    server = require('node:http').createServer((request, response) => {
      if (request.url === '/start') { response.writeHead(302, { location: '/page' }); response.end(); }
      else if (request.url === '/page') { response.writeHead(200, { 'content-type': 'text/html' }); response.end('<title>Fixture report</title>fixture'); }
      else { response.writeHead(200, { 'content-disposition': 'attachment; filename=report.txt', 'content-type': 'text/plain', 'content-length': bytes.length }); response.end(bytes); }
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const base = `http://127.0.0.1:${server.address().port}`;
    const browserSession = session.fromPartition(`stock-history-smoke-${crypto.randomUUID()}`);
    view = new WebContentsView({ webPreferences: { session: browserSession, sandbox: true, contextIsolation: true, nodeIntegration: false } });
    const wc = view.webContents, errors = [];
    const room = { active: true, session: browserSession, formation: { worker_environment_id: owner.workerEnvironmentId, formation_pin: owner.formationPin } };
    history = await openStandardNavigationHistorySource({ ...source(root, 'collection'), revalidateNow() {},
      validatePage(page) { assert.equal(page, wc); }, onError(error) { errors.push(error.message); } });
    history.observeReady(wc); await wc.loadURL(`${base}/start`);
    assert.equal((await history.search({ queries: ['Fixture report'] })).items[0]?.url, `${base}/page`);
    downloads = createHumanDownloads({ hostRoot: root, electron, historyBackend: createStandardDownloadHistory(),
      emit(event) { if (event.error) errors.push(event.error.message); } });
    await downloads.initialize(); await downloads.ready(owner);
    async function download(revoke) {
      const done = new Promise((resolve, reject) => browserSession.once('will-download', (_event, item, contents) => {
        try {
          assert.equal(contents, wc); assert.equal(typeof item.getGUID, 'undefined');
          downloads.start(room, { id: 'smoke-page' }, item);
          item.setSavePath(path.join(root, revoke ? 'cancelled.txt' : 'report.txt'));
          item.once('done', (_event, state) => resolve(state));
          if (revoke) { room.active = false; downloads.releaseRoom(room); }
        } catch (error) { item.cancel(); reject(error); }
      }));
      wc.downloadURL(`${base}/download`); return done;
    }
    assert.equal(await download(false), 'completed'); assert.deepEqual(fs.readFileSync(path.join(root, 'report.txt')), bytes);
    assert.equal(await download(true), 'cancelled');
    await downloads.action(owner, 'search', undefined, { text: '', offset: 0 }, async () => {});
    await downloads.stop(); downloads = null; history.close(); history = null;
    assert.deepEqual(errors, []);
    const restored = JSON.parse(require('node:child_process').execFileSync(process.execPath, [__filename, '--restore', root],
      { env: { ...process.env, ELECTRON_RUN_AS_NODE: '1' }, encoding: 'utf8', timeout: 10000 }));
    process.stdout.write(JSON.stringify({ electron: process.versions.electron, node: process.versions.node, downloadItemGetGUID: false,
      committedNavigation: true, downloadCompleted: true, revokedCancellationPersisted: true, ...restored }) + '\n');
  } finally {
    await downloads?.stop(); history?.close(); view?.webContents.close({ waitForBeforeUnload: false });
    if (server) await new Promise(resolve => server.close(resolve));
    clearTimeout(timer); fs.rmSync(root, { recursive: true, force: true });
  }
}
if (process.argv[2] === '--restore') restore(process.argv[3]).catch(error => { console.error(error); process.exitCode = 1; });
else smoke().then(() => require('electron').app.exit(0), error => { console.error(error); require('electron').app.exit(1); });
