'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const net = require('node:net');
const crypto = require('node:crypto');
const { EventEmitter, once } = require('node:events');
const { createBrowserEngineHost: createHost } = require('../electron-host.cjs');
const { createRequire } = require('node:module');
const path = require('node:path');
const repo = path.resolve(__dirname, '../../../../../../../');
const { blake3 } = createRequire(path.join(repo, 'app/package.json'))('@noble/hashes/blake3');
const descriptorHash = bytes => Buffer.from(blake3(bytes)).toString('hex');
const createBrowserEngineHost = options => createHost({ ...options, descriptorHash });
const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value && typeof value === 'object' ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value);
const descriptorBytes = id => Buffer.from(canonical(JSON.parse(require('node:fs').readFileSync(path.join(repo, 'registry/tool', id, 'tool.json'), 'utf8'))));
const wireOwners = new WeakMap();
const { clipboardItems } = require('../browser-rich-text.cjs');
const { waitForPageLoad } = require('../browser-load-wait.cjs');

function injectedElectron() {
  const sessions = new Map(); const contents = []; const factoryOptions = [];
  class Contents extends EventEmitter {
    constructor(session, owned = false) {
      super(); this.id = contents.length + 1; this.session = session; this.owned = owned; this.destroyed = false; this.url = ''; this.zoom = 1; this.calls = [];
      this.mainFrame = {}; this.frames = new Set(['frame-a', 'frame-b', 'frame-c']); this.nativeGeneration = 0; this.lastGeneration = 0; this.visibilityCalls = []; this.hostWebContents = null;
      this.navigationHistory = { canGoBack: () => false, canGoForward: () => false, getAllEntries: () => [{ url: this.url, title: 'Fixture' }] };
      this.debugger = Object.assign(new EventEmitter(), { isAttached: () => true, sendCommand: async (method, params, sessionId) => {
        this.calls.push({ method, params, sessionId });
        if (method === 'Page.getNavigationHistory') return { currentIndex: 1, entries: [{ id: 41, url: 'https://earlier.test', title: 'Earlier' }, { id: 42, url: this.url, title: this.getTitle() }] };
        if (method === 'Page.captureScreenshot') { setImmediate(() => this.debugger.emit('message', {}, 'Network.loadingFinished', { requestId: 'during-media' })); return { data: 'x'.repeat(17 * 1024 * 1024) }; }
        return {};
      } });
      contents.push(this);
    }
    isDestroyed() { return this.destroyed; } getTitle() { return 'Fixture'; } getURL() { return this.url; }
    isLoading() { return false; } getZoomFactor() { return this.zoom; } setZoomFactor(value) { this.zoom = value; }
    setBackgroundThrottling() {} setWindowOpenHandler(handler) { this.popup = handler; }
    async loadURL(url, options = {}) { this.loads ??= []; this.loads.push({ url, options, methods: this.calls.map(call => call.method) }); this.emit('did-start-navigation', {}, url, false, true); this.url = url; this.emit('did-navigate', {}, url); }
    reload() { this.reloads = (this.reloads || 0) + 1; } focus() { this.focused = true; }
    isOwnedGuest() { return this.owned; }
    createOwnedGuestLease(sender, frameToken, generation) {
      if (!sender.frames.has(frameToken) || generation <= this.lastGeneration || (this.hostWebContents && this.hostWebContents.session !== sender.session)) return '';
      this.lastGeneration = generation; this.lease = { sender, generation, token: crypto.randomUUID() }; return this.lease.token;
    }
    cancelOwnedGuestLease(token) { if (this.lease?.token !== token) return false; this.lease = null; return true; }
    adopt(token) {
      assert.equal(this.lease?.token, token); const lease = this.lease; this.lease = null;
      this.releaseOwnedGuest(this.nativeGeneration); this.hostWebContents = lease.sender; this.nativeGeneration = lease.generation; this.visible = false;
      this.emit('did-attach', {});
    }
    releaseOwnedGuest(generation) {
      if (!generation || generation !== this.nativeGeneration) return false;
      this.nativeGeneration = 0; this.hostWebContents = null; this.visible = false;
      this.emit('-owned-guest-released', {}, generation); return true;
    }
    setOwnedGuestVisibility(generation, visible) {
      this.visibilityCalls.push([generation, visible]); if (!generation || generation !== this.nativeGeneration) return false;
      this.visible = visible; return true;
    }
    setOwnedGuestPaintLease(value) { this.paintLease = value; }
    destroy() { this.lease = null; this.releaseOwnedGuest(this.nativeGeneration); this.paintLease = false; this.destroyed = true; this.emit('destroyed'); }
    findInPage(query) { this.query = query; return 1; } stopFindInPage() { this.query = ''; }
  }
  const histories = new Map();
  const historyFor = directory => { if (!histories.has(directory)) histories.set(directory, new Map()); return histories.get(directory); };
  class Session extends EventEmitter {
    constructor(name) { super(); this.name = name; this.items = new Map(); this.webRequest = { onBeforeRequest(handler) { this.handler = handler; } }; }
    setPermissionCheckHandler() {} setPermissionRequestHandler() {} setDevicePermissionHandler() {}
    setDownloadPath(value) { this.downloadPath = value; }
    async closeAllConnections() {} async clearStorageData() { this.cleared = true; }
    emit(name, ...args) {
      if (name === 'will-download') {
        const item = args[1]; this.items.set(item.getGUID(), item); item.nativeState = 'in_progress';
        item.on('updated', (_event, state) => { item.nativeState = state === 'interrupted' ? 'interrupted' : 'in_progress'; });
        item.on('done', (_event, state) => { item.nativeState = state === 'completed' ? 'complete' : state === 'cancelled' ? 'canceled' : 'interrupted'; });
        item.on('destroyed', () => this.items.delete(item.getGUID()));
      }
      return super.emit(name, ...args);
    }
    async configureOwnedDownloadHistory(directory) { assert(!this.historyDirectory || this.historyDirectory === directory); this.historyDirectory = directory; historyFor(directory); }
    async recordOwnedDownloadHistory(guid, metadata) {
      const item = this.items.get(guid); assert(item && this.historyDirectory);
      historyFor(this.historyDirectory).set(guid, { native: { guid, filename: item.getFilename(), state: item.nativeState,
        received_bytes: String(item.getReceivedBytes()), total_bytes: String(item.getTotalBytes()), interrupt_reason: '0',
        current_path: item.getSavePath(), target_path: item.getSavePath(), started_at_ms: item.getStartTime() * 1000, ended_at_ms: item.getEndTime() * 1000, url_chain: item.getURLChain() }, metadata });
      return { ok: true };
    }
  }
  const session = {
    fromPartition(name) { if (!sessions.has(name)) sessions.set(name, new Session(name)); return sessions.get(name); },
    async openOwnedDownloadHistory(directory) {
      const rows = historyFor(directory); let closed = false; const current = () => assert.equal(closed, false);
      return {
        async search({ text, offset, maxResults }) { current(); const matched = [...rows.values()].filter(row => !text || [row.native.filename, ...row.native.url_chain].some(value => value.toLowerCase().includes(text.toLowerCase()))).sort((a,b) => b.native.started_at_ms - a.native.started_at_ms || a.native.guid.localeCompare(b.native.guid));
          const records = matched.slice(offset, maxResults ? offset + maxResults : undefined); return { text, offset, hasMore: !!maxResults && offset + records.length < matched.length, records: structuredClone(records) }; },
        async update(guid, metadata) { current(); assert(rows.has(guid), 'recorded GUID'); rows.get(guid).metadata = metadata; return { ok: true }; },
        async remove(guid) { current(); assert.notEqual(rows.get(guid)?.native.state, 'in_progress'); return { removed: rows.delete(guid) }; },
        async clear() { current(); let removed = 0; for (const [guid, row] of rows) if (row.native.state !== 'in_progress') { rows.delete(guid); ++removed; } return { removed }; },
        async flush() { current(); return { ok: true }; }, close() { closed = true; }
      };
    }
  };
  class Window extends EventEmitter {
    constructor(options) { super(); this.webContents = new Contents(options.webPreferences.session); this.visible = true; }
    isDestroyed() { return !!this.destroyed; } isVisible() { return this.visible; } isMinimized() { return false; }
    show() { this.visible = true; this.emit('show'); } focus() { this.focused = true; }
    destroy() { this.destroyed = true; this.emit('closed'); }
  }
  const menus = [], fileActions = [];
  return { app: { getPath: () => '/test/downloads' }, fileActions, shell: { async openPath(path) { fileActions.push(['open', path]); return ''; }, showItemInFolder(path) { fileActions.push(['reveal', path]); } }, clipboard: { writeText(text) { fileActions.push(['copy', text]); } }, session, WebContentsView: class { constructor() { throw new Error('Unexpected WebContentsView'); } }, webContents: { createOwnedGuest(options) { factoryOptions.push(options); return new Contents(options.session, true); } }, BrowserWindow: Window, contents, sessions, factoryOptions, menus, Menu: { buildFromTemplate(items) { menus.push(items); return { popup() {} }; } } };
}
function formation(id) {
  return { worker_environment_id: id, run: `run-${id}`, ee: 'ee', worker: 'system_worker_test', door_interface: 'system_browser', door_descriptor_hash: '1'.repeat(64), worker_environment_kind: 'browser_session', worker_environment_kind_descriptor_hash: '2'.repeat(64), tool_descriptor_hashes: { 'browser.clipboard_read': '8'.repeat(64), 'browser.clipboard_write': '9'.repeat(64), 'browser.click': '6'.repeat(64), 'browser.key': '7'.repeat(64), 'browser.reload': 'c'.repeat(64), 'browser.back': 'd'.repeat(64), 'browser.navigate': '3'.repeat(64), 'browser.screenshot': '4'.repeat(64), 'browser.tab_mark': 'e'.repeat(64), 'browser.viewport_reset': 'a'.repeat(64), 'browser.viewport_set': 'b'.repeat(64) }, instance_config_hashes: {}, formation_pin: crypto.createHash('sha256').update(id).digest('hex'), granted_tools: ['browser.clipboard_read', 'browser.clipboard_write', 'browser.back', 'browser.click', 'browser.key', 'browser.navigate', 'browser.reload', 'browser.screenshot', 'browser.tab_mark', 'browser.viewport_reset', 'browser.viewport_set'].sort(), door_root_bindings: {}, readable_root_slots: [], writable_root_slots: [], network_ceiling: 'restricted', requires_release: false };
}
async function wireConnection(descriptor, key) {
  const socket = net.createConnection(descriptor.socket_path), lines = [], waiting = [];
  let buffer = '', counter = 0;
  socket.on('data', bytes => { buffer += bytes.toString(); let end; while ((end = buffer.indexOf('\n')) >= 0) { const frame = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1); const resolve = waiting.shift(); if (resolve) resolve(frame); else lines.push(frame); } });
  const next = () => lines.length ? Promise.resolve(lines.shift()) : new Promise(resolve => waiting.push(resolve));
  const challenge = await next();
  assert.equal(challenge.mac, crypto.createHmac('sha256', key).update(`browser.native.v1.challenge
${challenge.host_generation}
${challenge.challenge}`).digest('hex'));
  return { socket, next, challenge: challenge.challenge, async send(body) {
    const id = ++counter;
    body = { ...body, host_generation: descriptor.host_generation, challenge: challenge.challenge, request_id: id };
    const payload = Buffer.from(JSON.stringify(body));
    socket.write(JSON.stringify({ payload: payload.toString('base64'), mac: crypto.createHmac('sha256', key).update(payload).digest('hex') }) + '\n');
    let result = await next();
    while (result.method) { this.events ??= []; this.events.push(result); result = await next(); }
    if (result.type === 'response_chunk') {
      const chunks = [], total = result.total;
      for (let index = 0; index < total; index++) {
        assert.equal(result.id, id); assert.equal(result.index, index); assert.equal(result.total, total);
        assert(Buffer.byteLength(JSON.stringify(result)) + 1 <= 16 * 1024 * 1024);
        chunks.push(Buffer.from(result.data, 'base64')); if (index + 1 < total) result = await next();
      }
      result = JSON.parse(Buffer.concat(chunks).toString());
    }
    return result;
  } };
}
async function client(descriptor, key, formed) {
  for (const id of formed.granted_tools) formed.tool_descriptor_hashes[id] = descriptorHash(descriptorBytes(id));
  const wire = await wireConnection(descriptor, key);
  if (!wireOwners.has(descriptor)) wireOwners.set(descriptor, new Map());
  const owners = wireOwners.get(descriptor);
  const common = { schema_version: 2, request_kind: 'invocation', host_generation: descriptor.host_generation, challenge: wire.challenge, invocation_nonce: crypto.randomBytes(16).toString('hex'), invocation_handle: null, source_movement_seq: 1, artifact_content_hash: '5'.repeat(64), formation: formed, tool: { id: 'browser.navigate', operation: 'navigate' }, arguments: { url: 'https://example.test' }, effective_allow_network: formed.network_ceiling !== 'denied' };
  return { socket: wire.socket, common, next: wire.next, wire, owners, async request(method, params = {}, target_id = null, changes = {}) {
    const candidate = { ...common, operation: { method, params, target_id, session_id: null }, ...changes };
    const bytes = descriptorBytes(candidate.tool.id), declaration = JSON.parse(bytes);
    const ownerKey = `${formed.worker_environment_id}:${canonical(declaration.process)}`;
    let owner = owners.get(ownerKey);
    if (!owner) { owner = { wire: await wireConnection(descriptor, key), instance: crypto.randomBytes(16).toString('hex'), active: null, seq: 0 }; owners.set(ownerKey, owner); }
    candidate.executor = { instance_nonce: owner.instance, connection_role: 'member' };
    candidate.tool_descriptor = bytes.toString('base64');
    if (!owner.active || candidate.invocation_nonce !== owner.active.invocation_nonce && candidate.source_movement_seq > owner.seq) {
      if (owner.active) {
        const ended = await owner.wire.send({ ...owner.active, executor: { instance_nonce: owner.instance, connection_role: 'owner' }, operation: { target_id: null, session_id: null, method: 'OctopusBrowser.endInvocation', params: {} } });
        if (ended.error) return ended;
      }
      const begin = { ...candidate, invocation_handle: null, executor: { instance_nonce: owner.instance, connection_role: 'owner' }, operation: { target_id: null, session_id: null, method: 'OctopusBrowser.beginInvocation', params: {} } };
      const begun = await owner.wire.send(begin);
      if (begun.error) return begun;
      owner.active = { ...begin, invocation_handle: begun.result.invocation_handle }; owner.seq = candidate.source_movement_seq;
    }
    candidate.invocation_handle = owner.active.invocation_handle;
    if (candidate.invocation_nonce === common.invocation_nonce) common.invocation_handle = candidate.invocation_handle;
    const result = await wire.send(candidate);
    this.events = wire.events;
    return result;
  } };
}

