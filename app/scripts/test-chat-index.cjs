const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { DatabaseSync } = require('node:sqlite');
const { operatorChatIndexProjection } = require('../electron/projection/operatorChatIndex.cjs');
const { normalizeIndexedProjectionMovement, validateCanonicalMovement } = require('../electron/projection/movementRows.cjs');

const content = 'ab'.repeat(32);
function row(seq, run, kind, label, from, to) {
  return { seq, lamport: seq, recorded_at_unix_ms: 100 + seq, trace: 'index-test', kind,
    from: { Interface: from }, to: { Interface: to }, artifact: { id: `${label}:${content}`, content: Array(32).fill(171) },
    invocation_metadata: { runtime: { run, checkpoint: 'checkpoint', interface: 'chat', ee: 'ee', epoch: 0, worker: 'worker' } } };
}
function columns(row) {
  return { seq: row.seq, lamport: row.lamport, recorded_at_unix_ms: row.recorded_at_unix_ms, trace: row.trace, kind: row.kind,
    from_kind: 'interface', from_id: row.from.Interface, to_kind: 'interface', to_id: row.to.Interface,
    artifact_id: row.artifact.id, artifact_content: content, runtime_run: row.invocation_metadata.runtime.run,
    runtime_checkpoint: 'checkpoint', runtime_interface: 'chat' };
}

