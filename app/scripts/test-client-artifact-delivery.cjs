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

test('attachment previews stay inside the admitted manifest and exact movement coordinates', async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-attachment-preview-')), blobs = path.join(root, 'ledger/blobs'), rows = [];
  await fs.mkdir(blobs, { recursive: true }); await fs.mkdir(path.join(root, 'registry/interface'), { recursive: true });
  const store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, '../..'), octopusHome: () => root, sourceRegistry: () => path.join(root, 'registry') });
  t.after(async () => { store.close(); await fs.rm(root, { recursive: true, force: true }); });
  const bytes = Buffer.from('image bytes'), digest = crypto.createHash('sha256').update(bytes).digest('hex'), file = path.join(root, 'selected.png'); await fs.writeFile(file, bytes);
  const attachment = { name: 'selected.png', mime_type: 'image/png', size: bytes.length, payload_path: file, payload_content_hash: digest };
  const blob = async value => { const body = Buffer.from(JSON.stringify(value)), hash = Buffer.from(blake3(body)).toString('hex'); await fs.writeFile(path.join(blobs, hash), body); return hash; };
  const admit = async (attachments, kind = 'InterfaceArrival') => {
    const payload = await blob({ text: 'message', attachments }), hash = await blob({ label: 'operator.input', payload_path: '/unused/original', payload_content_hash: payload });
    const artifact = { id: `operator.input:${hash}`, content: [...Buffer.from(hash, 'hex')] }, seq = rows.length;
    rows.push({ seq, lamport: seq, trace: 'test', recorded_at_unix_ms: seq, kind, from: { Interface: 'client' }, to: { Interface: 'provider' }, artifact, invocation_metadata: { runtime: { run: 'run', ee: 'ee', worker: 'worker', checkpoint: 'checkpoint', interface: 'client', epoch: 0 } } });
    await fs.writeFile(path.join(root, 'ledger/movements.jsonl'), rows.map(JSON.stringify).join('\n') + '\n'); return artifact.id;
  };
  const id = await admit([attachment]), read = (params = {}) => store.read({ contract_version: 1, method: 'projection.artifact.bytes.read', params: { artifact_id: id, run_id: 'run', interface_id: 'client', attachment_id: digest, ...params } });
  assert.deepEqual(await read(), { artifact_id: id, run_id: 'run', interface_id: 'client', attachment_id: digest, filename: 'selected.png', mime_type: 'image/png', content_hash: digest, size: bytes.length, base64: bytes.toString('base64') });
  const second = await admit([{ ...attachment, payload_content_hash: 'f'.repeat(64), payload_path: '/must-not-read' }, attachment]);
  assert.equal((await read({ artifact_id: second, payload_path: '/must-not-read' })).base64, bytes.toString('base64'));
  const original = await store.read({ contract_version: 1, method: 'projection.artifact.bytes.read', params: { artifact_id: id, run_id: 'run', interface_id: 'client' } });
  assert.deepEqual(JSON.parse(Buffer.from(original.base64, 'base64')), { text: 'message', attachments: [attachment] }); assert.equal(Object.hasOwn(original, 'attachment_id'), false);
  for (const params of [{ run_id: 'foreign' }, { interface_id: 'foreign' }, { artifact_id: 'operator.input:unknown' }]) await assert.rejects(read(params), /not admitted/);
  for (const attachment_id of [null, '', '../selected.png', digest.toUpperCase()]) await assert.rejects(read({ attachment_id }), /exact SHA-256/);
  await assert.rejects(read({ attachment_id: 'f'.repeat(64), payload_path: file }), /exactly one/);
  for (const attachments of [[], [attachment, attachment], [{ ...attachment, size: '11' }], [{ ...attachment, size: 40 * 1024 * 1024 + 1 }], [{ ...attachment, size: bytes.length + 1 }], [{ ...attachment, mime_type: 'image/png\ntext/html' }], [{ ...attachment, payload_path: 'relative' }]]) {
    await assert.rejects(read({ artifact_id: await admit(attachments) }), /exactly one|size|mime_type|absolute/);
  }
  await assert.rejects(read({ artifact_id: await admit([{ ...attachment, name: 'refused' }], 'DeliveryRefused') }), /not admitted/);
  await fs.writeFile(file, Buffer.alloc(bytes.length, 0)); await assert.rejects(read(), /content hash/);
  await fs.unlink(file); const target = path.join(root, 'target'); await fs.writeFile(target, bytes); await fs.symlink(target, file); await assert.rejects(read(), /regular file/);
  const manifestFile = path.join(blobs, id.split(':')[1]); await fs.writeFile(manifestFile, '{}'); await assert.rejects(read(), /content-addressed digest/);
});

