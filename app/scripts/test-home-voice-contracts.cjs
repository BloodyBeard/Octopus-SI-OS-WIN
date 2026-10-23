const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const ts = require('typescript');
const { parseHomeVoiceContext, projectHomeVoiceContext } = require('../electron/homeVoiceProjection.cjs');

function load(name, mocks = {}) {
  const filename = path.join(__dirname, '../src/components/voice', name);
  const code = ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  Function('require', 'module', 'exports', code)(id => { assert.ok(id in mocks, id); return mocks[id]; }, module, module.exports);
  return module.exports;
}
const binding = { runId: 'run_voice', providerId: 'voice_home', controlInterfaceId: 'voice_control_home', delegationInterfaceId: 'voice_delegation_home' };
const call = () => ({ binding, correlationId: 'offer-1', answer: null, control: null, stop: null, closeRequested: false, creationDisposition: 'not_created', callId: '', remoteClosed: false });
const declaration = { run_id: binding.runId, provider_id: binding.providerId, control_interface_id: binding.controlInterfaceId, delegation_interface_id: binding.delegationInterfaceId };
const tick = () => new Promise(resolve => setImmediate(resolve));

function host(read, submit) {
  return load('voiceCallArtifacts.ts', {
    '../../projectionClient': { readProjection: read, submitInterfaceArtifact: submit },
    '../../projectionLive': { nextProjectionChange: tick },
  });
}

test('Home voice resolves its exact independent Run and refuses crossed ownership', () => {
  const state = { client_surface_role: 'home', voice_context: declaration };
  const record = { run_id: 'run_home', ee_id: 'existence', worker: 'home_worker', interface_state: {home_client: state}, checkpoints: [{ interface: binding.delegationInterfaceId, out_interfaces: ['text_home'] }] };
  const voice = { run_id: binding.runId, ee_id: 'existence', worker: 'home_worker', interface_state: {[binding.controlInterfaceId]: {home_run: record.run_id, home_interface: 'home_client'}}, checkpoints: [{ interface: binding.controlInterfaceId, out_interfaces: [binding.providerId] }, { interface: binding.providerId, out_interfaces: [binding.delegationInterfaceId] }] };
  const sidecar = { interface: 'home_client', surface_role: 'home', voice_context: declaration };
  const records = new Map([[voice.run_id, voice]]);
  assert.deepEqual(projectHomeVoiceContext(record, sidecar, state, records), declaration);
  assert.deepEqual(projectHomeVoiceContext(record, null, state, records), declaration);
  assert.equal(projectHomeVoiceContext(record, {}, {}, records), null);
  assert.throws(() => projectHomeVoiceContext({...record, run_id: binding.runId}, sidecar, state, records), /own execution Run/);
  assert.throws(() => projectHomeVoiceContext(record, sidecar, state, new Map()), /independent Run/);
  assert.throws(() => projectHomeVoiceContext(record, sidecar, { ...state, voice_context: { ...declaration, provider_id: 'another' } }, records), /conflicts/);
  assert.throws(() => projectHomeVoiceContext({ ...record, checkpoints: [] }, sidecar, state, records), /absent/);
  assert.throws(() => projectHomeVoiceContext(record, sidecar, state, new Map([[voice.run_id, {...voice, ee_id: 'another'}]])), /independent Run/);
  assert.throws(() => parseHomeVoiceContext({ ...declaration, provider_id: declaration.control_interface_id }), /separate/);
});

test('cancelling startup waits for the late answer and closes exactly that call once', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let allowAnswer = false;
  const writes = [];
  let stopId = '';
  const api = host(async (method, params) => {
    assert.equal(method, 'projection.interface_artifacts.list');
    assert.equal(params.interface_id, binding.providerId);
    assert.equal(params.run_id, binding.runId);
    const label = params.labels[0];
    if (label === 'realtime.session.answer' && allowAnswer) return { artifacts: [{ seq: 1, artifact_id: 'answer', label, payload: { correlation_id: 'offer-1', ok: true, call_id: 'rtc_exact', answer_sdp: 'v=0', sideband_connected: true } }] };
    if (label === 'realtime.session.closed' && stopId) return { artifacts: [{ seq: 2, artifact_id: 'closed', label, payload: { correlation_id: 'offer-1', call_id: 'rtc_exact', stop_id: stopId, remote_closed: true } }] };
    return { artifacts: [] };
  }, async request => {
    writes.push(request);
    assert.equal(request.interfaceId, binding.controlInterfaceId);
    if (request.label === 'realtime.session.stop') stopId = request.payload.stop_id;
  });
  const current = call();
  const opening = api.openVoiceCall(current, 'v=0');
  const closing = api.closeVoiceCall(current);
  assert.equal(api.closeVoiceCall(current), closing);
  await tick();
  assert.equal(writes.length, 1);
  t.mock.timers.tick(20_000); await tick();
  assert.equal(current.stop, closing);
  allowAnswer = true;
  await Promise.all([opening, closing]);
  assert.deepEqual(writes.map(row => row.label), ['realtime.session.offer', 'realtime.session.stop']);
  assert.equal(writes[0].payload.delegation_interface, binding.delegationInterfaceId);
  assert.equal(writes[1].payload.call_id, 'rtc_exact');
  assert.equal(current.remoteClosed, true);
});

test('output cursor crosses other calls and refuses sent-only close evidence', async () => {
  let stopId;
  const current = call();
  const api = host(async (_, params) => {
    const label = params.labels[0];
    if (label === 'realtime.session.answer') {
      if (params.after_seq < 500) return { artifacts: Array.from({ length: 500 }, (_, i) => ({ seq: i + 1, artifact_id: `other-${i}`, label, payload: { correlation_id: 'another-call', ok: true, call_id: 'wrong' } })) };
      return { artifacts: [{ seq: 501, artifact_id: 'answer', label, payload: { correlation_id: 'offer-1', ok: true, call_id: 'rtc_exact' } }] };
    }
    return { artifacts: [{ seq: 502, artifact_id: 'close', label, payload: { correlation_id: 'offer-1', call_id: 'rtc_exact', stop_id: stopId, provider_close_sent: true, remote_closed: false } }] };
  }, async request => { stopId = request.payload.stop_id; });
  await api.openVoiceCall(current, 'v=0');
  assert.equal(current.callId, 'rtc_exact');
  await assert.rejects(api.closeVoiceCall(current), /has not confirmed/);
  assert.equal(current.remoteClosed, false);
});

