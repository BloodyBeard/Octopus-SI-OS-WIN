'use strict';

const crypto = require('node:crypto');
const path = require('node:path');
const { openStandardHistoryStorage } = require('./standard-history-storage.cjs');
const API = 'octopus.standardDownloadHistory.v1';
const UUID = /^[a-f0-9]{8}(-[a-f0-9]{4}){3}-[a-f0-9]{12}$/;
const STATES = new Set(['progressing', 'completed', 'cancelled', 'interrupted']);
const scope = owner => JSON.stringify([owner.workerEnvironmentId, owner.formationPin]);
const roomOwner = room => ({ workerEnvironmentId: room.formation.worker_environment_id, formationPin: room.formation.formation_pin });
function check(value, message) {
  if (!value) throw Object.assign(new Error(message), { code: 'standard_download_history_failed' });
}
function metadataRecord(owner, id, metadata) {
  check(typeof metadata === 'string', 'Standard download metadata must be serialized');
  const value = JSON.parse(metadata), record = value?.record;
  check(value?.schema_version === 1 && Object.keys(value).length === 2 && record && UUID.test(id)
    && record.standardId === id && !Object.hasOwn(record, 'nativeGuid') && record.scope === scope(owner)
    && typeof record.targetId === 'string' && typeof record.filename === 'string' && typeof record.url === 'string'
    && typeof record.path === 'string' && (!record.path || path.isAbsolute(record.path))
    && typeof record.userInitiated === 'boolean' && typeof record.unacknowledged === 'boolean'
    && (record.userInitiated ? record.id === id : /^[a-f0-9]{32}$/.test(record.id)
      && record.modelReceipt?.download_id === record.id && record.modelReceipt?.target_id === record.targetId)
    && [record.startedAtMs, record.updatedAtMs, record.receivedBytes, record.totalBytes, record.sequence]
      .every(number => Number.isFinite(number) && number >= 0), 'Standard download metadata changed its scope or identity');
  return record;
}
function validRow(owner, row) {
  const observed = row?.standard;
  check(observed && UUID.test(observed.id) && STATES.has(observed.state)
    && typeof observed.terminal === 'boolean' && typeof observed.url === 'string' && typeof observed.filename === 'string'
    && Array.isArray(observed.urlChain) && observed.urlChain.every(url => typeof url === 'string')
    && typeof observed.savePath === 'string' && (!observed.savePath || path.isAbsolute(observed.savePath))
    && [observed.receivedBytes, observed.totalBytes, observed.startedAtMs, observed.observedAtMs].every(number => Number.isFinite(number) && number >= 0), 'Invalid stock DownloadItem observation');
  metadataRecord(owner, observed.id, row.metadata); return row;
}
function decode(owner, row) {
  validRow(owner, row);
  const observed = row.standard, record = metadataRecord(owner, observed.id, row.metadata);
  record.receivedBytes = observed.receivedBytes; record.totalBytes = observed.totalBytes;
  record.sequence = 0; record.unacknowledged = false;
  if (!observed.terminal) record.status = 'unavailable';
  else if (observed.state === 'cancelled') record.status = 'canceled';
  else if (observed.state === 'interrupted') record.status = 'failed';
  else if (observed.state === 'completed' && (record.userInitiated || ['completed', 'imported'].includes(record.modelReceipt?.status))) record.status = 'complete';
  else record.status = 'unavailable';
  if (record.userInitiated) record.path = observed.savePath;
  return record;
}
function createStandardDownloadHistory({ maxEntries = 2000, maxBytes = 16 * 1024 * 1024 } = {}) {
  const stores = new Map(), sessions = new WeakMap(), items = new WeakMap();
  function storeForRoom(room, requireActive = true) {
    const store = sessions.get(room.session);
    check((!requireActive || room.active) && store && !store.closed && store.scope === scope(roomOwner(room)), 'Download does not belong to the configured room owner');
    return store;
  }
  async function open({ directory, owner }) {
    check(typeof owner?.workerEnvironmentId === 'string' && owner.workerEnvironmentId.length > 0
      && /^[a-f0-9]{64}$/.test(owner.formationPin), 'Standard downloads require an exact formed owner');
    check(!stores.has(directory), 'Standard download storage is already open');
    const storage = openStandardHistoryStorage({ directory, create: true, maxEntries, maxBytes, identity: { api: API, owner } });
    const store = { storage, owner: structuredClone(owner), scope: scope(owner), closed: false, live: new Map() };
    const rows = () => storage.rows().map(row => {
      validRow(store.owner, row.value);
      check(row.id === row.value.standard.id && row.occurredAt === row.value.standard.startedAtMs, 'Standard download record key differs');
      return row.value;
    });
    try { rows(); } catch (error) { storage.close(); throw error; }
    stores.set(directory, store);
    const existing = id => { const row = rows().find(row => row.standard.id === id); check(row, 'Standard download record does not exist'); return row; };
    return Object.freeze({
      api: API,
      async search({ text, offset, maxResults }) {
        check(typeof text === 'string' && Buffer.byteLength(text) <= maxBytes && Number.isSafeInteger(offset) && offset >= 0
          && Number.isSafeInteger(maxResults) && maxResults >= 0, 'Invalid standard download history query');
        const term = text.toLowerCase(), matches = rows().filter(row => {
          const value = row.standard;
          return [value.filename, value.savePath, value.url, ...value.urlChain].some(text => text.toLowerCase().includes(term));
        });
        const records = matches.slice(offset, maxResults === 0 ? undefined : offset + maxResults);
        return { records, offset, text, hasMore: offset + records.length < matches.length, api: API, matchingAlgorithm: 'literalSubstring' };
      },
      async update(id, metadata) {
        const previous = existing(id), record = metadataRecord(store.owner, id, metadata);
        const original = metadataRecord(store.owner, id, previous.metadata);
        check(record.id === original.id && record.targetId === original.targetId && record.userInitiated === original.userInitiated,
          'Standard download metadata changed its original target');
        storage.put(id, previous.standard.startedAtMs, { ...previous, metadata }, new Set(store.live.keys()));
      },
      async remove(id) { check(!store.live.has(id), 'Active download cannot be removed'); existing(id); storage.remove([id]); },
      async clear() { storage.remove(rows().filter(row => !store.live.has(row.standard.id)).map(row => row.standard.id)); },
      async flush() { storage.flush(); },
      close() {
        if (store.closed) return;
        store.closed = true;
        for (const entry of store.live.values()) entry.detach();
        store.live.clear(); storage.close(); stores.delete(directory);
      }
    });
  }
  async function configure(room, directory) {
    const store = stores.get(directory);
    check(room.active && store && !store.closed && store.scope === scope(roomOwner(room)), 'Standard download storage belongs to another or revoked room');
    check(!sessions.has(room.session) || sessions.get(room.session) === store, 'A browser Session cannot change download-history owner');
    sessions.set(room.session, store);
  }
  function identify(room, item) {
    const store = storeForRoom(room), previous = items.get(item);
    if (previous) { check(previous.store === store, 'DownloadItem already belongs to another scope'); return previous.id; }
    check(item && ['getState', 'getURL', 'getURLChain', 'getSavePath', 'getStartTime', 'getReceivedBytes', 'getTotalBytes', 'getFilename', 'canResume', 'once'].every(name => typeof item[name] === 'function'), 'Stock DownloadItem API is unavailable');
    const id = crypto.randomUUID(), entry = { id, item, store, room, terminal: false, detach: null };
    const release = () => { entry.detach(); store.live.delete(id); };
    const done = () => { entry.terminal = true; queueMicrotask(release); };
    entry.detach = () => { item.removeListener('done', done); item.removeListener('destroyed', release); };
    items.set(item, entry); store.live.set(id, entry); item.once('done', done); item.once('destroyed', release);
    return id;
  }
  async function record(room, id, metadata) {
    const store = storeForRoom(room, false), entry = store.live.get(id);
    check(entry && entry.room === room, 'Standard download persistence requires its captured room and actual live DownloadItem');
    const item = entry.item, state = item.getState();
    const row = validRow(store.owner, { metadata, standard: { id, state,
      terminal: entry.terminal || state === 'completed' || state === 'cancelled' || state === 'interrupted' && !item.canResume(),
      filename: item.getFilename(), savePath: item.getSavePath(), url: item.getURL(), urlChain: item.getURLChain(),
      receivedBytes: item.getReceivedBytes(), totalBytes: Math.max(0, item.getTotalBytes()), startedAtMs: item.getStartTime() * 1000, observedAtMs: Date.now() } });
    const previous = store.storage.rows().find(row => row.id === id)?.value;
    if (previous) {
      const before = metadataRecord(store.owner, id, previous.metadata), next = metadataRecord(store.owner, id, metadata);
      check(before.id === next.id && before.targetId === next.targetId && before.userInitiated === next.userInitiated
        && previous.standard.startedAtMs === row.standard.startedAtMs, 'DownloadItem identity changed during recording');
    }
    store.storage.put(id, row.standard.startedAtMs, row, new Set(store.live.keys()));
  }
  return Object.freeze({ api: API, directoryName: 'standard-download-history', identityField: 'standardId',
    open, configure, identify, record, decode, validate() { check(stores.size === 0, 'Initialize the download backend before opening stores'); } });
}

module.exports = { createStandardDownloadHistory, API };