test('chat index preserves scoped history and durable tail while seeking each run without a global sort', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'oct-chat-index-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const ledger = path.join(root, 'movements.jsonl'), index = path.join(root, 'ledger-index.sqlite');
  const rows = [
    row(0, 'readonly', 'InterfaceOutbound', 'custom.output', 'provider', 'other'),
    row(1, 'task', 'InterfaceArrival', 'operator.message', 'chat', 'provider'),
    row(2, 'task', 'InterfaceArrival', 'operator.message', 'other', 'provider'),
    row(3, 'task', 'InterfaceOutbound', 'tool.call', 'provider', 'room'),
    row(4, 'task', 'InterfaceOutbound', 'turn.stream', 'provider', 'chat'),
    row(5, 'readonly', 'InterfaceArrival', 'custom.input', 'other', 'provider'),
    row(6, 'home', 'InterfaceArrival', 'realtime.delegation', 'voice', 'provider'),
    row(7, 'task', 'InterfaceOutbound', 'provider.failure', 'provider', 'chat'),
    row(8, 'task', 'InterfaceOutbound', 'turn.error', 'provider', 'chat'),
    row(9, 'task', 'InterfaceOutbound', 'turn.completed', 'provider', 'chat'),
    row(10, 'unrelated', 'ArtifactMoved', 'custom.output', 'provider', 'other'),
  ];
  const lines = rows.map(row => JSON.stringify(row) + '\n');
  fs.writeFileSync(ledger, lines.slice(0, 9).join('') + lines[9].slice(0, 20));
  const db = new DatabaseSync(index);
  db.exec(`CREATE TABLE index_meta(key TEXT PRIMARY KEY, value TEXT);
    CREATE TABLE movements(seq INTEGER PRIMARY KEY, lamport INTEGER, recorded_at_unix_ms INTEGER, trace TEXT, kind TEXT,
      from_kind TEXT, from_id TEXT, to_kind TEXT, to_id TEXT, artifact_id TEXT, artifact_content TEXT,
      runtime_run TEXT, runtime_checkpoint TEXT, runtime_interface TEXT);
    CREATE INDEX idx_mv_kind ON movements(kind, seq);
    CREATE INDEX idx_mv_runtime_run ON movements(runtime_run, seq) WHERE runtime_run IS NOT NULL;`);
  const insert = db.prepare('INSERT INTO movements VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)');
  for (const row of rows.slice(0, 8)) insert.run(...Object.values(columns(row)));
  const meta = db.prepare('INSERT INTO index_meta VALUES (?, ?)');
  for (const [key, value] of Object.entries({ schema_version: '4', watermark: '8', durable_end_offset: String(Buffer.byteLength(lines.slice(0, 8).join(''))) })) meta.run(key, value);
  db.close();
  const plans = [], prepare = DatabaseSync.prototype.prepare;
  t.mock.method(DatabaseSync.prototype, 'prepare', function (sql) {
    const statement = prepare.call(this, sql);
    if (/SELECT[\s\S]+runtime_run[\s\S]+FROM movements/.test(sql)) {
      const all = statement.all.bind(statement);
      statement.all = (...params) => { plans.push(...prepare.call(this, 'EXPLAIN QUERY PLAN ' + sql).all(...params)); return all(...params); };
    }
    return statement;
  });
  const scope = { client_runs: ['task', 'home'].map(run_id => ({ run_id, interface_id: 'chat' })),
    read_only_runs: ['readonly', ...Array.from({ length: 127 }, (_, i) => `empty-${i}`)].map(run_id => ({ run_id, interface_id: 'chat' })) };
  let refreshes = 0;
  const store = { projectionGeneration: 1, operatorOsScope: () => scope,
    parseCanonicalMovement: (bytes, file, line) => validateCanonicalMovement(JSON.parse(bytes), file, line),
    refreshLedgerProjection: async () => { refreshes++; } };
  Object.assign(store, operatorChatIndexProjection(store));
  const projection = { durableRoot: root, operatorChatStatusCursor: -1,
    operatorChatProtocol: { inputLabel: 'operator.message', terminalOutputLabel: 'turn.completed', errorOutputLabel: 'turn.error' },
    runRecords: ['task', 'home'].map(run_id => ({ run_id, interface_state: { chat: { provider_runtime_interface: 'provider',
      ...(run_id === 'home' ? { client_surface_role: 'home', voice_context: { delegation_interface_id: 'voice' } } : {}) } } })) };
  const entries = await store.operatorChatRunIndex(projection), byRun = new Map(entries.map(entry => [entry.runId, entry]));
  const sequences = (run, field) => byRun.get(run)[field].map(row => row.seq);
  assert.equal(entries.length, 130);
  assert.deepEqual(sequences('task', 'activity'), [1, 2, 3, 4, 7, 8]);
  assert.deepEqual(sequences('task', 'inbound'), [1]);
  assert.deepEqual(sequences('task', 'outbound'), [4, 7, 8]);
  assert.deepEqual(sequences('readonly', 'inbound'), [5]);
  assert.deepEqual(sequences('readonly', 'outbound'), [0]);
  assert.deepEqual(sequences('home', 'inbound'), [6]);
  assert.deepEqual(byRun.get('task').inbound[0], normalizeIndexedProjectionMovement(columns(rows[1]), ledger));
  assert.equal(byRun.get('task').inbound[0], byRun.get('task').activity[0]);
  assert.equal(await store.operatorChatRunIndex(projection), entries);
  assert.equal(projection.operatorChatStatusCursor, 8);
  assert.equal(refreshes, 0);
  assert.ok(plans.length > 0 && plans.every(row => /SEARCH movements USING INDEX idx_mv_runtime_run/.test(row.detail)), JSON.stringify(plans));
  fs.appendFileSync(ledger, lines[9].slice(20) + lines[10]);
  const delta = await store.operatorChatLedgerDelta(projection, new Set(['task']), 8);
  assert.deepEqual(delta.rowsByRun.get('task').map(row => row.seq), [9]);
  assert.equal(delta.nextMovementSeq, 10);
  const corrupt = new DatabaseSync(index);
  corrupt.prepare('UPDATE movements SET artifact_content = NULL WHERE seq = 1').run();
  corrupt.close();
  await assert.rejects(store.operatorChatLedgerRows(projection, new Set(['task'])), /incomplete artifact reference/);
  assert.equal(refreshes, 0);
  fs.unlinkSync(index);
  projection.ledgerRows = rows;
  projection.ledgerRowsByRunSources = [new Map([['task', rows.filter(row => row.invocation_metadata.runtime.run === 'task')]])];
  assert.equal((await store.operatorChatLedgerRows(projection, new Set(['task']))).get('task').length, 7);
  assert.equal(projection.operatorChatStatusCursor, 10);
  assert.equal(refreshes, 1);
});
