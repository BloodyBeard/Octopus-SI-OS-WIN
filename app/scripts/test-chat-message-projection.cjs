const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const { test } = require('node:test');

const source = fs.readFileSync(path.join(__dirname, '../electron/projection.cjs'), 'utf8');
const tree = ts.createSourceFile('projection.cjs', source, ts.ScriptTarget.Latest, true);
const declarations = new Map();
function visit(node) {
  if (ts.isFunctionDeclaration(node) && node.name) declarations.set(node.name.text, node.getText(tree));
  ts.forEachChild(node, visit);
}
visit(tree);
const protocol = { inputLabel: 'input', terminalOutputLabel: 'output', errorOutputLabel: 'error', hiddenActivityLabels: [] };

test('Stop aliases match actual tasks titles without reading terminal history', async () => {
  await require('./build-cli.cjs').buildCli();
  const { createClient } = require('../cli/client.cjs');
  const entries = ['home', 'task', 'named'].map((name, index) => ({
    runId: `run_${name}`, chatInterfaceId: `chat_${name}`, surfaceRole: name === 'home' ? 'home' : 'task',
    writable: true, activity: [], outbound: [], record: {},
    inbound: [{ label: 'input', seq: index + 1, time: index + 1, payload: { text: name === 'home' ? 'Review\n notes' : 'Not the authored title' } }],
  }));
  entries[0].inbound.unshift({ label: 'input', seq: 0, time: 10, payload: { text: 'Later input' } });
  const sidecars = new Map([['chat_home', { presentation: { name: 'Home' } }],
    ['chat_task', { presentation: { name: 'Task title' } }], ['chat_named', { presentation: { name: 'run_named' } }]]);
  const scope = { status: 'ready', home_run_id: 'run_home', home_interface_id: 'chat_home',
    client_runs: entries.map(entry => ({ run_id: entry.runId, interface_id: entry.chatInterfaceId, surface_role: entry.surfaceRole, archived: false })) };
  const projection = { operatorChatProtocol: protocol };
  let allowPayload = true, allowHistory = true, payloadReads = 0;
  const context = vm.createContext({
    stringValue: value => typeof value === 'string' ? value : '',
    objectOrEmpty: value => value && typeof value === 'object' && !Array.isArray(value) ? value : {},
    arrayValue: value => Array.isArray(value) ? value : [], numberValue: (value, fallback) => Number.isFinite(value) ? value : fallback,
    operatorOsScope: () => scope, octopusOsInterfaceSidecars: () => sidecars, operatorChatRunIndex: async () => entries,
    ledgerArtifactLabel: row => row.label, rowTimeMs: row => row.time, operatorChatTurnText: payload => payload.text,
    resolveLedgerArtifactPayload: async row => {
      assert(allowPayload, 'scope and last selectors must not read payloads'); payloadReads++;
      assert.equal(row, entries[0].inbound[1], 'only the first Home input supplies a derived title'); return row.payload;
    },
    operatorOsApplications: () => ({}), eeInstanceRows: () => [], operatorChatRunProvider: () => ({}),
    projectHomeVoiceContext: () => null, operatorActivityAgent: () => null,
    operatorChatTurnState: async () => { assert(allowHistory); return { open: false, state: 'idle' }; },
    operatorChatLastTerminalMessage: async () => { assert(allowHistory, 'control aliases must not read terminal history'); return {}; },
  });
  for (const name of ['operatorChatTitle', 'operatorChatControlTargets', 'buildOperatorChatsList']) {
    assert(declarations.has(name)); vm.runInContext(declarations.get(name), context);
  }
  const store = { async read({ method, params }) {
    if (method === 'projection.operator.os.scope') return scope;
    assert.equal(method, 'projection.operator.chats.list');
    return { chats: params?.control_targets_only
      ? await context.operatorChatControlTargets(projection, params.include_titles)
      : await context.buildOperatorChatsList(projection) };
  }, invalidate() {}, close() {} };
  const client = createClient({ home: require('node:os').tmpdir(), packageRoots: {}, status: async () => ({ runtimeReady: true }) }, {
    store, metadata: {}, loader: { load: async () => ({ submitOperatorTurnControl: async (_options, input) => ({ ...input, verdict: { admitted: { seq: 1 } } }) }) },
  });
  try {
    const tasks = await client.tasks();
    assert.deepEqual(Object.fromEntries(tasks.map(task => [task.id, task.title])), {
      run_home: 'Review notes', run_named: 'run_named', run_task: 'Task title',
    });
    assert.equal(payloadReads, 1);
    allowHistory = false; allowPayload = false;
    for (const [selector, id] of [['home', 'run_home'], ['run_task', 'run_task'], ['last', 'run_named']]) {
      assert.equal((await client.stop(selector)).runId, id);
    }
    assert.equal(payloadReads, 1);
    allowPayload = true;
    const aliases = await context.operatorChatControlTargets(projection, true);
    for (const task of tasks) {
      assert.equal(aliases.find(row => row.run_id === task.id).title, task.title);
      assert.equal((await client.stop(task.title)).runId, task.id);
    }
    sidecars.get('chat_task').presentation.name = tasks.find(task => task.isHome).title;
    await assert.rejects(client.stop('Review notes'), /ambiguous/);
  } finally { await client.close(); }
});

