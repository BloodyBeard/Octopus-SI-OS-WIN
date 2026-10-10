const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { submitClientInterfaceArtifact, createClientArtifactStreamStore } = require('../electron/clientArtifacts.cjs');
const { createProjectionStore } = require('../electron/projection.cjs');
const { blake3 } = require('@noble/hashes/blake3');
const { prepareContentAddressedPayloadBindings } = require('../electron/contentAddressedArtifact.cjs');
const { createClientAttachmentStagingStore } = require('../electron/clientAttachments.cjs');

test('attachment chunks serialize before sealing and concurrent overflow cannot publish', async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-stage-contract-'));
  const store = createClientAttachmentStagingStore({ root: () => root });
  t.after(async () => { await store.abortAll(); await fs.rm(root, { recursive: true, force: true }); });
  const metadata = { name: 'proof.txt', label: 'attachment.file', type: 'text/plain', lastModified: 0, size: 6 };
  const first = await store.begin('owner', metadata);
  const chunks = [Buffer.from('abc'), Buffer.from('def')];
  const writes = chunks.map(bytes => store.append('owner', { id: first.id, bytes }));
  const sealing = store.finalize('owner', first);
  chunks[0].fill(120);
  await assert.rejects(store.append('owner', { id: first.id, bytes: Buffer.from('!') }), /closing|unavailable/);
  assert.deepEqual((await Promise.all(writes)).map(value => value.bytes), [3, 6]);
  const sealed = await store.consume('owner', await sealing);
  assert.equal(await fs.readFile(sealed.payloadPath, 'utf8'), 'abcdef');
  assert.equal(sealed.contentHash, crypto.createHash('sha256').update('abcdef').digest('hex'));
  await store.discard(sealed);
  const second = await store.begin('owner', { ...metadata, size: 3 });
  const outcomes = await Promise.allSettled(['abc', 'def'].map(value => store.append('owner', { id: second.id, bytes: Buffer.from(value) })));
  assert.deepEqual(outcomes.map(value => value.status), ['fulfilled', 'rejected']);
  await assert.rejects(store.finalize('owner', second), /declared size/);
  await store.abort('owner', second);
  assert.deepEqual(await fs.readdir(path.join(root, 'attachment-staging')), []);
  for (const cleanup of [() => store.abortOwner('owner'), () => store.abortAll()]) {
    const ready = await store.stageBytes('owner', metadata, Buffer.from('abcdef'));
    const pending = await store.begin('owner', metadata);
    const writing = store.append('owner', { id: pending.id, bytes: Buffer.from('abcdef') });
    const finalizing = store.finalize('owner', pending);
    await cleanup();
    await Promise.all([writing, finalizing]);
    await assert.rejects(store.consume('owner', ready), /unavailable/);
    await assert.rejects(store.consume('owner', pending), /unavailable/);
    assert.deepEqual(await fs.readdir(path.join(root, 'attachment-staging')), []);
  }
});

test('durable attachment recovery preserves the client stage until payload binding finishes', async t => {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-attachment-contract-')));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const sourcePath = path.join(root, 'stage'), durable = path.join(root, 'durable'), blobsRoot = path.join(root, 'blobs');
  await Promise.all([fs.mkdir(durable), fs.mkdir(blobsRoot), fs.writeFile(sourcePath, 'selected file')]);
  const attachment = { name: 'proof.txt', label: 'attachment.file', type: 'text/plain', size: 13, lastModified: 0,
    payloadPath: sourcePath, contentHash: crypto.createHash('sha256').update('selected file').digest('hex') };
  const file = path.join(__dirname, '../../registry/interface/world_operator_chat/client.cjs');
  const api = Function('require', 'module', '__dirname', `${await fs.readFile(file, 'utf8')}\nreturn { planOperatorAttachments, materializeAttachments };`)(require, { exports: {} }, path.dirname(file));
  const plans = await api.planOperatorAttachments(durable, [attachment]);
  await api.materializeAttachments(plans);
  const bindings = await prepareContentAddressedPayloadBindings({ blobsRoot, attachments: [attachment], payloadArtifactBindings: [], revalidate: async () => {} });
  const manifest = JSON.parse(await fs.readFile(path.join(blobsRoot, Buffer.from(bindings[0].content).toString('hex')), 'utf8'));
  assert.equal(await fs.readFile(manifest.payload_path, 'utf8'), 'selected file');
  await api.materializeAttachments(plans);
  await fs.writeFile(plans[0].attachment.payload_path, 'corrupt file!');
  await assert.rejects(api.materializeAttachments(plans), /conflicts with its content address/);
  assert.equal(await fs.readFile(sourcePath, 'utf8'), 'selected file');
});