test('failed attachment preserves the created call for one exact confirmed cleanup', async () => {
  const writes = [], current = call();
  const api = host(async (_, params) => ({ artifacts: params.labels[0] === 'realtime.session.closed' && writes.at(-1)?.label !== 'realtime.session.stop' ? [] : [{ seq: 1, artifact_id: 'receipt', label: params.labels[0], payload: {
    correlation_id: current.correlationId, call_id: 'rtc_created',
    ...(params.labels[0] === 'realtime.session.answer'
      ? { ok: false, error: 'sideband attach refused' }
      : { stop_id: writes.at(-1).payload.stop_id, remote_closed: true }),
  } }] }), async request => { writes.push(request); });
  await assert.rejects(api.openVoiceCall(current, 'v=0'), /sideband attach refused/);
  assert.equal(current.callId, 'rtc_created');
  await api.closeVoiceCall(current);
  assert.deepEqual(writes.map(row => row.label), ['realtime.session.offer', 'realtime.session.stop']);
  assert.equal(writes[1].payload.call_id, 'rtc_created');
  assert.equal(current.remoteClosed, true);
});

test('expired offer waits recover late session-state identity without offer replay', async () => {
  const current = call(), writes = [], originalNow = Date.now;
  let now = 0, lateAnswer = false;
  Date.now = () => now;
  try {
    const api = host(async (_, params) => {
      const label = params.labels[0];
      if (label === 'realtime.session.answer' && !lateAnswer) { now += 45001; return { artifacts: [] }; }
      if (label === 'realtime.session.closed' && writes.at(-1)?.label !== 'realtime.session.stop') return { artifacts: [] };
      const evidenceLabel = lateAnswer && params.labels.includes('realtime.session.state') ? 'realtime.session.state' : label;
      return { artifacts: [{ seq: 1, artifact_id: evidenceLabel, label: evidenceLabel, payload: { correlation_id: current.correlationId, call_id: 'rtc_late', ok: true, stop_id: writes.at(-1)?.payload.stop_id, remote_closed: evidenceLabel === 'realtime.session.closed' } }] };
    }, async request => { writes.push(request); });
    await assert.rejects(api.openVoiceCall(current, 'v=0'), /within 45 seconds/);
    await assert.rejects(api.closeVoiceCall(current), /creation remains unconfirmed.*not.*replayed/);
    assert.equal(current.remoteClosed, false);
    lateAnswer = true;
    await api.closeVoiceCall(current);
    assert.deepEqual(writes.map(row => row.label), ['realtime.session.offer', 'realtime.session.stop']);
    assert.equal(writes[1].payload.call_id, 'rtc_late');
    assert.equal(current.remoteClosed, true);
  } finally { Date.now = originalNow; }
});

test('explicit closure retry uses a new stop intent on the same call and keeps context disabled', async () => {
  const current = { ...call(), creationDisposition: 'created', callId: 'rtc_exact', answer: Promise.resolve({}) }, writes = [];
  const api = host(async (_, params) => ({ artifacts: !writes.length ? [] : [{ seq: writes.length, artifact_id: 'closed', label: params.labels[0], payload: { correlation_id: current.correlationId, call_id: current.callId, stop_id: writes.at(-1).payload.stop_id, remote_closed: writes.length === 2 } }] }), async request => { writes.push(request); });
  await assert.rejects(api.closeVoiceCall(current), /not confirmed/);
  assert.equal(current.remoteClosed, false);
  await assert.rejects(api.updateVoiceContext(current, { id: 'late', role: 'user', text: 'Late' }, new AbortController().signal), /not available/);
  await api.closeVoiceCall(current);
  assert.deepEqual(writes.map(row => row.payload.call_id), ['rtc_exact', 'rtc_exact']);
  assert.notEqual(writes[0].payload.stop_id, writes[1].payload.stop_id);
  assert.equal(current.remoteClosed, true);
});

test('explicit not-created evidence releases a rejected offer without fabricated closure', async () => {
  const current = call(), writes = [];
  const api = host(async (_, params) => ({ artifacts: [{ seq: 1, artifact_id: 'refusal', label: params.labels[0], payload: { correlation_id: current.correlationId, ok: false, creation_disposition: 'not_created', error: 'Offer was rejected before creation' } }] }), async request => { writes.push(request); });
  await assert.rejects(api.openVoiceCall(current, 'v=0'), /rejected before creation/);
  await api.closeVoiceCall(current);
  assert.equal(current.creationDisposition, 'not_created');
  assert.equal(current.remoteClosed, false);
  assert.deepEqual(writes.map(row => row.label), ['realtime.session.offer']);
});

