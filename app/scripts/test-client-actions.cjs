const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { test, after } = require('node:test');
const { blake3 } = require('@noble/hashes/blake3');
const ts = require('typescript');
const React = require('react');
const { JSDOM } = require('jsdom');
const { createProjectionStore } = require('../electron/projection.cjs');
const { createFixtureProjectionStore } = require('../electron/fixtureProjection.cjs');
const contract = require('../electron/projectionReadContract.cjs');
const dom = new JSDOM('<body></body>', { url: 'https://client.test' });
for (const key of ['window', 'document', 'HTMLElement', 'Element', 'Node']) globalThis[key] = dom.window[key];
Object.defineProperty(globalThis, 'navigator', { configurable: true, value: dom.window.navigator });
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const { createRoot } = require('react-dom/client');
after(() => dom.window.close());
const action = surface_id => ({ schema: 'octopus.client_action', schema_version: 1, action: 'open_surface', surface_id });
const identity = (seq, overrides = {}) => ({ artifact_id: `client.action:${seq.toString(16).padStart(64, '0')}`, run_id: 'home-run', seq, movement_kind: 'InterfaceOutbound', from: { Interface: 'room' }, to: { Interface: 'home-client' }, action: action('dashboard'), ...overrides });
const response = (params, actions) => ({ home: params.home, run_id: params.run_id, interface_id: params.interface_id, actions });
const scope = { home: '/selected-home', runId: 'home-run', interfaceId: 'home-client' };

test('client action contract and fixture stay read-only and typed', async () => {
  assert.ok(contract.PROJECTION_READ_METHODS.includes('projection.client_actions.list'));
  assert.ok(!contract.CLIENT_SURFACE_PROJECTION_READ_METHODS.includes('projection.client_actions.list'));
  const params = { home: scope.home, run_id: scope.runId, interface_id: scope.interfaceId };
  const request = values => ({ contract_version: 1, method: 'projection.client_actions.list', params: values });
  for (const invalid of [{}, { ...params, after_seq: '0' }, { ...params, after_seq: -2 }, { ...params, limit: 0 }, { ...params, url: 'https://external.test' }]) assert.throws(() => contract.parseProjectionReadRequest(request(invalid)), /violates contract/);
  assert.deepEqual(await createFixtureProjectionStore().read(request(params)), response(params, []));
});

test('actions require exact admitted outbound evidence and immutable manifest/payload bytes', async t => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-client-actions-')), blobs = path.join(home, 'ledger/blobs'), rows = [];
  await fs.mkdir(blobs, { recursive: true });
  const store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, '../..'), octopusHome: () => home, sourceRegistry: () => path.join(home, 'registry'), projectEePolicies: () => new Promise(() => {}) });
  t.after(async () => { store.close(); await fs.rm(home, { recursive: true, force: true }); });
  const blob = async value => { const bytes = Buffer.from(JSON.stringify(value)), hash = Buffer.from(blake3(bytes)).toString('hex'); await fs.writeFile(path.join(blobs, hash), bytes); return hash; };
  const original = path.join(home, 'original.json'); await fs.writeFile(original, JSON.stringify(action('foreign')));
  const admit = async (payload, overrides = {}, manifestOverrides = {}) => {
    const payloadHash = await blob(payload), hash = await blob({ label: 'client.action', payload_path: original, payload_content_hash: payloadHash, ...manifestOverrides });
    const seq = rows.length;
    rows.push({ seq, lamport: seq, trace: 'test', recorded_at_unix_ms: seq, kind: 'InterfaceOutbound', from: { Interface: 'room' }, to: { Interface: scope.interfaceId }, artifact: { id: `client.action:${hash}`, content: [...Buffer.from(hash, 'hex')] }, invocation_metadata: { runtime: { run: scope.runId, ee: 'ee', worker: 'worker', checkpoint: 'checkpoint', interface: scope.interfaceId, epoch: 0 } }, ...overrides });
    await fs.writeFile(path.join(home, 'ledger/movements.jsonl'), rows.map(JSON.stringify).join('\n') + '\n');
    return { hash, payloadHash };
  };
  const read = (params = {}) => store.read({ contract_version: 1, method: 'projection.client_actions.list', params: { home, run_id: scope.runId, interface_id: scope.interfaceId, ...params } });
  const first = await admit(action('dashboard'));
  await admit(action('dashboard'), { kind: 'InterfaceArrival' }); await admit(action('dashboard'), { kind: 'DeliveryRefused' });
  await admit(action('dashboard'), { to: { Interface: 'other-client' } });
  await admit(action('dashboard'), { invocation_metadata: { runtime: { run: 'other-run', ee: 'ee', worker: 'worker', checkpoint: 'checkpoint', interface: scope.interfaceId, epoch: 0 } } });
  await admit({ ...action('dashboard'), schema_version: 2 });
  await admit(action('dashboard'), {}, { label: 'client.action.extra' });
  await admit(action('scheduled'));
  const projected = await read();
  assert.deepEqual(projected.actions.map(row => row.seq), [0, 5, 6, 7]);
  assert.deepEqual(projected.actions[0].action, action('dashboard'));
  assert.deepEqual(projected.actions[0].from, { Interface: 'room' }); assert.equal(projected.actions[0].run_id, scope.runId);
  assert.match(projected.actions[1].error, /exact/); assert.match(projected.actions[2].error, /identity/);
  assert.deepEqual((await read({ after_seq: 6, limit: 1 })).actions.map(row => row.seq), [7]);
  await assert.rejects(read({ home: '/other-home' }), /selected Home/);
  await fs.writeFile(path.join(blobs, first.payloadHash), '{}'); assert.match((await read({ limit: 1 })).actions[0].error, /digest/);
  await fs.unlink(path.join(blobs, first.payloadHash)); assert.match((await read({ limit: 1 })).actions[0].error, /ENOENT/);
  await fs.appendFile(path.join(home, 'ledger/movements.jsonl'), '{"from":{"Run":"retired"}}\n'); await assert.rejects(read(), /malformed ledger/);
});

