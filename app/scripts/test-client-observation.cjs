const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { test, after } = require('node:test');
const { JSDOM } = require('jsdom');
const ts = require('typescript');
const dom = new JSDOM('', { url: 'https://client.test' });
globalThis.window = dom.window; globalThis.document = dom.window.document; globalThis.StorageEvent = dom.window.StorageEvent;
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
after(() => dom.window.close());
const bundle = require('esbuild').buildSync({ stdin: { contents: 'export * from "./src/client-surfaces/clientObservation"; export * from "./src/client-surfaces/homeWidgetControls"; export * from "./src/client-surfaces/documentRegistry";', resolveDir: path.resolve(__dirname, '..') }, bundle: true, platform: 'node', format: 'cjs', write: false, external: ['react'], loader: { '.css': 'empty' }, define: { 'import.meta.env.DEV': 'false' } });
const loaded = { exports: {} }; Function('module', 'exports', 'require', bundle.outputFiles[0].text)(loaded, loaded.exports, require);
const { clientObservationContext, readHomeWidgetInventory, applyHomeWidgetAction, homeWidgetHiddenKey, saveHomeWidgetStrings } = loaded.exports;
const messages = require('../../registry/interface/world_operator_chat/client/operatorMessages.cjs');
const { readExistingOperatorMessageEnvelope } = require('../../registry/interface/world_operator_chat/client/messageAdmission.cjs');
function fixture(count = 2) {
  const extensions = Array.from({ length: count }, (_, index) => ({
    manifest: { id: `extension.vendor.${index}`, name: `Произвольный ${index}`, category: 'widget', contribution: { kind: 'surface' } },
    surface: { document: { id: `widget.vendor.${index}` } }, sourceDigest: `sha256:${index.toString(16).padStart(64, 'a')}`,
    installed: true, configured: index !== 1, admission: index === 1 ? 'pending' : 'reviewed',
  }));
  return { accessProfile: 'live', runId: 'arbitrary-home', interfaceId: 'arbitrary-client', ready: true,
    extensions: { hydrated: true, extensions }, surfaces: [],
    availableSurfaces: [{ id: 'surface.custom.navigator', label: 'Navigator', document: { sourceDigest: 'sha256:' + 'b'.repeat(64) } }],
  };
}

test('client observation keeps exact arbitrary inventory rows, hidden state and bounded whole-row omissions', async () => {
  window.localStorage.clear(); const options = fixture();
  let snapshot = clientObservationContext(options), data = JSON.parse(snapshot[0].text);
  assert.deepEqual(data.widgets, readHomeWidgetInventory(options));
  assert.equal(data.widgets[0].state, 'hidden'); assert.equal(data.widgets[1].available, false);
  assert.equal(data.widgets[1].configured, false); assert.equal(data.widgets[1].review, 'pending');
  assert.equal(data.surfaces[0].surface_id, options.availableSurfaces[0].id);
  saveHomeWidgetStrings(homeWidgetHiddenKey('live'), new Set(['widget.vendor.1']));
  snapshot = clientObservationContext(options); data = JSON.parse(snapshot[0].text);
  assert.equal(data.widgets[0].state, 'visible'); assert.equal(data.widgets[1].state, 'hidden');
  const many = fixture(80), inventory = readHomeWidgetInventory(many), bounded = clientObservationContext(many)[0].text;
  assert.ok(Buffer.byteLength(bounded) <= 4096); const compact = JSON.parse(bounded);
  assert.equal(compact.widgets.length + compact.omitted_widgets, inventory.length); assert.ok(compact.omitted_widgets > 0);
  assert.equal(compact.surfaces.length + compact.omitted_surfaces, many.availableSurfaces.length);
  for (const row of compact.widgets) assert.deepEqual(row, inventory.find(item => item.widget_id === row.widget_id));
  const target = data.widgets[0], action = { action: 'set_widget', changes: [{ ...target, state: 'hidden' }] };
  options.extensions.extensions[0].sourceDigest = 'sha256:' + 'c'.repeat(64);
  await assert.rejects(applyHomeWidgetAction(options, action, 'stale'), /revision changed/);
  await assert.rejects(applyHomeWidgetAction({ ...options, accessProfile: 'lab' }, action, 'ungranted'), /not ready/);
  window.localStorage.setItem(homeWidgetHiddenKey('live'), '{}');
  assert.equal(JSON.parse(clientObservationContext(options)[0].text).widgets_status, 'unavailable');
  await assert.rejects(applyHomeWidgetAction(options, action, 'corrupt'), /malformed/);
  window.localStorage.clear();
});

