const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { blake3 } = require('@noble/hashes/blake3');
const { createProjectionContext } = require('../electron/projection.cjs');

test('persisted task status is an exact incremental fold and rejects changed evidence', async (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-turn-cache-'));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const ledger = path.join(home, 'ledger/movements.jsonl'), blobs = path.join(home, 'ledger/blobs');
  fs.mkdirSync(blobs, { recursive: true });
  const protocol = { inputLabel: 'message', terminalOutputLabel: 'answer', errorOutputLabel: 'turn.error', artifactReadProjection: {
    schema_version: 1, json_object_labels: ['message', 'answer', 'turn.started', 'turn.completed', 'turn.control.acknowledged', 'turn.control.failed'],
    excluded_root_fields: ['private'], max_source_bytes: 100000, max_projected_bytes: 100000,
  } };
  const rows = [], payloadFiles = [];
  const blob = (value) => {
    const bytes = Buffer.isBuffer(value) ? value : Buffer.from(JSON.stringify(value)), hash = Buffer.from(blake3(bytes)).toString('hex');
    const file = path.join(blobs, hash); fs.writeFileSync(file, bytes); return { hash, file };
  };
  const add = (label, payload, incoming = false) => {
    const content = blob(payload), manifest = blob({ label, payload_path: content.file, payload_content_hash: content.hash });
    const seq = rows.length;
    rows.push({ seq, recorded_at_unix_ms: 100 + seq, kind: incoming ? 'InterfaceArrival' : 'InterfaceOutbound',
      from: { Interface: incoming ? 'chat' : 'provider' }, to: { Interface: incoming ? 'provider' : 'chat' },
      artifact: { id: `${label}:${manifest.hash}`, content: manifest.hash }, source_file: ledger, source_line: seq + 1,
      invocation_metadata: { runtime: { run: 'task', checkpoint: 'work' } } });
    payloadFiles.push(content.file);
    fs.writeFileSync(ledger, rows.map(row => JSON.stringify(row)).join('\n') + '\n');
  };
  const entry = { runId: 'task', chatInterfaceId: 'chat', activity: rows,
    record: { interface_state: { chat: { provider_runtime_interface: 'provider' } } } };
  const read = async (expectedReads) => {
    const store = createProjectionContext({ resolveRepoRoot: () => path.resolve(__dirname, '../..'), octopusHome: () => home });
    const resolve = store.resolveLedgerArtifactPayload, readSequences = [];
    store.resolveLedgerArtifactPayload = (...args) => { readSequences.push(args[0].seq); return resolve(...args); };
    const projection = { octopusHome: home, operatorChatTurnStateCache: new Map(), warnings: [] };
    try {
      const result = await store.operatorChatTurnState(projection, entry, protocol);
      if (expectedReads) assert.deepEqual([...new Set(readSequences)], expectedReads);
      store.resolveLedgerArtifactPayload = resolve;
      const uncached = await store.reduceOperatorChatTurnState({ open: false, state: 'idle', lastMovementSeq: -1, updatedAtMs: 0 }, rows, entry, protocol);
      assert.deepEqual(result, uncached);
      return result;
    } finally { store.close(); }
  };
  const native = (turn, source) => ({ thread_id: 'thread', turn_id: turn, source_movement_seq: source });
  add('message', { text: 'First', message_id: 'm1' }, true);
  add('turn.started', native('t1', 0));
  assert.equal((await read([0, 1])).state, 'working');
  await read([]);
  add('message', { text: 'Second', message_id: 'm2' }, true);
  add('turn.completed', { ...native('t1', 0), status: 'completed' });
  const queued = await read([2, 3]);
  assert.equal(queued.open, true);
  assert.deepEqual(queued.pendingInputs, [{ seq: 2, clientId: 'm2' }]);
  add('turn.started', native('t2', 2));
  add('turn.control.acknowledged', { ...native('t2', 2), action: 'interrupt', status: 'acknowledged', control_id: 'stop' });
  assert.equal((await read([4, 5])).state, 'idle');
  add('turn.control.acknowledged', { ...native('t3', 2), action: 'resume', status: 'acknowledged', control_id: 'resume' });
  assert.equal((await read([6])).state, 'working');
  add('turn.error', { ...native('t3', 2), error: 'Failure', will_retry: false });
  assert.equal((await read([7])).state, 'failed');
  await read([]);
  add('message', { text: 'Retry', message_id: 'm3' }, true);
  add('turn.started', native('t4', 8));
  add('message', { text: 'Include this too', message_id: 'm4' }, true);
  const stream = [
    { type: 'header', schema_version: 1, seq: 0, provider_interface: 'provider', run: 'task', thread_id: 'thread', turn_id: 't4',
      opened_source_movement_seq: 8, opened_at_unix_ms: 108 },
    { type: 'input_recorded', seq: 1, recorded_at_unix_ms: 110, source_movement_seq: 10, provider_interface: 'provider', run: 'task',
      thread_id: 'thread', turn_id: 't4', item_id: 'extra-item', client_id: 'm4', stage: 'input_recorded_in_turn', source_event: 'item/started', native_event_at_ms: 110 },
    { type: 'seal', seq: 2, recorded_at_unix_ms: 111, source_movement_seq: 11, terminal_label: 'turn.completed', terminal_artifact_payload_blake3: 'a'.repeat(64) },
  ];
  add('turn.stream', Buffer.from(stream.map(row => JSON.stringify(row)).join('\n') + '\n'));
  assert.equal((await read([8, 9, 10, 11])).state, 'working');
  assert.deepEqual([...(await read([])).recordedInputs.values()].map(ids => [...ids]), [['m4']]);
  add('turn.completed', { ...native('t4', 8), status: 'completed' });
  assert.equal((await read([12])).state, 'idle');
  const directory = path.join(home, 'client/projection-cache/turns');
  const cache = path.join(directory, fs.readdirSync(directory)[0]);
  fs.writeFileSync(cache, '{');
  await read(rows.map(row => row.seq));
  protocol.artifactReadProjection.excluded_root_fields.push('another_private_field');
  await read(rows.map(row => row.seq));
  rows[0].recorded_at_unix_ms++;
  await read(rows.map(row => row.seq));
  fs.renameSync(ledger, `${ledger}.old`);
  fs.copyFileSync(`${ledger}.old`, ledger);
  await read(rows.map(row => row.seq));
  entry.record.interface_state.chat.provider_runtime_interface = 'other-provider';
  await read([0, 2, 8, 10]);
  entry.record.interface_state.chat.provider_runtime_interface = 'provider';
  await read(rows.map(row => row.seq));
  const original = fs.readFileSync(payloadFiles[0]);
  fs.writeFileSync(payloadFiles[0], original);
  await read(rows.map(row => row.seq));
  fs.writeFileSync(payloadFiles[0], '{"text":"corrupt"}');
  await assert.rejects(read(), /digest/);
  fs.writeFileSync(payloadFiles[0], original);
  await read(rows.map(row => row.seq));
  fs.rmSync(payloadFiles[0]);
  await assert.rejects(read(), /ENOENT/);
  fs.writeFileSync(payloadFiles[0], original);
  fs.rmSync(directory, { recursive: true });
  fs.writeFileSync(directory, 'cache directory unavailable');
  assert.equal((await read(rows.map(row => row.seq))).state, 'idle');
});