async function harness(read, props = {}) {
  const source = await fs.readFile(path.join(__dirname, '../src/shell/useAdmittedClientActions.ts'), 'utf8');
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} }, opens = [], refusals = [], reads = [];
  let generation = 0;
  Function('module', 'exports', 'require', compiled)(module, module.exports, id => id === 'react' ? React : id === '../projectionClient' ? { readProjection: (method, params) => { assert.equal(method, 'projection.client_actions.list'); reads.push(params); return read(params); } } : { useProjectionGeneration: () => generation });
  function Consumer(options) { module.exports.useAdmittedClientActions(options); return null; }
  const options = { ...scope, enabled: true, surfaces: [{ id: 'dashboard' }, { id: 'scheduled' }], openSurface: surface => { const cursor = JSON.parse(window.localStorage.getItem(`octopus.client-actions.cursor.v1:${JSON.stringify([options.home, options.runId, options.interfaceId])}`)); assert.ok(cursor.last_consumed.seq >= 0); opens.push(surface.id); }, onRefusal: reason => refusals.push(reason), ...props };
  let root, container;
  const render = async (changes = {}, bump = false) => { Object.assign(options, changes); if (bump) generation++; if (!root) { container = document.createElement('div'); document.body.append(container); root = createRoot(container); } await React.act(async () => root.render(React.createElement(Consumer, { ...options }))); };
  const close = async () => { if (!root) return; await React.act(async () => root.unmount()); container.remove(); root = null; };
  await render();
  return { render, close, opens, refusals, reads, options };
}

test('durable consumption precedes navigation, never replays on generation/remount, and scopes each Home', async () => {
  window.localStorage.clear();
  const view = await harness(async params => response(params, [identity(0), identity(1, { action: action('scheduled') })].filter(row => row.seq > params.after_seq)));
  try {
    assert.deepEqual(view.opens, ['dashboard', 'scheduled']); await view.render({}, true); assert.equal(view.reads.at(-1).after_seq, 1);
    const reads = view.reads.length; await view.render({ surfaces: [...view.options.surfaces] }); assert.equal(view.reads.length, reads);
    await view.close(); await view.render(); assert.equal(view.opens.length, 2);
    await view.render({ home: '/second-home' }); assert.equal(view.opens.length, 4);
    await view.render({ home: scope.home }); assert.equal(view.opens.length, 4);
    await view.render({ interfaceId: 'foreign-client' }); assert.equal(view.opens.length, 4); assert.match(view.refusals.at(-1), /current Home/);
    await view.render({ interfaceId: scope.interfaceId, enabled: false }, true); assert.equal(view.opens.length, 4);
    assert.equal(window.localStorage.length, 2);
  } finally { await view.close(); }
});

test('malformed or unavailable actions are consumed with explicit refusals; broken storage cannot navigate', async () => {
  window.localStorage.clear();
  const rows = [identity(0, { action: { ...action('dashboard'), url: 'https://external.test' } }), identity(1, { action: action('missing') }), identity(2, { error: 'immutable payload digest mismatch' }), identity(3)];
  const view = await harness(async params => response(params, rows.filter(row => row.seq > params.after_seq)));
  try {
    assert.deepEqual(view.opens, ['dashboard']); assert.equal(view.refusals.length, 3);
    await view.render({}, true); assert.equal(view.refusals.length, 3);
    const originalSet = dom.window.Storage.prototype.setItem;
    dom.window.Storage.prototype.setItem = () => { throw new Error('storage write denied'); };
    try { await view.render({ home: '/cannot-save' }); assert.equal(view.opens.length, 1); assert.match(view.refusals.at(-1), /storage write denied/); }
    finally { dom.window.Storage.prototype.setItem = originalSet; }
  } finally { await view.close(); }
});

test('late reads from a previous Home or inactive client cannot execute', async () => {
  window.localStorage.clear(); let resolve;
  const view = await harness(params => params.home === scope.home ? new Promise(done => { resolve = () => done(response(params, [identity(0)])); }) : Promise.resolve(response(params, [identity(1, { action: action('scheduled') })])));
  try {
    await view.render({ home: '/next-home' }); assert.deepEqual(view.opens, ['scheduled']);
    await React.act(async () => resolve()); assert.deepEqual(view.opens, ['scheduled']);
    await view.render({ home: scope.home }); await view.render({ enabled: false }); await React.act(async () => resolve()); assert.deepEqual(view.opens, ['scheduled']);
    assert.equal(window.localStorage.length, 1);
  } finally { await view.close(); }
});
