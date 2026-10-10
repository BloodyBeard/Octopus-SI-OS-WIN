'use strict';

const crypto = require('node:crypto');
const INVOCATION_FIELDS = ['schema_version', 'request_kind', 'host_generation', 'challenge', 'request_id', 'executor', 'tool_descriptor', 'invocation_nonce', 'invocation_handle', 'source_movement_seq', 'artifact_content_hash', 'formation', 'tool', 'arguments', 'effective_allow_network', 'operation'];
const RELEASE_FIELDS = ['schema_version', 'request_kind', 'host_generation', 'challenge', 'request_id', 'release_nonce', 'formation', 'worker_environment_kind_descriptor_hash', 'release_mode'];
const LIMIT = 16;
function fail(message, code = 'authority_denied') { throw Object.assign(new Error(message), { code }); }
function need(ok, message, code) { if (!ok) fail(message, code); }
function shape(value, fields) { need(value && typeof value === 'object' && !Array.isArray(value), 'Expected native authority object', 'invalid_request'); if (fields) need(Object.keys(value).length === fields.length && fields.every(key => Object.hasOwn(value, key)), 'Native authority fields differ', 'invalid_request'); return value; }
function hex(value, size) { need(typeof value === 'string' && value.length === size && /^[0-9a-f]+$/.test(value), 'Invalid native authority identity'); }
function canonical(value) { if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`; if (value && typeof value === 'object') return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`; return JSON.stringify(value); }
function digest(value) { return crypto.createHash('sha256').update(canonical(value)).digest('hex'); }
function random() { return crypto.randomBytes(16).toString('hex'); }

