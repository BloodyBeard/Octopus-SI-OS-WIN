'use strict';

const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { requirePackagedEngine, sourceContract } = require('./macos/engine-artifact.cjs');
const API = 'electron.ownedNavigationHistory.v1';
const DEFINITION_BYTES = 65536;
const RESPONSE_BYTES = 96 * 1024 * 1024;
const directoryWrites = new Map();
const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/;
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value !== null && typeof value === 'object'
  ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value);
function check(condition, message) { if (!condition) throw new Error(message); }
function record(value, allowed, required = allowed) {
  check(value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).every(key => allowed.includes(key)) && required.every(key => Object.hasOwn(value, key)), 'Navigation history object fields differ from their declared schema');
  return value;
}
function text(value) { check(typeof value === 'string' && value.length > 0 && value.length <= 2048 && value.trim() === value && !/[\x00-\x1f\x7f]/.test(value), 'Navigation history identity is invalid'); return value; }
function hex(value) { check(typeof value === 'string' && /^[a-f0-9]{64}$/.test(value), 'Navigation history digest is invalid'); return value; }
function bound(value, maximum) { check(Number.isSafeInteger(value) && value > 0 && value <= maximum, 'Navigation history source bound is invalid'); return value; }
function artifact(value) { record(value, ['id', 'content']); text(value.id); check(Array.isArray(value.content) && value.content.length === 32 && value.content.every(byte => Number.isInteger(byte) && byte >= 0 && byte <= 255), 'Navigation history source must retain the exact ArtifactRef BLAKE3 byte array'); return value; }
function validateSource(sourceRef, payloadBytes, payloadDigest) {
  artifact(sourceRef); hex(payloadDigest);
  check(Buffer.isBuffer(payloadBytes) && payloadBytes.length <= DEFINITION_BYTES && sha(payloadBytes) === payloadDigest, 'Navigation history source bytes do not match the captured payload');
  const value = JSON.parse(payloadBytes.toString('utf8'));
  record(value, ['schema_version', 'source_id', 'owner_interface', 'collection', 'query_policy']);
  check(value.schema_version === 1 && typeof value.source_id === 'string' && UUID.test(value.source_id)
    && value.collection === 'explicit_door_selection', 'Navigation history source identity or collection contract differs');
  text(value.owner_interface);
  const limits = record(value.query_policy, ['model_read_enabled', 'request_timeout_ms', 'max_queries', 'max_query_bytes', 'max_results', 'max_result_bytes']);
  check(typeof limits.model_read_enabled === 'boolean', 'Navigation history source must declare model read availability');
  bound(limits.request_timeout_ms, 300000); bound(limits.max_queries, 2147483647);
  bound(limits.max_query_bytes, DEFINITION_BYTES); bound(limits.max_results, 2147483647); bound(limits.max_result_bytes, RESPONSE_BYTES);
  return value;
}
async function directory(file, create) {
  check(path.isAbsolute(file) && path.normalize(file) === file, 'Navigation history storage must be canonical');
  if (create) {
    try { await fs.mkdir(file, {mode:0o700}); } catch (error) { if (error.code !== 'EEXIST') throw error; }
  }
  const info = await fs.lstat(file);
  check(info.isDirectory() && !info.isSymbolicLink() && info.uid === process.getuid()
    && (info.mode & 0o777) === 0o700 && await fs.realpath(file) === file, 'Navigation history storage is not an owned mode0700 directory');
}
async function sourceDirectory(hostRoot, sourceRef, definition, payloadDigest, purpose) {
  const key = path.join(hostRoot, 'navigation-history', definition.source_id);
  const task = (directoryWrites.get(key) ?? Promise.resolve()).then(() => writeSourceDirectory(hostRoot, sourceRef, definition, payloadDigest, purpose));
  directoryWrites.set(key, task);
  try { return await task; } finally { if (directoryWrites.get(key) === task) directoryWrites.delete(key); }
}
async function writeSourceDirectory(hostRoot, sourceRef, definition, payloadDigest, purpose) {
  await directory(hostRoot, false);
  const parent = path.join(hostRoot, 'navigation-history'); await directory(parent, purpose === 'collection');
  const location = path.join(parent, definition.source_id); await directory(location, purpose === 'collection');
  const expected = Buffer.from(canonical({schema_version:1, source_ref:sourceRef, source_payload_digest:payloadDigest}));
  const metadata = path.join(location, 'source.json'); let file;
  if (purpose === 'collection') try {
    file = await fs.open(metadata, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    await file.writeFile(expected); await file.sync();
  } catch (error) { if (error.code !== 'EEXIST') throw error; }
  finally { await file?.close(); }
  file = await fs.open(metadata, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const info = await file.stat();
    check(info.isFile() && info.uid === process.getuid() && info.nlink === 1 && (info.mode & 0o777) === 0o600
      && info.size === expected.length, 'Navigation history source record ownership or size differs');
    check((await file.readFile()).equals(expected), 'Navigation history directory belongs to another immutable source');
  } finally { await file.close(); }
  if (purpose === 'collection') {
    const opened = await fs.open(location, constants.O_RDONLY); try { await opened.sync(); } finally { await opened.close(); }
  } else {
    const database = await fs.open(path.join(location, 'History'), constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const info = await database.stat(), header = Buffer.alloc(16);
      check(info.isFile() && info.uid === process.getuid() && info.nlink === 1 && info.size >= header.length, 'Selected history source has no existing owned native database');
      const {bytesRead} = await database.read(header, 0, header.length, 0);
      check(bytesRead === header.length && header.equals(Buffer.from('SQLite format 3\0')), 'Selected history source is not an existing native SQLite database');
    } finally { await database.close(); }
  }
  return location;
}
function normalizeQuery(query, limits, human) {
  const fields = human ? ['queries','from','to','limit','deduplicate_by_day','matching_algorithm'] : ['queries','from','to','limit'];
  record(query, fields, []);
  const queries = query.queries ?? [''];
  check(Array.isArray(queries) && queries.length > 0 && queries.length <= limits.max_queries
    && queries.every(value => typeof value === 'string'), 'Navigation history requires a bounded nonempty query array');
  check(Buffer.byteLength(JSON.stringify(queries)) <= limits.max_query_bytes, 'Navigation history query bytes exceed the selected source policy');
  const limit = bound(query.limit ?? 100, limits.max_results), native = {maxResults:limit};
  for (const [field, destination] of [['from','startTime'],['to','endTime']]) {
    if (!Object.hasOwn(query, field)) continue;
    check(typeof query[field] === 'string' && Number.isFinite(Date.parse(query[field])), 'Navigation history requires a valid date');
    native[destination] = Date.parse(query[field]);
  }
  if (human) {
    check(queries.length === 1 && query.deduplicate_by_day === true && query.matching_algorithm === 'alwaysPrefix', 'Human history must declare its actual native query policy');
    native.deduplicateByDay = true; native.matchingAlgorithm = 'alwaysPrefix';
  }
  return {queries, limit, native};
}
async function openNavigationHistorySource({electron, hostRoot, engineProof, sourceRef, payloadBytes, payloadDigest, purpose, revalidate, revalidateNow}) {
  check(purpose === 'collection' || purpose === 'read', 'History source opening requires an explicit collection or read purpose');
  const definition = validateSource(sourceRef, payloadBytes, payloadDigest);
  check(purpose !== 'collection' || typeof revalidateNow === 'function', 'Selected collection requires its exact live synchronous source witness');
  check(typeof revalidate === 'function', 'Navigation history requires the existing owner current-source check');
  await revalidate();
  check(typeof electron?.session?.createOwnedNavigationHistory === 'function', 'Selected engine has no native navigation history implementation');
  const verified = requirePackagedEngine(engineProof), contract = sourceContract();
  check(contract.navigation.history_api === API && verified.source.navigation_history_patch_sha256 === contract.navigation.patch.sha256
    && verified.source.navigation_history_manifest_sha256 === contract.navigation_sha256, 'Selected history implementation receipt differs from current package source');
  const location = await sourceDirectory(hostRoot, sourceRef, definition, payloadDigest, purpose);
  await revalidate();
  const reader = electron.session.createOwnedNavigationHistory({directory:location, sourceId:definition.source_id, sourceRefDigest:sha(canonical(sourceRef))});
  let closed = false, readerReleased = false, identity, timer;
  const active = new Map();
  const current = async () => { check(!closed, 'Navigation history source handle is closed'); await revalidate(); check(!closed, 'Navigation history source handle was closed'); };
  const close = () => { closed = true; clearTimeout(timer); for (const cancel of [...active.values()]) cancel(new Error('Navigation history source lifetime ended')); active.clear(); if (!readerReleased) { reader.close(); readerReleased = true; } };
  try {
    identity = await Promise.race([reader.ready(), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Native navigation history backend readiness deadline')), definition.query_policy.request_timeout_ms); })]);
    clearTimeout(timer);
    record(identity, ['sourceId','sourceRefDigest','epoch','ready','bookmarks','visitedLinks']);
    check(identity.sourceId === definition.source_id && identity.sourceRefDigest === sha(canonical(sourceRef)) && identity.ready === true
      && UUID.test(identity.epoch) && identity.bookmarks === false && identity.visitedLinks === false, 'Native history readiness identity differs');
    await current();
  } catch (error) { try { close(); } catch (cleanup) { throw new AggregateError([error, cleanup], 'Navigation history readiness and reader cleanup failed'); } throw error; }
  async function search(query, {human = false, signal} = {}) {
    await current();
    check(human || definition.query_policy.model_read_enabled, 'Model history reads are not selected for this source');
    const normalized = normalizeQuery(query, definition.query_policy, human);
    const requestId = crypto.randomBytes(16).toString('hex'), nativeIds = [], completion = new Set();
    let rejectCancellation;
    const cancelled = new Promise((_, reject) => { rejectCancellation = reject; });
    let cancelledError;
    const cancel = error => { if (cancelledError) return; cancelledError = error; for (const id of nativeIds) if (!completion.has(id)) reader.cancel(id); rejectCancellation(error); };
    const abort = () => cancel(new Error('Navigation history query lifetime was cancelled'));
    const deadline = setTimeout(() => cancel(new Error('Navigation history query deadline')), definition.query_policy.request_timeout_ms);
    active.set(requestId, cancel); signal?.addEventListener('abort', abort, {once:true}); if (signal?.aborted) abort();
    try {
      const work = async () => {
        if (cancelledError) throw cancelledError;
        const parts = await Promise.all(normalized.queries.map(async text => {
          const id = crypto.randomBytes(16).toString('hex'); nativeIds.push(id);
          const result = await reader.search(id, {text, ...normalized.native}); completion.add(id);
          check(result.requestId === id && result.sourceId === identity.sourceId && result.sourceRefDigest === identity.sourceRefDigest
            && result.epoch === identity.epoch && result.ready === true && Array.isArray(result.items)
            && result.items.length <= normalized.limit, 'Native history query returned another source or oversized result');
          return result.items.map(row => {
            record(row, ['url','title','lastVisitTime']);
            check(typeof row.url === 'string' && ['http:','https:'].includes(new URL(row.url).protocol) && typeof row.title === 'string'
              && Number.isFinite(row.lastVisitTime) && Number.isFinite(new Date(row.lastVisitTime).valueOf()), 'Native history visit evidence is invalid');
            return {url:row.url, title:row.title, dateVisited:new Date(row.lastVisitTime).toISOString()};
          });
        }));
        const seen = new Set(), rows = parts.flat().sort((a,b) => Date.parse(b.dateVisited) - Date.parse(a.dateVisited));
        const items = human ? rows.slice(0, normalized.limit) : rows.filter(row => { if (seen.has(row.url)) return false; seen.add(row.url); return true; }).slice(0, normalized.limit);
        const result = {source_ref:sourceRef, source_payload_digest:payloadDigest, source_epoch:identity.epoch, items,
          native_query_evidence:{request_id:requestId, native_request_ids:nativeIds, query_digest:sha(canonical(query))}};
        check(Buffer.byteLength(JSON.stringify(result)) <= definition.query_policy.max_result_bytes, 'Navigation history response exceeds its selected source byte bound');
        await current(); return result;
      };
      return await Promise.race([work(), cancelled]);
    } finally { clearTimeout(deadline); active.delete(requestId); signal?.removeEventListener('abort', abort); for (const id of nativeIds) if (!completion.has(id)) reader.cancel(id); }
  }
  return Object.freeze({sourceRef:structuredClone(sourceRef), payloadDigest, sourceEpoch:identity.epoch, definition:structuredClone(definition),
    search, close, observeReady:guest => {
      check(purpose === 'collection' && !closed && identity?.ready, 'Only the selected ready collector may synchronously enroll a page');
      requirePackagedEngine(engineProof); revalidateNow();
      check(!closed && reader.observe(guest), 'Native source did not enroll this exact retained page before navigation');
      try { revalidateNow(); check(!closed, 'Native collector was withdrawn during enrollment'); } catch (error) { reader.unobserve(guest); throw error; }
    }, observe:async guest => { check(purpose === 'collection', 'A history read cannot enroll a page'); await current(); check(reader.observe(guest), 'Native source did not enroll this exact owned page'); try { await current(); } catch (error) { reader.unobserve(guest); throw error; } },
    unobserve:guest => { check(purpose === 'collection', 'A history read cannot change page enrollment'); check(!closed, 'Navigation history source is closed'); return reader.unobserve(guest); }});
}

module.exports = {openNavigationHistorySource, validateSource, normalizeQuery, API};