test('steering binds owned attachments and cleans up partially consumed selections', async () => {
  const source = await fs.readFile(path.join(__dirname, '../electron/main.cjs'), 'utf8');
  const start = source.indexOf('ipcMain.handle("octopus:submit-chat-steer",');
  const end = source.indexOf('\nipcMain.handle(', start + 1);
  assert.ok(start >= 0 && end > start);
  let handler, submitted = 0;
  const discarded = [], consumed = { payloadPath: '/owned-stage' };
  const binding = { id: 'selected', content: Array(32).fill(1) };
  const dependencies = {
    ipcMain: { handle: (_name, callback) => { handler = callback; } },
    getClientAccessController: () => ({ requireSession: () => ({ profile: 'live' }) }),
    prepareLiveProjectionRead: async () => {}, resolveOctopusHome: () => '/fixture', path,
    getClientAttachmentStagingStore: () => ({ consume: async (_owner, stage) => { if (stage === 'bad') throw new Error('invalid stage'); return consumed; }, discard: async value => { discarded.push(value); } }),
    resolveInstallationRoot: () => '/installation', payloadArtifactBindings: value => value,
    prepareContentAddressedPayloadBindings: async input => { assert.deepEqual(input.attachments, [consumed]); await input.revalidate(); return [binding]; },
    loadOperatorChatClient: async () => ({ submitOperatorTurn: async (options, request) => {
      submitted++;
      assert.deepEqual(await options.preparePayloadArtifactBindings({ payloadArtifactBindings: [] }), [binding]);
      assert.equal(request.reasoningEffort, 'high');
      return { chatInterfaceId: 'exact-client', messageId: 'exact-message', attachmentCount: 1 };
    } }),
  };
  Function(...Object.keys(dependencies), source.slice(start, end))(...Object.values(dependencies));
  await assert.rejects(handler({}, 'owner', { attachments: [{ stage: 'good' }, { stage: 'bad' }] }), /invalid stage/);
  assert.deepEqual(discarded, [consumed]); assert.equal(submitted, 0);
  assert.equal((await handler({}, 'owner', { attachments: [{ stage: 'good' }], reasoningEffort: 'high' })).attachmentCount, 1);
  assert.deepEqual(discarded, [consumed, consumed]); assert.equal(submitted, 1);
});

