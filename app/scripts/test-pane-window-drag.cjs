const assert = require('node:assert/strict');
const { test } = require('node:test');
const { EventEmitter } = require('node:events');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const { createClientWindowRegistry } = require('../electron/clientWindows.cjs');
const { createPaneWindowDragHost } = require('../electron/paneWindowDrag.cjs');
const target = { kind: 'surface', workspaceKey: 'octopus-os', surfaceId: 'chat', runId: 'run-exact-1' };
const ref = (value) => ({ transferId: value.transferId });
const tick = () => new Promise((resolve) => setImmediate(resolve));
function fixture(options = {}) {
  let id = 0, contentsId = 0, point = { x: 20, y: 20 };
  const events = [], previews = [], destinations = [];
  const registry = createClientWindowRegistry({ idFactory: () => `boundary-${++id}`, transferTimeoutMs: options.timeout ?? 2000, onTransferEnd: (event) => events.push(event) });
  function window(bounds, token = 'auth-a', extra = {}) {
    const win = new EventEmitter();
    Object.assign(win, {
      destroyed: false, visible: true, minimized: false, zoom: 1,
      webContents: { id: ++contentsId, getZoomFactor: () => win.zoom },
      getBounds: () => bounds, getContentBounds: () => bounds,
      isDestroyed: () => win.destroyed, isVisible: () => win.visible, isMinimized: () => win.minimized,
      close() { this.destroyed = true; this.emit('closed'); },
      show() { this.visible = true; }, focus() { this.emit('focus'); },
    });
    registry.register(win, { accessToken: token, ...extra });
    return registry.entryForWindow(win);
  }
  const source = window({ x: 0, y: 0, width: 800, height: 600 }, 'auth-a', { primary: true });
  function preview() {
    const value = { closed: false, visible: false, positions: [], close() { this.closed = true; }, hide() { this.visible = false; }, showInactive() { this.visible = true; }, setPosition(x, y) { this.positions.push({ x, y }); } };
    previews.push(value); return value;
  }
  const host = createPaneWindowDragHost({
    registry, cursor: () => point,
    capturePreview: options.capturePreview ?? (async () => ({})),
    openPreview: options.openPreview ?? (async () => preview()),
    positionDestination: options.positionDestination,
    discardDestination: options.discardDestination,
    createDestination(owner, at) {
      const receiver = window({ ...at, width: 800, height: 600 }, owner.accessToken, { reattachWindowId: owner.bootstrap.windowId });
      receiver.win.visible = false; destinations.push(receiver); return receiver;
    },
    notifyDestination: (owner, accepted) => events.push({ owner, accepted }),
    notifySource: (owner, state) => events.push({ owner, state }),
    reportError: (error) => events.push({ error }),
  });
  const lease = (owner = source, paneTarget = target, tabId = 'chat:run-exact-1') => registry.beginPaneTransfer(owner.webContentsId, { sourcePane: 'secondary', sourceTabId: tabId, target: paneTarget });
  const begin = (transfer, owner = source) => host.begin(owner.webContentsId, { ...ref(transfer), captureRect: { x: 8, y: 9, width: 156, height: 28 }, rowRect: { x: 0, y: 0, width: 800, height: 46 }, offset: { x: 12, y: 11 } });
  function dispose() { for (const entry of registry.entries()) entry.win.close(); }
  return { registry, source, host, window, preview, previews, destinations, events, lease, begin, dispose, point: (next) => { point = next; } };
}
async function commit(f, transfer, destination) {
  const request = ref(transfer);
  const result = f.registry.commitPaneTransfer(destination.webContentsId, request);
  f.registry.acknowledgePaneTransfer(f.source.webContentsId, { ...request, released: true });
  await result.outcome;
  return () => f.registry.finalizePaneTransfer(destination.webContentsId, request);
}

