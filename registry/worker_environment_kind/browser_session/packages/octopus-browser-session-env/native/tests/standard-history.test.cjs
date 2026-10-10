'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { EventEmitter } = require('node:events');
const { openStandardHistoryStorage } = require('../standard-history-storage.cjs');
const { openStandardNavigationHistorySource, API } = require('../standard-navigation-history.cjs');
const { createStandardDownloadHistory } = require('../standard-download-history.cjs');
const { createHumanDownloads } = require('../browser-human-downloads.cjs');
const { createRoomHistory } = require('../browser-history-control.cjs');
const { canonical } = require('../standard-history-storage.cjs');
const owner = { workerEnvironmentId: 'room-test', formationPin: 'a'.repeat(64) };
function root(t) {
  const value = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-standard-history-test-')));
  t.after(() => fs.rmSync(value, { recursive: true, force: true })); return value;
}
function source(hostRoot) {
  const payloadBytes = Buffer.from(JSON.stringify({ schema_version: 1, source_id: '11111111-1111-4111-8111-111111111111',
    owner_interface: 'system_browser_test', collection: 'explicit_door_selection', query_policy: {
      model_read_enabled: true, request_timeout_ms: 1000, max_queries: 4, max_query_bytes: 4096, max_results: 100, max_result_bytes: 65536 } }));
  return { hostRoot, sourceRef: { id: 'history-test', content: Array(32).fill(1) }, payloadBytes,
    payloadDigest: crypto.createHash('sha256').update(payloadBytes).digest('hex'), revalidate: async () => {}, revalidateNow() {},
    validatePage(page) { assert.equal(page.owner, owner); }, onError(error) { throw error; } };
}
class Page extends EventEmitter {
  owner = owner; url = '';
  isDestroyed() { return false; }
  getURL() { return this.url; }
  visit(url) { this.url = url; this.emit('did-navigate', {}, url, 200, 'OK'); }
}
class Item extends EventEmitter {
  state = 'progressing'; bytes = 0; paused = false; destination = '';
  getState() { return this.state; }
  getURL() { return 'https://example.test/report'; }
  getURLChain() { return ['https://example.test/start', this.getURL()]; }
  getSavePath() { return this.destination; }
  getStartTime() { return 1700000000; }
  getReceivedBytes() { return this.bytes; }
  getTotalBytes() { return 10; }
  getFilename() { return 'report.txt'; }
  canResume() { return this.paused; }
  isPaused() { return this.paused; }
  pause() { this.paused = true; }
  resume() { this.paused = false; }
  cancel() { this.state = 'cancelled'; this.emit('done', {}, 'cancelled'); }
  setSaveDialogOptions() {}
  complete() { this.bytes = 10; this.state = 'completed'; this.emit('done', {}, 'completed'); }
}
function downloads(hostRoot) {
  const errors = [], session = { setDownloadPath() {} }, room = { active: true, session,
    formation: { worker_environment_id: owner.workerEnvironmentId, formation_pin: owner.formationPin } };
  const controller = createHumanDownloads({ hostRoot, electron: { session: {}, app: { getPath: () => hostRoot } },
    historyBackend: createStandardDownloadHistory(), emit: event => { if (event.error) errors.push(event); } });
  return { controller, room, errors };
}

test('standard storage bounds records transactionally, preserves identity and rejects unsafe/corrupt databases', t => {
  const directory = root(t), options = { directory, identity: { api: 'test.standard.v1', scope: 'one' }, create: true, maxEntries: 2, maxBytes: 1024 };
  let db = openStandardHistoryStorage(options);
  db.put('one', 1, { title: 'One' }); db.put('two', 2, { title: 'Two' }); db.put('three', 3, { title: 'Three' });
  assert.deepEqual(db.rows().map(row => row.id), ['three', 'two']);
  assert.throws(() => db.put('four', 4, {}, new Set(['two', 'three'])), /exhaust/);
  assert.equal(db.rows().length, 2); db.close();
  assert.throws(() => openStandardHistoryStorage({ ...options, identity: { api: 'test.native.v1', scope: 'one' } }), /another backend/);
  db = openStandardHistoryStorage({ ...options, create: false }); assert.equal(db.rows()[0].value.title, 'Three');
  assert.throws(() => db.remove(['three']), /cannot mutate/); db.close();
  const file = path.join(directory, 'standard-history.sqlite'); fs.chmodSync(file, 0o644);
  assert.throws(() => openStandardHistoryStorage(options), /private owned/); fs.chmodSync(file, 0o600);
  fs.writeFileSync(file, 'not a database'); assert.throws(() => openStandardHistoryStorage(options), /database/);
});