test('client observation stays stable during projection refresh and becomes unavailable on read failure', async () => {
  const React = require('react'), { createRoot } = require('react-dom/client');
  const source = await fs.readFile(path.join(__dirname, '../src/client-surfaces/useClientSurfaceDocuments.ts'), 'utf8');
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  let generation = 0, resolve, reject, state;
  const module = { exports: {} }, mocks = {
    react: React, './transport': {}, './documentRegistry': loaded.exports,
    '../projectionLive': { useProjectionGeneration: () => generation },
    '../projectionClient': { readProjection: () => new Promise((yes, no) => { resolve = yes; reject = no; }) },
  };
  Function('require', 'module', 'exports', code)(id => mocks[id], module, module.exports);
  function View() { state = module.exports.useClientSurfaceDocuments('live'); return null; }
  const root = createRoot(document.createElement('div'));
  const observation = () => clientObservationContext({ ...fixture(), ready: state.hydrated, surfaces: state.voiceSurfaces });
  try {
    await React.act(async () => root.render(React.createElement(View)));
    assert.equal(JSON.parse(observation()[0].text).widgets_status, 'unavailable');
    await React.act(async () => resolve({ generation: 1, root: '/surfaces', documents: [], errors: [] }));
    const before = observation(); assert.equal(JSON.parse(before[0].text).widgets_status, 'ready');
    generation += 1; await React.act(async () => root.render(React.createElement(View)));
    assert.equal(state.loading, true); assert.equal(state.hydrated, true); assert.deepEqual(observation(), before);
    await React.act(async () => resolve({ generation: 2, root: '/surfaces', documents: [], errors: [] }));
    assert.deepEqual(observation(), before);
    generation += 1; await React.act(async () => root.render(React.createElement(View)));
    await React.act(async () => reject(new Error('projection unavailable')));
    assert.equal(state.hydrated, false); assert.equal(JSON.parse(observation()[0].text).widgets_status, 'unavailable');
  } finally { await React.act(async () => root.unmount()); }
});

test('text context carries current client IDs and a prepared envelope replays its original snapshot', async t => {
  const snapshot = clientObservationContext(fixture()), reads = [];
  snapshot.forEach(Object.freeze); Object.freeze(snapshot);
  const context = await messages.operatorTurnClientContext({ readProjection: async method => {
    reads.push(method); return { chats: [], scheduled_interfaces: [], projects: [] };
  } }, 'home', snapshot);
  assert.deepEqual(context[1], snapshot[0]); assert.equal(reads.length, 3);
  assert.deepEqual(await messages.operatorTurnClientContext({}, 'task', snapshot), snapshot);
  for (const [name, bytes] of [['client_observation', 4096], ['home_state', 12288]]) {
    assert.throws(() => messages.operatorClientContext([{ name, text: 'é'.repeat(bytes / 2 + 1) }]), /exceeds/);
  }
  assert.doesNotThrow(() => messages.operatorClientContext([{ name: 'home_state', text: 'x'.repeat(12288) }, { name: 'client_observation', text: 'x'.repeat(4096) }]));
  assert.throws(() => messages.operatorClientContext([...snapshot, { name: 'extra', text: 'x'.repeat(16384) }]), /exceeds/);
  assert.throws(() => messages.operatorClientContext([{ ...snapshot[0], grant: true }]), /field set/);
  const artifactRoot = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-observation-'));
  t.after(() => fs.rm(artifactRoot, { recursive: true, force: true }));
  await fs.mkdir(path.join(artifactRoot, 'payloads'));
  const bytes = Buffer.from(JSON.stringify({ text: 'Show it', message_id: 'same_input', attachments: [], client_context: context }));
  await fs.writeFile(messages.operatorMessagePayloadPath(path.join(artifactRoot, 'payloads'), 'same_input'), bytes);
  const replay = await readExistingOperatorMessageEnvelope({}, { artifactRoot, attachmentRoot: path.join(artifactRoot, 'attachments'), messageId: 'same_input', requestedText: 'Show it', attachmentInputs: [], clientContext: clientObservationContext(fixture(8)) });
  assert.deepEqual(replay.messageBytes, bytes); assert.deepEqual(replay.payload.client_context, context);
});