async function historySourceLifetime(t) {
  const vm = require('node:vm');
  const controllerFile = path.resolve(__dirname, '../browser-history-control.cjs');
  const hostFile = path.resolve(__dirname, '../electron-host.cjs');
  const actualHistory = require('../browser-navigation-history.cjs');
  const leases = [], definitions = new Map(); let failNextClose = false;
  const fakeOpen = async options => {
    const id = canonical(options.sourceRef), existing = definitions.get(id);
    const epoch = existing?.epoch || crypto.randomUUID(); definitions.set(id, { epoch });
    const state = { options, epoch, closed: false, queries: 0, observations: 0, closes: 0 }; leases.push(state);
    return { sourceRef: options.sourceRef, payloadDigest: options.payloadDigest, sourceEpoch: epoch,
      async search(query) {
        assert.equal(state.closed, false); options.revalidate(); state.queries++;
        return { source_ref: options.sourceRef, source_payload_digest: options.payloadDigest, source_epoch: epoch,
          items: [{ url: 'https://observed.test/', title: 'Native fixture row', dateVisited: '2026-09-01T00:00:00.000Z' }],
          native_query_evidence: { query_digest: crypto.createHash('sha256').update(canonical(query)).digest('hex') } };
      },
      observeReady(guest) { options.revalidateNow(); assert.equal(state.closed, false); state.observations++; assert.equal(guest.loads, undefined); },
      close() { state.closes++; if (failNextClose) { failNextClose = false; throw new Error('Injected history close failure'); } state.closed = true; }
    };
  };
  function load(file, replacement) {
    const module = { exports: {} }, localRequire = createRequire(file);
    vm.runInNewContext(require('node:fs').readFileSync(file, 'utf8'), {
      module, exports: module.exports, require: id => Object.hasOwn(replacement, id) ? replacement[id] : localRequire(id),
      __filename: file, __dirname: path.dirname(file), Buffer, process, structuredClone, AbortController,
      setTimeout, clearTimeout, setImmediate, clearImmediate, URL, TextDecoder, queueMicrotask
    }, { filename: file });
    return module.exports;
  }
  const controller = load(controllerFile, { './browser-navigation-history.cjs': { ...actualHistory, openNavigationHistorySource: fakeOpen } });
  const native = load(hostFile, { './browser-history-control.cjs': controller });
  const root = await fs.realpath(await fs.mkdtemp('/tmp/octopus-native-history-contract-'));
  const electron = injectedElectron(), host = native.createBrowserEngineHost({ electron, hostRoot: root, descriptorHash });
  const descriptor = await host.start(), key = await fs.readFile(`${root}/auth.key`), sockets = [];
  t.after(async () => { sockets.forEach(value => value.socket.destroy()); await host.stop(); await fs.rm(root, { recursive: true }); });
  const formed = formation('history-source');
  formed.granted_tools.push('browser.history', 'browser.history_resume'); formed.granted_tools.sort();
  const first = await client(descriptor, key, formed); sockets.push(first);
  const source = { id: 'selected-history-source', content: Array(32).fill(6) };
  const definition = { schema_version: 1, source_id: '33333333-3333-3333-3333-333333333333', owner_interface: 'system_browser_history', collection: 'explicit_door_selection',
    query_policy: { model_read_enabled: true, request_timeout_ms: 1000, max_queries: 8, max_query_bytes: 4096, max_results: 100, max_result_bytes: 1048576 } };
  const payload = Buffer.from(JSON.stringify(definition));
  const sourceParams = { source_ref: source, source_payload_digest: crypto.createHash('sha256').update(payload).digest('hex'), payload_base64: payload.toString('base64') };
  const query = { queries: ['observed'] };
  const readCall = { source_movement_seq: 1, invocation_nonce: 'a'.repeat(32), tool: { id: 'browser.history', operation: 'history' }, arguments: { source, ...query }, effective_allow_network: false };
  const prepared = await first.request('OctopusBrowser.prepareHistorySource', { ...sourceParams, query }, null, readCall);
  assert.ifError(prepared.error); const identity = prepared.result;
  assert.equal(leases.length, 1); assert.equal(leases[0].queries, 0); assert.equal(electron.sessions.size, 0); assert.equal(electron.contents.length, 0);
  const commitment = { schema_version: 1, original: { run: formed.run, ee: formed.ee, worker: formed.worker, door_interface: formed.door_interface,
    tool: readCall.tool.id, correlation_id: 'history-call', arguments: readCall.arguments, source_movement_seq: 1,
    source_artifact: { id: 'original-history-call', content: Array(32).fill(0x55) } },
    scope: { worker_environment_id: formed.worker_environment_id, formation_pin: formed.formation_pin, host_generation: descriptor.host_generation },
    approval_scope: 'browser_history_read', history: { source, source_payload_digest: sourceParams.source_payload_digest, source_epoch: identity.source_epoch, query },
    permission_offer: { client_interface: 'world_operator_history_fixture', offered_choices: [{ decision: 'accept', persistence: 'once' }, { decision: 'accept', persistence: 'always' }, { decision: 'decline', persistence: 'once' }] } };
  const hash = crypto.createHash('sha256').update(canonical(commitment)).digest('hex');
  const request = { ...commitment, request_id: `sha256:${hash}`, call_id: 'history-call', resume: { tool: 'browser.history_resume', correlation_id: `browser-history-resume:${hash}` } };
  assert.equal((await first.request('OctopusBrowser.stageHistoryRequest', { request }, null, readCall)).result.status, 'prepared');
  assert.equal((await first.request('Target.createBrowserContext', {}, null, readCall)).error.code, 'method_denied');
  const owner = [...first.owners.values()][0];
  assert.equal((await owner.wire.send({ ...owner.active, operation: { method: 'OctopusBrowser.endInvocation', params: {}, target_id: null, session_id: null } })).result.ended, true);
  owner.active = null;
  assert.equal(leases[0].closed, false, 'ending a control invocation does not release the room-owned Reader');
  const closedMember = once(first.socket, 'close'); first.socket.destroy(); await closedMember;
  const closedOwner = once(owner.wire.socket, 'close'); owner.wire.socket.destroy(); await closedOwner;
  first.owners.clear();
  await new Promise(resolve => setTimeout(resolve, 20));
  const resume = await client(descriptor, key, formed); sockets.push(resume);
  const resumeCall = { source_movement_seq: 2, invocation_nonce: 'b'.repeat(32), tool: { id: 'browser.history_resume', operation: 'history_resume' }, arguments: { request_id: request.request_id }, effective_allow_network: false };
  const saved = await resume.request('OctopusBrowser.rememberPermission', { request_id: request.request_id,
    response: { request_id: request.request_id, call_id: request.call_id, decision: 'accept', selected_persistence: 'always' },
    source_artifact: { id: 'admitted-history-answer', content: Array(32).fill(0x55) } }, null, resumeCall);
  assert.ifError(saved.error); assert.equal(saved.result.status, 'saved'); assert.equal(leases[0].queries, 0);
  const result = await resume.request('OctopusBrowser.applyHistoryRequest', { request_id: request.request_id, decision: 'accept' }, null, resumeCall);
  assert.ifError(result.error); assert.equal(result.result.source_epoch, identity.source_epoch); assert.equal(result.result.status, 'completed');
  assert.equal(leases.length, 1, 'resume uses the retained Reader across member and owner reconnect'); assert.equal(leases[0].queries, 1);
  assert.equal(electron.sessions.size, 0); assert.equal(electron.contents.length, 0);
  assert((await resume.request('OctopusBrowser.applyHistoryRequest', { request_id: request.request_id, decision: 'accept' }, null, resumeCall)).error);
  assert.equal(leases[0].queries, 1, 'consumed native history request is not queried twice');
  const again = { ...readCall, source_movement_seq: 3, invocation_nonce: 'c'.repeat(32) };
  const retained = await resume.request('OctopusBrowser.prepareHistorySource', { ...sourceParams, query }, null, again);
  assert.ifError(retained.error); assert.equal(retained.result.source_epoch, identity.source_epoch); assert.equal(leases.length, 1);
  const nextCommitment = { ...commitment, original: { ...commitment.original, correlation_id: 'second-history-call', source_movement_seq: 3 } };
  const nextHash = crypto.createHash('sha256').update(canonical(nextCommitment)).digest('hex');
  const nextRequest = { ...nextCommitment, request_id: `sha256:${nextHash}`, call_id: 'second-history-call', resume: { tool: 'browser.history_resume', correlation_id: `browser-history-resume:${nextHash}` } };
  assert.equal((await resume.request('OctopusBrowser.stageHistoryRequest', { request: nextRequest }, null, again)).result.status, 'prepared');
  const reused = await resume.request('OctopusBrowser.lookupPermission', { request_id: nextRequest.request_id }, null, again);
  assert.ifError(reused.error); assert.equal(reused.result.id, saved.result.evidence.id);
  assert.equal(leases[0].queries, 1, 'reusing prior decision evidence does not execute a read');
  const narrowedCall = { ...again, source_movement_seq: 4, invocation_nonce: 'e'.repeat(32) };
  const narrowedCommitment = { ...commitment, original: { ...commitment.original, correlation_id: 'narrowed-history-call', source_movement_seq: 4 },
    permission_offer: { ...commitment.permission_offer, offered_choices: commitment.permission_offer.offered_choices.filter(choice => choice.persistence !== 'always') } };
  const narrowedHash = crypto.createHash('sha256').update(canonical(narrowedCommitment)).digest('hex');
  const narrowed = { ...narrowedCommitment, request_id: `sha256:${narrowedHash}`, call_id: 'narrowed-history-call', resume: { tool: request.resume.tool, correlation_id: `browser-history-resume:${narrowedHash}` } };
  assert.equal((await resume.request('OctopusBrowser.stageHistoryRequest', { request: narrowed }, null, narrowedCall)).result.status, 'prepared');
  assert.equal((await resume.request('OctopusBrowser.lookupPermission', { request_id: narrowed.request_id }, null, narrowedCall)).result, null, 'ineligible always choice never becomes once');
  const declineCall = { ...resumeCall, source_movement_seq: 5, invocation_nonce: 'f'.repeat(32), arguments: { request_id: narrowed.request_id } };
  const declined = await resume.request('OctopusBrowser.applyHistoryRequest', { request_id: narrowed.request_id, decision: 'decline' }, null, declineCall);
  assert.equal(declined.result.status, 'declined'); assert.equal(leases[0].queries, 1);

  const collector = { source_movement_seq: 1, invocation_nonce: 'd'.repeat(32), tool: { id: 'browser.navigate', operation: 'navigate' }, arguments: { url: 'https://observed.test/' }, effective_allow_network: true };
  const collecting = await client(descriptor, key, formed); sockets.push(collecting);
  const selected = await collecting.request('OctopusBrowser.selectHistoryCollection', sourceParams, null, collector);
  assert.ifError(selected.error); assert.equal(selected.result.source_epoch, identity.source_epoch);
  assert.equal(leases.length, 2, 'read and collection are distinct Reader leases over the existing native source pool');
  assert.equal(electron.sessions.size, 0, 'collector preparation also precedes Session birth');
  const context = (await collecting.request('Target.createBrowserContext', {}, null, collector)).result;
  assert((await collecting.request('Target.createTarget', { browserContextId: context.browserContextId, url: 'https://observed.test/' }, null, collector)).result.targetId);
  assert.equal(leases[1].observations, 1); assert.equal(leases[0].observations, 0);
  failNextClose = true;
  const withdrawal = { worker_environment_id: formed.worker_environment_id, formation_pin: formed.formation_pin, reason: 'Exact selected room withdrawn' };
  await assert.rejects(host.revokeFormation(withdrawal), /Native history reader cleanup/);
  assert.equal(leases[0].closed, false); assert.equal(leases[1].closed, true);
  resume.owners.clear();
  const withdrawn = await client(descriptor, key, formed); sockets.push(withdrawn);
  const lateResume = { ...resumeCall, source_movement_seq: 6, invocation_nonce: 'e'.repeat(32), arguments: { request_id: nextRequest.request_id } };
  assert.equal((await withdrawn.request('OctopusBrowser.applyHistoryRequest', { request_id: nextRequest.request_id, decision: 'accept' }, null, lateResume)).error.code, 'formation_withdrawn');
  assert.equal(leases[0].queries, 1, 'withdrawal invalidates the unconsumed original proof before another query');
  await host.revokeFormation(withdrawal);
  assert.equal(leases[0].closed, true); assert.equal(leases[0].closes, 2); assert.equal(leases[1].closes, 1);
  const listed = await host.savedPermissions({ operation: 'list' }, () => {});
  assert.equal(listed.entries.length, 1); assert.equal(listed.entries[0].id, saved.result.evidence.id);
  const remove = { operation: 'remove', hostGeneration: listed.hostGeneration, id: listed.entries[0].id };
  await assert.rejects(host.savedPermissions({ ...remove, hostGeneration: 'old' }, () => {}), /replaced/);
  await assert.rejects(host.savedPermissions(remove, () => { throw new Error('Client access withdrawn'); }), /withdrawn/);
  assert.equal((await host.savedPermissions({ operation: 'list' }, () => {})).entries.length, 1);
  assert.equal((await host.savedPermissions(remove, () => {})).removed, true);
  assert.equal((await host.savedPermissions(remove, () => {})).removed, false);
  assert.equal(leases[0].queries, 1, 'Settings removal never queries or recreates a history source');
}

test('standard mode keeps capture and model geometry across native presentation transfers', async t => {
  const root = await fs.realpath(await fs.mkdtemp('/tmp/octopus-standard-contract-'));
  const electron = injectedElectron();
  const window = new electron.BrowserWindow({ webPreferences: { session: electron.session.fromPartition('client') } });
  const Contents = window.webContents.constructor;
  for (const key of ['isOwnedGuest', 'createOwnedGuestLease', 'cancelOwnedGuestLease', 'releaseOwnedGuest', 'setOwnedGuestVisibility', 'setOwnedGuestPaintLease', 'destroy']) delete Contents.prototype[key];
  Contents.prototype.close = function(options) { assert.deepEqual(options, { waitForBeforeUnload: false }); this.destroyed = true; this.emit('destroyed'); };
  delete electron.webContents.createOwnedGuest; delete electron.session.openOwnedDownloadHistory;
  const views = [];
  electron.WebContentsView = class {
    constructor(options) { this.webContents = options.webContents ?? new Contents(options.webPreferences.session); views.push(this); }
    setBounds(value) { this.bounds = value; } setVisible(value) { this.visible = value; }
  };
  const prepareWindow = value => {
    value.children = new Set(); value.contentView = { addChildView(view) { value.children.add(view); }, removeChildView(view) { value.children.delete(view); } };
    value.getContentBounds = () => ({ x: 0, y: 0, width: 1200, height: 800 }); return value;
  };
  electron.BaseWindow = class extends EventEmitter {
    constructor() { super(); prepareWindow(this); } isDestroyed() { return this.destroyed === true; } destroy() { this.destroyed = true; }
  };
  prepareWindow(window);
  const host = createBrowserEngineHost({ electron, hostRoot: root, engineMode: 'standard' });
  const descriptor = await host.start(), key = await fs.readFile(`${root}/auth.key`), formed = formation('standard');
  formed.granted_tools.push('browser.history'); formed.granted_tools.sort();
  const a = await client(descriptor, key, formed);
  const b = await client(descriptor, key, formed);
  t.after(async () => { a.socket.destroy(); b.socket.destroy(); await host.stop(); await fs.rm(root, { recursive: true }); });
  const context = (await a.request('Target.createBrowserContext', {})).result;
  const created = await a.request('Target.createTarget', { browserContextId: context.browserContextId, url: 'about:blank' });
  assert.ifError(created.error); const id = created.result.targetId, page = views[0].webContents;
  assert.equal(page.getURL(), 'about:blank'); assert.equal(page.setOwnedGuestPaintLease, undefined);
  const projection = { protocol: 'browser.native.v1', engine_handle: context.engine_handle, host_generation: descriptor.host_generation,
    target_id: id, browser_context_id: context.browserContextId, worker_environment_id: formed.worker_environment_id, formation_pin: formed.formation_pin };
  const call = (seq, id, operation, args) => ({ source_movement_seq: seq, invocation_nonce: String(seq).repeat(32), tool: { id, operation }, arguments: args });
  const viewport = await a.request('OctopusBrowser.setViewport', { viewport: { width: 900, height: 600 } }, id, call(2, 'browser.viewport_set', 'viewport_set', { width: 900, height: 600 }));
  assert.ifError(viewport.error); assert.equal(viewport.result.model_viewport.revision, 2);
  const history = await b.request('OctopusBrowser.prepareHistorySource', {}, null, { ...call(3, 'browser.history', 'history', { source: { id: 'history', content: Array(32).fill(1) }, queries: ['test'] }), effective_allow_network: false });
  assert.equal(history.error.code, 'history_contract_failed', history.error.message);
  const screenshot = call(4, 'browser.screenshot', 'screenshot', {});
  assert.ifError((await a.request('Page.captureScreenshot', {}, id, screenshot)).error);
  const prepared = host.preparePresentation({ attachmentId: 'view', projection, window, sender: window.webContents, surfaceId: 'surface', frameToken: 'standard-frame' });
  assert.equal(prepared.adoptionToken, null); assert.equal(prepared.state.presentationPhase, 'attached');
  assert.equal(prepared.state.presentationProtocol, 'electron.webContentsView.v1'); assert.equal(prepared.state.engine.capabilities.hiddenPageCapture, true);
  const packet = { attachmentId: 'view', hostGeneration: descriptor.host_generation, mountGeneration: prepared.mountGeneration,
    projection, sender: window.webContents, viewportSequence: 1, metricsRevision: 2, scale: 1, visible: true,
    stageBounds: { x: 100, y: 100, width: 800, height: 600 }, pageBounds: { x: 100, y: 100, width: 800, height: 600 }, visualBounds: { x: 100, y: 100, width: 800, height: 600 } };
  const ready = await host.acknowledgePresentation(packet);
  assert.equal(ready.presentationPhase, 'ready'); assert.equal(views[0].visible, true); assert.deepEqual(views[0].bounds, packet.visualBounds);
  const captured = await a.request('Page.captureScreenshot', {}, id, call(5, 'browser.screenshot', 'screenshot', {})); assert.ifError(captured.error);
  await assert.rejects(host.syncPresentation({ ...packet, viewportSequence: 2, scale: 0.5 }), /scaled guest/); assert.equal(window.children.size, 0);
  const replacement = prepareWindow(new electron.BrowserWindow({ webPreferences: { session: window.webContents.session } }));
  const moved = host.preparePresentation({ attachmentId: 'moved', projection, window: replacement, sender: replacement.webContents, surfaceId: 'surface', frameToken: 'destination' });
  assert.equal(window.children.size, 0); assert.equal(replacement.children.size, 0); assert.equal(views.length, 1); assert.equal(page.isDestroyed(), false);
  await host.acknowledgePresentation({ ...packet, attachmentId: 'moved', sender: replacement.webContents, mountGeneration: moved.mountGeneration });
  assert.equal(replacement.children.has(views[0]), true);
  const { target_id: _standardTarget, ...standardRoom } = projection; await host.syncTurn(standardRoom, { open: false, openingMovementSeq: 0, observedMovementSeq: 5 });
  assert.equal(page.isDestroyed(), false); assert.equal(host.getTarget(projection).lifetime, 'persistent'); assert.equal(host.getTarget(projection).model_controlled, false);
  await host.acknowledgePresentation({ ...packet, attachmentId: 'moved', sender: replacement.webContents, mountGeneration: moved.mountGeneration, metricsRevision: 5, viewportSequence: 2 });
  assert.equal((await host.viewerAction({ attachmentId: 'moved', sender: replacement.webContents, hostGeneration: descriptor.host_generation, mountGeneration: moved.mountGeneration, action: 'navigate', params: { url: 'https://retained.test/' } })).url, 'https://retained.test/'); assert.equal(views.length, 1);
  assert.throws(() => host.releasePresentation({ attachmentId: 'moved', sender: window.webContents, hostGeneration: descriptor.host_generation, mountGeneration: moved.mountGeneration }), /another sender/);
  const downloads = await host.listDownloadHistoryForProjection({ worker_environment_id: formed.worker_environment_id, formation_pin: formed.formation_pin });
  assert.equal(downloads.downloads.error, null); assert.equal(downloads.downloads.historyBackend, 'octopus.standardDownloadHistory.v1'); assert.deepEqual(downloads.downloads.downloads, []);
  await host.closeTarget(projection); assert.equal(page.isDestroyed(), true); assert.equal(replacement.children.size, 0);
});

