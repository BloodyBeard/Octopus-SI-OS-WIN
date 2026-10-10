'use strict';

const crypto = require('node:crypto');
const { validatePermissionOffer } = require('./browser-permissions.cjs');
const { openNavigationHistorySource, validateSource, normalizeQuery } = require('./browser-navigation-history.cjs');
const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value && typeof value === 'object'
  ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value);
const digest = value => crypto.createHash('sha256').update(canonical(value)).digest('hex');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
function requireValue(value, message) { if (!value) throw Object.assign(new Error(message), { code: 'history_contract_failed' }); }
function object(value, fields) { requireValue(value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length === fields.length && fields.every(key => Object.hasOwn(value, key)), 'History control fields differ'); return value; }
function sourceBytes(params) {
  requireValue(typeof params.payload_base64 === 'string' && params.payload_base64.length <= 87384, 'History source payload exceeds its captured definition bound');
  const bytes = Buffer.from(params.payload_base64, 'base64');
  requireValue(bytes.toString('base64') === params.payload_base64 && sha(bytes) === params.source_payload_digest, 'History source payload changed in native control');
  const definition = validateSource(params.source_ref, bytes, params.source_payload_digest);
  return { bytes, definition };
}
function createRoomHistory({ electron, hostRoot, engineProof, engineMode, room, generation, ensureLive, pendingLimit, permissions, onError }) {
  requireValue(engineMode === 'standard' || engineMode === 'owned', 'History control requires an explicit engineMode');
  requireValue(engineMode !== 'standard' || typeof onError === 'function', 'Standard history requires a collection error reporter');
  const openSource = engineMode === 'standard' ? require('./standard-navigation-history.cjs').openStandardNavigationHistorySource : openNavigationHistorySource;
  requireValue(Number.isSafeInteger(pendingLimit) && pendingLimit > 0, 'History control requires the existing finite room retention bound');
  const readers = new Map(), requests = new Map();
  let active = true, collection = null;
  const current = () => { requireValue(active, 'History source lifetime was withdrawn'); ensureLive(room); };
  const key = params => digest({ source_ref: params.source_ref, source_payload_digest: params.source_payload_digest });
  async function readerFor(params, purpose, validate) {
    current(); validate(); const { bytes, definition } = sourceBytes(params), id = key(params);
    const ownedId = `${purpose}:${id}`, existing = readers.get(ownedId);
    if (existing) {
      return existing;
    }
    requireValue(readers.size < pendingLimit, 'History source reader retention bound is exhausted');
    const reader = await openSource({ electron, hostRoot, engineProof, sourceRef: params.source_ref,
      payloadBytes: bytes, payloadDigest: params.source_payload_digest, purpose, revalidate: current, revalidateNow: current,
      validatePage: guest => requireValue(room.session && guest.session === room.session && !guest.isDestroyed(), 'History page is outside its selected room Session'), onError });
    try {
      current(); validate();
      requireValue(!readers.has(ownedId), 'History source was concurrently opened through another owner');
      const entry = { reader, purpose, definition }; readers.set(ownedId, entry); return entry;
    } catch (error) { reader.close(); throw error; }
  }
  async function prepare(body, params, validate) {
    object(params, ['source_ref','source_payload_digest','payload_base64','query']);
    requireValue(body.tool.operation === 'history', 'History source preparation requires its declared operation');
    requireValue(canonical(body.arguments) === canonical({ source: params.source_ref, ...params.query }), 'History preparation changed original signed arguments');
    const { definition } = sourceBytes(params);
    requireValue(definition.query_policy.model_read_enabled, 'Model history read is not selected for this source');
    normalizeQuery(params.query, definition.query_policy, false);
    const entry = await readerFor(params, 'read', validate);
    current(); validate();
    return { source_ref: entry.reader.sourceRef, source_payload_digest: entry.reader.payloadDigest,
      source_epoch: entry.reader.sourceEpoch, host_generation: generation(), query: structuredClone(params.query) };
  }
  function stage(body, params) {
    current(); object(params, ['request']);
    requireValue(body.tool.operation === 'history', 'History request staging requires its original declared operation');
    const request = object(params.request, ['schema_version','request_id','call_id','original','scope','approval_scope','history','resume','permission_offer']);
    validatePermissionOffer(request);
    const original = object(request.original, ['run','ee','worker','door_interface','tool','correlation_id','arguments','source_movement_seq','source_artifact']);
    const scope = object(request.scope, ['worker_environment_id','formation_pin','host_generation']);
    const history = object(request.history, ['source','source_payload_digest','source_epoch','query']);
    const resume = object(request.resume, ['tool','correlation_id']);
    const hash = digest({ schema_version: request.schema_version, original, scope, approval_scope: request.approval_scope, history, permission_offer: request.permission_offer });
    const entry = readers.get(`read:${key({ source_ref: history.source, source_payload_digest: history.source_payload_digest })}`);
    requireValue(request.schema_version === 1 && request.approval_scope === 'browser_history_read' && request.request_id === `sha256:${hash}`
      && request.call_id === original.correlation_id && typeof request.call_id === 'string' && request.call_id.length > 0
      && resume.correlation_id === `browser-history-resume:${hash}` && resume.tool !== original.tool && room.formation.granted_tools.includes(resume.tool)
      && scope.worker_environment_id === room.formation.worker_environment_id && scope.formation_pin === room.formation.formation_pin && scope.host_generation === generation()
      && original.run === room.formation.run && original.ee === room.formation.ee && original.worker === room.formation.worker && original.door_interface === room.formation.door_interface
      && original.tool === body.tool.id && original.source_movement_seq === body.source_movement_seq
      && Array.isArray(original.source_artifact?.content) && original.source_artifact.content.length === 32
      && original.source_artifact.content.every(byte => Number.isInteger(byte) && byte >= 0 && byte <= 255)
      && Buffer.from(original.source_artifact.content).toString('hex') === body.artifact_content_hash
      && canonical(original.arguments) === canonical(body.arguments) && canonical(body.arguments) === canonical({ source: history.source, ...history.query })
      && entry && entry.reader.sourceEpoch === history.source_epoch, 'History request changed its admitted original call, source epoch or formed room');
    requireValue(!requests.has(hash) && requests.size < pendingLimit, 'History request already exists or pending retention is full');
    requests.set(hash, { request: structuredClone(request), entry, started: false });
    return { status: 'prepared', request_id: request.request_id, source_epoch: history.source_epoch, host_generation: generation() };
  }
  async function apply(body, params, signal, validate) {
    current(); validate();
    const cached = Object.hasOwn(params, 'permission_entry_id');
    object(params, cached ? ['request_id','decision','permission_entry_id'] : ['request_id','decision']);
    const request = permissionRequest(body, params.request_id), hash = params.request_id.slice(7), pending = requests.get(hash);
    let reuse;
    if (cached) {
      requireValue(body.tool.operation === 'history', 'Saved history consent must continue the original read');
      reuse = await permissions.requireSaved(request, params.permission_entry_id, params.decision);
    } else {
      object(body.arguments, ['request_id']);
      requireValue(body.tool.operation === 'history_resume' && ['accept','decline','cancel'].includes(params.decision), 'History completion requires the admitted response');
    }
    current(); validate();
    requireValue(requests.get(hash) === pending && !pending.started, 'History request changed before query dispatch');
    pending.started = true;
    try {
      if (params.decision !== 'accept') return { status: params.decision === 'cancel' ? 'cancelled' : 'declined', request_id: params.request_id, effect_performed: false };
      requireValue(pending.entry.reader.sourceEpoch === pending.request.history.source_epoch, 'Approved native history source epoch changed');
      const result = await pending.entry.reader.search(pending.request.history.query, { signal });
      current(); validate();
      return { status: 'completed', request_id: params.request_id, effect_performed: false, host_generation: generation(), ...result, ...(reuse ? { permission_reuse: reuse } : {}) };
    } finally { requests.delete(hash); }
  }
  function permissionRequest(body, requestId) {
    current(); const pending = requests.get(requestId.slice(7));
    requireValue(pending && pending.request.request_id === requestId && !pending.started, 'Permission has no current native history request');
    const request = pending.request;
    if (body.tool.operation === 'history') requireValue(request.original.tool === body.tool.id && request.original.source_movement_seq === body.source_movement_seq && canonical(request.original.arguments) === canonical(body.arguments), 'Permission lost original history invocation');
    else requireValue(body.tool.operation === 'history_resume' && request.resume.tool === body.tool.id && body.arguments.request_id === requestId && body.source_movement_seq > request.original.source_movement_seq, 'Permission lost admitted history continuation');
    requireValue(pending.entry.reader.sourceEpoch === request.history.source_epoch, 'Permission source epoch changed');
    return request;
  }
  async function selectCollection(body, params, validate) {
    object(params, ['source_ref','source_payload_digest','payload_base64']);
    requireValue(['navigate','tab_new'].includes(body.tool.operation), 'Only admitted page creation can select its declared history collector');
    current(); validate(); sourceBytes(params);
    if (collection) {
      requireValue(canonical(collection.reader.sourceRef) === canonical(params.source_ref) && collection.reader.payloadDigest === params.source_payload_digest, 'Selected collector changed within this formation');
      return { source_ref: collection.reader.sourceRef, source_payload_digest: collection.reader.payloadDigest, source_epoch: collection.reader.sourceEpoch, host_generation: generation() };
    }
    requireValue(!room.session && room.targets.size === 0, 'A collector cannot be added by changing an existing page or context');
    const entry = await readerFor(params, 'collection', validate);
    requireValue(!room.session && room.targets.size === 0, 'Page context was born before collector selection completed');
    requireValue(collection === null || collection === entry, 'Room collector selection is immutable for this formation');
    collection = entry;
    return { source_ref: entry.reader.sourceRef, source_payload_digest: entry.reader.payloadDigest, source_epoch: entry.reader.sourceEpoch, host_generation: generation() };
  }
  function observe(guest) { current(); if (collection) collection.reader.observeReady(guest); current(); }
  function close() {
    active = false; requests.clear(); const errors = [];
    for (const [id, entry] of readers) try { entry.reader.close(); readers.delete(id); } catch (error) { errors.push(error); }
    if (errors.length) throw new AggregateError(errors, 'Native history reader cleanup did not complete');
    collection = null;
  }
  return { permissionRequest, prepare, stage, apply, selectCollection, observe, close };
}

module.exports = { createRoomHistory };