test('artifact cache separates raw bytes, projection contracts and exact artifact identities', async () => {
  const { artifactReadProjectionForLabel } = require('../electron/boundedJsonProjection.cjs');
  let reads = 0;
  const contract = { schema_version: 1, json_object_labels: ['reply'], excluded_root_fields: ['private'], max_source_bytes: 100, max_projected_bytes: 100 };
  const row = { source_file: 'ledger/movements.jsonl', id: 'reply:checked', label: 'reply' };
  const context = vm.createContext({ resolvedArtifactPayloads: new Map(), resolvedArtifactPayloadBytes: 0,
    RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES: 65536, RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_ENTRIES: 16,
    contentHashHex: () => 'a'.repeat(64), ledgerArtifactLabel: row => row.label, ledgerArtifactId: row => row.id, artifactReadProjectionForLabel,
    loadLedgerArtifactPayload: async (row, _hex, _required, projection) => {
      reads++;
      if (projection && row.id !== 'reply:checked') throw new Error('artifact identity mismatch');
      if (projection?.max_source_bytes < 50) throw new Error('source byte bound');
      return { payload: projection ? { text: 'Reply' } : { text: 'Reply', private: 'original' }, cacheable: true, retainedBytes: 32 };
    },
  });
  for (const name of ['resolveLedgerArtifactPayload', 'removeResolvedArtifactPayload', 'trimResolvedArtifactPayloads']) vm.runInContext(declarations.get(name), context);
  assert.equal((await context.resolveLedgerArtifactPayload(row)).private, 'original');
  assert.equal((await context.resolveLedgerArtifactPayload(row, { projection: contract })).private, undefined);
  await context.resolveLedgerArtifactPayload(row, { projection: contract }); assert.equal(reads, 2);
  assert.equal((await context.resolveLedgerArtifactPayload({ ...row, label: 'binary' }, { projection: contract })).private, 'original');
  await assert.rejects(context.resolveLedgerArtifactPayload({ ...row, id: 'foreign' }, { projection: contract }), /identity/);
  await assert.rejects(context.resolveLedgerArtifactPayload(row, { projection: { ...contract, max_source_bytes: 1 } }), /source byte bound/);
  await assert.rejects(context.resolveLedgerArtifactPayload(row, { projection: undefined }), /projection/i);
});
function row(seq, kind, text, item = 'a', phase = null, from = 'provider') {
  return { seq, source_line: seq, from, to: 'chat', label: kind === 'message' ? 'output' : kind,
    payload: { kind, text, turn_id: 'turn', item_id: item, phase, lifecycle: ['message', 'commentary'].includes(kind) ? 'completed' : 'streaming' } };
}
async function project(rows, writable) {
  const context = vm.createContext({
    stringValue: value => typeof value === 'string' ? value : '',
    objectOrEmpty: value => value && typeof value === 'object' && !Array.isArray(value) ? value : {},
    arrayValue: value => Array.isArray(value) ? value : [], numberValue: (value, fallback) => Number.isFinite(value) ? value : fallback,
    baseInterfaceRows: () => [{ id: 'provider', role: 'provider' }, { id: 'other-provider', role: 'provider' }],
    movementBoundaryInterface: (value, side) => value[side], ledgerArtifactId: value => `artifact-${value.seq}`,
    ledgerArtifactLabel: value => value.label, contentHashHex: () => '', rowTimeMs: value => value.seq * 100,
    compareMovementOrder: (a, b) => a.seq - b.seq,
    resolveLedgerArtifactPayloads: async values => new Map(values.map(value => [value, value.payload])),
    operatorChatExpandedStreamRows: values => values, operatorChatRuntimeProviderId: () => 'provider',
    operatorChatEventData: value => ({ ...value }), operatorChatEventKind: (_label, _protocol, value) => value.kind,
    operatorChatEventText: value => value.text, operatorChatTurnText: value => value.text || '', operatorChatAttachments: () => [],
    operatorChatProjectedRowId: value => `row-${value.seq}`, operatorChatProviderFailureActivity: () => null,
    operatorChatActivityPayload: value => ({ eventKind: value.kind, text: value.text, payload: value, error: false }),
    operatorChatRequestKey: () => '', operatorChatIncrementalEventKey: () => '', operatorTurnControlOutcome: () => null,
    operatorTurnControlEndsTurn: () => false,
  });
  for (const name of ['operatorChatStreamPositionCovered', 'operatorChatAssistantItemCompleted', 'appendOperatorChatAssistantMessage', 'buildSingleOperatorChatMessages', 'operatorReadOnlyRunMessages']) {
    assert(declarations.has(name)); vm.runInContext(declarations.get(name), context);
  }
  const entry = { runId: 'run', chatInterfaceId: 'chat', writable, inbound: [], outbound: rows, activity: rows };
  const result = await context.buildSingleOperatorChatMessages({}, entry, protocol);
  return JSON.parse(JSON.stringify(result));
}