test('task previews reuse verified text and invalidate source, identity and projection changes', async (t) => {
  const { cachedOperatorChatArtifactText } = require('../electron/projection/operatorChatCache.cjs');
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-preview-cache-'));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const blobs = path.join(home, 'ledger/blobs'); fs.mkdirSync(blobs, { recursive: true });
  const blob = (value) => {
    const bytes = Buffer.from(JSON.stringify(value)), hash = Buffer.from(blake3(bytes)).toString('hex');
    const file = path.join(blobs, hash); fs.writeFileSync(file, bytes); return { hash, file };
  };
  const payload = blob({ reply: ' First\n response ', text: ' \n ', private: 'Hidden' });
  const manifest = blob({ label: 'answer', payload_path: payload.file, payload_content_hash: payload.hash });
  const row = { source_file: path.join(home, 'ledger/movements.jsonl'), artifact: { id: `answer:${manifest.hash}`, content: manifest.hash } };
  const protocol = { artifactReadProjection: { schema_version: 1, json_object_labels: ['answer'], excluded_root_fields: ['private'],
    max_source_bytes: 1000, max_projected_bytes: 1000 } };
  const entry = { runId: 'run', chatInterfaceId: 'chat' };
  const createStore = () => createProjectionContext({ resolveRepoRoot: () => path.resolve(__dirname, '../..'), octopusHome: () => home });
  const read = async (expectedLoads, direction = 'out', existingStore) => {
    const store = existingStore ?? createStore();
    let loads = 0;
    try {
      const text = await cachedOperatorChatArtifactText({ home, entry, row, protocol, direction, limit: 240, warnings: [],
        resolve: (value, onVerifiedFiles) => { loads++; return store.resolveLedgerArtifactPayload(value, { projection: protocol.artifactReadProjection, onVerifiedFiles }); } });
      assert.equal(loads, expectedLoads); return text;
    } finally { if (!existingStore) store.close(); }
  };
  assert.equal(await read(1), 'First response');
  assert.equal(await read(0), 'First response');
  const directory = path.join(home, 'client/projection-cache/previews');
  const cache = path.join(directory, fs.readdirSync(directory)[0]);
  const poisoned = JSON.parse(fs.readFileSync(cache)); poisoned.value.text = 'invented';
  fs.writeFileSync(cache, JSON.stringify(poisoned));
  assert.equal(await read(1), 'First response');
  protocol.artifactReadProjection.excluded_root_fields.push('reply');
  assert.equal(await read(1), '');
  assert.equal(await read(0), '');
  assert.equal(fs.readdirSync(directory).length, 1);
  assert.equal(await read(1, 'in'), '');
  assert.equal(await read(0, 'in'), '');
  protocol.artifactReadProjection.excluded_root_fields.push('text');
  assert.equal(await read(1, 'in'), '');
  assert.equal(await read(0, 'in'), '');
  assert.equal(fs.readdirSync(directory).length, 2);
  row.artifact.id = `answer:${'f'.repeat(64)}`;
  await assert.rejects(read(1), /identity/);
  row.artifact.id = `answer:${manifest.hash}`;
  const warm = createStore();
  t.after(() => warm.close());
  await warm.resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
  fs.writeFileSync(payload.file, '{"reply":"changed"}');
  await assert.rejects(read(1, 'out', warm), /digest/);
  await assert.rejects(read(1), /digest/);
});
