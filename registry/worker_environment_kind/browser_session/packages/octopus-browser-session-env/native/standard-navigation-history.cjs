'use strict';

const crypto = require('node:crypto');
const { validateSource, normalizeQuery } = require('./browser-navigation-history.cjs');
const { openStandardHistoryStorage, childDirectory, canonical } = require('./standard-history-storage.cjs');
const API = 'octopus.standardNavigationHistory.v1';
const MATCHING = 'literalSubstring';
const sha = value => crypto.createHash('sha256').update(value).digest('hex');
function check(value, message, code = 'standard_history_failed') {
  if (!value) throw Object.assign(new Error(message), { code });
}
function pageURL(value) {
  let url;
  try { url = new URL(value); } catch { return null; }
  if (!['http:', 'https:'].includes(url.protocol)) return null;
  url.username = ''; url.password = '';
  check(url.href.length <= 16384, 'Committed history URL exceeds the recording bound');
  return url.href;
}
function validVisit(row) {
  const visit = row.value;
  check(visit && visit.id === row.id && visit.observedAtMs === row.occurredAt && Number.isSafeInteger(visit.observedAtMs)
    && visit.observedAtMs >= 0 && Number.isFinite(new Date(visit.observedAtMs).valueOf())
    && typeof visit.url === 'string' && pageURL(visit.url) === visit.url && typeof visit.title === 'string'
    && visit.title.length <= 4096 && ['did-navigate', 'did-navigate-in-page'].includes(visit.event), 'Invalid standard navigation history record');
  return visit;
}
async function openStandardNavigationHistorySource({ hostRoot, sourceRef, payloadBytes, payloadDigest, purpose,
  revalidate, revalidateNow, validatePage, onError, maxEntries = 5000, maxBytes = 16 * 1024 * 1024 }) {
  check(purpose === 'collection' || purpose === 'read', 'Standard history requires an explicit opening purpose');
  const definition = validateSource(sourceRef, payloadBytes, payloadDigest), reference = structuredClone(sourceRef);
  check(typeof revalidate === 'function', 'Standard history requires its current-source gate');
  if (purpose === 'collection') check([revalidateNow, validatePage, onError].every(value => typeof value === 'function'), 'Collection requires live source/page gates and an error reporter');
  await revalidate();
  const parent = childDirectory(hostRoot, 'standard-navigation-history', purpose === 'collection');
  const directory = childDirectory(parent, definition.source_id, purpose === 'collection');
  const storage = openStandardHistoryStorage({ directory, create: purpose === 'collection', maxEntries, maxBytes,
    identity: { api: API, sourceRef: reference, payloadDigest } });
  const pages = new Map(), epoch = crypto.randomUUID();
  let closed = false, failure = null;
  function currentNow() { check(!closed, 'Standard history source is closed'); if (failure) throw failure; }
  async function current() { currentNow(); await revalidate(); currentNow(); }
  function detach(page) {
    const entry = pages.get(page);
    if (!entry) return false;
    for (const [name, listener] of entry.listeners) page.removeListener(name, listener);
    pages.delete(page); return true;
  }
  function close() {
    if (closed) return;
    closed = true;
    for (const page of pages.keys()) detach(page);
    storage.close();
  }
  try { storage.rows().forEach(validVisit); await current(); } catch (error) { close(); throw error; }
  function observeReady(page) {
    currentNow(); check(purpose === 'collection', 'A history reader cannot enroll pages'); revalidateNow(); validatePage(page);
    check(page && !page.isDestroyed() && typeof page.on === 'function' && typeof page.getURL === 'function', 'History collection requires a live admitted WebContents');
    if (pages.has(page)) return;
    const entry = { visit: null, listeners: [] };
    const guard = operation => (...args) => {
      if (closed || !pages.has(page) || failure) return;
      try { revalidateNow(); validatePage(page); operation(...args); }
      catch (error) { failure = error; for (const observed of pages.keys()) detach(observed); onError(error); }
    };
    const committed = (raw, event) => {
      const url = pageURL(raw);
      entry.visit = null;
      if (!url) return;
      const visit = { id: crypto.randomUUID(), url, title: '', observedAtMs: Date.now(), event };
      storage.put(visit.id, visit.observedAtMs, visit); entry.visit = visit;
    };
    entry.listeners = [
      ['did-navigate', guard((_event, url) => committed(url, 'did-navigate'))],
      ['did-navigate-in-page', guard((_event, url, isMainFrame) => { if (isMainFrame) committed(url, 'did-navigate-in-page'); })],
      ['page-title-updated', guard((_event, title) => {
        const visit = entry.visit;
        if (!visit || pageURL(page.getURL()) !== visit.url) return;
        check(typeof title === 'string' && title.length <= 4096, 'History title exceeds the recording bound');
        if (storage.rows().some(row => row.id === visit.id)) { visit.title = title; storage.put(visit.id, visit.observedAtMs, visit); }
      })],
      ['destroyed', () => detach(page)]
    ];
    pages.set(page, entry);
    try { for (const [event, listener] of entry.listeners) page.on(event, listener); revalidateNow(); validatePage(page); }
    catch (error) { detach(page); throw error; }
  }
  async function search(query, { human = false, signal } = {}) {
    await current();
    check(!signal?.aborted, 'Standard history query was cancelled');
    check(human || definition.query_policy.model_read_enabled, 'Model history reads are not selected for this source');
    let input = query;
    if (human) {
      check(query?.matching_algorithm === MATCHING && query.deduplicate_by_day === true,
        'Standard event history supports explicit literalSubstring matching with UTC-day deduplication; Chromium alwaysPrefix is unavailable', 'engine_capability_unavailable');
      const { matching_algorithm, deduplicate_by_day, ...rest } = query; input = rest;
    }
    const normalized = normalizeQuery(input, definition.query_policy, false), started = Date.now();
    const from = normalized.native.startTime ?? 0, to = normalized.native.endTime ?? Infinity;
    check(from <= to, 'History query time range is reversed');
    const terms = normalized.queries.map(value => value.toLowerCase()), seen = new Set();
    const visits = storage.rows().map(validVisit);
    const items = [];
    for (const visit of visits) {
      if (visit.observedAtMs < from || visit.observedAtMs >= to || !terms.some(term => visit.url.toLowerCase().includes(term) || visit.title.toLowerCase().includes(term))) continue;
      const dateVisited = new Date(visit.observedAtMs).toISOString(), key = human ? `${dateVisited.slice(0, 10)}\0${visit.url}` : visit.url;
      if (seen.has(key)) continue;
      seen.add(key); items.push({ url: visit.url, title: visit.title, dateVisited });
      if (items.length === normalized.limit) break;
    }
    const result = { source_ref: structuredClone(reference), source_payload_digest: payloadDigest, source_epoch: epoch, items,
      standard_query_evidence: { api: API, request_id: crypto.randomUUID(), query_digest: sha(canonical(query)),
        matching_algorithm: MATCHING, timestamp: 'main_process_event_observation', day_timezone: human ? 'UTC' : null,
        retention: { max_entries: maxEntries, max_bytes: maxBytes } } };
    check(Buffer.byteLength(JSON.stringify(result)) <= definition.query_policy.max_result_bytes, 'Standard history result exceeds source byte bound');
    await current();
    check(!signal?.aborted && Date.now() - started <= definition.query_policy.request_timeout_ms, 'Standard history query was cancelled or exceeded its deadline');
    return result;
  }
  return Object.freeze({ api: API, sourceRef: structuredClone(reference), payloadDigest, sourceEpoch: epoch,
    definition: structuredClone(definition), search, observeReady,
    observe: async page => { await current(); observeReady(page); },
    unobserve: page => { currentNow(); check(purpose === 'collection', 'A reader cannot change collection'); return detach(page); },
    flush: () => { currentNow(); storage.flush(); }, close });
}

module.exports = { openStandardNavigationHistorySource, API, MATCHING };