test('staged attachment previews preserve handles and reject ownership, lifetime and byte substitutions', async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-stage-preview-')), store = createClientAttachmentStagingStore({ root: () => root });
  t.after(async () => { await store.abortAll(); await fs.rm(root, { recursive: true, force: true }); });
  const bytes = Buffer.from('image bytes'), meta = { name: 'image.png', label: 'operator.file', type: 'image/png', lastModified: 0 };
  const handle = await store.stageBytes('owner', meta, bytes), request = { id: handle.id, contentHash: handle.contentHash }, preview = () => store.readPreview('owner', request);
  assert.deepEqual(await preview(), { base64: bytes.toString('base64'), mimeType: 'image/png', contentHash: handle.contentHash, size: bytes.length }); assert.deepEqual(await preview(), await preview());
  await assert.rejects(store.readPreview('other', request), /unavailable/);
  await assert.rejects(store.readPreview('owner', { ...request, contentHash: 'f'.repeat(64) }), /does not match/);
  await assert.rejects(store.readPreview('owner', { ...request, payloadPath: '/arbitrary' }), /exact stage/);
  const pending = await store.begin('owner', { ...meta, size: bytes.length }); await assert.rejects(store.readPreview('owner', { ...request, id: pending.id }), /unavailable/); await store.abort('owner', pending);
  const text = await store.stageBytes('owner', { ...meta, name: 'original.txt', type: 'text/plain' }, bytes);
  assert.deepEqual(await store.readPreview('owner', { id: text.id, contentHash: text.contentHash }), { base64: bytes.toString('base64'), mimeType: 'text/plain', contentHash: text.contentHash, size: bytes.length });
  await assert.rejects(store.readPreview('other', { id: text.id, contentHash: text.contentHash }), /unavailable/);
  const invalidMime = await store.stageBytes('owner', { ...meta, type: 'text/plain\ntext/html' }, bytes);
  await assert.rejects(store.readPreview('owner', { id: invalidMime.id, contentHash: invalidMime.contentHash }), /valid MIME/);
  const large = await store.stageBytes('owner', meta, Buffer.alloc(40 * 1024 * 1024 + 1)); await assert.rejects(store.readPreview('owner', { id: large.id, contentHash: large.contentHash }), /preview boundary/);
  const now = Date.now; Date.now = () => now() + 300_001; try { await assert.rejects(preview(), /unavailable/); } finally { Date.now = now; }
  const stagedFile = path.join(root, 'attachment-staging', `${handle.id}.ready`);
  await fs.writeFile(stagedFile, Buffer.alloc(bytes.length, 0)); await assert.rejects(preview(), /content hash/);
  await fs.writeFile(stagedFile, 'short'); await assert.rejects(preview(), /size/);
  await fs.unlink(stagedFile); const target = path.join(root, 'target'); await fs.writeFile(target, bytes); await fs.symlink(target, stagedFile); await assert.rejects(preview(), /regular file/);
  await fs.unlink(stagedFile); await fs.writeFile(stagedFile, bytes);
  const open = fs.open; let consumed;
  fs.open = async (...args) => { const file = await open(...args); if (args[0] === stagedFile) { const close = file.close.bind(file); file.close = async () => { await close(); consumed = await store.consume('owner', request); }; } return file; };
  try { await assert.rejects(preview(), /unavailable/); } finally { fs.open = open; }
  assert.equal(consumed.contentHash, handle.contentHash); await store.discard(consumed); await assert.rejects(preview(), /unavailable/);
});