test('authenticated room ownership, native presentation and bounded media transport with an injected engine', async t => {
  const root = await fs.realpath(await fs.mkdtemp('/tmp/octopus-native-contract-'));
  const electron = injectedElectron(); const events = []; let cursorHook;
  const host = createBrowserEngineHost({ electron, hostRoot: root, onEvent: event => { events.push(event); cursorHook?.(event); } });
  const descriptor = await host.start(); const key = await fs.readFile(`${root}/auth.key`);
  const clients = [];
  t.after(async () => { clients.forEach(client => client.socket.destroy()); await host.stop(); await fs.rm(root, { recursive: true }); });
  assert.equal((await fs.stat(`${root}/auth.key`)).mode & 0o777, 0o600);
  assert.equal(electron.contents.length, 0);
  const a = await client(descriptor, key, formation('a')); clients.push(a);
  assert.equal((await a.request('Target.createBrowserContext', {}, null, { effective_allow_network: false })).error.code, 'authority_denied');
  assert.equal(electron.contents.length, 0); assert.equal(electron.sessions.size, 0);
  const context = (await a.request('Target.createBrowserContext', { disposeOnDetach: false })).result;
  const factory = electron.webContents.createOwnedGuest; delete electron.webContents.createOwnedGuest;
  assert.equal((await a.request('Target.createTarget', { browserContextId: context.browserContextId, url: 'about:blank' })).error.code, 'engine_unavailable');
  assert.equal(electron.contents.length, 0); electron.webContents.createOwnedGuest = factory;
  const id = (await a.request('Target.createTarget', { browserContextId: context.browserContextId, url: 'about:blank' })).result.targetId;
  const b = await client(descriptor, key, formation('b')); clients.push(b);
  const other = (await b.request('Target.createBrowserContext', {})).result;
  assert.notEqual(context.browserContextId, other.browserContextId);
  assert.deepEqual((await b.request('Target.getTargets')).result.targetInfos, []);
  assert.equal((await b.request('Page.enable', {}, id)).error.code, 'target_unavailable');
  assert.equal((await a.request('Page.navigate', { url: 'file:///private/example' }, id)).error.code, 'navigation_denied');
  assert.equal((await a.request('Browser.close', {}, id)).error.code, 'method_denied');
  assert.equal((await a.request('Page.enable', {}, id, { arguments: { url: 'https://changed.test' } })).error.code, 'replay_rejected');
  assert.deepEqual((await a.request('Page.enable', {}, id)).result, {});
  for (const [method, params] of [['Page.getFrameTree', {}], ['Page.setLifecycleEventsEnabled', { enabled: true }]]) assert.deepEqual((await a.request(method, params, id)).result, {});
  assert.equal((await b.request('Page.getFrameTree', {}, id)).error.code, 'target_unavailable');
  assert.equal((await a.request('Page.getFrameTree', { sessionId: 'foreign' }, id)).error.code, 'authority_denied');
  assert.equal((await a.request('DOM.getContentQuads', { backendNodeId: 1 }, id)).error.code, 'method_denied');
  const projection = { protocol: 'browser.native.v1', engine_handle: context.engine_handle, host_generation: descriptor.host_generation, target_id: id, browser_context_id: context.browserContextId, worker_environment_id: 'a', formation_pin: formation('a').formation_pin };
  const window = new electron.BrowserWindow({ webPreferences: { session: electron.session.fromPartition('client') } }); window.webContents.zoom = 2;
  const guest = electron.contents.find(contents => contents.isOwnedGuest());
  assert.equal(guest.paintLease, true); assert.equal(guest.hostWebContents, null);
  assert.equal(electron.factoryOptions.length, 1); assert.equal(electron.factoryOptions[0].session, guest.session);
  assert.deepEqual(electron.factoryOptions[0].initialSurfaceSize, { width: 1280, height: 720 });
  for (const field of ['width', 'height', 'type', 'embedder', 'offscreen']) assert.equal(Object.hasOwn(electron.factoryOptions[0], field), false);
  const prepare = (attachmentId, owner = window, frameToken = 'frame-a') => host.preparePresentation({ attachmentId, projection, window: owner, sender: owner.webContents, frameToken, surfaceId: 'page' });
  const epochs = (result, attachmentId = 'view', owner = window) => ({ attachmentId, sender: owner.webContents, hostGeneration: result.hostGeneration, mountGeneration: result.mountGeneration });
  const layout = (result, attachmentId = 'view', owner = window, viewportSequence = 1) => ({ ...epochs(result, attachmentId, owner), projection, viewportSequence, metricsRevision: 0, stageBounds: { x: -5, y: 40, width: 200, height: 200 }, pageBounds: { x: -5, y: 40, width: 200, height: 200 }, visualBounds: { x: -5, y: 40, width: 200, height: 200 }, scale: 1, visible: true });
  const prepared = prepare('view');
  assert.equal(prepared.state.connected, true); assert.equal(prepared.state.presentationPhase, 'prepared'); assert.equal(prepared.state.visible, false);
  assert.deepEqual(prepared.state.modelViewport, { revision: 0, emulatedSize: null }); assert.equal(prepared.state.viewportSequence, 0);
  assert.equal(prepared.state.readOnly, false); assert.equal(Object.hasOwn(prepared.state, 'manualControl'), false);
  assert(host.validatePreparedGuest({ sender: window.webContents, guest }));
  assert.equal(host.validatePreparedGuest({ sender: guest, guest }), false);
  assert.throws(() => prepare('duplicate-frame'), { code: 'presentation_owned' });
  assert.throws(() => prepare('invalid-frame', window, 'unknown-frame'), { code: 'attachment_unavailable' });
  await assert.rejects(host.acknowledgePresentation(layout(prepared)), { code: 'attachment_unavailable' });
  await assert.rejects(host.viewerAction({ ...epochs(prepared), action: 'focus' }), { code: 'attachment_unavailable' });
  assert.equal((await host.syncPresentation(layout(prepared))).presentationPhase, 'prepared');
  assert.equal(host.getTarget(projection).lifetime, 'temporary', 'failed adoption and unacknowledged preparation cannot retain a page');
  guest.adopt(prepared.adoptionToken);
  assert.equal(events.at(-1).state.presentationPhase, 'attached'); assert.equal(guest.visible, false);
  assert.equal(host.validatePreparedGuest({ sender: window.webContents, guest }), false);
  const mounted = await host.acknowledgePresentation(layout(prepared));
  assert.equal(mounted.presentationPhase, 'ready'); assert.equal(mounted.visible, true); assert.equal(guest.focused, undefined);
  const popupResponse = guest.popup({ url: 'https://popup.test', referrer: { url: 'https://example.test', policy: 'default' } });
  assert.equal(popupResponse.outlivesOpener, true);
  const supplied = electron.webContents.createOwnedGuest({ session: guest.session });
  assert.throws(() => popupResponse.createWindow({}), { code: 'engine_unavailable' });
  assert.throws(() => popupResponse.createWindow({ webContents: guest }), { code: 'authority_denied' });
  assert.equal(popupResponse.createWindow({ webContents: supplied }), supplied);
  assert.equal(popupResponse.overrideBrowserWindowOptions.webPreferences.session, guest.session);
  assert.equal(supplied.focused, undefined); assert.equal(supplied.url, ''); assert.equal(supplied.paintLease, false);
  guest.emit('context-menu', {}, { linkURL: 'https://link.test', editFlags: {}, mediaType: 'none' });
  electron.menus.at(-1).find(item => item.label === 'Open link in new tab').click();
  const listing = await client(descriptor, key, formation('a')); clients.push(listing); Object.assign(listing.common, a.common, { challenge: listing.common.challenge });
  const owned = (await listing.request('Target.getTargets')).result.targetInfos;
  assert.equal(owned.length, 3);
  assert(owned.slice(1).every(info => info.openerId === id && info.browserContextId === context.browserContextId));
  assert(owned.some(info => info.url === 'https://link.test'));
  assert.deepEqual((await b.request('Target.getTargets')).result.targetInfos, []);
  const { target_id: _target, ...roomProjection } = projection;
  const inventory = host.listTargetsForProjection(roomProjection);
  assert.equal(inventory.targets.length, 3);
  assert.equal(inventory.openIntent.source, 'context-menu');
  assert.equal(inventory.openIntent.id, 2);
  assert(events.some(event => event.type === 'targets' && event.browserContextId === context.browserContextId));
  await host.closeTarget({ ...projection, target_id: inventory.openIntent.targetId });
  assert.equal(host.listTargetsForProjection(roomProjection).targets.length, 2);
  assert.equal(host.listTargetsForProjection(roomProjection).openIntent, null);
  const nativeCallsBeforeFocus = guest.calls.length;
  assert.equal(guest.focused, undefined);
  host.requestPageFocus(projection);
  assert.equal(host.listTargetsForProjection(roomProjection).openIntent.source, 'pip');
  assert.equal(guest.focused, true); assert.equal(window.focused, true);
  assert.equal(guest.calls.length, nativeCallsBeforeFocus, 'human PiP return cannot synthesize a model command');
  assert.throws(() => host.requestPageFocus({ ...projection, host_generation: 'stale' }), { code: 'host_unavailable' });
  await host.viewerAction({ ...epochs(prepared), action: 'navigate', params: { url: 'https://example.test' } });
  assert.equal(guest.url, 'https://example.test');
  assert.equal((await host.syncPresentation({ ...layout(prepared, 'view', window, 2), visible: false })).visible, false);
  assert.equal(guest.paintLease, true);
  const shown = layout(prepared, 'view', window, 3);
  assert.equal((await host.syncPresentation(shown)).visible, true);
  const visibilityCount = guest.visibilityCalls.length;
  await host.syncPresentation(shown); assert.equal(guest.visibilityCalls.length, visibilityCount);
  await assert.rejects(host.syncPresentation({ ...shown, visible: false }), { code: 'viewport_sequence_conflict' });
  await assert.rejects(host.syncPresentation(layout(prepared)), { code: 'viewport_sequence_stale' });
  await assert.rejects(host.syncPresentation({ ...shown, viewportSequence: 4, metricsRevision: 1 }), { code: 'metrics_revision_stale' });
  assert.throws(() => host.syncPresentation({ ...shown, hostGeneration: 'stale' }), { code: 'host_unavailable' });
  window.visible = false; window.emit('hide'); assert.equal(guest.visible, false);
  window.visible = true; window.emit('show'); assert.equal(guest.visible, true);
  assert.equal(guest.calls.some(call => call.method === 'Emulation.setDeviceMetricsOverride'), false);
  const clickChanges = { source_movement_seq: 2, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: 'browser.click', operation: 'click' }, arguments: { ref: '@m1e1' } };
  assert.deepEqual((await a.request('DOM.describeNode', { backendNodeId: 1 }, id, clickChanges)).result, {});
  cursorHook = event => {
    if (event.type !== 'state' || !event.state.cursor) return;
    const cursor = event.state.cursor;
    assert.deepEqual([cursor.x, cursor.y], [83, 97]);
    assert.equal(host.cursorArrived({ ...epochs(prepared), projection, moveSequence: cursor.moveSequence, metricsRevision: 1 }).accepted, false);
    assert.equal(host.cursorArrived({ ...epochs(prepared), projection, moveSequence: cursor.moveSequence, metricsRevision: 0 }).accepted, true);
  };
  const inputCount = guest.calls.length;
  assert.equal((await a.request('OctopusBrowser.moveCursor', { x: 83, y: 97, waitForArrival: true }, id, clickChanges)).result.status, 'arrived');
  assert.equal(guest.calls.length, inputCount, 'presentation cursor cannot synthesize CDP/native input');
  cursorHook = null;
  window.visible = false;
  assert.equal((await a.request('OctopusBrowser.moveCursor', { x: 90, y: 100, waitForArrival: true }, id, clickChanges)).result.status, 'unpresented');
  window.visible = true;
  for (const method of ['Page.getFrameTree', 'Page.setLifecycleEventsEnabled']) assert.deepEqual((await a.request(method, method === 'Page.getFrameTree' ? {} : { enabled: true }, id, { source_movement_seq: 3, invocation_nonce: '8'.repeat(32), tool: { id: 'browser.key', operation: 'key' }, arguments: { key: 'Enter' } })).result, {});
  const captureChanges = { source_movement_seq: 4, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: 'browser.screenshot', operation: 'screenshot' }, arguments: {} };
  for (const method of ['Page.getFrameTree', 'Page.getLayoutMetrics', 'Runtime.evaluate']) assert.deepEqual((await a.request(method, method === 'Runtime.evaluate' ? { expression: 'window.devicePixelRatio', returnByValue: true } : {}, id, captureChanges)).result, {});
  assert.equal((await a.request('Page.setLifecycleEventsEnabled', { enabled: true }, id, captureChanges)).error.code, 'method_denied');
  const media = await a.request('Page.captureScreenshot', {}, id, captureChanges);
  assert.equal(media.result.data.length, 17 * 1024 * 1024);
  assert.equal((await a.next()).method, 'Network.loadingFinished');
  const frameChanges = { source_movement_seq: 5, invocation_nonce: '9'.repeat(32), tool: { id: 'browser.click', operation: 'click' }, arguments: { ref: '@m1e1' } };
  const callFrame = (method, session_id, params = {}) => a.request(method, params, id, { ...frameChanges, operation: { target_id: id, session_id, method, params } });
  guest.debugger.emit('message', {}, 'Target.attachedToTarget', { sessionId: 'child', targetInfo: { targetId: 'iframe-1', type: 'iframe' } });
  assert.equal((await a.next()).params.sessionId, 'child');
  let frameReply = await callFrame('Page.enable', null);
  assert.deepEqual(frameReply.frame_inventory.sessions, [{ session_id: 'child', parent_session_id: null, target_id: 'iframe-1', attachment_seq: 1 }]);
  assert.equal((await callFrame('Page.enable', 'foreign')).error.code, 'session_unavailable');
  assert.equal((await callFrame('Target.setAutoAttach', 'child', { autoAttach: true, flatten: true, waitForDebuggerOnStart: false })).error.code, 'invalid_request');
  assert.deepEqual((await callFrame('Target.setAutoAttach', 'child', { autoAttach: true, flatten: true, waitForDebuggerOnStart: false, filter: [{ type: 'iframe', exclude: false }] })).result, {});
  guest.debugger.emit('message', {}, 'Target.attachedToTarget', { sessionId: 'nested', targetInfo: { targetId: 'iframe-2', type: 'iframe' } }, 'child');
  assert.equal((await a.next()).sessionId, 'child');
  frameReply = await callFrame('Page.enable', null);
  assert.deepEqual(frameReply.frame_inventory.sessions.map(entry => [entry.session_id, entry.parent_session_id, entry.attachment_seq]), [['child', null, 1], ['nested', 'child', 2]]);
  frameReply = await callFrame('Input.dispatchMouseEvent', 'nested', { type: 'mouseMoved', x: 1, y: 2 });
  assert.equal(frameReply.sessionId, 'nested');
  assert.equal(guest.calls.at(-1).sessionId, 'nested');
  assert.equal(guest.calls.at(-2).method, 'Emulation.setFocusEmulationEnabled');
  assert.equal(guest.calls.at(-2).sessionId, 'nested');
  assert.equal((await callFrame('Page.createIsolatedWorld', 'nested', { frameId: 'iframe-2', worldName: 'octopus.browser.actions', grantUniveralAccess: true })).error.code, 'authority_denied');
  const originalSend = guest.debugger.sendCommand;
  guest.debugger.sendCommand = async (method, params, sessionId) => method === 'Page.getFrameTree' ? { frameTree: { frame: { id: 'iframe-2', loaderId: 'loader-2' } } } : method === 'Page.createIsolatedWorld' ? { executionContextId: 90 } : originalSend(method, params, sessionId);
  assert.equal((await callFrame('Page.createIsolatedWorld', 'nested', { frameId: 'foreign-frame', worldName: 'octopus.browser.actions', grantUniveralAccess: false })).error.code, 'authority_denied');
  assert.equal((await callFrame('Page.createIsolatedWorld', 'nested', { frameId: 'iframe-2', worldName: 'octopus.browser.actions', grantUniveralAccess: false })).result.executionContextId, 90);
  assert.deepEqual((await callFrame('DOM.resolveNode', 'nested', { backendNodeId: 7, executionContextId: 90 })).result, {});
  assert.equal((await callFrame('DOM.resolveNode', 'child', { backendNodeId: 7, executionContextId: 90 })).error.code, 'authority_denied');
  guest.debugger.emit('message', {}, 'Runtime.executionContextsCleared', {}, 'nested');
  assert.equal((await a.next()).method, 'Runtime.executionContextsCleared');
  assert.equal((await callFrame('DOM.resolveNode', 'nested', { backendNodeId: 7, executionContextId: 90 })).error.code, 'authority_denied');

  guest.debugger.sendCommand = async (method, params, sessionId) => {
    if (method === 'Input.dispatchMouseEvent') {
      guest.debugger.emit('message', {}, 'Target.detachedFromTarget', { sessionId: 'child' });
      guest.debugger.emit('message', {}, 'Target.attachedToTarget', { sessionId: 'child', targetInfo: { targetId: 'iframe-new', type: 'iframe' } });
    }
    return originalSend(method, params, sessionId);
  };
  assert.equal((await callFrame('Input.dispatchMouseEvent', 'nested', { type: 'mouseMoved', x: 1, y: 2 })).error.code, 'execution_uncertain');
  guest.debugger.sendCommand = originalSend;
  assert.equal((await callFrame('Input.dispatchMouseEvent', 'child', { type: 'mouseMoved', x: 1, y: 2 })).error.code, 'session_unavailable');
  frameReply = await callFrame('Page.enable', null);
  assert.deepEqual(frameReply.frame_inventory.sessions.map(entry => [entry.session_id, entry.attachment_seq]), [['child', 3]]);
  assert.equal((await callFrame('Page.enable', 'nested')).error.code, 'session_unavailable');
  const reconnected = await client(descriptor, key, formation('a')); clients.push(reconnected);
  assert.equal((await reconnected.request('Page.enable', {}, id, { ...frameChanges, operation: { target_id: id, session_id: 'child', method: 'Page.enable', params: {} } })).error.code, 'session_unavailable');
  assert.equal((await reconnected.request('Page.enable', {}, id, frameChanges)).frame_inventory.sessions[0].attachment_seq, 3);
  assert.equal((await reconnected.request('Page.enable', {}, id, { ...frameChanges, operation: { target_id: id, method: 'Page.enable', params: {} } })).error.code, 'invalid_request');
  const destination = new electron.BrowserWindow({ webPreferences: { session: window.webContents.session } });
  const canceled = prepare('destination', destination);
  assert.equal(guest.visible, true); host.releasePresentation(epochs(canceled, 'destination', destination));
  assert.equal(guest.hostWebContents, window.webContents); assert.equal(guest.visible, true);
  const transfer = prepare('destination', destination);
  assert(transfer.mountGeneration > canceled.mountGeneration);
  guest.adopt(transfer.adoptionToken);
  host.releasePresentation(epochs(prepared));
  assert.equal((await host.acknowledgePresentation(layout(transfer, 'destination', destination))).visible, true);
  assert.equal(guest.hostWebContents, destination.webContents); assert.equal(guest.url, 'https://example.test');
  assert.equal((await callFrame('Page.enable', null)).frame_inventory.sessions[0].attachment_seq, 3);
  const pending = prepare('view');
  host.releasePresentation(epochs(transfer, 'destination', destination));
  assert(host.validatePreparedGuest({ sender: window.webContents, guest }));
  guest.adopt(pending.adoptionToken);
  const lateAck = host.acknowledgePresentation(layout(pending));
  host.releasePresentation(epochs(pending));
  const successor = prepare('view');
  await assert.rejects(lateAck, { code: 'attachment_unavailable' });
  host.releasePresentation(epochs(pending)); host.releasePresentation({ ...epochs(successor), hostGeneration: 'old-host' });
  assert(host.validatePreparedGuest({ sender: window.webContents, guest }));
  assert.throws(() => host.releasePresentation({ ...epochs(successor), sender: destination.webContents }), { code: 'attachment_unavailable' });
  guest.adopt(successor.adoptionToken);
  assert.equal((await host.acknowledgePresentation(layout(successor))).visible, true);
  const roomSession = guest.session, birthsBeforeCrash = electron.factoryOptions.length;
  const loadsBeforeCrash = guest.loads.length;
  guest.emit('render-process-gone', {}, { reason: 'crashed' });
  assert.equal(guest.isDestroyed(), false); assert.equal(guest.session, roomSession); assert.equal(electron.factoryOptions.length, birthsBeforeCrash);
  assert.equal(guest.loads.length, loadsBeforeCrash + 1); assert.equal(guest.loads.at(-1).options.replaceCurrentEntry, true);
  const diagnosticURL = guest.getURL(); assert.match(diagnosticURL, /^data:text\/html;charset=utf-8,/);
  assert.equal(host.getTarget(projection).targetId, id); assert.equal(host.getTarget(projection).url, 'https://example.test');
  assert.equal(host.getTarget(projection).title, 'This page crashed');
  assert.equal(host.listTargetsForProjection(roomProjection).targets.find(page => page.target_id === id).url, 'https://example.test');
  guest.emit('render-process-gone', {}, { reason: 'crashed' }); assert.equal(guest.loads.length, loadsBeforeCrash + 1);
  const afterCrash = await client(descriptor, key, formation('a')); clients.push(afterCrash);
  const reloadCall = { source_movement_seq: 6, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: 'browser.reload', operation: 'reload' }, arguments: {} };
  assert.deepEqual((await afterCrash.request('Page.reload', {}, id, reloadCall)).result, {});
  assert.equal(guest.getURL(), diagnosticURL); assert.equal(guest.loads.length, loadsBeforeCrash + 1);
  const historyCall = { source_movement_seq: 7, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: 'browser.back', operation: 'back' }, arguments: {} };
  const history = (await afterCrash.request('Page.getNavigationHistory', {}, id, historyCall)).result;
  assert.equal(history.entries[1].id, 42); assert.equal(history.entries[1].url, 'https://example.test'); assert.equal(history.entries[1].title, 'This page crashed');
  const refreshed = await host.viewerAction({ ...epochs(successor), action: 'reload' });
  assert.equal(guest.getURL(), 'https://example.test'); assert.equal(guest.loads.length, loadsBeforeCrash + 2);
  assert.equal(refreshed.url, 'https://example.test'); assert.equal(refreshed.loadError, undefined); assert.equal(electron.factoryOptions.length, birthsBeforeCrash);
  guest.emit('did-fail-load', {}, -3, 'ERR_ABORTED', 'https://example.test', true);
  guest.emit('did-fail-load', {}, -105, 'ERR_NAME_NOT_RESOLVED', 'https://frame.test', false);
  assert.equal(guest.loads.length, loadsBeforeCrash + 2);
  guest.emit('did-fail-load', {}, -105, 'ERR_NAME_NOT_RESOLVED', 'https://failed.test', true);
  assert.equal(host.getTarget(projection).url, 'https://failed.test'); assert.equal(guest.loads.at(-1).options.replaceCurrentEntry, true);
  assert.equal(electron.factoryOptions.length, birthsBeforeCrash);
  window.webContents.emit('did-start-navigation', {}, 'https://different-client.test', false, true);
  assert.equal(guest.hostWebContents, null); assert.equal(guest.paintLease, true);
  assert.equal(host.getTarget(projection).targetId, id);
  assert(events.every(event => event.hostGeneration === descriptor.host_generation));
  assert.equal(JSON.stringify(events).includes(successor.adoptionToken), false);
  await host.revokeFormation({ worker_environment_id: 'a', formation_pin: formation('a').formation_pin, reason: 'contract withdrawal' });
  assert.throws(() => host.getTarget(projection), { code: 'authority_denied' });
  assert.throws(() => host.listTargetsForProjection(roomProjection), { code: 'authority_denied' });
  assert(events.some(event => event.type === 'closed')); assert.equal(guest.isDestroyed(), true); assert.equal(guest.paintLease, false); assert.equal(supplied.isDestroyed(), true);
  const v = await client(descriptor, key, formation('viewport')); clients.push(v);
  const intent = (seq, operation, args) => Object.assign(v.common, { source_movement_seq: seq, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: `browser.${operation}`, operation }, arguments: args });
  const beforeBirth = electron.contents.length; const beforeSessions = electron.sessions.size;
  intent(1, 'viewport_set', { width: 10, height: 9000 });
  const vc = (await v.request('Target.createBrowserContext', { disposeOnDetach: false })).result;
  assert.equal(electron.contents.length, beforeBirth); assert.equal(electron.sessions.size, beforeSessions + 1);
  assert.equal((await v.request('Target.createTarget', { browserContextId: vc.browserContextId, url: 'about:blank' })).error.code, 'authority_denied');
  let vr = (await v.request('OctopusBrowser.setViewport', { viewport: { width: 240, height: 4096 } })).result;
  assert.equal(vr.target_id, null); assert.equal(vr.model_viewport, null); assert.equal(vr.pending.revision, 1);
  intent(2, 'viewport_reset', {}); vr = (await v.request('OctopusBrowser.setViewport', { viewport: null })).result;
  assert.equal(vr.pending, null); assert.equal(electron.contents.length, beforeBirth);
  intent(3, 'viewport_set', { width: 800, height: 600 });
  assert.equal((await v.request('OctopusBrowser.setViewport', { viewport: { width: 801, height: 600 } })).error.code, 'authority_denied');
  assert.equal((await v.request('OctopusBrowser.setViewport', { viewport: { width: 800, height: 600 } })).result.pending.revision, 3);
  intent(4, 'navigate', { url: 'https://viewport.test' });
  const vid = (await v.request('Target.createTarget', { browserContextId: vc.browserContextId, url: 'https://viewport.test' })).result.targetId;
  const vw = electron.contents.at(-1); assert.equal(vw.getURL(), 'https://viewport.test');
  assert.deepEqual(vw.loads.map(load => load.url), ['about:blank', 'https://viewport.test']);
  assert.equal(vw.loads[0].methods.includes('Emulation.setDeviceMetricsOverride'), false); assert.equal(vw.loads[1].methods.includes('Emulation.setDeviceMetricsOverride'), true);
  assert.deepEqual(vw.calls.filter(c => c.method === 'Emulation.setDeviceMetricsOverride'), [{ method: 'Emulation.setDeviceMetricsOverride', params: { width: 800, height: 600, deviceScaleFactor: 1, mobile: false }, sessionId: undefined }]);
  vr = (await v.request('OctopusBrowser.getViewportState', {}, vid)).result;
  assert.equal(vr.pending, null); assert.deepEqual(vr.model_viewport, { revision: 3, emulatedSize: { width: 800, height: 600 } });
  const vp = { ...projection, ...{ engine_handle: vc.engine_handle, browser_context_id: vc.browserContextId, target_id: vid, worker_environment_id: 'viewport', formation_pin: formation('viewport').formation_pin } };
  const preparedV = host.preparePresentation({ attachmentId: 'viewport', projection: vp, window, sender: window.webContents, frameToken: 'frame-a', surfaceId: 'page' });
  vw.adopt(preparedV.adoptionToken);
  const packet = (metricsRevision, viewportSequence) => ({ ...layout(preparedV, 'viewport', window, viewportSequence), projection: vp, metricsRevision });
  assert.equal((await host.acknowledgePresentation(packet(3, 1))).visible, true);
  intent(5, 'viewport_set', { width: 1024, height: 768 });
  assert.equal((await v.request('Emulation.setDeviceMetricsOverride', { width: 1024, height: 768, deviceScaleFactor: 1, mobile: false }, vid)).error.code, 'method_denied');
  vr = (await v.request('OctopusBrowser.setViewport', { viewport: { width: 1024, height: 768 } }, vid)).result;
  assert.equal(vr.model_viewport.revision, 5); assert.equal(vw.visible, false);
  await assert.rejects(host.acknowledgePresentation(packet(3, 2)), { code: 'metrics_revision_stale' });
  assert.equal((await host.syncPresentation(packet(5, 2))).presentationPhase, 'attached');
  assert.equal(vw.visible, false); assert.equal((await host.acknowledgePresentation(packet(5, 2))).visible, true);
  const metricsCount = vw.calls.length;
  intent(6, 'viewport_set', { width: 1024, height: 768 });
  assert.equal((await v.request('OctopusBrowser.setViewport', { viewport: { width: 1024, height: 768 } }, vid)).result.model_viewport.revision, 5);
  assert.equal(vw.calls.length, metricsCount);
  intent(7, 'viewport_set', { width: 777, height: 555 });
  const method = 'OctopusBrowser.setViewport'; const params = { viewport: { width: 777, height: 555 } };
  assert.equal((await v.request(method, params, vid, { operation: { target_id: vid, session_id: 'child', method, params } })).error.code, 'authority_denied');
  const sendViewport = vw.debugger.sendCommand; let failedMetrics = 0;
  vw.debugger.sendCommand = async (method, ...args) => { if (method === 'Emulation.setDeviceMetricsOverride') { failedMetrics++; throw new Error('injected dispatch failure'); } return sendViewport(method, ...args); };
  assert.equal((await v.request(method, params, vid)).error.code, 'execution_failed');
  assert.equal((await v.request(method, params, vid)).error.code, 'replay_rejected');
  const reread = await client(descriptor, key, formation('viewport')); clients.push(reread);
  Object.assign(reread.common, v.common, { challenge: reread.common.challenge });
  vr = (await reread.request('OctopusBrowser.getViewportState', {}, vid)).result;
  assert.equal(vr.model_viewport, null); assert.equal(vr.application.status, 'uncertain'); assert.equal(failedMetrics, 1);
  assert.equal(host.getTarget(vp).targetId, vid); assert.equal(host.getTarget(vp).viewport_confirmed, false);
  await assert.rejects(host.acknowledgePresentation(packet(5, 3)), { code: 'viewport_unconfirmed' });
  vw.debugger.sendCommand = sendViewport;
  intent(8, 'viewport_reset', {});
  assert.deepEqual((await v.request('OctopusBrowser.setViewport', { viewport: null }, vid)).result.model_viewport, { revision: 8, emulatedSize: null });
  assert.equal((await host.acknowledgePresentation(packet(8, 3))).visible, true);
  vw.debugger.emit('detach');
  assert.equal(host.getTarget(vp).targetId, vid); assert.equal(host.getTarget(vp).viewport_confirmed, false); assert.equal(vw.visible, false);
  await host.revokeFormation({ worker_environment_id: 'viewport', formation_pin: formation('viewport').formation_pin, reason: 'fixture complete' });
  const turnClient = await client(descriptor, key, formation('turn')); clients.push(turnClient);
  const turnContext = (await turnClient.request('Target.createBrowserContext')).result;
  const turnProjection = { ...roomProjection, worker_environment_id: 'turn', formation_pin: formation('turn').formation_pin, engine_handle: turnContext.engine_handle, browser_context_id: turnContext.browserContextId };
  let turnSeq = 0;
  const turnCall = (tool, args) => Object.assign(turnClient.common, { source_movement_seq: ++turnSeq, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: `browser.${tool}`, operation: tool }, arguments: args });
  turnSeq = 1;
  const newPage = async () => { turnCall('navigate', { url: 'about:blank' }); const result = await turnClient.request('Target.createTarget', { browserContextId: turnContext.browserContextId, url: 'about:blank' }); return { id: result.result.targetId, contents: electron.contents.at(-1) }; };
  const temporary = await newPage(), handoff = await newPage(), deliverable = await newPage(), presented = await newPage();
  for (const [page, visible] of [[temporary, false], [presented, true]]) {
    const pageProjection = { ...turnProjection, target_id: page.id }, attachmentId = `retained-${page.id}`;
    const view = host.preparePresentation({ attachmentId, projection: pageProjection, window, sender: window.webContents, surfaceId: page.id, frameToken: 'frame-b' }); page.contents.adopt(view.adoptionToken);
    assert.equal(host.getTarget(pageProjection).lifetime, 'temporary');
    const ready = await host.acknowledgePresentation({ ...layout(view, attachmentId), projection: pageProjection, visible }); assert.equal(ready.lifetime, visible ? 'persistent' : 'temporary');
    host.releasePresentation(epochs(view, attachmentId));
  }
  const humanPopup = electron.webContents.createOwnedGuest({ session: temporary.contents.session });
  temporary.contents.popup({ url: 'https://human.test', disposition: 'foreground-tab' }).createWindow({ webContents: humanPopup });
  turnCall('tab_mark', { target_id: id, status: 'deliverable' });
  assert.equal((await turnClient.request('OctopusBrowser.markTarget', turnClient.common.arguments)).error.code, 'target_unavailable');
  for (const [page, status] of [[handoff, 'handoff'], [deliverable, 'deliverable']]) {
    turnCall('tab_mark', { target_id: page.id, status });
    const marked = (await turnClient.request('OctopusBrowser.markTarget', turnClient.common.arguments)).result;
    assert.equal(marked.target_id, page.id); assert.equal(marked.disposition.status, status); assert.equal(marked.disposition.source_movement_seq, turnSeq);
  }
  const viewportClient = await client(descriptor, key, formation('turn')); clients.push(viewportClient);
  turnCall('viewport_set', { target_id: deliverable.id, width: 800, height: 600 });
  assert.equal((await viewportClient.request('OctopusBrowser.setViewport', { viewport: { width: 800, height: 600 } }, deliverable.id, { ...turnClient.common, challenge: viewportClient.common.challenge })).result.model_viewport.revision, turnSeq);
  await host.syncTurn(turnProjection, { open: false, openingMovementSeq: 0, observedMovementSeq: turnSeq - 1 });
  assert.equal(temporary.contents.isDestroyed(), false, 'stale terminal projection cannot close current pages');
  const terminal = { open: false, openingMovementSeq: 0, observedMovementSeq: turnSeq };
  await host.syncTurn(turnProjection, terminal);
  assert.equal(temporary.contents.isDestroyed(), true); assert.equal(handoff.contents.isDestroyed(), false); assert.equal(deliverable.contents.isDestroyed(), false); assert.equal(humanPopup.isDestroyed(), false);
  assert.equal(presented.contents.isDestroyed(), false); assert.equal(host.getTarget({ ...turnProjection, target_id: presented.id }).model_controlled, false);
  const retained = host.listTargetsForProjection(turnProjection).targets;
  assert.equal(retained.find(page => page.target_id === handoff.id).lifetime, 'temporary');
  assert.equal(retained.find(page => page.target_id === handoff.id).disposition, null); assert.equal(handoff.contents.paintLease, false);
  assert.equal(retained.find(page => page.target_id === deliverable.id).lifetime, 'persistent');
  assert.equal(retained.find(page => page.target_id === deliverable.id).model_controlled, false);
  assert.equal(deliverable.contents.paintLease, false);
  assert.equal(deliverable.contents.calls.filter(call => call.method === 'Emulation.clearDeviceMetricsOverride').length, 1);
  const cleanupCalls = deliverable.contents.calls.length;
  await host.syncTurn(turnProjection, terminal); assert.equal(deliverable.contents.calls.length, cleanupCalls, 'completion is not replayed');
  assert.equal((await turnClient.request('OctopusBrowser.markTarget', { target_id: deliverable.id, status: 'deliverable' })).error.code, 'authority_denied');
  const nextPage = await client(descriptor, key, formation('turn')); clients.push(nextPage);
  const priorEnd = turnSeq; turnCall('navigate', { target_id: handoff.id, url: 'about:blank' });
  assert.equal((await nextPage.request('Page.enable', {}, handoff.id, { ...turnClient.common, challenge: nextPage.common.challenge })).error, undefined);
  assert.equal(handoff.contents.paintLease, true);
  await host.syncTurn(turnProjection, { open: false, openingMovementSeq: priorEnd, observedMovementSeq: turnSeq });
  assert.equal(handoff.contents.isDestroyed(), true, 'handoff protects one completion, not all future turns');
  assert.equal(deliverable.contents.isDestroyed(), false); assert.equal(humanPopup.isDestroyed(), false);
  await host.revokeFormation({ worker_environment_id: 'turn', formation_pin: formation('turn').formation_pin, reason: 'turn fixture complete' });
  assert.equal(deliverable.contents.isDestroyed(), true); assert.equal(humanPopup.isDestroyed(), true);
  assert.equal(presented.contents.isDestroyed(), true);
  const clipA = await client(descriptor, key, formation('clipboard-a')), clipB = await client(descriptor, key, formation('clipboard-b')); clients.push(clipA, clipB);
  const clipCall = (client, operation, args) => Object.assign(client.common, { source_movement_seq: client.common.source_movement_seq + 1, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: `browser.${operation}`, operation }, arguments: args });
  const items = [{ entries: [{ mime_type: 'text/plain', text: 'room-owned' }] }];
  clipCall(clipA, 'clipboard_write', { items }); clipCall(clipB, 'clipboard_read', {});
  const ca = (await clipA.request('Target.createBrowserContext')).result;
  await clipB.request('Target.createBrowserContext');
  const roomParams = client => ({ arguments: client.common.arguments, target_id: null, max_clipboard_bytes: 1024 });
  assert.deepEqual((await clipA.request('OctopusBrowser.roomOperation', roomParams(clipA))).result.value, { written: true });
  assert.deepEqual((await clipB.request('OctopusBrowser.roomOperation', roomParams(clipB))).result.value, { items: [] });
  clipCall(clipA, 'clipboard_read', {});
  assert.equal((await clipA.request('OctopusBrowser.roomOperation', { ...roomParams(clipA), arguments: { target_id: 'foreign' } })).error.code, 'authority_denied');
  assert.deepEqual((await clipA.request('OctopusBrowser.roomOperation', roomParams(clipA))).result.value, { items });
  clipCall(clipA, 'clipboard_write', { items: [{ entries: [{ mime_type: 'text/plain', text: 'x'.repeat(1024) }] }] });
  assert.equal((await clipA.request('OctopusBrowser.roomOperation', roomParams(clipA))).error.code, 'invalid_request');
  clipCall(clipA, 'clipboard_read', {});
  assert.deepEqual((await clipA.request('OctopusBrowser.roomOperation', roomParams(clipA))).result.value, { items });
  clipCall(clipA, 'navigate', { url: 'about:blank' });
  const pageId = (await clipA.request('Target.createTarget', { browserContextId: ca.browserContextId, url: 'about:blank' })).result.targetId;
  const keyboardPage = electron.contents.at(-1);
  clipCall(clipA, 'key', { keys: ['Meta', 'A'] });
  const perform = { arguments: clipA.common.arguments, page_editing_profile: { rich_text_paste: true, plain_text_documents: [] }, limits: { max_type_chars: 1024, max_eval_chars: 1024 } };
  assert.equal((await clipA.request('OctopusBrowser.perform', { ...perform, arguments: { keys: ['Meta', 'Q'] } }, pageId)).error.code, 'authority_denied');
  const keyResult = (await clipA.request('OctopusBrowser.perform', perform, pageId)).result;
  assert.deepEqual(keyResult.value, { pressed: true }); assert.deepEqual(keyResult.load_waits, []);
  assert.deepEqual(clipboardItems(' **bold**\n', true), [{ presentation_style: 'unspecified', entries: [{ mime_type: 'text/plain', text: ' **bold**\n' }, { mime_type: 'text/html', text: '&nbsp;<strong>bold</strong><br>' }] }]);
  assert.deepEqual(clipboardItems(' **bold**\n', false), [{ presentation_style: 'unspecified', entries: [{ mime_type: 'text/plain', text: ' **bold**\n' }] }]);
  const loadEvents = new EventEmitter();
  const emitLoad = (method, source, params = {}) => loadEvents.emit('event', { method, source, params });
  const frameLoad = waitForPageLoad(loadEvents, { tabId: 31, sessionId: 'child' }, { timeoutMs: 100 });
  const pageLoad = waitForPageLoad(loadEvents, { tabId: 31 }, { timeoutMs: 5 });
  emitLoad('Page.frameStartedLoading', { tabId: 31, sessionId: 'child' }); emitLoad('Page.frameStartedLoading', { tabId: 31 });
  await Promise.resolve(); emitLoad('Page.loadEventFired', { tabId: 31, sessionId: 'child' });
  assert.equal((await frameLoad).outcome, 'load_completed');
  const pageDeadline = await pageLoad; assert.equal(pageDeadline.outcome, 'completion_deadline'); assert.equal(pageDeadline.completion_event, null);
  const refused = waitForPageLoad(loadEvents, { tabId: 31 }, { timeoutMs: 100, rejectSubframeNavigationBlocked: true });
  const refusal = assert.rejects(refused, { code: 'browser_navigation_blocked' });
  emitLoad('Page.navigationBlocked', { tabId: 31 }, { resourceType: 'subFrame', url: 'file:///private/refused' }); await refusal;
  assert.equal(loadEvents.listenerCount('event'), 0);
  const originalCommand = keyboardPage.debugger.sendCommand;
  keyboardPage.debugger.sendCommand = async (method, params, sessionId) => {
    const result = await originalCommand(method, params, sessionId);
    if (method === 'Input.dispatchMouseEvent' && params.type === 'mousePressed') setImmediate(() => keyboardPage.debugger.emit('message', {}, 'Page.frameStartedLoading', { frameId: 'main' }));
    return result;
  };
  clipCall(clipA, 'click', { x: 12, y: 24, snapshot: false, timeout_ms: 100 });
  const clickResult = (await clipA.request('OctopusBrowser.perform', { ...perform, arguments: clipA.common.arguments }, pageId)).result;
  assert.deepEqual(clickResult.value, { clicked: true }); assert.equal(clickResult.load_waits[0].outcome, 'completion_deadline');
  assert.deepEqual(keyboardPage.calls.filter(call => call.method === 'Input.dispatchMouseEvent').map(call => call.params.type), ['mouseMoved', 'mousePressed', 'mouseReleased'], 'a missing load event never repeats completed input');
  keyboardPage.debugger.sendCommand = originalCommand;
  assert.deepEqual(keyboardPage.calls.filter(call => call.params?.commands).map(call => call.params.commands), [['selectAll']]);
  assert.equal(keyboardPage.calls.some(call => Object.hasOwn(call.params ?? {}, 'nativeVirtualKeyCode')), false);
  assert.equal((await clipA.request('Input.dispatchKeyEvent', { type: 'keyDown', commands: ['quit'] }, pageId)).error.code, 'method_denied');
  assert.deepEqual(electron.fileActions, [], 'page keyboard and virtual clipboard never access the system pasteboard');
  const waitFormation = formation('wait-v2');
  for (const id of ['browser.wait', 'browser.wait_complete', 'browser.snapshot']) waitFormation.tool_descriptor_hashes[id] = descriptorHash(descriptorBytes(id));
  waitFormation.granted_tools = Object.keys(waitFormation.tool_descriptor_hashes).sort();
  for (const id of waitFormation.granted_tools) waitFormation.tool_descriptor_hashes[id] = descriptorHash(descriptorBytes(id));
  const sourceOwner = await wireConnection(descriptor, key), observerOwner = await wireConnection(descriptor, key);
  const sourceMember = await wireConnection(descriptor, key), observerMember = await wireConnection(descriptor, key);
  for (const wire of [sourceOwner, observerOwner, sourceMember, observerMember]) clients.push(wire);
  const sourceInstance = '1'.repeat(32), observerInstance = '2'.repeat(32);
  const begin = async (wire, instance, seq, operation, args, formation = waitFormation) => {
    const body = { schema_version: 2, request_kind: 'invocation', executor: { instance_nonce: instance, connection_role: 'owner' }, tool_descriptor: descriptorBytes(`browser.${operation}`).toString('base64'), invocation_nonce: crypto.randomBytes(16).toString('hex'), invocation_handle: null, source_movement_seq: seq, artifact_content_hash: '5'.repeat(64), formation, tool: { id: `browser.${operation}`, operation }, arguments: args, effective_allow_network: operation !== 'wait_complete' && formation.network_ceiling !== 'denied', operation: { target_id: null, session_id: null, method: 'OctopusBrowser.beginInvocation', params: {} } };
    const response = await wire.send(body); assert.equal(response.error, undefined, JSON.stringify(response));
    return { ...body, invocation_handle: response.result.invocation_handle };
  };
  const command = (wire, body, method, params = {}, target_id = null) => wire.send({ ...body, executor: { ...body.executor, connection_role: wire === sourceOwner || wire === observerOwner ? 'owner' : 'member' }, operation: { method, params, target_id, session_id: null } });
  const end = (wire, body, params = {}) => command(wire, body, 'OctopusBrowser.endInvocation', params);
  let source = await begin(sourceOwner, sourceInstance, 90, 'navigate', { url: 'about:blank' });
  const waitContext = (await command(sourceMember, source, 'Target.createBrowserContext')).result;
  const waitTarget = (await command(sourceMember, source, 'Target.createTarget', { browserContextId: waitContext.browserContextId, url: 'about:blank' })).result.targetId;
  const waitPage = electron.contents.at(-1);
  assert.equal(host.getTarget({ protocol: 'browser.native.v1', worker_environment_id: waitFormation.worker_environment_id, formation_pin: waitFormation.formation_pin, host_generation: descriptor.host_generation, engine_handle: waitContext.engine_handle, browser_context_id: waitContext.browserContextId, target_id: waitTarget }).targetId, waitTarget);
  const pageWire = await wireConnection(descriptor, key); clients.push(pageWire);
  assert.equal((await end(sourceOwner, source)).result.ended, true);
  const observedMethods = []; let arrival = '', actionEntered, releaseAction;
  waitPage.debugger.sendCommand = async (method, params) => {
    observedMethods.push(method);
    if (method === 'Page.getFrameTree') return { frameTree: { frame: { id: 'main', url: 'about:blank' } } };
    if (method === 'Runtime.evaluate') return { result: { value: { href: 'about:blank', readyState: 'complete' } } };
    if (method === 'Input.dispatchMouseEvent') {
      actionEntered?.(); if (releaseAction) await new Promise(resolve => { releaseAction.resolve = resolve; });
      waitPage.debugger.emit('message', {}, 'Page.frameNavigated', { frame: { id: 'main', url: arrival } });
    }
    return {};
  };
  for (const [base, order] of [[100, 'read-first'], [200, 'action-first'], [300, 'delayed-callback']]) {
    arrival = `https://wait.test/${base}`;
    source = await begin(sourceOwner, sourceInstance, base, 'wait', { phase: 'start', target_id: waitTarget, url_pattern: arrival, wait_until: 'commit', timeout_ms: 2000 });
    const preparedWait = (await command(pageWire, source, 'OctopusBrowser.startWait', { arguments: source.arguments, limits: { max_waiters: 1, max_event_bytes: 65536 } }, waitTarget)).result;
    assert.equal(preparedWait.status, 'pending'); assert.equal((await end(sourceOwner, source)).result.ended, true);
    const sessionsBefore = electron.sessions.size, pagesBefore = electron.contents.length;
    let action, completion, completionBody;
    if (order === 'action-first') {
      source = await begin(sourceOwner, sourceInstance, base + 1, 'click', { target_id: waitTarget, x: 1, y: 2 });
      const entered = new Promise(resolve => { actionEntered = resolve; }); releaseAction = {};
      action = command(pageWire, source, 'Input.dispatchMouseEvent', { type: 'mousePressed', x: 1, y: 2 }, waitTarget);
      await entered;
      completionBody = await begin(observerOwner, observerInstance, base + 2, 'wait_complete', { waiter: preparedWait.waiter });
      completion = command(observerMember, completionBody, 'OctopusBrowser.completeWait', completionBody.arguments);
      await new Promise(resolve => setImmediate(resolve)); releaseAction.resolve(); releaseAction = null; actionEntered = null;
    } else {
      completionBody = await begin(observerOwner, observerInstance, base + 2, 'wait_complete', { waiter: preparedWait.waiter });
      completion = command(observerMember, completionBody, 'OctopusBrowser.completeWait', completionBody.arguments);
      await new Promise(resolve => setImmediate(resolve));
      if (order === 'delayed-callback') {
        const otherRead = await begin(sourceOwner, sourceInstance, base + 1, 'snapshot', { target_id: waitTarget });
        assert.equal((await command(pageWire, otherRead, 'Runtime.evaluate', { expression: 'document.readyState', returnByValue: true }, waitTarget)).error, undefined);
        assert.equal((await end(sourceOwner, otherRead)).result.ended, true);
      }
      source = await begin(sourceOwner, sourceInstance, base + (order === 'delayed-callback' ? 3 : 1), 'click', { target_id: waitTarget, x: 1, y: 2 });
      action = command(pageWire, source, 'Input.dispatchMouseEvent', { type: 'mousePressed', x: 1, y: 2 }, waitTarget);
    }
    assert.equal((await action).error, undefined);
    assert.equal(electron.sessions.size, sessionsBefore); assert.equal(electron.contents.length, pagesBefore);
    const completedWait = (await completion).result; assert.equal(completedWait.status, 'satisfied'); assert.equal(completedWait.observation.url, arrival);
    const captured_wait = { wait_id: preparedWait.waiter.wait_id, outcome_digest: crypto.createHash('sha256').update(canonical(completedWait)).digest('hex') };
    assert.equal((await end(observerOwner, completionBody, { captured_wait: { ...captured_wait, outcome_digest: '0'.repeat(64) } })).error.code, 'authority_denied');
    assert.equal((await end(observerOwner, completionBody, { captured_wait })).result.ended, true);
    assert.equal((await end(sourceOwner, source)).result.ended, true);
    assert.equal((await command(pageWire, source, 'Input.dispatchMouseEvent', { type: 'mousePressed', x: 1, y: 2 }, waitTarget)).error.code, 'replay_rejected');
  }
  assert.equal(observedMethods.filter(method => method === 'Input.dispatchMouseEvent').length, 3);
  source = await begin(sourceOwner, sourceInstance, 400, 'wait', { phase: 'start', target_id: waitTarget, url_pattern: 'about:blank', wait_until: 'commit', timeout_ms: 2000 });
  const immediate = (await command(pageWire, source, 'OctopusBrowser.startWait', { arguments: source.arguments, limits: { max_waiters: 1, max_event_bytes: 65536 } }, waitTarget)).result;
  assert.equal(immediate.status, 'satisfied'); assert.equal((await end(sourceOwner, source)).result.ended, true);
  let observer = await begin(observerOwner, observerInstance, 402, 'wait_complete', { waiter: immediate.waiter });
  const uncaptured = (await command(observerMember, observer, 'OctopusBrowser.completeWait', observer.arguments)).result;
  assert.equal(uncaptured.status, 'satisfied'); assert.equal((await end(observerOwner, observer)).result.ended, true);
  source = await begin(sourceOwner, sourceInstance, 401, 'wait', { phase: 'start', target_id: waitTarget, url_pattern: 'https://wait.test/full', timeout_ms: 1000 });
  assert.equal((await command(pageWire, source, 'OctopusBrowser.startWait', { arguments: source.arguments, limits: { max_waiters: 1, max_event_bytes: 65536 } }, waitTarget)).error.code, 'waiter_capacity');
  assert.equal((await end(sourceOwner, source)).result.ended, true);
  observer = await begin(observerOwner, observerInstance, 403, 'wait_complete', { waiter: immediate.waiter });
  const recoveredObservation = (await command(observerMember, observer, 'OctopusBrowser.completeWait', observer.arguments)).result;
  assert.deepEqual(recoveredObservation, uncaptured);
  assert.equal((await end(observerOwner, observer, { captured_wait: { wait_id: immediate.waiter.wait_id, outcome_digest: crypto.createHash('sha256').update(canonical(recoveredObservation)).digest('hex') } })).result.ended, true);
  source = await begin(sourceOwner, sourceInstance, 404, 'wait', { phase: 'start', target_id: waitTarget, url_pattern: 'https://wait.test/owner-gone', timeout_ms: 2000 });
  const orphan = (await command(pageWire, source, 'OctopusBrowser.startWait', { arguments: source.arguments, limits: { max_waiters: 1, max_event_bytes: 65536 } }, waitTarget)).result;
  assert.equal((await end(sourceOwner, source)).result.ended, true);
  sourceOwner.socket.destroy(); await once(sourceOwner.socket, 'close');
  observer = await begin(observerOwner, observerInstance, 405, 'wait_complete', { waiter: orphan.waiter });
  const lost = (await command(observerMember, observer, 'OctopusBrowser.completeWait', observer.arguments)).result;
  assert.equal(lost.status, 'source_lost');
  assert.equal((await end(observerOwner, observer, { captured_wait: { wait_id: orphan.waiter.wait_id, outcome_digest: crypto.createHash('sha256').update(canonical(lost)).digest('hex') } })).result.ended, true);
  const releaseWire = await wireConnection(descriptor, key); clients.push(releaseWire);
  const releaseBody = { schema_version: 2, request_kind: 'release_formation', release_nonce: 'e'.repeat(32), formation: waitFormation, worker_environment_kind_descriptor_hash: waitFormation.worker_environment_kind_descriptor_hash, release_mode: 'cold_recovery' };
  let completeCleanup, cleanupEntered, closeCalls = 0, storageCalls = 0;
  const cleanupStarted = new Promise(resolve => { cleanupEntered = resolve; });
  const cleanupBarrier = new Promise(resolve => { completeCleanup = resolve; });
  waitPage.session.closeAllConnections = async () => { closeCalls++; cleanupEntered(); await cleanupBarrier; };
  waitPage.session.clearStorageData = async () => { storageCalls++; waitPage.session.cleared = true; };
  const projectedRelease = host.revokeFormation({ worker_environment_id: waitFormation.worker_environment_id, formation_pin: waitFormation.formation_pin, reason: 'Exact projection withdrawal' });
  await cleanupStarted; assert.equal(waitPage.destroyed, true); assert.equal(storageCalls, 0);
  const joiningWire = await wireConnection(descriptor, key); clients.push(joiningWire);
  let firstReturned = false, secondReturned = false;
  const releasing = releaseWire.send(releaseBody).then(result => { firstReturned = true; return result; });
  const joining = joiningWire.send({ ...releaseBody, release_nonce: 'd'.repeat(32) }).then(result => { secondReturned = true; return result; });
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(firstReturned, false, 'withdrawn authority is not a completed cleanup receipt');
  assert.equal(secondReturned, false, 'concurrent releases join the same pending cleanup');
  assert.equal(closeCalls, 1); completeCleanup();
  assert.equal((await releasing).result.status, 'released'); assert.equal((await joining).result.status, 'released'); await projectedRelease;
  assert.equal(closeCalls, 1); assert.equal(storageCalls, 1);
  const releasedAgain = await wireConnection(descriptor, key); clients.push(releasedAgain);
  assert.equal((await releasedAgain.send({ ...releaseBody, release_nonce: 'f'.repeat(32) })).result.status, 'already_released');
  assert.equal(closeCalls, 1); assert.equal(storageCalls, 1);
  const failedFormation = formation('release-failure');
  const failedClient = await client(descriptor, key, failedFormation); clients.push(failedClient);
  const failedContext = (await failedClient.request('Target.createBrowserContext')).result;
  const failedTarget = (await failedClient.request('Target.createTarget', { browserContextId: failedContext.browserContextId, url: 'about:blank' })).result.targetId;
  const failedProjection = { protocol: 'browser.native.v1', worker_environment_id: failedFormation.worker_environment_id, formation_pin: failedFormation.formation_pin, host_generation: descriptor.host_generation, engine_handle: failedContext.engine_handle, browser_context_id: failedContext.browserContextId, target_id: failedTarget };
  const failedPage = electron.contents.at(-1); assert.equal(host.getTarget(failedProjection).targetId, failedTarget);
  let failedCloseCalls = 0, failedStorageCalls = 0;
  failedPage.session.closeAllConnections = async () => { failedCloseCalls++; };
  failedPage.session.clearStorageData = async () => {
    failedStorageCalls++;
    if (failedStorageCalls === 1) throw Object.assign(new Error('Injected storage cleanup failure'), { code: 'EIO' });
    failedPage.session.cleared = true;
  };
  const failedReleaseBody = { ...releaseBody, formation: failedFormation, release_nonce: 'a'.repeat(32) };
  const failedRelease = await wireConnection(descriptor, key); clients.push(failedRelease);
  assert.equal((await failedRelease.send(failedReleaseBody)).error.code, 'EIO');
  assert.equal(failedPage.destroyed, true); assert.equal(failedPage.session.cleared, undefined);
  assert.throws(() => host.getTarget(failedProjection), { code: 'formation_withdrawn' });
  const rejectedOwner = await wireConnection(descriptor, key); clients.push(rejectedOwner);
  const refusedBegin = { ...source, formation: failedFormation, source_movement_seq: 2, invocation_handle: null, invocation_nonce: 'b'.repeat(32), tool: { id: 'browser.navigate', operation: 'navigate' }, tool_descriptor: descriptorBytes('browser.navigate').toString('base64'), arguments: { url: 'about:blank' }, operation: { method: 'OctopusBrowser.beginInvocation', target_id: null, session_id: null, params: {} } };
  assert.equal((await rejectedOwner.send(refusedBegin)).error.code, 'formation_withdrawn');
  const retryRelease = await wireConnection(descriptor, key); clients.push(retryRelease);
  assert.equal((await retryRelease.send({ ...failedReleaseBody, release_nonce: 'c'.repeat(32) })).result.status, 'released');
  assert.equal(failedCloseCalls, 1, 'completed cleanup stages are retained across a failed attempt');
  assert.equal(failedStorageCalls, 2, 'a later release actually retries the failed remaining cleanup');
  assert.equal(failedPage.session.cleared, true);
  const cleanupConfirmed = await wireConnection(descriptor, key); clients.push(cleanupConfirmed);
  assert.equal((await cleanupConfirmed.send({ ...failedReleaseBody, release_nonce: 'd'.repeat(32) })).result.status, 'already_released');
  assert.equal(failedStorageCalls, 2);
  await historySourceLifetime(t);
  const unsigned = net.createConnection(descriptor.socket_path); await once(unsigned, 'data'); unsigned.write('{"payload":"e30=","mac":"' + '0'.repeat(64) + '"}\n'); await once(unsigned, 'close');
});