test('both chat history paths preserve interleaved item identity and replace completed snapshots', async () => {
  for (const writable of [true, false]) for (const finalText of ['', 'Done!']) {
    const messages = await project([
      row(1, 'message_delta', 'Pro'), row(2, 'dynamic_tool', 'Read', 'tool'),
      row(3, 'message_delta', 'First', 'b'), row(4, 'message_delta', 'gress'),
      row(5, 'commentary', 'Progress', 'a', 'commentary'), row(6, 'dynamic_tool', 'Read complete', 'tool'),
      row(7, 'message_delta', ' second', 'b'), row(8, 'commentary', 'First second', 'b', 'commentary'),
      row(9, 'message_delta', ' late'), row(10, 'message_delta', 'Done', 'final', 'final_answer'),
      row(11, 'message', finalText, 'final', 'final_answer'),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Progress', 'Read', 'First second', 'Read complete', finalText || 'Done']);
    assert.deepEqual(messages.filter(message => message.event_kind === 'commentary').map(message => message.event_data.item_id), ['a', 'b']);
    assert.equal(messages[0].created_at_ms, 100); assert.equal(messages[0].movement_seq, 5);
    assert.equal(messages.at(-1).event_data.item_id, 'final'); assert.equal(messages.at(-1).terminal, true);
  }
});

test('provider identity and missing item identity never merge unrelated text', async () => {
  for (const writable of [true, false]) {
    const messages = await project([
      row(1, 'message_delta', 'Same'), row(2, 'message_delta', 'Same', 'a', null, 'other-provider'),
      row(3, 'message_delta', 'Unknown', ''), row(4, 'message_delta', 'Unknown', ''),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Same', 'Same', 'Unknown', 'Unknown']);
  }
});

test('completed partial answers retain their text without completing the turn', async () => {
  const classify = vm.runInNewContext(`(${declarations.get('operatorChatEventKind')})`, {
    stringValue: value => typeof value === 'string' ? value : '',
  });
  assert.equal(classify('turn.agent_message', protocol, { phase: 'partial_answer' }), 'message');
  for (const writable of [true, false]) {
    const partial = row(2, 'message', 'Answer so far', 'partial', 'partial_answer');
    partial.label = 'turn.agent_message';
    const messages = await project([
      row(1, 'message_delta', 'Answer', 'partial', 'partial_answer'), partial,
      row(3, 'dynamic_tool', 'Continue researching', 'tool'),
      row(4, 'message', 'Complete answer', 'final', 'final_answer'),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Answer so far', 'Continue researching', 'Complete answer']);
    assert.equal(messages[0].terminal, false);
    assert.equal(messages[0].event_data.phase, 'partial_answer');
    assert.equal(messages.at(-1).terminal, true);
  }
});

test('a sealed stream folds each delta position once even when it is delivered twice', async () => {
  const delta = (seq, text, stream_seq) => ({ ...row(seq, 'message_delta', text, 'answer', 'final_answer'),
    payload: { ...row(seq, 'message_delta', text, 'answer', 'final_answer').payload, _stream: { stream_id: 'sealed', stream_seq } } });
  for (const writable of [true, false]) {
    const messages = await project([delta(1, 'Hel', 3), delta(2, 'lo', 4), delta(3, 'Hel', 3), delta(4, 'lo', 4), delta(5, '!', 6)], writable);
    assert.deepEqual(messages.map(message => message.text), ['Hello!']);
  }
});
