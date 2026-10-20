const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { blake3 } = require('@noble/hashes/blake3');
const { createProjectionContext } = require('../electron/projection.cjs');
const { operatorChatEventKind, operatorChatEventText, operatorChatProjectedRowId, operatorChatRunProvider } = require('../electron/projection/operatorChatEvents.cjs');
const { compareMovementOrder } = require('../electron/projection/movementRows.cjs');

const repoRoot = path.resolve(__dirname, '../..');
const protocol = { inputLabel: 'operator.message', terminalOutputLabel: 'interface.artifact.output', errorOutputLabel: 'interface.artifact.error',
  toolCallLabel: 'tool.call', hiddenActivityLabels: [] };

function projectionStore(overrides = {}) {
  return Object.assign(createProjectionContext({ resolveRepoRoot: () => repoRoot, octopusHome: () => os.tmpdir() }), overrides);
}
function projectionFixture(fields = {}) {
  return { operatorChatProtocol: protocol, descriptors: { interface: [{ id: 'provider', role: 'provider' }, { id: 'other-provider', role: 'provider' }], ee_template: [] },
    descriptorIndexes: { interface: new Map() }, eeInstances: [], runRecords: [], clientAgentCatalog: { document: { agents: [] } },
    structureMemo: new Map(), ledgerMemo: new Map(), ...fields };
}
function movement(seq, label, payload, { from = 'provider', to = 'chat', kind = 'InterfaceOutbound' } = {}) {
  const hex = seq.toString(16).padStart(64, '0');
  return { seq, recorded_at_unix_ms: seq * 100, kind, from: { Interface: from }, to: { Interface: to },
    artifact: { id: `${label}:${hex}`, content: hex }, invocation_metadata: { runtime: { run: 'run', interface: to } }, payload };
}