function createBrowserInvocations({ descriptorHash, generation, rooms, createRoom, ensureLive, form, methods, ownerEnded, invocationEnded }) {
  need(typeof descriptorHash === 'function', 'Native browser requires its declared BLAKE3 descriptor verifier');
  function header(body, connection) {
    shape(body, body.request_kind === 'release_formation' ? RELEASE_FIELDS : INVOCATION_FIELDS);
    need(body.schema_version === 2 && body.host_generation === generation() && body.challenge === connection.challenge, 'Native browser request version, challenge or generation differs', 'authentication_failed');
    need(Number.isSafeInteger(body.request_id) && body.request_id > 0 && body.request_id === connection.nextId, 'Native browser request counter is stale', 'replay_rejected');
    connection.nextId++;
    form(body.formation);
  }
  function descriptor(body) {
    need(typeof body.tool_descriptor === 'string' && body.tool_descriptor.length <= 1024 * 1024, 'Pinned tool descriptor exceeds its native boundary');
    const bytes = Buffer.from(body.tool_descriptor, 'base64');
    need(bytes.toString('base64') === body.tool_descriptor, 'Pinned tool descriptor is not canonical base64');
    const pin = descriptorHash(bytes); hex(pin, 64);
    need(pin === body.formation.tool_descriptor_hashes[body.tool.id], 'Original tool descriptor does not match the formed BLAKE3 pin');
    const value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
    need(value.id === body.tool.id && value.interface === body.formation.door_interface && value.handler_config?.operation === body.tool.operation, 'Pinned tool descriptor does not own this invocation');
    need(canonical(value.supported_worker_environment_kinds) === canonical([body.formation.worker_environment_kind]), 'Pinned descriptor names another execution kind');
    shape(value.process); need(typeof value.process.bin === 'string' && value.process.bin.length > 0, 'Pinned execution owner has no binary');
    need(body.effective_allow_network === (value.process.authority?.allow_network === true && body.formation.network_ceiling !== 'denied'), 'Effective network permission differs from its pinned declaration');
    return { key: digest(value.process), pin };
  }
  function immutableDigest(body, declared) {
    return digest({ formation: body.formation, executor_instance: body.executor.instance_nonce, owner: declared.key, descriptor: declared.pin, tool: body.tool, invocation_nonce: body.invocation_nonce, source_movement_seq: body.source_movement_seq, artifact_content_hash: body.artifact_content_hash, arguments: body.arguments, effective_allow_network: body.effective_allow_network });
  }
  function validate(lease) {
    const { room, owner, active, connection } = lease;
    ensureLive(room);
    need(!connection.socket.destroyed && connection.room === room && owner.connection && !owner.connection.socket.destroyed && owner.active === active && owner.instance === active.instance && !active.abort.signal.aborted, 'Native invocation ended after dispatch; its effect may have completed', 'execution_uncertain');
    return lease;
  }
  function accept(body, connection) {
    header(body, connection);
    const formationDigest = digest(body.formation);
    let room = rooms.get(body.formation.worker_environment_id);
    if (room) need(room.formationDigest === formationDigest, 'Immutable formed room changed');
    need(!connection.room || connection.room === room, 'Native connection cannot change its formed room');
    if (body.request_kind === 'release_formation') {
      hex(body.release_nonce, 32); hex(body.worker_environment_kind_descriptor_hash, 64);
      need(body.worker_environment_kind_descriptor_hash === body.formation.worker_environment_kind_descriptor_hash && ['live_owner', 'cold_recovery'].includes(body.release_mode), 'Release does not name the exact formed kind');
      if (!room) return { kind: 'release', room: null, status: 'already_absent' };
      if (room.releaseCleanup?.complete) return { kind: 'release', room, status: 'already_released' };
      room.releaseNonces ??= new Set();
      need(!room.releaseNonces.has(body.release_nonce) && room.releaseNonces.size < LIMIT, 'Native release nonce is stale or its retention bound is exhausted', 'replay_rejected');
      room.releaseNonces.add(body.release_nonce); connection.room = room; connection.releasing = true;
      return { kind: 'release', room, status: 'released' };
    }
    need(body.request_kind === 'invocation', 'Unknown native request kind');
    shape(body.executor, ['instance_nonce', 'connection_role']); hex(body.executor.instance_nonce, 32);
    need(['owner', 'member'].includes(body.executor.connection_role), 'Invalid native executor connection role');
    shape(body.tool, ['id', 'operation']); shape(body.arguments);
    need(Object.hasOwn(methods, body.tool.operation) && body.formation.granted_tools.includes(body.tool.id), 'Tool is outside the formed browser operations');
    hex(body.invocation_nonce, 32); hex(body.artifact_content_hash, 64);
    need(Number.isSafeInteger(body.source_movement_seq) && body.source_movement_seq > 0, 'Invalid source movement sequence');
    need(typeof body.effective_allow_network === 'boolean', 'Missing effective network authority');
    shape(body.operation, ['target_id', 'session_id', 'method', 'params']); shape(body.operation.params);
    need(typeof body.operation.method === 'string' && body.operation.method.length > 0 && body.operation.method.length <= 2048, 'Invalid native operation method');
    need(body.operation.target_id === null || typeof body.operation.target_id === 'string' && body.operation.target_id.length > 0 && body.operation.target_id.length <= 2048, 'Invalid native target');
    need(body.operation.session_id === null || body.operation.target_id !== null && typeof body.operation.session_id === 'string' && body.operation.session_id.length > 0 && body.operation.session_id.length <= 2048, 'Invalid native child session');
    const declared = descriptor(body), invocationDigest = immutableDigest(body, declared);
    const begin = body.operation.method === 'OctopusBrowser.beginInvocation';
    const end = body.operation.method === 'OctopusBrowser.endInvocation';
    if (begin || end) need(body.operation.target_id === null && body.operation.session_id === null && connection.targetId === null && body.executor.connection_role === 'owner', 'Invocation lifetime belongs to its owner control connection');
    if (begin) { shape(body.operation.params, []); need(body.invocation_handle === null, 'Invocation begin cannot reuse a handle'); }
    else hex(body.invocation_handle, 32);
    if (!room) {
      need(begin && !['wait_complete', 'history_resume'].includes(body.tool.operation), 'Completion requires an existing formed browser source', 'context_unavailable');
      room = createRoom(body.formation, formationDigest);
    }
    ensureLive(room); connection.room = room;
    room.executionOwners ??= new Map();
    let owner = room.executionOwners.get(declared.key);
    if (!owner) {
      need(begin && room.executionOwners.size < Math.min(LIMIT, room.formation.granted_tools.length), 'Declared native execution owner is absent or its retention bound is exhausted', 'owner_unavailable');
      owner = { key: declared.key, seq: 0, instance: null, connection: null, active: null, retired: new Set() };
      room.executionOwners.set(declared.key, owner);
    }
    if (connection.executionOwner) need(connection.executionOwner === owner && connection.executorInstance === body.executor.instance_nonce && connection.executorRole === body.executor.connection_role, 'Native connection changed its declared execution owner');
    if (begin) {
      need(!owner.active && body.source_movement_seq > owner.seq, 'Execution owner has an active or later invocation', 'replay_rejected');
      need(!owner.retired.has(body.executor.instance_nonce), 'Retired executor instance cannot return', 'replay_rejected');
      if (owner.connection) need(owner.connection === connection && owner.instance === body.executor.instance_nonce, 'Live native execution owner cannot be replaced', 'owner_unavailable');
      else {
        need(owner.retired.size < LIMIT, 'Retired execution owner bound is exhausted', 'owner_capacity');
        owner.connection = connection; owner.instance = body.executor.instance_nonce;
      }
      owner.seq = body.source_movement_seq;
      owner.active = Object.freeze({ handle: random(), instance: owner.instance, nonce: body.invocation_nonce, digest: invocationDigest, sourceSeq: body.source_movement_seq, captures: new Map(), pending: { count: 0 }, abort: new AbortController() });
      room.seq = Math.max(room.seq, owner.seq);
    } else {
      need(owner.connection && !owner.connection.socket.destroyed && owner.instance === body.executor.instance_nonce && owner.active?.handle === body.invocation_handle && owner.active.nonce === body.invocation_nonce && owner.active.digest === invocationDigest, 'Invocation handle is absent, ended or changed', 'replay_rejected');
      if (body.executor.connection_role === 'owner') need(owner.connection === connection, 'Invocation owner control connection differs');
    }
    connection.executionOwner = owner; connection.executorInstance = owner.instance; connection.executorRole = body.executor.connection_role;
    const lease = Object.freeze({ kind: begin ? 'begin' : end ? 'end' : 'command', room, owner, active: owner.active, connection });
    validate(lease);
    return lease;
  }
  function end(lease) {
    validate(lease);
    const { owner, active, room } = lease;
    need(active.pending.count === 0, 'Invocation still has admitted work in flight', 'invocation_busy');
    owner.active = null; active.abort.abort(); active.captures.clear(); invocationEnded(room, active.handle);
    return { ended: true };
  }
  function disconnect(connection) {
    const owner = connection.executionOwner, room = connection.room;
    if (!owner) return;
    if (owner.connection !== connection) { if (connection.readerHandle) invocationEnded(room, connection.readerHandle); return; }
    if (owner.active) { owner.active.abort.abort(); invocationEnded(room, owner.active.handle); owner.active = null; }
    owner.retired.add(owner.instance);
    ownerEnded(room, `${owner.key}:${owner.instance}`);
    owner.connection = null;
  }
  function revoke(room) {
    for (const owner of room.executionOwners?.values() ?? []) {
      if (owner.active) { owner.active.abort.abort(); invocationEnded(room, owner.active.handle); owner.active = null; }
      if (owner.instance) ownerEnded(room, `${owner.key}:${owner.instance}`);
    }
  }
  function validateCreator(room, owner, instance) { ensureLive(room); need(owner.instance === instance && owner.connection && !owner.connection.socket.destroyed, 'Wait source executor has disconnected', 'source_lost'); }
  return { accept, validate, end, disconnect, revoke, validateCreator };
}
module.exports = { createBrowserInvocations };