test('artifact polling and exact hand-in lookup do not wait for unrelated policy or catalog projection', { timeout: 3000 }, async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-projection-contract-'));
  const ledger = path.join(root, 'ledger'), registry = path.join(root, 'registry');
  await fs.mkdir(path.join(ledger, 'blobs'), { recursive: true });
  const rows = [];
  for (let seq = 0; seq < 4; seq++) {
    const bytes = Buffer.from(JSON.stringify({ sequence: seq })), content = blake3(bytes);
    await fs.writeFile(path.join(ledger, 'blobs', Buffer.from(content).toString('hex')), bytes);
    rows.push({ seq, lamport: seq, trace: 'test', recorded_at_unix_ms: 1000 - seq, kind: 'InterfaceOutbound', from: { Interface: 'provider' }, to: { Interface: 'client' }, artifact: { id: `receipt:${seq}`, content: [...content] }, invocation_metadata: { runtime: { run: seq === 1 ? 'other-run' : 'test-run', ee: 'test-ee', worker: 'system_worker_test', checkpoint: 'test-checkpoint', interface: 'provider', epoch: 0 } } });
  }
  await fs.writeFile(path.join(ledger, 'movements.jsonl'), rows.map(JSON.stringify).join('\n') + '\n');
  await fs.mkdir(path.join(registry, 'interface', 'provider'), { recursive: true });
  await fs.writeFile(path.join(registry, 'interface', 'provider', 'interface.json'), JSON.stringify({ id: 'provider', interface_type: 'world', role: 'provider', direction: 'bidirectional', source_names: ['test'] }));
  let policyReads = 0, beforeReads = 0;
  const store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, '../..'), octopusHome: () => root, sourceRegistry: () => registry, beforeRead: async () => { beforeReads++; }, projectEePolicies: () => { policyReads++; return new Promise(() => {}); } });
  t.after(async () => { store.close(); await fs.rm(root, { recursive: true, force: true }); });
  const read = (method, params) => store.read({ contract_version: 1, method, params });
  const params = { run_id: 'test-run', interface_id: 'provider', labels: ['receipt'], after_seq: -1, limit: 2 };
  const first = await read('projection.interface_artifacts.list', params);
  assert.deepEqual(first.artifacts.map(row => row.payload.sequence), [0, 2]);
  const second = await read('projection.interface_artifacts.list', { ...params, after_seq: 2 });
  assert.deepEqual(second.artifacts.map(row => row.payload.sequence), [3]);
  const appended = Buffer.from(JSON.stringify({ ...rows[0], seq: 4, lamport: 4, trace: '🎙'.repeat(20_000), artifact: { ...rows[0].artifact, id: 'receipt:4' } }) + '\r\n');
  await fs.appendFile(path.join(ledger, 'movements.jsonl'), appended.subarray(0, -1));
  assert.deepEqual((await read('projection.interface_artifacts.list', { ...params, after_seq: 3 })).artifacts, []);
  await fs.appendFile(path.join(ledger, 'movements.jsonl'), appended.subarray(-1));
  assert.deepEqual((await read('projection.interface_artifacts.list', { ...params, after_seq: 3 })).artifacts.map(row => row.seq), [4]);
  const targets = await read('projection.interfaces.artifact_transfer.targets', { interface_id: 'provider' });
  assert.equal(targets.artifact_transfer_targets[0].status, 'not_declared');
  assert.equal(policyReads, 0);
  assert.equal(beforeReads, 5, 'The host access/read preparation remains mandatory');
  const runPath = path.join(root, 'state', 'runs', 'test-run.json');
  const run = { id: 'test-run', ee: 'test-ee', worker: 'worker', trace: 'trace', status: 'running', active_checkpoints: ['input'], checkpoints: [{ id: 'input', run: 'test-run', trace: 'trace', epoch: 0, interface_binding: { interface: 'provider', config: { artifact_policy: { mode: 'all' } } } }] };
  await fs.mkdir(path.dirname(runPath), { recursive: true });
  await fs.writeFile(runPath, JSON.stringify(run));
  const scoped = { interface_id: 'provider', run_id: 'test-run' };
  assert.equal((await read('projection.interfaces.artifact_transfer.targets', scoped)).artifact_transfer_targets.length, 1);
  await fs.writeFile(runPath, JSON.stringify({ ...run, active_checkpoints: [] }));
  assert.deepEqual((await read('projection.interfaces.artifact_transfer.targets', scoped)).artifact_transfer_targets, []);
  await fs.writeFile(runPath, '{}');
  await assert.rejects(read('projection.interfaces.artifact_transfer.targets', scoped), /run is not present/);
  assert.equal(policyReads, 0);
});