test('live label precedes release; target and source remain until finalization; reload receipts are owner bound', async (t) => {
  const f = fixture(); t.after(f.dispose);
  const transfer = f.lease();
  await f.begin(transfer); await tick();
  assert.equal(f.destinations.length, 0);
  f.point({ x: 1000, y: 200 });
  assert.equal(f.host.move(f.source.webContentsId, ref(transfer)).phase, 'floating');
  assert.equal(f.previews[0].visible, true);
  assert.deepEqual(f.previews[0].positions.at(-1), { x: 988, y: 189 });
  assert.equal(f.destinations.length, 0);
  const moved = f.host.finish(f.source.webContentsId, ref(transfer));
  const destination = f.destinations[0];
  assert.equal(destination.win.visible, false);
  assert.equal(destination.bootstrap.reattachWindowId, 'main');
  assert.deepEqual(f.registry.pendingDestinationPaneTransfers(destination.webContentsId)[0].target, target);
  assert.equal(f.registry.pendingDestinationPaneTransfers(destination.webContentsId)[0].committed, false);
  assert.equal(f.source.win.isDestroyed(), false);
  let complete = false; moved.then(() => { complete = true; });
  const finalize = await commit(f, transfer, destination);
  await tick();
  assert.equal(complete, false);
  assert.equal(f.registry.pendingSourceFinalizations(f.source.webContentsId).length, 0);
  assert.equal(f.registry.pendingDestinationPaneTransfers(destination.webContentsId)[0].committed, true);
  assert.deepEqual(await f.registry.resumePaneTransfer(destination.webContentsId, ref(transfer)), { ...transfer, sourcePane: 'secondary', sourceTabId: 'chat:run-exact-1', target });
  finalize();
  assert.equal((await moved).status, 'transferred');
  assert.equal(f.previews[0].closed, true);
  assert.deepEqual(f.registry.pendingSourceFinalizations(f.source.webContentsId)[0].target, target);
  assert.throws(() => f.registry.acknowledgeSourceFinalization(destination.webContentsId, ref(transfer)), /authenticated owner/);
  f.registry.acknowledgeSourceFinalization(f.source.webContentsId, ref(transfer));
  assert.deepEqual(f.registry.pendingSourceFinalizations(f.source.webContentsId), []);
  assert.throws(() => f.registry.finalizePaneTransfer(destination.webContentsId, ref(transfer)), /unavailable/);
});

test('ready preview samples release cursor before the next move timer', async (t) => {
  const f = fixture(); t.after(f.dispose);
  const transfer = f.lease();
  await f.begin(transfer); await tick();
  assert.equal(f.previews[0].visible, false);
  f.point({ x: 1000, y: 200 });
  const moved = f.host.finish(f.source.webContentsId, ref(transfer));
  assert.equal(f.previews[0].visible, true);
  assert.deepEqual(f.previews[0].positions.at(-1), { x: 988, y: 189 });
  assert.equal(f.destinations.length, 1);
  assert.equal(f.registry.pendingDestinationPaneTransfers(f.destinations[0].webContentsId).length, 1);
  (await commit(f, transfer, f.destinations[0]))();
  assert.equal((await moved).status, 'transferred');
  assert.equal(f.previews[0].closed, true);
});

test('release before capture cancels; a late preview is disposed, never opened retroactively', async (t) => {
  let resolveCapture;
  const f = fixture({ capturePreview: () => new Promise((resolve) => { resolveCapture = resolve; }) }); t.after(f.dispose);
  const transfer = f.lease(), preparing = f.begin(transfer);
  f.point({ x: 1000, y: 200 });
  assert.deepEqual(await f.host.finish(f.source.webContentsId, ref(transfer)), { status: 'cancelled' });
  resolveCapture({});
  assert.equal((await preparing).phase, 'cancelled');
  assert.equal(f.previews.length, 0);
  assert.equal(f.destinations.length, 0);
  let resolvePreview;
  const g = fixture({ openPreview: () => new Promise((resolve) => { resolvePreview = resolve; }) }); t.after(g.dispose);
  const opening = g.lease(); await g.begin(opening); await tick();
  g.host.cancel(g.source.webContentsId, ref(opening));
  const late = g.preview(); resolvePreview(late); await tick();
  assert.equal(late.closed, true);
  const unprepared = f.lease();
  assert.equal((await f.host.finish(f.source.webContentsId, ref(unprepared))).status, 'cancelled');
});

test('15px detach distance, return to source, and zoom cancellation retain source', async (t) => {
  const f = fixture(); t.after(f.dispose);
  const transfer = f.lease(); await f.begin(transfer);
  f.point({ x: 200, y: 60 });
  assert.equal(f.host.move(f.source.webContentsId, ref(transfer)).phase, 'prepared');
  f.point({ x: 200, y: 62 });
  assert.equal(f.host.move(f.source.webContentsId, ref(transfer)).phase, 'floating');
  f.point({ x: 200, y: 44 });
  assert.equal(f.host.move(f.source.webContentsId, ref(transfer)).phase, 'prepared');
  assert.equal((await f.host.finish(f.source.webContentsId, ref(transfer))).status, 'cancelled');
  const next = f.lease(); await f.begin(next); await tick(); f.source.win.zoom = 1.25;
  assert.throws(() => f.host.move(f.source.webContentsId, ref(next)), /zoom changed/);
  assert.equal(f.previews[1].closed, true);
  assert.equal(f.destinations.length, 0);
});