test('event history persists selected main-frame visits and titles, with explicit limited search evidence', async t => {
  const options = source(root(t)), page = new Page();
  const history = await openStandardNavigationHistorySource({ ...options, purpose: 'collection', maxEntries: 2 });
  history.observeReady(page);
  page.visit('https://example.test/one'); page.emit('did-navigate-in-page', {}, 'https://frame.test/', false);
  page.visit('https://example.test/two'); page.emit('page-title-updated', {}, 'Second page');
  page.url = 'https://example.test/two#section'; page.emit('did-navigate-in-page', {}, page.url, true);
  page.emit('page-title-updated', {}, 'Section');
  const result = await history.search({});
  assert.equal(result.standard_query_evidence.api, API); assert.equal(result.native_query_evidence, undefined);
  assert.deepEqual(result.items.map(item => item.title), ['Section', 'Second page']);
  await assert.rejects(history.search({ queries: [''], deduplicate_by_day: true, matching_algorithm: 'alwaysPrefix' }, { human: true }), /unavailable/);
  await assert.rejects(history.search({}, { signal: AbortSignal.abort() }), /cancelled/);
  assert.throws(() => history.observeReady(Object.assign(new Page(), { owner: {} })), assert.AssertionError);
  history.close(); assert.equal(page.listenerCount('did-navigate'), 0);
});

test('stock downloads preserve search, live controls and exact owner checks', async t => {
  const fixture = downloads(root(t)), { controller, room } = fixture;
  await controller.initialize(); await controller.ready(owner);
  const item = new Item(); assert.equal(item.getGUID, undefined); controller.start(room, { id: 'target-1' }, item);
  const id = controller.snapshot(owner).downloads[0].id;
  await controller.action(owner, 'pause', id, undefined, async () => {});
  assert.equal(controller.snapshot(owner).downloads[0].canResume, true);
  await controller.action(owner, 'resume', id, undefined, async () => {}); item.complete();
  const snapshot = await controller.action(owner, 'search', undefined, { text: 'START', offset: 0 }, async () => {});
  assert.equal(snapshot.history.downloads[0].id, id); assert.equal(snapshot.history.downloads[0].status, 'complete');
  await assert.rejects(controller.action({ ...owner, formationPin: 'b'.repeat(64) }, 'remove', id, undefined, async () => {}), /exact room/);
  await controller.action(owner, 'remove', id, undefined, async () => {}); assert.equal(controller.snapshot(owner).downloads.length, 0);
  await controller.stop(); assert.deepEqual(fixture.errors, []);
});

test('restored active download is unavailable; clearing history preserves current live items', async t => {
  const fixture = downloads(root(t)), { controller, room } = fixture;
  await controller.initialize(); const store = await controller.ready(owner);
  const item = new Item(); controller.start(room, { id: 'target-1' }, item);
  const id = controller.snapshot(owner).downloads[0].id;
  await controller.action(owner, 'clear-history', undefined, undefined, async () => {});
  assert.equal(controller.snapshot(owner).downloads[0].id, id);
  const { records } = await store.reader.search({ text: '', offset: 0, maxResults: 0 });
  const backend = createStandardDownloadHistory(); assert.equal(backend.decode(owner, records[0]).status, 'unavailable');
  const forged = structuredClone(records[0]); forged.standard.id = crypto.randomUUID();
  assert.throws(() => backend.decode(owner, forged), /identity/);
  room.active = false; controller.releaseRoom(room);
  await controller.action(owner, 'search', undefined, { text: '', offset: 0 }, async () => {});
  assert.equal(controller.snapshot(owner).downloads[0].status, 'canceled');
  assert.equal((await store.reader.search({ text: '', offset: 0, maxResults: 0 })).records[0].standard.state, 'cancelled');
  assert.throws(() => controller.start(room, { id: 'target-2' }, new Item()), /configured room/);
  await controller.stop(); assert.deepEqual(fixture.errors, []);
});