test('hangup releases a pending context update and hands in stop and recording only after its receipt re-arms control', async () => {
  const writes = [], receipt = deferred();
  const current = { ...call(), creationDisposition: 'created', callId: 'rtc_exact', answer: Promise.resolve({}) };
  const api = host(async (_, params) => {
    const label = params.labels[0];
    if (label === 'realtime.session.updated') await receipt.promise;
    if (label === 'realtime.session.closed' && writes.at(-1)?.label !== 'realtime.session.stop') return { artifacts: [] };
    return { artifacts: [{ seq: 1, artifact_id: label, label, payload: {
      ok: true, correlation_id: current.correlationId, call_id: current.callId,
      update_id: 'home-context:message-1', status: 'transport_written',
      stop_id: writes.at(-1)?.payload?.stop_id, remote_closed: true,
    } }] };
  }, async request => { writes.push(request); });
  const abort = new AbortController();
  const update = api.updateVoiceContext(current, { id: 'message-1', role: 'user', text: 'Look at this' }, abort.signal);
  const rejected = assert.rejects(update, /abort/i);
  await tick();
  abort.abort();
  await rejected;
  const closing = api.closeVoiceCall(current);
  const recorded = api.deliverVoiceRecording(current, async () => { writes.push({ label: 'realtime.session.recording' }); });
  await tick();
  assert.deepEqual(writes.map(row => row.label), ['realtime.session.update'], 'A spent control generation admits nothing before the provider receipt');
  await assert.rejects(api.updateVoiceContext(current, { id: 'message-2', role: 'user', text: 'Late' }, new AbortController().signal), /not available/);
  receipt.resolve();
  await Promise.all([closing, recorded]);
  assert.deepEqual(writes.map(row => row.label), ['realtime.session.update', 'realtime.session.stop', 'realtime.session.recording']);
  assert.equal(current.remoteClosed, true);
});

test('transcript updates retain protocol identity, partial state and distinct repeated phrases', () => {
  const transcript = load('voiceTranscript.ts').createVoiceTranscript();
  const partial = { id: 'turn-1', role: 'user', text: 'Check', final: false };
  assert.equal(transcript.artifact(partial)[0].final, false);
  const finished = transcript.artifact({ ...partial, text: 'Check the task', final: true });
  assert.equal(finished.length, 1);
  assert.equal(transcript.artifact({ ...partial, text: 'Check the task', final: true }), finished);
  assert.equal(transcript.artifact({ ...partial, id: 'turn-2', text: 'Check the task', final: true }).length, 2);
  assert.throws(() => transcript.artifact(partial), /completed/);
  assert.throws(() => transcript.artifact({ ...partial, id: '' }), /identity/);
});

test('hangup hands in stop once the pending control receipt settles and before any history read', async () => {
  const receipt = deferred();
  const current = { ...call(), creationDisposition: 'created', callId: 'rtc_exact', answer: Promise.resolve({}), control: receipt.promise };
  const writes = [];
  const api = host(async (_, params) => {
    assert.equal(writes.length, 1, 'The stop intent must precede any evidence reads');
    assert.deepEqual(params.labels, ['realtime.session.closed']);
    return { artifacts: [{ seq: 1, artifact_id: 'closed', label: 'realtime.session.closed', payload: { correlation_id: current.correlationId, call_id: current.callId, remote_closed: true } }] };
  }, async request => writes.push(request));
  const closing = api.closeVoiceCall(current);
  await tick();
  assert.equal(writes.length, 0);
  receipt.resolve();
  await closing;
  assert.equal(api.canRetireVoiceCall(current), true);
});

test('hangup bounds each phase across stalled answer, update, read and stop submission', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  for (const stage of ['answer', 'control', 'read', 'stop']) {
    const pending = deferred(), writes = [];
    const current = { ...call(), creationDisposition: 'created', callId: 'rtc_exact', answer: Promise.resolve({}) };
    if (stage === 'answer') { current.answer = pending.promise; current.callId = ''; current.creationDisposition = 'unconfirmed'; }
    if (stage === 'control') current.control = pending.promise;
    let delayed = true;
    const api = host(async (_, params) => {
      if (stage === 'read' && delayed) return pending.promise;
      const stop = writes.at(-1);
      return { artifacts: delayed || !stop ? [] : [{ seq: 1, artifact_id: 'closed', label: 'realtime.session.closed', payload: { correlation_id: current.correlationId, call_id: current.callId, stop_id: stop.payload.stop_id, remote_closed: true } }] };
    }, async request => { writes.push(request); if (stage === 'stop' && delayed) await pending.promise; });
    const closing = api.closeVoiceCall(current);
    const rejected = assert.rejects(closing, /shutdown timed out while waiting/);
    await tick(); t.mock.timers.tick(45_000); await rejected;
    assert.equal(current.stop, null, stage);
    assert.equal(api.canRetireVoiceCall(current), false, stage);
    const count = writes.length;
    if (stage === 'answer') { current.callId = 'rtc_exact'; current.creationDisposition = 'created'; }
    delayed = false; pending.resolve(stage === 'read' ? { artifacts: [] } : undefined);
    await tick();
    assert.equal(writes.length, count, 'Timed-out attempts must not submit a late stop');
    await api.closeVoiceCall(current);
    assert.equal(api.canRetireVoiceCall(current), true, stage);
    assert.equal(writes.length, 1, 'Retry uses late closure evidence or sends one stop on the same call');
  }
});

test('an artifact read that never settles cannot bypass the provider evidence timeout', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const current = call();
  const api = host(() => deferred().promise, async () => {});
  const opening = api.openVoiceCall(current, 'v=0');
  const rejected = assert.rejects(opening, /within 45 seconds/);
  await tick(); t.mock.timers.tick(45_000); await rejected;
  assert.equal(current.creationDisposition, 'unconfirmed');
  assert.equal(api.canRetireVoiceCall(current), false);
});