test('existing authenticated drop zone adopts exact target; foreign and outside-zone drops cancel', async (t) => {
  const f = fixture(); t.after(f.dispose);
  const peer = f.window({ x: 900, y: 0, width: 800, height: 600 });
  f.host.registerDropZone(peer.webContentsId, { x: 0, y: 0, width: 800, height: 500 });
  const transfer = f.lease(); await f.begin(transfer); f.point({ x: 1000, y: 100 });
  assert.equal(f.host.move(f.source.webContentsId, ref(transfer)).destinationWindowId, peer.bootstrap.windowId);
  const moved = f.host.finish(f.source.webContentsId, ref(transfer));
  assert.equal(f.destinations.length, 0);
  assert.ok(f.events.some((event) => event.accepted?.transferId === transfer.transferId));
  (await commit(f, transfer, peer))(); await moved;
  for (const [token, point] of [['auth-a', { x: 1000, y: 550 }], ['auth-b', { x: 1000, y: 100 }]]) {
    f.registry.setAccessToken(peer.webContentsId, token);
    const next = f.lease(); await f.begin(next); f.point(point);
    f.host.move(f.source.webContentsId, ref(next));
    assert.equal((await f.host.finish(f.source.webContentsId, ref(next))).status, 'cancelled');
  }
  assert.equal(f.destinations.length, 0);
});

test('source authentication, request shape, geometry, terminal and unresolved/live-browser adoption fail closed', async (t) => {
  const f = fixture(); t.after(f.dispose);
  const peer = f.window({ x: 900, y: 0, width: 800, height: 600 });
  const transfer = f.lease();
  await assert.rejects(f.begin(transfer, peer), /another authenticated source/);
  assert.throws(() => f.host.move(f.source.webContentsId, { ...ref(transfer), screenX: 5 }), /invalid/);
  await assert.rejects(f.host.begin(f.source.webContentsId, { ...ref(transfer), captureRect: { x: -1, y: 0, width: 100, height: 28 }, rowRect: { x: 0, y: 0, width: 800, height: 46 }, offset: { x: 0, y: 0 } }), /inside source chrome/);
  f.host.cancel(f.source.webContentsId, ref(transfer));
  assert.throws(() => f.lease(f.source, { kind: 'terminal' }), /Terminal panes/);
  for (const extra of [{ panelKind: 'browser' }, { surfaceId: 'opaque-browser-surface' }]) {
    const browser = f.lease(f.source, { kind: 'worker_environment', workspaceKey: 'octopus-os', target: { schemaVersion: 1, workerEnvironmentId: 'we-1', ...extra } });
    await assert.rejects(f.begin(browser), /Live browser adoption is unavailable/);
    await assert.rejects(f.host.detach(f.source.webContentsId, ref(browser)), /Live browser adoption is unavailable/);
    f.host.cancel(f.source.webContentsId, ref(browser));
  }
  assert.equal(f.previews.length, 0); assert.equal(f.destinations.length, 0);
});

test('destination failure or deadline after ACK rejects handoff and preserves source ownership', async (t) => {
  for (const mode of ['close', 'reject', 'deadline']) {
    const f = fixture({ timeout: mode === 'deadline' ? 30 : 2000 }); t.after(f.dispose);
    const transfer = f.lease(); await f.begin(transfer); f.point({ x: 1000, y: 100 }); f.host.move(f.source.webContentsId, ref(transfer));
    const moved = f.host.finish(f.source.webContentsId, ref(transfer));
    const rejected = assert.rejects(moved, /finalization|handoff/);
    const destination = f.destinations[0]; await commit(f, transfer, destination);
    if (mode === 'close') destination.win.close();
    if (mode === 'reject') f.registry.rejectDestinationPaneTransfer(destination.webContentsId, ref(transfer));
    if (mode === 'deadline') await new Promise((resolve) => setTimeout(resolve, 45));
    await rejected;
    assert.equal(f.source.win.isDestroyed(), false);
    assert.deepEqual(f.registry.pendingSourceFinalizations(f.source.webContentsId), []);
    assert.equal(f.previews[0].closed, true);
  }
});

test('reattach uses original authenticated host and does not create another window', async (t) => {
  const f = fixture(); t.after(f.dispose);
  const detached = f.window({ x: 900, y: 0, width: 800, height: 600 }, 'auth-a', { reattachWindowId: 'main' });
  const transfer = f.lease(detached);
  const moved = f.host.reattach(detached.webContentsId, ref(transfer));
  const result = f.registry.commitPaneTransfer(f.source.webContentsId, ref(transfer));
  f.registry.acknowledgePaneTransfer(detached.webContentsId, { ...ref(transfer), released: true });
  await result.outcome;
  f.registry.finalizePaneTransfer(f.source.webContentsId, ref(transfer));
  assert.equal((await moved).windowId, 'main');
  assert.equal(f.destinations.length, 0);
  f.registry.setAccessToken(f.source.webContentsId, 'auth-b');
  const next = f.lease(detached);
  await assert.rejects(f.host.reattach(detached.webContentsId, ref(next)), /Original authenticated window/);
});