test('Stop aliases match actual tasks titles without reading terminal history', async () => {
  await require('./build-cli.cjs').buildCli();
  const { createClient } = require('../cli/client.cjs');
  const entries = ['home', 'task', 'named'].map((name, index) => ({
    runId: `run_${name}`, chatInterfaceId: `chat_${name}`, surfaceRole: name === 'home' ? 'home' : 'task', writable: true, activity: [], record: {},
    inbound: [movement(index + 1, protocol.inputLabel, { text: name === 'home' ? 'Review\n notes' : 'Not the authored title' }, { from: `chat_${name}`, to: 'provider', kind: 'InterfaceArrival' })],
    outbound: [movement(index + 20, protocol.terminalOutputLabel, { reply: `Answer for ${name}` }, { to: `chat_${name}` })],
  }));
  entries[0].inbound.unshift(movement(10, protocol.inputLabel, { text: 'Later input' }, { from: 'chat_home', to: 'provider', kind: 'InterfaceArrival' }));
  entries[0].outbound.push(movement(30, protocol.terminalOutputLabel, { reply: 'Delivered elsewhere' }, { to: 'chat_other' }));
  const sidecar = (name) => ({ workspace: 'octopus_os', presentation: { name } });
  const projection = projectionFixture({ descriptors: { ee_template: [], interface: [
    { id: 'chat_home', octopus_os_sidecar: sidecar('Home') }, { id: 'chat_task', octopus_os_sidecar: sidecar('Task title') },
    { id: 'chat_named', octopus_os_sidecar: sidecar('run_named') }] } });
  const scope = { status: 'ready', home_run_id: 'run_home', home_interface_id: 'chat_home',
    client_runs: entries.map(entry => ({ run_id: entry.runId, interface_id: entry.chatInterfaceId, surface_role: entry.surfaceRole, archived: false })) };
  let allowPayload = true, allowHistory = true, inputReads = 0;
  const projectionReads = projectionStore({
    operatorOsScope: () => scope, operatorChatRunIndex: async () => entries, operatorOsApplications: () => ({}),
    operatorChatTurnState: async () => { assert(allowHistory, 'control aliases must not read turn history'); return { open: false, state: 'idle' }; },
    resolveLedgerArtifactPayload: async row => {
      if (row.kind === 'InterfaceOutbound') { assert(allowHistory, 'control aliases must not read terminal history'); return row.payload; }
      assert(allowPayload, 'scope and last selectors must not read payloads'); inputReads++;
      assert.equal(row, entries[0].inbound[1], 'only the first Home input supplies a derived title'); return row.payload;
    },
  });
  const store = { async read({ method, params }) {
    if (method === 'projection.operator.os.scope') return scope;
    assert.equal(method, 'projection.operator.chats.list');
    return { chats: params?.control_targets_only
      ? await projectionReads.operatorChatControlTargets(projection, params.include_titles)
      : await projectionReads.buildOperatorChatsList(projection) };
  }, invalidate() {}, close() {} };
  const client = createClient({ home: os.tmpdir(), packageRoots: {}, status: async () => ({ runtimeReady: true }) }, {
    store, metadata: {}, loader: { load: async () => ({ submitOperatorTurnControl: async (_options, input) => ({ ...input, verdict: { admitted: { seq: 1 } } }) }) },
  });
  try {
    const tasks = await client.tasks();
    assert.deepEqual(Object.fromEntries(tasks.map(task => [task.id, task.title])), {
      run_home: 'Review notes', run_named: 'run_named', run_task: 'Task title',
    });
    assert.equal(inputReads, 1);
    allowHistory = false; allowPayload = false;
    for (const [selector, id] of [['home', 'run_home'], ['run_task', 'run_task'], ['last', 'run_named']]) {
      assert.equal((await client.stop(selector)).runId, id);
    }
    assert.equal(inputReads, 1);
    allowPayload = true;
    const aliases = await projectionReads.operatorChatControlTargets(projection, true);
    for (const task of tasks) {
      assert.equal(aliases.find(row => row.run_id === task.id).title, task.title);
      assert.equal((await client.stop(task.title)).runId, task.id);
    }
    projection.descriptors.interface[1].octopus_os_sidecar.presentation.name = tasks.find(task => task.isHome).title;
    await assert.rejects(client.stop('Review notes'), /ambiguous/);
    allowHistory = true;
    const listed = await projectionReads.buildOperatorChatsList(projection);
    assert.deepEqual(listed.map(chat => [chat.run_id, chat.last_terminal_message.text, chat.last_terminal_message.movement_seq]).sort(),
      [['run_home', 'Answer for home', 20], ['run_named', 'Answer for named', 22], ['run_task', 'Answer for task', 21]]);
  } finally { await client.close(); }
});

test('artifact cache separates raw bytes, projection contracts and exact artifact identities', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-artifact-cache-')), blobs = path.join(root, 'blobs');
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(blobs);
  const blob = (bytes) => { const hex = Buffer.from(blake3(bytes)).toString('hex'); fs.writeFileSync(path.join(blobs, hex), bytes); return hex; };
  const artifact = (label, value) => {
    const payloadHex = blob(Buffer.from(JSON.stringify(value)));
    const manifestHex = blob(Buffer.from(JSON.stringify({ label, payload_path: path.join(blobs, payloadHex), payload_content_hash: payloadHex })));
    return { row: { source_file: path.join(root, 'movements.jsonl'), artifact: { id: `${label}:${manifestHex}`, content: manifestHex } }, payloadHex };
  };
  const contract = { schema_version: 1, json_object_labels: ['reply'], excluded_root_fields: ['private'], max_source_bytes: 100, max_projected_bytes: 100 };
  const store = projectionStore();
  const reply = artifact('reply', { text: 'Reply', private: 'original' }), binary = artifact('binary', { text: 'Reply', private: 'original' });
  assert.equal((await store.resolveLedgerArtifactPayload(reply.row)).private, 'original');
  assert.deepEqual(await store.resolveLedgerArtifactPayload(reply.row, { projection: contract }), { text: 'Reply' });
  assert.equal((await store.resolveLedgerArtifactPayload(binary.row, { projection: contract })).private, 'original');
  await assert.rejects(store.resolveLedgerArtifactPayload({ ...reply.row, artifact: { ...reply.row.artifact, id: `reply:${'f'.repeat(64)}` } }, { projection: contract }));
  await assert.rejects(store.resolveLedgerArtifactPayload(reply.row, { projection: { ...contract, max_source_bytes: 1 } }));
  await assert.rejects(store.resolveLedgerArtifactPayload(reply.row, { projection: undefined }));
  fs.rmSync(path.join(blobs, reply.payloadHex));
  assert.equal((await store.resolveLedgerArtifactPayload(reply.row)).private, 'original');
  assert.deepEqual(await store.resolveLedgerArtifactPayload(reply.row, { projection: contract }), { text: 'Reply' });
});