test('voice context includes only exact admitted Home text, with bounded omission disclosure and quiet updates', async () => {
  const writes = [];
  let updateStatus = 'transport_written';
  const api = host(async (_, params) => ({ artifacts: ['chunk_injected', updateStatus].map((status, index) => ({ seq: index + 1, label: params.labels[0], artifact_id: `receipt-${index}`, payload: { correlation_id: 'offer-1', call_id: 'rtc_exact', update_id: writes.at(-1)?.payload.update_id, status } })) }), async request => { writes.push(request); });
  const message = (id, patch = {}) => ({ id, artifactId: id, runId: binding.runId, interfaceId: 'home-client', text: id, role: 'user', direction: 'in', eventKind: 'message', createdAtMs: 1, ...patch });
  const messages = Array.from({ length: 30 }, (_, i) => message(`message-${i}`));
  const selection = api.projectHomeVoiceContext([...messages, message('task', { runId: 'other' }), message('internal', { interfaceId: binding.providerId }), message('pending', { pending: true }), message('tool', { eventKind: 'tool' }), message('partial', { role: 'assistant', direction: 'out', eventKind: 'message_delta' }), message('reply', { role: 'assistant', direction: 'out', terminal: true })], binding.runId, 'home-client');
  assert.equal(selection.initialItems.length, 1);
  assert.equal(selection.omittedMessageCount, 7);
  assert.equal(selection.truncated, false);
  assert.match(selection.disclosure, /7 older visible messages omitted/);
  assert.equal(selection.initialItems[0].role, 'developer');
  assert.equal(selection.initialItems[0].content.length, 1);
  const history = JSON.parse(selection.initialItems[0].content[0].text.split('\n\n')[1]);
  assert.equal(history.source, 'home_text_history');
  assert.equal(history.messages.length, 24);
  assert.equal(history.messages.at(-1).role, 'assistant');
  assert.equal(history.messages.at(-1).text, 'reply');
  const long = api.projectHomeVoiceContext([message('long', { text: '🙂'.repeat(30000) })], binding.runId, 'home-client');
  assert.match(long.disclosure, /truncated/);
  assert.equal(long.omittedMessageCount, 0);
  assert.equal(long.truncated, true);
  assert.ok(Buffer.byteLength(JSON.parse(long.initialItems[0].content[0].text.split('\n\n')[1]).messages[0].text) <= 24000);
  const voiceInput = message('spoken request', { interfaceId: binding.delegationInterfaceId, label: 'realtime.delegation' });
  const spoken = api.projectHomeVoiceContext([voiceInput, { ...voiceInput, id: 'foreign', interfaceId: 'another-voice' },
    message('spoken result', { role: 'assistant', direction: 'out', terminal: true })], binding.runId, 'home-client', binding.delegationInterfaceId);
  assert.deepEqual(spoken.messages.map(row => row.text), ['spoken request', 'spoken result']);
  const current = { ...call(), callId: 'rtc_exact' };
  await api.updateVoiceContext(current, spoken.messages[0], new AbortController().signal);
  assert.equal(writes.length, 0, 'Native voice input is retained in history without being echoed into the live call');
  await api.updateVoiceContext(current, { id: 'message-31', role: 'user', text: 'A typed correction' }, new AbortController().signal);
  assert.equal(writes[0].label, 'realtime.session.update');
  assert.deepEqual([writes[0].payload.channel, writes[0].payload.update_id, writes[0].payload.call_id], ['context', 'home-context:message-31', 'rtc_exact']);
  assert.equal(writes[0].payload.delegation_id, null);
  const voicedReply = api.projectHomeVoiceContext([message('voiced-result', { role: 'assistant', direction: 'out', terminal: true,
    eventData: { voice: { correlation_id: current.correlationId, delegation_id: 'native-request' } } })], binding.runId, 'home-client').messages[0];
  await api.updateVoiceContext(current, voicedReply, new AbortController().signal);
  assert.equal(writes.at(-1).payload.delegation_id, 'native-request');
  assert.equal(writes.at(-1).payload.channel, 'commentary');
  const delivered = writes.length;
  await api.updateVoiceContext(current, { ...voicedReply, id: 'old-call', voiceRequest: { correlationId: 'previous-call', delegationId: 'old-request' } }, new AbortController().signal);
  assert.equal(writes.length, delivered);
  updateStatus = 'uncertain';
  await assert.rejects(api.updateVoiceContext(current, { id: 'message-32', role: 'user', text: 'Another correction' }, new AbortController().signal), /did not accept/);
});

test('voice routes progress and answers to their exact delegation without replaying text', async () => {
  const writes = [];
  const api = host(async () => ({ artifacts: [{ seq: 1, label: 'realtime.session.updated', payload: { correlation_id: 'offer-1', call_id: 'rtc_exact', update_id: writes.at(-1)?.payload.update_id, status: 'transport_written' } }] }), async request => writes.push(request));
  const base = { id: 'rendered-item', artifactId: '', runId: 'home', interfaceId: 'client', role: 'assistant', direction: 'out', createdAtMs: 1, eventKind: 'commentary_delta', text: ' Checking the controls ', eventData: { voice: { correlation_id: 'offer-1', delegation_id: 'native-request' }, _event: { turn_id: 'turn', item_id: 'item' }, _stream: { stream_id: 'stream', stream_seq: 1, source_movement_seq: 20, byte_start_offset: 50, byte_end_offset: 150 } } };
  const first = api.projectHomeVoiceContext([base], 'home', 'client').messages[0];
  const complete = api.projectHomeVoiceContext([{ ...base, artifactId: 'completed-ref', eventKind: 'commentary', text: 'Checking the controls now\n' }], 'home', 'client').messages[0];
  assert.equal(first.id, complete.id);
  const delta = api.voiceContextDelta(first, complete);
  assert.equal(delta.text, ' now'); assert.equal(delta.offset, first.text.length);
  assert.equal(api.voiceContextDelta(complete, complete), null);
  assert.throws(() => api.voiceContextDelta(first, { ...complete, text: 'Changed prefix' }), /already shared/);
  const failed = api.projectHomeVoiceContext([{ ...base, artifactId: 'error-ref', eventKind: 'error', error: true, terminal: true, text: 'Home stopped: invalid context', eventData: {} }], 'home', 'client').messages[0];
  const current = { ...call(), callId: 'rtc_exact' };
  for (const message of [first, delta, failed]) await api.updateVoiceContext(current, message, new AbortController().signal);
  assert.deepEqual(writes.map(write => write.payload.channel), ['context', 'context', 'commentary']);
  assert.deepEqual(writes.map(write => write.payload.delegation_id), ['native-request', 'native-request', null]);
  assert.deepEqual(writes.map(write => write.payload.text), ['Checking the controls', ' now', 'Home stopped: invalid context']);
});