test('destination rollback retains unrelated edits and changed target instances', () => {
  const module = { exports: {} };
  const source = fs.readFileSync(require.resolve('../src/shell/paneTransferState.ts'), 'utf8');
  vm.runInNewContext(ts.transpile(source, { module: ts.ModuleKind.CommonJS }), { exports: module.exports, module });
  const rollback = module.exports.rollbackDestinationPaneTransfer;
  const staged = { id: 'staged', target }, existing = { id: 'old', target: { ...target, runId: 'old' } }, added = { id: 'new', target: { ...target, runId: 'new' } };
  const before = { activeTabId: existing.id, tabs: [existing] };
  const current = { activeTabId: added.id, tabs: [existing, staged, added] };
  assert.deepEqual(JSON.parse(JSON.stringify(rollback(current, before, staged))), { activeTabId: added.id, tabs: [existing, added] });
  const changed = { activeTabId: staged.id, tabs: [{ ...staged, target: { ...target, runId: 'changed' } }] };
  assert.equal(rollback(changed, before, staged), changed);
});

test('source veto and auth revocation cannot finalize or retain an orphan preview', async (t) => {
  const f = fixture(); t.after(f.dispose);
  const transfer = f.lease(); await f.begin(transfer); f.point({ x: 1000, y: 100 }); f.host.move(f.source.webContentsId, ref(transfer));
  const moved = f.host.finish(f.source.webContentsId, ref(transfer));
  const rejected = assert.rejects(moved, /finalization/);
  const destination = f.destinations[0];
  const committed = f.registry.commitPaneTransfer(destination.webContentsId, ref(transfer));
  assert.throws(() => f.registry.acknowledgePaneTransfer(destination.webContentsId, { ...ref(transfer), released: true }), /committed source/);
  f.registry.acknowledgePaneTransfer(f.source.webContentsId, { ...ref(transfer), released: false });
  await assert.rejects(committed.outcome, /source retained ownership/); await rejected;
  assert.equal(f.source.win.isDestroyed(), false);
  assert.equal(destination.win.isDestroyed(), true);
  const next = f.lease(); await f.begin(next);
  f.registry.clearAccessToken('auth-a'); await tick();
  assert.equal(f.previews[1].closed, true);
  assert.throws(() => f.host.move(f.source.webContentsId, ref(next)), /unavailable/);
});

test('detached destination is prewarmed while floating and anchored so its tab lands under the release point', async (t) => {
  const placed = [], discarded = [];
  const f = fixture({
    positionDestination: (entry, origin) => placed.push({ id: entry.webContentsId, x: origin.x, y: origin.y }),
    discardDestination: (entry) => { discarded.push(entry.webContentsId); entry.win.close(); },
  }); t.after(f.dispose);
  const transfer = f.lease(); await f.begin(transfer); await tick();
  f.point({ x: 1000, y: 200 }); f.host.move(f.source.webContentsId, ref(transfer));
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(f.destinations.length, 1);
  const prewarm = f.destinations[0];
  assert.equal(prewarm.win.visible, false);
  f.point({ x: 1100, y: 300 });
  const moved = f.host.finish(f.source.webContentsId, ref(transfer));
  assert.equal(f.destinations.length, 1);
  assert.deepEqual(placed.at(-1), { id: prewarm.webContentsId, x: 1100 - 12 - 8, y: 300 - 11 - 9 });
  assert.deepEqual(f.host.anchor(f.source.webContentsId, { ...ref(transfer), rect: { x: 0, y: 0, width: 10, height: 10 } }), { anchored: false });
  assert.deepEqual(f.host.anchor(prewarm.webContentsId, { ...ref(transfer), rect: { x: 300, y: 10, width: 200, height: 28 } }), { anchored: true });
  assert.deepEqual(placed.at(-1), { id: prewarm.webContentsId, x: 1100 - 300 - 12, y: 300 - 10 - 11 });
  (await commit(f, transfer, prewarm))();
  assert.equal((await moved).windowId, prewarm.bootstrap.windowId);
  assert.deepEqual(discarded, []);
  const unused = f.lease(); await f.begin(unused); await tick();
  f.point({ x: 1000, y: 200 }); f.host.move(f.source.webContentsId, ref(unused));
  await new Promise((resolve) => setTimeout(resolve, 5));
  f.point({ x: 200, y: 20 });
  assert.equal((await f.host.finish(f.source.webContentsId, ref(unused))).status, 'cancelled');
  assert.deepEqual(discarded, [f.destinations[1].webContentsId]);
});