test('host key and root symlinks cannot become native browser authority', async () => {
  const root = await fs.realpath(await fs.mkdtemp('/tmp/octopus-native-path-')); const linked = `${root}-link`;
  await fs.symlink(root, linked);
  try { await assert.rejects(createBrowserEngineHost({ electron: injectedElectron(), hostRoot: linked }).start(), { code: 'host_path_denied' }); }
  finally { await fs.unlink(linked); await fs.rm(root, { recursive: true }); }
});

test('native downloads arm without waiting, consume one response grant and import only terminal owned bytes', async () => {
  const root = await fs.mkdtemp(path.join(await fs.realpath(require('node:os').tmpdir()), 'octopus-download-host-')); await fs.chmod(root, 0o700);
  const electron = injectedElectron(), events = [], host = createBrowserEngineHost({ electron, hostRoot: root, onEvent: event => events.push(event) });
  const descriptor = await host.start(), key = await fs.readFile(`${root}/auth.key`), sockets = [];
  const formed = formation('download');
  const historyOwner = { worker_environment_id: formed.worker_environment_id, formation_pin: formed.formation_pin };
  const historyProjection = { ...historyOwner, protocol: 'browser.native.downloads.v1', host_generation: descriptor.host_generation };
  const validateHistory = async () => assert.equal((await host.listDownloadHistoryForProjection(historyOwner)).host_generation, descriptor.host_generation);
  const downloadAction = (action, id, query) => host.downloadAction(historyProjection, action, id, query, validateHistory);
  const acceptArtifact = body => host.acceptDownloadArtifact(historyProjection, body, validateHistory);
  const setPreferences = changes => host.setDownloadPreferences(historyProjection, changes, validateHistory);
  let restarted = null;
  for (const name of ['download_arm', 'download_wait', 'download_cancel', 'tab_new', 'tab_select', 'tab_close']) formed.tool_descriptor_hashes[`browser.${name}`] = 'e'.repeat(64);
  formed.granted_tools = Object.keys(formed.tool_descriptor_hashes).sort();
  let c = await client(descriptor, key, formed); sockets.push(c.socket);
  let seq = 1;
  const invocation = (operation, arguments_) => ({ source_movement_seq: ++seq, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: `browser.${operation}`, operation }, arguments: arguments_ });
  const flush = () => new Promise(resolve => setImmediate(resolve));
  try {
    const context = (await c.request('Target.createBrowserContext')).result;
    const target = (await c.request('Target.createTarget', { browserContextId: context.browserContextId, url: 'about:blank' })).result.targetId;
    const guest = electron.contents.find(value => value.owned);
    const control = await client(descriptor, key, formed); sockets.push(control.socket);
    Object.assign(control.common, c.common, { challenge: control.common.challenge });
    const arm = invocation('download_arm', { max_bytes: 32, timeout_ms: 1000 });
    const admitted = await c.request('OctopusBrowser.armDownload', arm.arguments, target, arm);
    assert.equal(admitted.result.status, 'armed'); const id = admitted.result.download_id;
    assert.equal(guest.loads[0].methods.includes('Fetch.enable'), true);
    assert.equal((await c.request('OctopusBrowser.armDownload', { ...arm.arguments, max_bytes: 33 }, target, arm)).error.code, 'download_contract_failed');
    const trigger = invocation('click', { x: 2, y: 3 });
    await c.request('Input.dispatchMouseEvent', { type: 'mousePressed', x: 2, y: 3 }, target, trigger);
    guest.debugger.emit('message', {}, 'Fetch.requestPaused', { requestId: 'response-1', resourceType: 'Document', request: { url: 'https://download.test/file.pdf' }, responseStatusCode: 200, responseHeaders: [{ name: 'Content-Type', value: 'application/pdf' }] });
    await flush();
    assert.equal(guest.calls.at(-1).method, 'Fetch.continueResponse');
    assert.deepEqual(guest.calls.at(-1).params.responseHeaders.at(-1), { name: 'Content-Disposition', value: 'attachment' });
    class Item extends EventEmitter {
      nativeGuid = crypto.randomUUID(); getGUID() { return this.nativeGuid; }
      getURL() { return 'https://download.test/file.pdf'; } getURLChain() { return [this.getURL()]; }
      getFilename() { return '../../foreign.pdf'; } getMimeType() { return 'application/pdf'; }
      getStartTime() { return 1; } getEndTime() { return 2; } getTotalBytes() { return 5; } getReceivedBytes() { return this.bytes ?? 0; }
      getSavePath() { return this.path ?? ''; } isPaused() { return !!this.paused; } canResume() { return true; } pause() { this.paused = true; } resume() { this.paused = false; }
      setSavePath(path) { this.path = path; } setSaveDialogOptions(options) { this.saveDialog = options; } cancel() { this.cancelled = true; this.emit('done', {}, 'cancelled'); }
    }
    const item = new Item(); let refused = false;
    guest.session.emit('will-download', { preventDefault() { refused = true; } }, item, guest);
    assert.equal(refused, false); assert(item.path.startsWith(root)); assert(!item.path.includes('foreign')); assert.equal(guest.session.downloadPath, undefined);
    assert.equal(events.some(value => value.type === 'download-history-error'), false);
    const duplicate = new Item(); guest.session.emit('will-download', { preventDefault() { refused = true; } }, duplicate, guest); assert.equal(refused, true); assert.equal(duplicate.path, undefined);
    const projection = { protocol: 'browser.native.v1', worker_environment_id: formed.worker_environment_id, formation_pin: formed.formation_pin, engine_handle: context.engine_handle, host_generation: descriptor.host_generation, browser_context_id: context.browserContextId };
    assert.equal(duplicate.cancelled, true);
    const humanPopup = electron.webContents.createOwnedGuest({ session: guest.session });
    guest.popup({ url: 'https://human-download.test', disposition: 'foreground-tab' }).createWindow({ webContents: humanPopup });
    const popupTarget = host.listTargetsForProjection(projection).targets.find(value => value.opener_id === target).target_id;
    humanPopup.debugger.emit('message', {}, 'Fetch.requestPaused', { requestId: 'human-response', resourceType: 'Document', request: { url: 'https://download.test/human' }, responseStatusCode: 200, responseHeaders: [{ name: 'Content-Disposition', value: 'attachment' }] });
    await flush(); assert.equal(humanPopup.calls.at(-1).method, 'Fetch.continueResponse');
    const interrupted = new Item(); interrupted.canResume = () => false; refused = false;
    guest.session.emit('will-download', { preventDefault() { refused = true; } }, interrupted, humanPopup); assert.equal(refused, false);
    assert.equal(guest.session.downloadPath, '/test/downloads');
    interrupted.emit('updated', {}, 'interrupted');
    const interruptedRecord = (await downloadAction('snapshot')).downloads.find(value => value.targetId === popupTarget);
    assert.equal(interruptedRecord.status, 'failed'); assert.equal(interruptedRecord.canCancel, false); assert.equal(interruptedRecord.canResume, false);
    interrupted.emit('done', {}, 'completed'); assert.equal((await downloadAction('snapshot')).downloads.find(value => value.id === interruptedRecord.id).status, 'failed');
    await downloadAction('remove', interruptedRecord.id);
    await host.closeTarget({ ...projection, target_id: popupTarget });
    const second = (await control.request('Target.createTarget', { browserContextId: context.browserContextId, url: 'about:blank' }, null, invocation('tab_new', {}))).result.targetId;
    await control.request('Target.activateTarget', { targetId: second }, null, invocation('tab_select', { target_id: second }));
    const direct = invocation('click', { target_id: target, x: 2, y: 3 });
    assert.equal((await c.request('Page.enable', {}, target, direct)).error, undefined);
    assert.equal((await control.request('Page.enable', {}, second, direct)).error.code, 'authority_denied');
    const cannotCreate = invocation('navigate', { target_id: target, url: 'https://example.test' });
    assert.equal((await control.request('Target.createTarget', { browserContextId: context.browserContextId, url: 'about:blank' }, null, cannotCreate)).error.code, 'authority_denied');
    const wait = invocation('download_wait', { download_id: id, timeout_ms: 10 });
    assert.equal((await control.request('OctopusBrowser.waitDownload', wait.arguments, null, wait)).result.status, 'progressing');
    assert.equal((await control.request('OctopusBrowser.readDownload', { download_id: id, offset: 0, length: 5 }, null, wait)).error.code, 'download_contract_failed');
    await fs.writeFile(item.path, 'hello'); item.bytes = 5; item.emit('done', {}, 'completed');
    let result;
    for (let i = 0; i < 20; i++) { result = (await control.request('OctopusBrowser.waitDownload', wait.arguments, null, wait)).result; if (result.status === 'completed') break; }
    assert.equal(result.target_id, target); assert.notEqual(result.target_id, second); assert.equal(result.browser_context_id, context.browserContextId);
    assert.equal(result.status, 'completed'); assert.equal(result.native_event.state, 'completed');
    assert.equal(result.arm.source_movement_seq, arm.source_movement_seq); assert.equal(result.trigger.invocation_nonce, trigger.invocation_nonce); assert.equal(result.response.request_id, 'response-1');
    const bytes = (await control.request('OctopusBrowser.readDownload', { download_id: id, offset: 0, length: 5 }, null, wait)).result;
    assert.equal(Buffer.from(bytes.payload_base64, 'base64').toString(), 'hello'); assert.equal(bytes.sha256, crypto.createHash('sha256').update('hello').digest('hex'));
    assert.equal((await control.request('OctopusBrowser.finishDownload', { download_id: id, sha256: bytes.sha256 }, null, wait)).result.status, 'imported');
    await assert.rejects(fs.stat(item.path), { code: 'ENOENT' });
    const importedFile = `${root}/imported-model.pdf`; await fs.writeFile(importedFile, 'wrong');
    const importedBody = { ...result, worker_environment_id: formed.worker_environment_id, source_movement_seq: wait.source_movement_seq,
      arm: Object.fromEntries(Object.entries(result.arm).sort(([a], [b]) => a.localeCompare(b))), path: importedFile, content_hash: 'c'.repeat(64),
      artifact: { label: 'browser.download', payload_path: importedFile, payload_content_hash: 'c'.repeat(64) } };
    let modelHistory = (await downloadAction('snapshot')).downloads.find(value => value.id === id);
    assert.equal(modelHistory.userInitiated, false); assert.equal(modelHistory.artifactPending, true); assert.equal(modelHistory.fileExists, false);
    await assert.rejects(acceptArtifact(importedBody), /bytes differ/);
    await fs.writeFile(importedFile, 'hello'); await acceptArtifact(importedBody);
    const historySequence = (await downloadAction('snapshot')).sequence;
    await acceptArtifact(importedBody); assert.equal((await downloadAction('snapshot')).sequence, historySequence);
    modelHistory = (await downloadAction('snapshot')).downloads.find(value => value.id === id);
    assert.equal(modelHistory.fileExists, true); assert.equal(modelHistory.artifactPending, false); assert.equal(modelHistory.artifactError, null);
    await downloadAction('open', id); assert.deepEqual(electron.fileActions.at(-1), ['open', importedFile]);
    await assert.rejects(acceptArtifact({ ...importedBody, browser_context_id: 'foreign' }), /original native download/);
    await downloadAction('remove', id); assert.equal(await fs.readFile(importedFile, 'utf8'), 'hello');
    for (const target_id of [target, second]) {
      const marked = invocation('tab_mark', { target_id, status: 'handoff' });
      assert.equal((await control.request('OctopusBrowser.markTarget', marked.arguments, null, marked)).error, undefined);
    }
    await host.syncTurn(projection, { open: false, openingMovementSeq: 1, observedMovementSeq: seq - 1 }); refused = false;
    guest.session.emit('will-download', { preventDefault() { refused = true; } }, new Item(), guest); assert.equal(refused, true);
    await host.syncTurn(projection, { open: false, openingMovementSeq: 1, observedMovementSeq: seq }); refused = false;
    await Promise.all([setPreferences({ directory: root }), setPreferences({ promptForLocation: true })]);
    assert.deepEqual((await downloadAction('snapshot')).preferences, { directory: root, promptForLocation: true });
    const preferencesPath = `${root}/download-preferences.json`;
    assert.deepEqual(JSON.parse(await fs.readFile(preferencesPath, 'utf8')), { schema_version: 1, directory: root, promptForLocation: true });
    assert.equal((await fs.stat(preferencesPath)).mode & 0o777, 0o600);
    const humanItem = new Item(); guest.session.emit('will-download', { preventDefault() { refused = true; } }, humanItem, guest); assert.equal(refused, false);
    assert(events.some(event => event.type === 'human-download'));
    assert.deepEqual(humanItem.saveDialog, { defaultPath: `${root}/foreign.pdf` }); assert.equal(humanItem.path, undefined);
    const { createHumanDownloads } = require('../browser-human-downloads.cjs');
    const restored = createHumanDownloads({ hostRoot: root, electron, emit() {} }); await restored.initialize();
    assert.deepEqual(restored.snapshot({ workerEnvironmentId: formed.worker_environment_id, formationPin: formed.formation_pin }).preferences, { directory: root, promptForLocation: true }); await restored.stop();
    await setPreferences({ promptForLocation: false });
    const invalidItem = new Item(); invalidItem.getFilename = () => { throw new Error('native filename unavailable'); };
    guest.session.emit('will-download', { preventDefault() { refused = true; } }, invalidItem, guest);
    assert.equal(refused, true); assert(events.some(event => event.type === 'download-error' && /native filename unavailable/.test(event.error)));
    let humanSnapshot = await downloadAction('snapshot');
    const humanId = humanSnapshot.downloads.find(value => value.userInitiated).id;
    const automaticHuman = new Item(); guest.session.emit('will-download', { preventDefault() { throw new Error('human nonprompt admission refused'); } }, automaticHuman, guest);
    assert.equal(guest.session.downloadPath, root); assert.equal(automaticHuman.path, undefined); assert.equal(automaticHuman.saveDialog, undefined); automaticHuman.cancel();
    assert.equal(humanSnapshot.downloads.find(value => value.id === humanId).status, 'started'); assert.equal(humanSnapshot.downloads.find(value => value.id === humanId).canPause, true);
    humanSnapshot = await downloadAction('pause', humanId); assert.equal(humanSnapshot.downloads.find(value => value.id === humanId).status, 'paused'); assert.equal(humanItem.isPaused(), true);
    humanSnapshot = await downloadAction('resume', humanId); assert.equal(humanSnapshot.downloads.find(value => value.id === humanId).status, 'in_progress');
    await assert.rejects(downloadAction('remove', humanId), /Active download/);
    c = await client(descriptor, key, formed); sockets.push(c.socket);
    const anotherArm = invocation('download_arm', { max_bytes: 32, timeout_ms: 1000 });
    const pending = (await c.request('OctopusBrowser.armDownload', anotherArm.arguments, target, anotherArm)).result;
    const cancelled = (await control.request('OctopusBrowser.cancelDownload', { download_id: pending.download_id }, null, invocation('download_cancel', { download_id: pending.download_id }))).result;
    assert.equal(cancelled.status, 'cancelled'); assert.equal(cancelled.target_id, target);
    const foreign = { ...formation('foreign-download'), tool_descriptor_hashes: formed.tool_descriptor_hashes, granted_tools: formed.granted_tools };
    const outsider = await client(descriptor, key, foreign); sockets.push(outsider.socket);
    await outsider.request('Target.createBrowserContext');
    assert.equal((await outsider.request('OctopusBrowser.waitDownload', { download_id: pending.download_id, timeout_ms: 1 }, null, invocation('download_wait', { download_id: pending.download_id, timeout_ms: 1 }))).error.code, 'download_contract_failed');
    const closingArm = invocation('download_arm', { max_bytes: 32, timeout_ms: 1000 });
    const closing = (await c.request('OctopusBrowser.armDownload', closingArm.arguments, target, closingArm)).result;
    await c.request('Input.dispatchMouseEvent', { type: 'mousePressed', x: 2, y: 3 }, target, invocation('click', { x: 2, y: 3 }));
    guest.debugger.emit('message', {}, 'Fetch.requestPaused', { requestId: 'response-close', resourceType: 'Document', request: { url: 'https://download.test/file.pdf' }, responseStatusCode: 200, responseHeaders: [{ name: 'Content-Type', value: 'application/pdf' }] });
    await flush();
    const independent = new Item(); guest.session.emit('will-download', { preventDefault() { throw new Error('admitted independent item refused'); } }, independent, guest);
    await control.request('Target.closeTarget', { targetId: target }, null, invocation('tab_close', { target_id: target }));
    assert.equal(guest.isDestroyed(), true); assert.equal(independent.cancelled, undefined, 'closing the page must not synthesize DownloadItem cancellation');
    assert.equal(humanItem.cancelled, undefined);
    humanItem.path = `${root}/user-chosen.pdf`; await fs.writeFile(humanItem.path, 'human'); humanItem.bytes = 5; humanItem.emit('done', {}, 'completed');
    humanSnapshot = await downloadAction('snapshot');
    const humanComplete = humanSnapshot.downloads.find(value => value.id === humanId);
    assert.equal(humanComplete.status, 'complete'); assert.equal(humanComplete.targetId, target); assert.equal(humanComplete.fileExists, true); assert.equal('path' in humanComplete, false);
    assert.deepEqual(humanSnapshot.unacknowledgedIds, [humanId]);
    await downloadAction('open', humanId); assert.deepEqual(electron.fileActions.at(-1), ['open', humanItem.path]);
    await downloadAction('show-in-folder', humanId); assert.deepEqual(electron.fileActions.at(-1), ['reveal', humanItem.path]);
    await downloadAction('copy-address', humanId); assert.deepEqual(electron.fileActions.at(-1), ['copy', humanItem.getURL()]);
    assert.deepEqual((await downloadAction('snapshot')).unacknowledgedIds, []);
    await downloadAction('remove', humanId); assert.equal((await downloadAction('snapshot')).downloads.some(value => value.id === humanId), false);
    assert.equal((await downloadAction('snapshot')).downloads.find(value => value.id === closing.download_id).userInitiated, false);
    assert.equal(await fs.readFile(humanItem.path, 'utf8'), 'human', 'removing native history cannot delete the user file');
    const afterClose = invocation('download_wait', { download_id: closing.download_id, timeout_ms: 5 });
    assert.equal((await control.request('OctopusBrowser.waitDownload', afterClose.arguments, null, afterClose)).result.status, 'progressing');
    assert.equal((await control.request('Target.getTargets', {}, null, afterClose)).result.targetInfos[0].targetId, second);
    await fs.writeFile(independent.path, 'later'); independent.bytes = 5; independent.emit('done', {}, 'completed');
    let terminal;
    for (let i = 0; i < 20; i++) { terminal = (await control.request('OctopusBrowser.waitDownload', afterClose.arguments, null, afterClose)).result; if (terminal.status === 'completed') break; }
    assert.equal(terminal.status, 'completed'); assert.equal(terminal.target_id, target); assert.equal(terminal.response.request_id, 'response-close');
    const retainedBytes = (await control.request('OctopusBrowser.readDownload', { download_id: closing.download_id, offset: 0, length: 5 }, null, afterClose)).result;
    assert.equal(Buffer.from(retainedBytes.payload_base64, 'base64').toString(), 'later');
    const closedPageImport = `${root}/closed-page-model.pdf`; await fs.writeFile(closedPageImport, 'later');
    const closedPageBody = { ...terminal, worker_environment_id: formed.worker_environment_id, source_movement_seq: afterClose.source_movement_seq,
      path: closedPageImport, content_hash: 'd'.repeat(64), artifact: { label: 'browser.download', payload_path: closedPageImport, payload_content_hash: 'd'.repeat(64) } };
    await acceptArtifact(closedPageBody);
    await downloadAction('open', closing.download_id); assert.deepEqual(electron.fileActions.at(-1), ['open', closedPageImport]);
    assert.equal((await outsider.request('OctopusBrowser.waitDownload', afterClose.arguments, null, invocation('download_wait', afterClose.arguments))).error.code, 'download_contract_failed');
    await host.revokeFormation({ worker_environment_id: formed.worker_environment_id, formation_pin: formed.formation_pin, reason: 'fixture withdrawal' });
    await flush(); await assert.rejects(fs.stat(independent.path), { code: 'ENOENT' });
    assert.equal(electron.contents.filter(contents => contents.owned && !contents.isDestroyed()).length, 0);
    await host.stop(); const createdCount = electron.contents.length;
    restarted = createBrowserEngineHost({ electron, hostRoot: root }); const nextHost = await restarted.start();
    const restoredProjection = { ...historyProjection, host_generation: nextHost.host_generation };
    const validateRestored = async () => assert.equal((await restarted.listDownloadHistoryForProjection(historyOwner)).host_generation, nextHost.host_generation);
    assert.notEqual(nextHost.host_generation, descriptor.host_generation);
    const stored = (await restarted.listDownloadHistoryForProjection(historyOwner)).downloads.downloads.find(value => value.id === closing.download_id);
    assert.equal(stored.status, 'complete'); assert.equal(stored.fileExists, true); assert.equal(stored.canResume, false);
    await assert.rejects(restarted.downloadAction(historyProjection, 'snapshot', undefined, undefined, validateRestored), /generation/);
    await assert.rejects(restarted.downloadAction(restoredProjection, 'resume', closing.download_id, undefined, validateRestored), /cannot resume/);
    await restarted.acceptDownloadArtifact(restoredProjection, closedPageBody, validateRestored);
    await assert.rejects(restarted.acceptDownloadArtifact(restoredProjection, { ...closedPageBody, host_generation: nextHost.host_generation }, validateRestored), /original native download/);
    await assert.rejects(restarted.downloadAction(restoredProjection, 'open', closing.download_id, undefined, async () => { throw new Error('source withdrawn'); }), /source withdrawn/);
    assert.deepEqual((await restarted.listDownloadHistoryForProjection({ ...historyOwner, formation_pin: '0'.repeat(64) })).downloads.downloads, []);
    assert.equal(electron.contents.length, createdCount, 'history readback cannot materialize any page or context');
  } finally { for (const socket of sockets) socket.destroy(); await host.stop(); await restarted?.stop(); await fs.rm(root, { recursive: true, force: true }); }
});