function deferred() { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }

test('voice streams answer text once and completion never repeats its prefix', async () => {
  const writes = [];
  const api = host(async () => ({ artifacts: [{ seq: 1, payload: { correlation_id: 'offer-1', call_id: 'rtc_exact', update_id: writes.at(-1)?.payload.update_id, status: 'transport_written' } }] }), async request => writes.push(request));
  const partial = { id: 'answer', role: 'assistant', phase: 'reply', complete: false, text: 'Enabled' };
  const complete = { ...partial, complete: true, text: 'Enabled Insights.' };
  const delta = api.voiceContextDelta(partial, complete);
  assert.deepEqual([delta.text, delta.offset], [' Insights.', 7]);
  assert.throws(() => api.voiceContextDelta(partial, { ...complete, text: 'Insights is enabled.' }), /already shared/);
  const current = { ...call(), callId: 'rtc_exact' };
  for (const message of [partial, delta]) await api.updateVoiceContext(current, message, new AbortController().signal);
  await api.updateVoiceContext(current, api.voiceContextDelta({ ...complete, complete: false }, complete), new AbortController().signal);
  assert.deepEqual(writes.map(write => write.payload.channel), ['commentary', 'commentary']);
  assert.equal(writes.map(write => write.payload.text).join(''), complete.text);
  assert.equal(api.voiceContextDelta(complete, complete), null);
});