test('room control selects stock collection explicitly and preserves scoped consent on both query paths', async t => {
  const options = source(root(t)), page = new Page(), generation = 'b'.repeat(32), errors = [];
  const room = { active: true, session: null, targets: new Map(), formation: { run: 'run', ee: 'ee', worker: 'worker', door_interface: 'door',
    worker_environment_id: owner.workerEnvironmentId, formation_pin: owner.formationPin, granted_tools: ['browser.history_resume'] } };
  let consentCalls = 0;
  const configuration = { electron: {}, hostRoot: options.hostRoot, engineMode: 'standard', room, generation: () => generation, pendingLimit: 4,
    ensureLive: room => assert.ok(room.active), onError: error => errors.push(error.message),
    permissions: { async requireSaved(_request, id, decision) { assert.equal(id, 'saved'); assert.equal(decision, 'accept'); consentCalls++; return { id }; } } };
  assert.throws(() => createRoomHistory({ ...configuration, engineMode: undefined }), /explicit engineMode/);
  const control = createRoomHistory(configuration); t.after(() => control.close());
  const params = { source_ref: options.sourceRef, source_payload_digest: options.payloadDigest, payload_base64: options.payloadBytes.toString('base64') };
  await control.selectCollection({ tool: { operation: 'navigate' } }, params, () => {});
  room.session = {}; page.session = room.session; control.observe(page); page.visit('https://example.test/fixture');
  assert.throws(() => control.observe(Object.assign(new Page(), { session: {} })), /outside.*Session/);
  const query = { queries: ['fixture'] }, call = { tool: { id: 'browser.history', operation: 'history' }, arguments: { source: options.sourceRef, ...query },
    source_movement_seq: 1, artifact_content_hash: '55'.repeat(32) };
  const identity = await control.prepare(call, { ...params, query }, () => {});
  for (const cached of [false, true]) {
    const original = { ...room.formation, tool: call.tool.id, correlation_id: `call-${cached}`, arguments: call.arguments,
      source_movement_seq: 1, source_artifact: { id: 'original', content: Array(32).fill(0x55) } };
    for (const key of ['worker_environment_id', 'formation_pin', 'granted_tools']) delete original[key];
    const commitment = { schema_version: 1, original, scope: { worker_environment_id: owner.workerEnvironmentId, formation_pin: owner.formationPin, host_generation: generation },
      approval_scope: 'browser_history_read', history: { source: options.sourceRef, source_payload_digest: options.payloadDigest, source_epoch: identity.source_epoch, query },
      permission_offer: { client_interface: 'operator', offered_choices: [{ decision: 'accept', persistence: 'once' }] } };
    const hash = crypto.createHash('sha256').update(canonical(commitment)).digest('hex');
    const request = { ...commitment, request_id: `sha256:${hash}`, call_id: original.correlation_id,
      resume: { tool: 'browser.history_resume', correlation_id: `browser-history-resume:${hash}` } };
    assert.throws(() => control.stage({ ...call, artifact_content_hash: '66'.repeat(32) }, { request }), /original call/);
    control.stage(call, { request });
    const body = cached ? call : { tool: { id: 'browser.history_resume', operation: 'history_resume' }, arguments: { request_id: request.request_id }, source_movement_seq: 2 };
    const approval = { request_id: request.request_id, decision: 'accept', ...(cached ? { permission_entry_id: 'saved' } : {}) };
    const result = await control.apply(body, approval, undefined, () => {});
    assert.equal(result.standard_query_evidence.api, API); assert.equal(result.native_query_evidence, undefined); assert.equal(result.items.length, 1);
    await assert.rejects(control.apply(body, approval, undefined, () => {}), /current native history request/);
  }
  assert.equal(consentCalls, 1); room.active = false;
  await assert.rejects(control.prepare(call, { ...params, query }, () => {})); assert.deepEqual(errors, []);
});