const delta = (seq, text, item, phase, from) => movement(seq, 'agent.message.delta', { text, item_id: item, turn_id: 'turn', phase, lifecycle: 'streaming' }, { from });
const commentary = (seq, text, item) => movement(seq, 'turn.agent_message', { text, item_id: item, turn_id: 'turn', phase: 'commentary', lifecycle: 'completed' });
const partialAnswer = (seq, text, item) => movement(seq, 'turn.agent_message', { text, item_id: item, turn_id: 'turn', phase: 'partial_answer', lifecycle: 'completed' });
const toolCall = (seq, status) => movement(seq, 'turn.dynamic_tool_call', { tool: 'Read', call_id: 'read', status, turn_id: 'turn' });
const finalAnswer = (seq, text) => movement(seq, protocol.terminalOutputLabel, { reply: text, item_id: 'final', turn_id: 'turn', phase: 'final_answer', lifecycle: 'completed' });

async function project(rows, writable) {
  const store = projectionStore({ resolveLedgerArtifactPayloads: async values => new Map(values.map(value => [value, value.payload])) });
  const entry = { runId: 'run', chatInterfaceId: 'chat', writable, inbound: [], outbound: rows, activity: rows,
    record: { interface_state: { chat: { provider_runtime_interface: 'provider' } } } };
  return JSON.parse(JSON.stringify(await store.buildSingleOperatorChatMessages(projectionFixture(), entry, protocol)));
}

test('both chat history paths preserve interleaved item identity and replace completed snapshots', async () => {
  for (const writable of [true, false]) for (const finalText of ['', 'Done!']) {
    const messages = await project([
      delta(1, 'Pro', 'a', 'commentary'), toolCall(2, 'started'),
      delta(3, 'First', 'b', 'commentary'), delta(4, 'gress', 'a', 'commentary'),
      commentary(5, 'Progress', 'a'), toolCall(6, 'completed'),
      delta(7, ' second', 'b', 'commentary'), commentary(8, 'First second', 'b'),
      delta(9, ' late', 'a', 'commentary'), delta(10, 'Done', 'final', 'final_answer'),
      finalAnswer(11, finalText),
    ], writable);
    assert.deepEqual(messages.map(message => [message.event_kind, message.text]), [
      ['commentary', 'Progress'], ['dynamic_tool', 'Read'], ['commentary', 'First second'], ['dynamic_tool', 'Read'], ['message', finalText || 'Done']]);
    assert.deepEqual(messages.filter(message => message.event_kind === 'commentary').map(message => message.event_data.item_id), ['a', 'b']);
    assert.equal(messages[0].created_at_ms, 100); assert.equal(messages[0].movement_seq, 5);
    assert.equal(messages.at(-1).event_data.item_id, 'final'); assert.equal(messages.at(-1).terminal, true);
  }
});