test('preview transport authenticates its window before and after reads and preload retains the access token', async () => {
  const main = await fs.readFile(path.join(__dirname, '../electron/main.cjs'), 'utf8'), start = main.indexOf('ipcMain.handle("octopus:client-attachment-preview",');
  let handler, authenticated = true, live = true, endAccess = false, reads = 0;
  const sender = { mainFrame: {}, isDestroyed: () => !live }, event = { sender, senderFrame: sender.mainFrame }, request = { id: 'id', contentHash: 'hash' }, response = { base64: 'bytes', mimeType: 'image/png', contentHash: 'hash', size: 3 };
  const deps = { ipcMain: { handle: (_name, fn) => { handler = fn; } }, BrowserWindow: { fromWebContents: () => ({ webContents: sender, isDestroyed: () => !live }) }, getClientAccessController: () => ({ requireSession: token => { if (!authenticated || token !== 'owner') throw new Error('unauthorized'); } }), getClientAttachmentStagingStore: () => ({ readPreview: async (token, input) => { reads++; assert.equal(token, 'owner'); assert.equal(input, request); if (endAccess) authenticated = false; return response; } }) };
  assert.ok(start >= 0); Function(...Object.keys(deps), main.slice(start, main.indexOf('\nipcMain.handle(', start + 1)))(...Object.values(deps));
  await assert.rejects(handler(event, 'other', request), /unauthorized/); await assert.rejects(handler({ ...event, senderFrame: {} }, 'owner', request), /active client window/);
  live = false; await assert.rejects(handler(event, 'owner', request), /active client window/); live = true; assert.equal(reads, 0);
  assert.equal(await handler(event, 'owner', request), response); endAccess = true; await assert.rejects(handler(event, 'owner', request), /unauthorized/);
  let bridge; const electron = { contextBridge: { exposeInMainWorld: (_name, api) => { bridge = api; } }, ipcRenderer: { sendSync: () => 'owner', invoke: async (...args) => { assert.deepEqual(args, ['octopus:client-attachment-preview', 'owner', request]); return response; } } };
  Function('require', 'process', await fs.readFile(path.join(__dirname, '../electron/preload.cjs'), 'utf8'))(() => electron, { argv: ['--octopus-access-claim=test'] });
  assert.equal(await bridge.readClientAttachmentPreview(request), response);
});

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
  const api = require('../../registry/interface/world_operator_chat/client/operatorMessages.cjs');
  const plans = await api.planOperatorAttachments(durable, [attachment]);
  await api.materializeAttachments(plans);
  const renamed = await api.planOperatorAttachments(durable, [{ ...attachment, name: 'another name.txt' }]);
  assert.equal(renamed[0].attachment.payload_path, plans[0].attachment.payload_path);
  assert.equal(renamed[0].attachment.name, 'another name.txt');
  const beforeReuse = await fs.stat(plans[0].attachment.payload_path);
  await api.materializeAttachments(renamed);
  const afterReuse = await fs.stat(plans[0].attachment.payload_path);
  assert.equal(afterReuse.ino, beforeReuse.ino);
  assert.equal(afterReuse.mtimeMs, beforeReuse.mtimeMs);
  assert.equal((await fs.readdir(durable)).length, 1);
  const retained = { ...plans[0].attachment, payload_path: path.join(durable, 'already-authored-path') };
  await api.materializeAttachments(await api.planOperatorAttachments(durable, [attachment], [retained]));
  assert.equal(await fs.readFile(retained.payload_path, 'utf8'), 'selected file');
  await assert.rejects(api.planOperatorAttachments(durable, [attachment], [{ ...retained, payload_path: sourcePath }]), /owned root/);
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
  const { writeContentAddressedPayload: writeControl } = require('../../registry/interface/world_operator_chat/client/operatorMessages.cjs');
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