test('voice offers the same observation and replaces it only after acceptance, with no unchanged update', async () => {
  const code = ts.transpileModule(await fs.readFile(path.join(__dirname, '../src/components/voice/voiceCallArtifacts.ts'), 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const writes = []; let resolveReceipt, receiptStatus = 'context_retained', admitted = true;
  const module = { exports: {} }, mocks = {
    '../../projectionClient': { submitInterfaceArtifact: async request => { writes.push(request); return { verdict: admitted ? { admitted: { seq: writes.length } } : { refused: { reason: 'context refused' } } }; }, readProjection: async (_, params) => {
      const label = params.labels[0], latest = writes.at(-1).payload;
      if (label === 'realtime.session.updated') await new Promise(resolve => { resolveReceipt = resolve; });
      return { artifacts: [{ seq: writes.length, label, payload: { correlation_id: 'offer', call_id: 'rtc_snapshot', ok: receiptStatus !== 'uncertain', status: receiptStatus, update_id: latest.update_id } }] };
    } }, '../../projectionLive': { nextProjectionChange: () => Promise.resolve() },
  };
  Function('require', 'module', 'exports', code)(id => mocks[id], module, module.exports);
  const api = module.exports, first = clientObservationContext(fixture()), next = clientObservationContext(fixture(3));
  first.forEach(Object.freeze); Object.freeze(first); next.forEach(Object.freeze); Object.freeze(next);
  const call = { binding: { controlInterfaceId: 'voice-control', providerId: 'voice-provider', runId: 'voice-run', delegationInterfaceId: 'home-input' }, correlationId: 'offer', answer: null, control: null, stop: null, closeRequested: false, callId: '', remoteClosed: false };
  await api.openVoiceCall(call, 'v=0', [], first); assert.deepEqual(writes[0].payload.client_context, first);
  const signal = new AbortController().signal;
  await api.updateVoiceClientContext(call, first, signal); assert.equal(writes.length, 1);
  const updating = api.updateVoiceClientContext(call, next, signal);
  await new Promise(setImmediate); assert.deepEqual(call.clientContext, first); resolveReceipt(); await updating;
  assert.deepEqual(call.clientContext, next); assert.deepEqual(writes[1].payload.client_context, next); assert.equal(writes[1].payload.channel, 'context');
  assert.equal(writes[1].payload.text, undefined);
  await api.updateVoiceClientContext(call, next, signal); assert.equal(writes.length, 2);
  for (const status of ['uncertain', 'transport_written']) {
    receiptStatus = status;
    const failed = api.updateVoiceClientContext(call, first, signal);
    const refused = assert.rejects(failed, /did not retain/); await new Promise(setImmediate); resolveReceipt(); await refused;
    assert.deepEqual(call.clientContext, next);
  }
  admitted = false;
  await assert.rejects(api.updateVoiceClientContext(call, first, signal), /context refused/);
  assert.deepEqual(call.clientContext, next); assert.equal(writes.length, 5);
});