test('provider identity and missing item identity never merge unrelated text', async () => {
  for (const writable of [true, false]) {
    const messages = await project([
      delta(1, 'Same', 'a', null), delta(2, 'Same', 'a', null, 'other-provider'),
      delta(3, 'Unknown', '', null), delta(4, 'Unknown', '', null),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Same', 'Same', 'Unknown', 'Unknown']);
  }
});

test('completed partial answers retain their text without completing the turn', async () => {
  assert.equal(operatorChatEventKind('turn.agent_message', protocol, { phase: 'partial_answer' }), 'message');
  for (const writable of [true, false]) {
    const messages = await project([
      delta(1, 'Answer', 'partial', 'partial_answer'), partialAnswer(2, 'Answer so far', 'partial'),
      toolCall(3, 'started'), finalAnswer(4, 'Complete answer'),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Answer so far', 'Read', 'Complete answer']);
    assert.equal(messages[0].terminal, false);
    assert.equal(messages[0].event_data.phase, 'partial_answer');
    assert.equal(messages.at(-1).terminal, true);
  }
});

test('a sealed stream folds each delta position once even when it is delivered twice', async () => {
  const opened = 1_000;
  const records = [
    { type: 'header', schema_version: 1, seq: 0, provider_interface: 'provider', run: 'run', thread_id: 'thread', turn_id: 'turn', opened_source_movement_seq: 1, opened_at_unix_ms: opened },
    ...['Hel', 'lo', '!'].map((text, index) => ({ type: 'event', seq: index + 1, source_movement_seq: 1, recorded_at_unix_ms: opened + index + 1,
      label: 'agent.message.delta', payload: { text, item_id: 'answer', turn_id: 'turn', phase: 'final_answer', lifecycle: 'streaming' } })),
    { type: 'seal', seq: 4, source_movement_seq: 2, recorded_at_unix_ms: opened + 9, terminal_label: 'turn.completed', terminal_artifact_payload_blake3: 'a'.repeat(64) },
  ];
  const raw = records.map(record => JSON.stringify(record)).join('\n') + '\n';
  for (const writable of [true, false]) {
    const messages = await project([movement(2, 'turn.stream', { __raw: raw }), movement(3, 'turn.stream', { __raw: raw })], writable);
    assert.deepEqual(messages.map(message => message.text), ['Hello!']);
  }
});

test('provider labels keep their transcript meaning, row identity and movement order', () => {
  assert.equal(operatorChatEventKind(protocol.terminalOutputLabel, protocol, {}), 'message');
  assert.equal(operatorChatEventKind(protocol.errorOutputLabel, protocol, {}), 'error');
  assert.equal(operatorChatEventKind('agent.message.delta', protocol, { phase: 'commentary' }), 'commentary_delta');
  assert.equal(operatorChatEventKind('agent.message.delta', protocol, { phase: 'final_answer' }), 'message_delta');
  assert.equal(operatorChatEventKind('turn.reasoning.delta', protocol, {}), 'reasoning_delta');
  assert.equal(operatorChatEventKind('turn.context.compaction', protocol, {}), 'compaction');
  assert.equal(operatorChatEventText({ text: 'summary' }, 'compaction'), '');
  assert.equal(operatorChatProjectedRowId({ __stream: { stream_id: 'stream', stream_seq: 3 }, artifact: { id: 'turn.stream:a' } }, 'fallback'), 'stream:3');
  assert.equal(operatorChatProjectedRowId({ artifact: { id: 'interface.artifact.output:b' } }, 'fallback'), 'interface.artifact.output:b');
  assert.equal(operatorChatProjectedRowId({}, 'fallback'), 'fallback');
  const ordered = [{ seq: 4, recorded_at_unix_ms: 20 }, { seq: 9, recorded_at_unix_ms: 10 }, { seq: 2, recorded_at_unix_ms: 20 }].sort(compareMovementOrder);
  assert.deepEqual(ordered.map(row => row.seq), [9, 2, 4]);
});

test('a run reports its provider only for the exact recorded selection and its projected runtime', () => {
  const projection = projectionFixture({ descriptorIndexes: { interface: new Map([['bound', { interface_driver_config: { private_config: { model: 'gpt', reasoning_effort: 'high' } } }]]) } });
  const entry = (choice, bound = 'bound', status = 'projected') => ({ chatInterfaceId: 'chat', providerChoiceInterfaceId: 'selected', record: {
    interface_state: { chat: { provider_choice_interface: choice, provider_runtime_interface: bound } },
    checkpoints: [{ checkpoint_id: 'output', status, interface: 'bound', out_interfaces: ['chat'] }] } });
  assert.deepEqual([operatorChatRunProvider(projection, entry('selected'))].map(({ id, boundId, model, reasoningEffort }) => ({ id, boundId, model, reasoningEffort })),
    [{ id: 'selected', boundId: 'bound', model: 'gpt', reasoningEffort: 'high' }]);
  for (const candidate of [entry('another'), entry('selected', ''), entry('selected', 'bound', 'retired')]) {
    assert.equal(operatorChatRunProvider(projection, candidate).id, '');
  }
});

test('turn state opens on input, pauses on interrupt, reopens on an acknowledged resume and fails on provider failure', async () => {
  const store = projectionStore({ resolveLedgerArtifactPayload: async row => row.payload });
  const entry = { runId: 'run', chatInterfaceId: 'chat', writable: true, record: { interface_state: { chat: { provider_runtime_interface: 'provider' } } } };
  const native = (turn) => ({ thread_id: 'thread', turn_id: turn });
  const rows = [
    movement(1, protocol.inputLabel, { text: 'Go', message_id: 'm1' }, { from: 'chat', to: 'provider', kind: 'InterfaceArrival' }),
    movement(2, 'turn.started', native('t1')),
    movement(3, 'turn.control.acknowledged', { ...native('t1'), control_id: 'c1', action: 'interrupt', status: 'acknowledged' }),
    movement(4, 'turn.control.acknowledged', { ...native('t2'), control_id: 'c2', action: 'resume', status: 'acknowledged' }),
    movement(5, protocol.terminalOutputLabel, { ...native('t2'), reply: 'Done' }),
    movement(6, protocol.inputLabel, { text: 'Again', message_id: 'm2' }, { from: 'chat', to: 'provider', kind: 'InterfaceArrival' }),
    movement(7, 'turn.started', native('t3')),
    movement(8, 'provider.failure', { failure_class: 'transport', source_movement_seq: 6, details: { error: 'Provider transport closed' } }),
  ];
  const states = [];
  for (let count = 1; count <= rows.length; count += 1) {
    const reduced = await store.reduceOperatorChatTurnState({ open: false, state: 'idle', lastMovementSeq: -1, updatedAtMs: 0 }, rows.slice(0, count), entry, protocol);
    states.push(`${reduced.open ? 'open' : 'closed'}:${reduced.state}`);
  }
  assert.deepEqual(states, ['open:waiting', 'open:working', 'closed:idle', 'open:working', 'closed:idle', 'open:waiting', 'open:working', 'closed:failed']);
  const messages = await project(rows.slice(5).filter(row => row.kind === 'InterfaceOutbound'), true);
  assert.deepEqual(messages.map(message => [message.event_kind, message.text, message.error]).at(-1), ['error', 'Provider transport closed', true]);
});

test('a sealed stream without its seal row is not projected', async () => {
  const header = { type: 'header', schema_version: 1, seq: 0, provider_interface: 'provider', run: 'run', thread_id: 'thread', turn_id: 'turn', opened_source_movement_seq: 1, opened_at_unix_ms: 1_000 };
  const event = { type: 'event', seq: 1, source_movement_seq: 1, recorded_at_unix_ms: 1_001, label: 'agent.message.delta', payload: { text: 'Hel', item_id: 'answer', turn_id: 'turn' } };
  await assert.rejects(project([movement(2, 'turn.stream', { __raw: [header, event].map(row => JSON.stringify(row)).join('\n') + '\n' })], true), /no seal row/);
});