async function hookHarness() {
  const { JSDOM } = require('jsdom');
  const React = require('react'); const { createRoot } = require('react-dom/client');
  const dom = new JSDOM('<body></body>', { url: 'https://octopus.test' });
  const globals = new Map(['window', 'document', 'navigator', 'AudioContext', 'RTCPeerConnection', 'MediaStream', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  const h = { microphones: [], peers: [], offers: [], closes: [], events: [], updates: [], mediaRead: null, playCount: 0, playError: '', offerGate: null, answerGate: null, sinkGate: null, current: null, samples: {}, frames: new Map(), offerSdp: 'v=0\r\ns=offer\r\nt=0 0\r\n', answerSdp: 'v=0\r\ns=answer\r\nt=0 0\r\n' };
  class Track extends dom.window.EventTarget {
    constructor(id) { super(); this.id = id; this.kind = 'audio'; this.readyState = 'live'; this.enabled = true; }
    stop() { this.readyState = 'ended'; this.enabled = false; }
  }
  class Stream { constructor(tracks) { this.tracks = tracks; this.id = 'stream'; } getTracks() { return this.tracks; } getAudioTracks() { return this.tracks; } }
  class Peer extends dom.window.EventTarget {
    constructor() { super(); this.connectionState = 'new'; this.iceGatheringState = h.icePending ? 'gathering' : 'complete'; this.senders = []; h.peers.push(this); }
    createDataChannel() {}
    addTrack(track) { this.senders.push({ track, replaceTrack: async function (next) { if (h.replaceGate) { const gate = h.replaceGate; h.replaceGate = null; await gate.promise; } this.track = next; } }); }
    getSenders() { return this.senders; }
    async createOffer() { if (h.offerGate) await h.offerGate.promise; return { type: 'offer', sdp: h.offerSdp }; }
    async setLocalDescription(offer) { this.localDescription = offer; }
    async setRemoteDescription(answer) { this.remoteDescription = answer; this.transition('connected'); }
    transition(state) { this.connectionState = state; this.onconnectionstatechange?.(); this.dispatchEvent(new dom.window.Event('connectionstatechange')); }
    close() { this.transition('closed'); }
  }
  const node = (stream = null) => ({ stream, connect(target) { target.stream = this.stream; }, disconnect() {}, gain: { value: 1 }, fftSize: 512, getByteTimeDomainData(samples) { samples.fill(128 + (h.samples[this.stream?.getAudioTracks()[0].id] ?? 0)); } });
  class AudioContext { constructor() { this.state = 'running'; this.destination = node(); } async resume() {} async close() { this.state = 'closed'; } createMediaStreamSource(stream) { return node(stream); } createAnalyser() { return node(); } createGain() { return node(); } createMediaElementSource() { return node(); } }
  const preferences = { audioInputDeviceId: 'mic-a', audioOutputDeviceId: 'speaker', incomingAudioArtifactBehavior: 'ask' };
  const mediaDevices = new dom.window.EventTarget();
  mediaDevices.enumerateDevices = async () => ['mic-a', 'mic-b', 'mic-c'].map(deviceId => ({ deviceId, label: deviceId, kind: 'audioinput' })).concat({ deviceId: 'speaker', label: 'Speaker', kind: 'audiooutput' });
  mediaDevices.getUserMedia = async () => { const request = deferred(); h.microphones.push(request); return request.promise; };
  Object.defineProperty(dom.window.navigator, 'mediaDevices', { value: mediaDevices });
  let frameId = 0;
  dom.window.requestAnimationFrame = callback => { h.frames.set(++frameId, callback); return frameId; }; dom.window.cancelAnimationFrame = id => h.frames.delete(id);
  h.frame = now => { const frames = [...h.frames.values()]; h.frames.clear(); frames.forEach(callback => callback(now)); };
  dom.window.HTMLMediaElement.prototype.play = async function () { h.playCount++; if (h.playError) throw new Error(h.playError); };
  dom.window.HTMLMediaElement.prototype.pause = function () {};
  dom.window.HTMLMediaElement.prototype.setSinkId = async function () { if (h.sinkGate) await h.sinkGate.promise; };
  for (const [key, value] of Object.entries({ window: dom.window, document: dom.window.document, navigator: dom.window.navigator, AudioContext, RTCPeerConnection: Peer, MediaStream: Stream, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  const api = {
    assertVoiceControlReady: async () => {},
    waitForVoiceProviderReady: async () => { if (h.readyGate) await h.readyGate.promise; },
    canRetireVoiceCall: current => !current.answer || current.creationDisposition === 'not_created' || current.remoteClosed,
    openVoiceCall(current, sdp, items) { h.offers.push({ current, sdp, items }); current.creationDisposition = 'unconfirmed'; current.answer = (async () => { if (h.answerGate) await h.answerGate.promise; current.callId = 'rtc_exact'; current.creationDisposition = 'created'; return { answer_sdp: h.answerSdp, sideband_connected: false }; })(); return current.answer; },
    closeVoiceCall(current) { if (current.remoteClosed) return Promise.resolve(); if (current.stop) return current.stop; current.closeRequested = true; const stopping = (async () => { if (!current.answer) return; await current.answer; h.closes.push(current.callId); if (h.closeGate) await h.closeGate.promise; if (h.requireLivePeerOnClose) assert.equal(h.peers.at(-1).connectionState, 'connected', 'Provider finalization requires the primary transport'); if (h.closeError) throw new Error(h.closeError); current.closedPayload = { transcript: h.closedTranscript ?? [] }; current.remoteClosed = true; })(); current.stop = stopping; void stopping.catch(() => { if (current.stop === stopping) current.stop = null; }); return stopping; },
    readVoiceArtifacts: async () => ({ artifacts: h.events.splice(0), afterSeq: 1, more: false }),
    updateVoiceContext: async (_, message) => { h.updates.push(message); if (h.updateGate) await h.updateGate.promise; if (h.updateError) throw new Error(h.updateError); },
    voiceContextDelta: host(async () => {}, async () => {}).voiceContextDelta,
  };
  const { useVoiceSession } = load('useVoiceSession.ts', {
    react: React,
    '../../projectionClient': { readProjection: async (_, params) => params?.labels?.[0] === 'realtime.session.audio' && h.mediaRead ? h.mediaRead.promise : { artifacts: [] } },
    '../../projectionLive': { nextProjectionChange: () => new Promise(resolve => setTimeout(resolve, 2)) },
    '../../clientPreferences': { useClientPreferences: () => preferences, useSetClientPreferences: () => setPreferences },
    './voicePresentation': load('voicePresentation.ts'),
    './voiceTranscript': load('voiceTranscript.ts'),
    './voiceCallArtifacts': api,

  });
  function setPreferences(values) { Object.assign(preferences, values); }
  function Harness({ profile }) { h.current = useVoiceSession(profile); return null; }
  const container = document.createElement('div'); document.body.append(container); let root = createRoot(container);
  h.act = React.act;
  h.flush = async () => { await React.act(async () => { await tick(); await new Promise(resolve => setTimeout(resolve, 5)); }); };
  h.profile = async profile => { await React.act(async () => root.render(React.createElement(Harness, { profile }))); };
  h.stream = id => new Stream([new Track(id)]);
  h.start = () => { let pending; React.act(() => { pending = h.current.start({ ...binding, homeRunId: 'run_home', runTitle: 'Home', providerLabel: 'Voice' }, { context: { runId: 'run_home', interfaceId: 'home-client', messages: [], initialItems: [] } }); }); return pending; };
  h.connect = async () => { const starting = h.start(); await h.flush(); const stream = h.stream('mic-a'); await React.act(async () => h.microphones.at(-1).resolve(stream)); await React.act(async () => starting); return stream; };
  h.unmount = async () => { if (root) { await React.act(async () => root.unmount()); root = null; } };
  h.close = async () => { await h.unmount(); await tick(); dom.window.close(); for (const [key, descriptor] of globals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key]; } };
  await h.profile('live'); return h;
}

test('microphone stays gated until the provider confirms its session, after WebRTC connects', async () => {
  const h = await hookHarness();
  try {
    h.readyGate = deferred();
    const starting = h.start(); await h.flush();
    const stream = h.stream('mic-a');
    await h.act(async () => h.microphones.at(-1).resolve(stream)); await h.flush();
    assert.equal(h.peers[0].connectionState, 'connected');
    assert.equal(stream.getAudioTracks()[0].enabled, false);
    assert.equal(h.current.phase, 'connecting');
    await h.act(async () => h.readyGate.resolve()); await h.act(async () => starting);
    assert.equal(stream.getAudioTracks()[0].enabled, true);
    assert.equal(h.current.phase, 'listening');
  } finally { await h.close(); }
});

test('WebRTC offer and answer preserve their SDP bytes, including the final CRLF', async () => {
  const h = await hookHarness();
  try {
    await h.connect();
    assert.equal(h.offers[0].sdp, h.offerSdp);
    assert.equal(h.peers[0].remoteDescription.sdp, h.answerSdp);
  } finally { await h.close(); }
});

test('hook hangup closes local capture across late microphone, offer and answer stages', async () => {
  for (const stage of ['microphone', 'offer', 'ice', 'answer', 'unmount']) {
    const h = await hookHarness();
    try {
      const gate = deferred(); if (stage === 'offer') h.offerGate = gate; if (stage === 'answer') h.answerGate = gate; if (stage === 'ice') h.icePending = true;
      const start = h.start(); await h.flush(); const stream = h.stream('late');
      if (stage !== 'microphone' && stage !== 'unmount') { await h.act(async () => h.microphones[0].resolve(stream)); await h.flush(); }
      let end; if (stage === 'unmount') await h.unmount(); else h.act(() => { end = h.current.end(); });
      if (stage === 'microphone' || stage === 'unmount') await h.act(async () => h.microphones[0].resolve(stream)); else await h.act(async () => gate.resolve());
      await h.act(async () => Promise.all([start, end]));
      assert.equal(stream.getAudioTracks()[0].readyState, 'ended', stage);
      if (stage !== 'unmount') assert.equal(h.current.phase, 'ended', stage);
      assert.equal(h.offers.length, stage === 'answer' ? 1 : 0, stage);
      assert.deepEqual(h.closes, stage === 'answer' ? ['rtc_exact'] : [], stage);
    } finally { await h.close(); }
  }
});

test('scope change rejects late audio prompts, playback and closure captions from the prior call', async () => {
  const h = await hookHarness();
  try {
    h.mediaRead = deferred(); h.sinkGate = deferred();
    const stream = await h.connect(); const peer = h.peers[0];
    await h.act(async () => peer.ontrack({ streams: [h.stream('remote')] }));
    await h.profile('lab');
    assert.equal(stream.getAudioTracks()[0].readyState, 'ended');
    assert.equal(h.current.phase, 'idle');
    await h.act(async () => { h.sinkGate.resolve(); h.mediaRead.resolve({ artifacts: [{ seq: 2, artifact_id: 'late', payload: { correlation_id: h.offers[0].current.correlationId } }] }); });
    await h.flush();
    assert.equal(h.playCount, 0); assert.equal(h.current.pendingAudioArtifact, null); assert.deepEqual(h.current.transcript, []);
  } finally { await h.close(); }
});

test('model meter follows remote audio independently of microphone mute and stops before remote closure', async () => {
  const h = await hookHarness();
  try {
    const microphone = await h.connect();
    await h.act(async () => h.peers[0].ontrack({ streams: [h.stream('remote')] }));
    h.samples['mic-a'] = 24; h.frame(100);
    assert.ok(h.current.getMeterSnapshot().level > 0);
    assert.equal(h.current.getOutputMeterSnapshot().level, 0);
    h.samples.remote = 12; h.frame(150);
    const output = h.current.getOutputMeterSnapshot().level;
    assert.ok(output > 0);
    await h.act(async () => h.current.toggleMute()); h.frame(200);
    assert.equal(h.current.getMeterSnapshot().level, 0);
    assert.equal(h.current.getOutputMeterSnapshot().level, output);
    h.requireLivePeerOnClose = true;
    h.closeGate = deferred(); let ending;
    h.act(() => { ending = h.current.end(); }); await h.flush();
    assert.equal(microphone.getAudioTracks()[0].readyState, 'ended');
    assert.equal(h.peers[0].connectionState, 'connected');
    assert.equal(h.current.getOutputMeterSnapshot().level, 0);
    assert.equal(h.current.phase, 'ending');
    assert.equal(load('voicePresentation.ts').isVoiceCallActive(h.current.phase), false);
    await h.act(async () => { h.closeGate.resolve(); await ending; });
    assert.equal(h.current.phase, 'ended');
    assert.equal(h.peers[0].connectionState, 'closed');
  } finally { await h.close(); }
});

test('voice activity follows new input, replies and pending context without relighting on old captions', async () => {
  const h = await hookHarness();
  const caption = async (id, role, text, final = false) => {
    h.events.push({ label: 'realtime.transcript', payload: { call_id: 'rtc_exact', id, role, text, final } });
    await h.flush();
  };
  try {
    await h.connect();
    assert.equal(h.current.providerActivity, false);
    await caption('user-1', 'user', 'Explain this');
    assert.equal(h.current.providerActivity, false);
    await caption('user-1', 'user', 'Explain this', true);
    assert.equal(h.current.providerActivity, true);
    await caption('reply-1', 'assistant', 'Here is the explanation');
    assert.equal(h.current.providerActivity, false);
    await caption('user-1', 'user', 'Explain this', true);
    assert.equal(h.current.providerActivity, false);
    await caption('user-2', 'user', 'Continue', true);
    assert.equal(h.current.providerActivity, true);
    await h.act(async () => h.peers[0].ontrack({ streams: [h.stream('remote')] }));
    h.samples.remote = 12;
    await h.act(async () => h.frame(100));
    assert.equal(h.current.providerActivity, true, 'Audio samples do not guess provider response state');
    await caption('reply-2', 'assistant', 'Continuing');
    assert.equal(h.current.providerActivity, false);
    for (const endBeforeDelivery of [false, true]) {
      h.updateGate = deferred(); let updating;
      h.act(() => { updating = h.current.updateContext({ runId: 'run_home', interfaceId: 'home-client', messages: [{ id: `context-${endBeforeDelivery}`, role: 'user', text: 'New Home context' }] }); });
      await h.flush();
      assert.equal(h.current.providerActivity, true);
      if (endBeforeDelivery) await h.act(async () => h.current.end());
      await h.act(async () => { h.updateGate.resolve(); await updating; });
      assert.equal(h.current.providerActivity, false);
    }
  } finally { await h.close(); }
});

test('voice coalesces updates arriving during delivery and sends the remaining text once', async () => {
  const h = await hookHarness();
  try {
    await h.connect(); h.updateGate = deferred(); let first;
    const selection = text => ({ runId: 'run_home', interfaceId: 'home-client', messages: [{ id: 'item', role: 'assistant', phase: 'progress', complete: false, text }] });
    h.act(() => { first = h.current.updateContext(selection('Checking')); }); await h.flush();
    h.act(() => { void h.current.updateContext(selection('Checking the')); void h.current.updateContext(selection('Checking the controls')); });
    await h.act(async () => { h.updateGate.resolve(); await first; });
    assert.deepEqual(h.updates.map(message => message.text), ['Checking', ' the controls']);
    await h.act(async () => h.current.updateContext(selection('Checking the controls')));
    assert.equal(h.updates.length, 2);
  } finally { await h.close(); }
});

test('a failed creation without identity proof remains unconfirmed when cleanup awaits the failed offer', async () => {
  const h = await hookHarness();
  try {
    h.answerGate = deferred();
    const start = h.start(); await h.flush();
    const stream = h.stream('mic-a');
    await h.act(async () => h.microphones[0].resolve(stream)); await h.flush();
    await h.act(async () => { h.answerGate.reject(new Error('Live call refused: HTTP 400')); await start; });
    assert.equal(h.current.phase, 'closure_unconfirmed');
    assert.equal(h.current.error, 'Live call refused: HTTP 400');
    assert.equal(stream.getAudioTracks()[0].readyState, 'ended');
    assert.deepEqual(h.closes, []);
  } finally { await h.close(); }
});

test('late exact closure evidence retires a timed-out call without another stop or offer', async () => {
  const h = await hookHarness();
  try {
    await h.connect();
    h.closeError = 'Remote closure unconfirmed';
    await h.act(async () => h.current.end());
    assert.equal(h.current.phase, 'closure_unconfirmed');
    const closes = h.closes.length;
    h.events.push({ label: 'realtime.session.closed', payload: { call_id: 'foreign-call', remote_closed: true } });
    await h.flush(); assert.equal(h.current.phase, 'closure_unconfirmed');
    h.events.push({ label: 'realtime.session.closed', payload: { call_id: 'rtc_exact', remote_closed: false } });
    await h.flush(); assert.equal(h.current.phase, 'closure_unconfirmed');
    h.events.push({ label: 'realtime.session.closed', payload: { call_id: 'rtc_exact', remote_closed: true, transcript: [] } });
    await h.flush();
    assert.equal(h.current.phase, 'ended'); assert.equal(h.current.error, '');
    assert.equal(h.closes.length, closes); assert.equal(h.offers.length, 1);
  } finally { await h.close(); }
});

test('unconfirmed closure retains the call, blocks another offer and exposes explicit cleanup retry', async () => {
  const h = await hookHarness(), presentation = load('voicePresentation.ts');
  try {
    const stream = await h.connect();
    h.closeError = 'Remote closure unconfirmed';
    await h.act(async () => h.current.end());
    assert.equal(stream.getAudioTracks()[0].readyState, 'ended');
    assert.equal(h.current.phase, 'closure_unconfirmed');
    assert.equal(presentation.voicePresentation(h.current.phase).canStart, false);
    assert.equal(presentation.voicePresentation(h.current.phase).canEnd, true);
    await h.act(async () => { h.current.dismissError(); await h.start(); });
    assert.equal(h.offers.length, 1);
    await h.profile('lab'); await h.flush();
    assert.equal(h.current.phase, 'closure_unconfirmed');
    await h.act(async () => h.start()); assert.equal(h.offers.length, 1);
    h.closeError = '';
    await h.act(async () => h.current.end());
    assert.equal(h.current.phase, 'ended');
    assert.deepEqual(h.closes, ['rtc_exact', 'rtc_exact', 'rtc_exact']);
  } finally { await h.close(); }
});

test('microphone replacements serialize, restore superseded tracks and preserve reconnect mute gating', async () => {
  const h = await hookHarness();
  try {
    const first = await h.connect(); await h.act(async () => h.current.toggleMute());
    h.replaceGate = deferred(); const replace = h.replaceGate;
    let changeA, changeB;
    h.act(() => { changeA = h.current.selectInputDevice('mic-b'); }); await h.flush();
    const second = h.stream('mic-b'); await h.act(async () => h.microphones[1].resolve(second)); await h.flush();
    h.act(() => { changeB = h.current.selectInputDevice('mic-c'); });
    const staleToggle = h.current.toggleMute;
    await h.act(async () => { h.peers[0].transition('disconnected'); staleToggle(); });
    assert.equal(first.getAudioTracks()[0].enabled, false);
    await h.act(async () => replace.resolve()); await h.flush();
    assert.equal(second.getAudioTracks()[0].readyState, 'ended');
    assert.equal(h.peers[0].senders[0].track, first.getAudioTracks()[0]);
    const third = h.stream('mic-c'); await h.act(async () => h.microphones[2].resolve(third));
    await h.act(async () => Promise.all([changeA, changeB]));
    assert.equal(third.getAudioTracks()[0].enabled, false);
    await h.act(async () => h.peers[0].transition('connected'));
    assert.equal(third.getAudioTracks()[0].enabled, true);
    assert.equal(h.current.selectedInputId, 'mic-c');
  } finally { await h.close(); }
});

test('final captions merge on closure and context failures remain visible without delegation resubmission', async () => {
  const h = await hookHarness();
  try {
    await h.connect();
    h.playError = 'Speaker playback blocked';
    await h.act(async () => h.peers[0].ontrack({ streams: [h.stream('remote')] }));
    await h.flush();
    await h.act(async () => h.current.refreshDevices());
    assert.match(h.current.deviceError, /Speaker playback blocked/);
    await h.act(async () => h.current.selectOutputDevice('speaker-new'));
    assert.equal(h.current.selectedOutputId, 'speaker');
    h.playError = '';
    await h.act(async () => h.current.selectOutputDevice('speaker-new'));
    assert.equal(h.current.selectedOutputId, 'speaker-new');
    assert.equal(h.current.deviceError, '');
    h.events.push({ label: 'realtime.transcript', payload: { call_id: 'rtc_exact', id: 'caption', role: 'user', text: 'Check', final: false } }); await h.flush();
    h.updateError = 'delivery uncertain';
    const selection = { runId: 'run_home', interfaceId: 'home-client', messages: [{ id: 'typed', role: 'user', text: 'Change the task' }] };
    await h.act(async () => h.current.updateContext(selection));
    await h.act(async () => h.current.updateContext(selection));
    assert.equal(h.updates.length, 1); assert.match(h.current.error, /delivery uncertain.*no automatic retry/);
    selection.messages.push({ id: 'second', role: 'user', text: 'Second' }, { id: 'third', role: 'user', text: 'Third' });
    await h.act(async () => h.current.updateContext(selection));
    h.updateError = '';
    await h.act(async () => h.current.updateContext(selection));
    assert.deepEqual(h.updates.map(message => message.id), ['typed', 'second', 'third']);
    h.closedTranscript = [{ id: 'caption', role: 'user', text: 'Check the task', complete: true }];
    await h.act(async () => h.current.end());
    assert.deepEqual(h.current.transcript, [{ id: 'caption', role: 'user', text: 'Check the task', final: true }]);
  } finally { await h.close(); }
});