test('file upload holds exact native bytes until one verified private resume, with no selected-tab switch', async t => {
  const root = await fs.realpath(await fs.mkdtemp('/tmp/octopus-upload-contract-'));
  const electron = injectedElectron(), host = createBrowserEngineHost({ electron, hostRoot: root });
  const descriptor = await host.start(), key = await fs.readFile(`${root}/auth.key`), formed = formation('upload');
  for (const name of ['arm', 'wait', 'set_files', 'cancel', 'resume']) { const tool = `browser.filechooser_${name}`; formed.granted_tools.push(tool); formed.tool_descriptor_hashes[tool] = '7'.repeat(64); }
  formed.granted_tools.sort(); const control = await client(descriptor, key, formed), page = await client(descriptor, key, formed);
  t.after(async () => { control.socket.destroy(); page.socket.destroy(); await host.stop(); await fs.rm(root, { recursive: true }); });
  const context = (await control.request('Target.createBrowserContext', { disposeOnDetach: false })).result;
  const target = (await control.request('Target.createTarget', { browserContextId: context.browserContextId, url: 'about:blank' })).result.targetId;
  const guest = electron.contents[0], effects = []; guest.url = 'https://upload.test/form';
  guest.debugger.sendCommand = async (method, params) => {
    guest.calls.push({ method, params });
    if (method === 'Page.getFrameTree') return { frameTree: { frame: { id: 'main', loaderId: 'document-one', url: guest.url } } };
    if (method === 'DOM.resolveNode') return { object: { objectId: 'actual-input' } };
    if (method === 'Runtime.callFunctionOn') return { result: { value: { connected: true, file: true, documentURL: guest.url } } };
    if (method === 'DOM.setFileInputFiles') effects.push({ params, bytes: await fs.readFile(params.files[0]) });
    return {};
  };
  let sequence = 1;
  const phase = (peer, operation, args) => Object.assign(peer.common, { source_movement_seq: ++sequence, invocation_nonce: crypto.randomBytes(16).toString('hex'), tool: { id: `browser.${operation}`, operation }, arguments: args });
  phase(page, 'filechooser_arm', { target_id: target });
  const armed = (await page.request('OctopusBrowser.armFileChooser', {}, target)).result;
  assert.equal(armed.status, 'armed'); assert.equal(effects.length, 0);
  guest.debugger.emit('message', {}, 'Page.fileChooserOpened', { backendNodeId: 19, frameId: 'main', mode: 'selectSingle' });
  phase(control, 'filechooser_wait', { reservation_id: armed.reservation_id, timeout_ms: 0 });
  const opened = (await control.request('OctopusBrowser.waitFileChooser', control.common.arguments)).result;
  assert.equal(opened.status, 'opened'); assert.equal(opened.target_id, target);
  const bytes = Buffer.from('Exact admitted résumé bytes'), artifact = { id: 'file-manifest', content: Array(32).fill(8) };
  const args = { reservation_id: armed.reservation_id, file_chooser_id: opened.file_chooser_id, files: [{ artifact_ref: artifact, filename: 'résumé.txt' }] };
  phase(control, 'filechooser_set_files', args);
  const observed = (await control.request('OctopusBrowser.inspectFileChooser', { reservation_id: args.reservation_id, file_chooser_id: args.file_chooser_id })).result;
  const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value && typeof value === 'object' ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value);
  const hash = value => crypto.createHash('sha256').update(value).digest('hex');
  const upload = { browser_context_id: context.browserContextId, target_id: target, reservation_id: args.reservation_id, file_chooser_id: args.file_chooser_id, document_token: observed.document_token, backend_node_id: 19, top_level_origin: 'https://upload.test', files: [{ artifact_ref: artifact, filename: 'résumé.txt', payload_sha256: hash(bytes), size_bytes: bytes.length }] };
  const original = { run: formed.run, ee: formed.ee, worker: formed.worker, door_interface: formed.door_interface, tool: control.common.tool.id, correlation_id: 'original-file-selection', arguments: args, source_movement_seq: control.common.source_movement_seq, source_artifact: { id: 'original-call', content: Array.from(Buffer.from(control.common.artifact_content_hash, 'hex')) } };
  const scope = { worker_environment_id: formed.worker_environment_id, formation_pin: formed.formation_pin, host_generation: descriptor.host_generation };
  const permission_offer = { client_interface: 'world_operator_upload_fixture', offered_choices: ['once', 'session', 'always'].map(persistence => ({ decision: 'accept', persistence })).concat([{ decision: 'decline', persistence: 'once' }]) };
  const digest = hash(canonical({ schema_version: 1, original, scope, approval_scope: 'browser_file_upload', upload, permission_offer }));
  const request = { schema_version: 1, request_id: `sha256:${digest}`, call_id: original.correlation_id, original, scope, approval_scope: 'browser_file_upload', upload, permission_offer, resume: { tool: 'browser.filechooser_resume', correlation_id: `browser-upload-resume:${digest}` } };
  assert.equal((await control.request('OctopusBrowser.stageFileChooser', { reservation_id: args.reservation_id, request, max_staged_bytes: 268435456 })).result.status, 'staging');
  assert.equal((await control.request('OctopusBrowser.writeFileChooser', { reservation_id: args.reservation_id, request_id: request.request_id, index: 0, offset: 0, payload_base64: bytes.toString('base64') })).result.offset, bytes.length);
  assert.equal((await control.request('OctopusBrowser.prepareFileChooser', { reservation_id: args.reservation_id, request_id: request.request_id })).result.status, 'prepared');
  assert.equal(effects.length, 0);
  assert.equal(guest.calls.some(call => call.method === 'DOM.setFileInputFiles'), false);
  assert.equal((await control.request('OctopusBrowser.lookupPermission', { request_id: request.request_id })).result, null);
  phase(control, 'filechooser_resume', { request_id: request.request_id });
  const parameters = { reservation_id: args.reservation_id, request_id: request.request_id, decision: 'accept' };
  assert.ok((await control.request('OctopusBrowser.applyFileChooser', { ...parameters, request_id: `sha256:${'f'.repeat(64)}` })).error);
  assert.equal(effects.length, 0);
  const admittedAnswer = { request_id: request.request_id, call_id: request.call_id, decision: 'accept', selected_persistence: 'session' };
  const answerArtifact = { id: 'admitted-upload-answer', content: Array.from(Buffer.from(control.common.artifact_content_hash, 'hex')) };
  assert.ok((await control.request('OctopusBrowser.rememberPermission', { request_id: request.request_id, response: { ...admittedAnswer, selected_persistence: null }, source_artifact: answerArtifact })).error);
  const saved = (await control.request('OctopusBrowser.rememberPermission', { request_id: request.request_id, response: admittedAnswer, source_artifact: answerArtifact })).result;
  assert.equal(saved.status, 'saved'); assert.equal(saved.evidence.session.run, formed.run); assert.equal(effects.length, 0);
  assert.deepEqual((await host.savedPermissions({ operation: 'list' }, () => {})).entries, [], 'Settings exposes Always decisions, not task-scoped consent');
  const applied = (await control.request('OctopusBrowser.applyFileChooser', parameters)).result;
  assert.equal(applied.status, 'applied'); assert.equal(applied.effect_performed, true); assert.deepEqual(effects[0].bytes, bytes);
  assert.equal(JSON.stringify(applied).includes(root), false);
  assert.ok((await control.request('OctopusBrowser.applyFileChooser', parameters)).error); assert.equal(effects.length, 1);
  phase(page, 'filechooser_arm', { target_id: target });
  const second = (await page.request('OctopusBrowser.armFileChooser', {}, target)).result;
  guest.debugger.emit('message', {}, 'Page.fileChooserOpened', { backendNodeId: 19, frameId: 'main', mode: 'selectSingle' });
  phase(control, 'filechooser_wait', { reservation_id: second.reservation_id, timeout_ms: 0 });
  const secondOpen = (await control.request('OctopusBrowser.waitFileChooser', control.common.arguments)).result;
  const secondArgs = { ...args, reservation_id: second.reservation_id, file_chooser_id: secondOpen.file_chooser_id };
  phase(control, 'filechooser_set_files', secondArgs);
  const secondObserved = (await control.request('OctopusBrowser.inspectFileChooser', { reservation_id: secondArgs.reservation_id, file_chooser_id: secondArgs.file_chooser_id })).result;
  const secondOriginal = { ...original, arguments: secondArgs, correlation_id: 'second-selection', source_movement_seq: control.common.source_movement_seq };
  const secondUpload = { ...upload, reservation_id: secondArgs.reservation_id, file_chooser_id: secondArgs.file_chooser_id, document_token: secondObserved.document_token, files: [{ ...upload.files[0], size_bytes: 268435456 }] };
  const secondDigest = hash(canonical({ schema_version: 1, original: secondOriginal, scope, approval_scope: 'browser_file_upload', upload: secondUpload, permission_offer }));
  const secondRequest = { ...request, original: secondOriginal, call_id: secondOriginal.correlation_id, upload: secondUpload, request_id: `sha256:${secondDigest}`, resume: { tool: request.resume.tool, correlation_id: `browser-upload-resume:${secondDigest}` } };
  assert.match((await control.request('OctopusBrowser.stageFileChooser', { reservation_id: secondArgs.reservation_id, request: secondRequest, max_staged_bytes: 268435456 })).error.message, /quota is exhausted/);
  assert.equal(effects.length, 1);
  const cachedUpload = { ...secondUpload, files: upload.files };
  const cachedDigest = hash(canonical({ schema_version: 1, original: secondOriginal, scope, approval_scope: 'browser_file_upload', upload: cachedUpload, permission_offer }));
  const cachedRequest = { ...secondRequest, upload: cachedUpload, request_id: `sha256:${cachedDigest}`, resume: { tool: request.resume.tool, correlation_id: `browser-upload-resume:${cachedDigest}` } };
  const prepare = async (value, reservation) => {
    const staged = await control.request('OctopusBrowser.stageFileChooser', { reservation_id: reservation, request: value, max_staged_bytes: 268435456 }); assert.ifError(staged.error);
    assert.equal((await control.request('OctopusBrowser.writeFileChooser', { reservation_id: reservation, request_id: value.request_id, index: 0, offset: 0, payload_base64: bytes.toString('base64') })).result.offset, bytes.length);
    assert.equal((await control.request('OctopusBrowser.prepareFileChooser', { reservation_id: reservation, request_id: value.request_id })).result.status, 'prepared');
  };
  await prepare(cachedRequest, secondArgs.reservation_id);
  const entry = (await control.request('OctopusBrowser.lookupPermission', { request_id: cachedRequest.request_id })).result;
  assert.equal(entry.id, saved.evidence.id); assert.equal(entry.response.request_id, request.request_id);
  const cached = await control.request('OctopusBrowser.applyFileChooser', { reservation_id: secondArgs.reservation_id, request_id: cachedRequest.request_id, decision: 'accept', permission_entry_id: entry.id });
  assert.ifError(cached.error); assert.equal(cached.result.effect_performed, true); assert.equal(effects.length, 2); assert.deepEqual(effects[1].bytes, bytes);
  for (const decision of ['decline', 'cancel']) {
    phase(page, 'filechooser_arm', { target_id: target });
    const reservation = (await page.request('OctopusBrowser.armFileChooser', {}, target)).result.reservation_id;
    guest.debugger.emit('message', {}, 'Page.fileChooserOpened', { backendNodeId: 19, frameId: 'main', mode: 'selectSingle' });
    phase(control, 'filechooser_wait', { reservation_id: reservation, timeout_ms: 0 });
    const chooser = (await control.request('OctopusBrowser.waitFileChooser', control.common.arguments)).result.file_chooser_id;
    const nextArgs = { ...args, reservation_id: reservation, file_chooser_id: chooser }; phase(control, 'filechooser_set_files', nextArgs);
    const observation = (await control.request('OctopusBrowser.inspectFileChooser', { reservation_id: reservation, file_chooser_id: chooser })).result;
    const nextOriginal = { ...original, arguments: nextArgs, correlation_id: decision, source_movement_seq: sequence };
    const nextUpload = { ...upload, reservation_id: reservation, file_chooser_id: chooser, document_token: observation.document_token };
    const onceOffer = { ...permission_offer, offered_choices: permission_offer.offered_choices.filter(choice => choice.persistence === 'once') };
    const nextDigest = hash(canonical({ schema_version: 1, original: nextOriginal, scope, approval_scope: 'browser_file_upload', upload: nextUpload, permission_offer: onceOffer }));
    const nextRequest = { ...request, original: nextOriginal, call_id: decision, upload: nextUpload, permission_offer: onceOffer, request_id: `sha256:${nextDigest}`, resume: { tool: request.resume.tool, correlation_id: `browser-upload-resume:${nextDigest}` } };
    await prepare(nextRequest, reservation);
    assert.equal((await control.request('OctopusBrowser.lookupPermission', { request_id: nextRequest.request_id })).result, null, 'saved session cannot override fresh once-only eligibility');
    phase(control, 'filechooser_resume', { request_id: nextRequest.request_id });
    const answer = { request_id: nextRequest.request_id, call_id: decision, decision, ...(decision === 'decline' ? { selected_persistence: 'once' } : {}) };
    assert.equal((await control.request('OctopusBrowser.rememberPermission', { request_id: nextRequest.request_id, response: answer, source_artifact: answerArtifact })).result.status, 'not_saved');
    const ended = await control.request('OctopusBrowser.applyFileChooser', { reservation_id: reservation, request_id: nextRequest.request_id, decision });
    assert.equal(ended.result.status, 'cancelled'); assert.match(ended.result.error, decision === 'cancel' ? /cancelled/ : /declined/); assert.equal(effects.length, 2);
  }
  guest.destroy(); await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(await fs.readFile(effects[0].params.files[0]), bytes, 'applied FileList bytes outlive origin tab until context disposal');
  assert.equal(guest.calls.some(call => call.method === 'Target.activateTarget'), false);
  const { waitForUrl } = require('../browser-load-wait.cjs');
  const events = Object.assign(new EventEmitter(), { async call(_id, method) { return method === 'Runtime.evaluate' ? { result: { value: { href: 'https://wait.test/a/b?x=1', readyState: 'complete' } } } : method === 'Page.getFrameTree' ? { frameTree: { frame: { id: 'main' } } } : {}; } });
  assert.equal((await waitForUrl(events, 1, 'https://wait.test/**?x=1', 'load', 20)).url, 'https://wait.test/a/b?x=1');
  const waiting = waitForUrl(events, 1, 'https://wait.test/*?x=1', 'commit', 100);
  setImmediate(() => { events.emit('event', { source: { tabId: 1 }, method: 'Page.frameNavigated', params: { frame: { id: 'other', url: 'https://wait.test/a?x=1' } } }); events.emit('event', { source: { tabId: 1 }, method: 'Page.frameNavigated', params: { frame: { id: 'main', url: 'https://wait.test/a?x=1' } } }); });
  assert.equal((await waiting).main_frame_id, 'main'); assert.equal(events.listenerCount('event'), 0);
  const { createBrowserWaiters } = require('../browser-waiters.cjs');
  const waiterStore = createBrowserWaiters({maxWaiters:1, maxBytes:65536}); t.after(() => waiterStore.stop());
  const pin = {host_generation:'1'.repeat(32), worker_environment_id:'wait-room', formation_pin:'2'.repeat(64), browser_context_id:'CONTEXT',
    target_id:'TARGET', debugger_epoch:0, start_source_movement_seq:100, start_artifact_content_hash:'3'.repeat(64)};
  const reads = [], waitObservation = Object.assign(new EventEmitter(), {async call(tab, method) { reads.push({tab, method}); return events.call(tab, method); }});
  const start = {pin, creator:'page-owner', arguments:{phase:'start', target_id:'TARGET', url_pattern:'https://wait.test/arrival', timeout_ms:1000},
    cdp:waitObservation, validateSource:async () => {}};
  const prepared = await waiterStore.start(start);
  assert.equal(prepared.status, 'pending'); assert.equal(waitObservation.listenerCount('event'), 1);
  await assert.rejects(waiterStore.start(start), /retained-record bound/);
  const setupCount = reads.length;
  const completion = waiterStore.complete(prepared.waiter, {reader:'completion102', validate:async () => {}});
  waitObservation.emit('event', {source:{tabId:'TARGET'}, method:'Page.frameNavigated', params:{frame:{id:'main', url:'https://wait.test/arrival'}}});
  const complete = await completion;
  assert.equal(complete.status, 'satisfied'); assert.equal(complete.observation.url, 'https://wait.test/arrival');
  assert.equal(reads.length, setupCount, 'completion reads an existing observation without another CDP command');
  assert.equal(waiterStore.retained().records, 1); assert.ok(waiterStore.retained().bytes > 0);
  assert.throws(() => waiterStore.capture(prepared.waiter, 'another-invocation'), /admitted terminal/);
  waiterStore.capture(prepared.waiter, 'completion102'); assert.deepEqual(waiterStore.retained(), {records:0, bytes:0});
  await assert.rejects(waiterStore.complete(prepared.waiter, {reader:'stale', validate:async () => {}}), /exact owned source/);
  const next = await waiterStore.start({...start, pin:{...pin, start_source_movement_seq:101}});
  const ended = waiterStore.complete(next.waiter, {reader:'completion103', validate:async () => {}});
  waiterStore.releaseCreator('page-owner');
  assert.equal((await ended).status, 'source_lost'); assert.equal(waitObservation.listenerCount('event'), 0);
  waiterStore.capture(next.waiter, 'completion103'); assert.deepEqual(waiterStore.retained(), {records:0, bytes:0});
});