async function fixture(t, handIn) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-artifact-contract-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const options = {
    clientArtifactsRoot: () => root,
    authorizeSubmission: async () => {},
    projectionStore: () => ({ read: async () => ({ artifact_transfer_targets: [{ interface_id: 'client_test', status: 'ready', transfer_path: '/unused-test-socket' }] }) }),
    handInArtifact: handIn,
  };
  return { root, options };
}

test('artifact delivery publishes complete bytes and refuses corrupt existing identity', async t => {
  let delivered = 0;
  const payload = { text: 'exact payload', correlation_id: 'request-1' };
  const bytes = Buffer.from(JSON.stringify(payload));
  const digest = crypto.createHash('sha256').update(bytes).digest('hex');
  const { root, options } = await fixture(t, async ({ manifest }) => {
    assert.deepEqual(await fs.readFile(manifest.payload_path), bytes);
    delivered++;
    return { admitted: { seq: delivered } };
  });
  const request = { interfaceId: 'client_test', label: 'test.payload', payload };
  await Promise.all(Array.from({ length: 12 }, () => submitClientInterfaceArtifact(options, request)));
  assert.equal(delivered, 12);
  const destination = path.join(root, 'interface-artifacts', `test.payload-${digest}.json`);
  const damaged = Buffer.alloc(bytes.length, 120);
  await fs.writeFile(destination, damaged);
  await assert.rejects(submitClientInterfaceArtifact(options, request), /digest/);
  assert.equal(delivered, 12);
  assert.deepEqual(await fs.readFile(destination), damaged);
  assert.deepEqual(await fs.readdir(path.dirname(destination)), [path.basename(destination)]);
  const operatorPath = path.join(__dirname, '../../registry/interface/world_operator_chat/client.cjs');
  const writeControl = Function('require', 'module', '__dirname', `${await fs.readFile(operatorPath, 'utf8')}\nreturn writeContentAddressedPayload;`)(require, { exports: {} }, path.dirname(operatorPath));
  const control = await writeControl(root, 'stop', Buffer.from('immutable control'));
  await fs.writeFile(control, 'changed control');
  await assert.rejects(writeControl(root, 'stop', Buffer.from('immutable control')), /stable message identity/);
});

test('media finalization preserves immutable bytes and cannot be reported as aborted in flight', async t => {
  let submitted, release, reached;
  const entered = new Promise(resolve => { reached = resolve; });
  const gate = new Promise(resolve => { release = resolve; });
  const { options } = await fixture(t, async ({ manifest }) => {
    submitted = JSON.parse(await fs.readFile(manifest.payload_path, 'utf8'));
    assert.equal(await fs.readFile(submitted.media.path, 'utf8'), 'firstsecond');
    reached(); await gate;
    return { admitted: { seq: 1 } };
  });
  const streams = createClientArtifactStreamStore(options);
  const request = { interfaceId: 'client_test', label: 'test.audio', mediaType: 'audio/webm', extension: 'webm' };
  const first = await streams.begin('view', request);
  const bytes = Buffer.from('first');
  const accepted = streams.append('view', { streamId: first.streamId, bytes });
  bytes.fill(120);
  await accepted;
  await streams.append('view', { streamId: first.streamId, bytes: Buffer.from('second') });
  const finishing = streams.finalize('view', first);
  await entered;
  try { await assert.rejects(streams.abort('view', first), /already closing/); }
  finally { release(); }
  assert.equal((await finishing).mediaBytes, 11);
  const damaged = Buffer.from('corruptdata');
  await fs.writeFile(submitted.media.path, damaged);
  const second = await streams.begin('view', request);
  await streams.append('view', { streamId: second.streamId, bytes: Buffer.from('firstsecond') });
  await assert.rejects(streams.finalize('view', second), /digest/);
  assert.deepEqual(await fs.readFile(submitted.media.path), damaged);
  assert.deepEqual(await fs.readdir(path.dirname(submitted.media.path)), [path.basename(submitted.media.path)]);
});
