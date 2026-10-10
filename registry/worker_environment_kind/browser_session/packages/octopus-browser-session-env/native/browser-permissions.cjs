'use strict';

const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value && typeof value === 'object'
  ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value);
const hash = value => crypto.createHash('sha256').update(canonical(value)).digest('hex');
function assert(ok, message) { if (!ok) throw Object.assign(new Error(message), { code: 'browser_permission_invalid' }); }
function exact(value, fields) { assert(value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length === fields.length && fields.every(key => Object.hasOwn(value, key)), 'Browser permission fields differ'); }
function text(value) { assert(typeof value === 'string' && value.length > 0 && Buffer.byteLength(value) <= 4096 && value.trim() === value && !value.includes('\0'), 'Browser permission identity is invalid'); }
function reference(value) { exact(value, ['id', 'content']); text(value.id); assert(Array.isArray(value.content) && value.content.length === 32 && value.content.every(byte => Number.isInteger(byte) && byte >= 0 && byte <= 255), 'Browser permission artifact identity is invalid'); }
function offer(request) {
  const value = request.permission_offer; exact(value, ['client_interface', 'offered_choices']); text(value.client_interface);
  assert(Array.isArray(value.offered_choices) && value.offered_choices.length > 0 && value.offered_choices.length <= 6, 'Browser permission offer bound is invalid');
  const seen = new Set();
  for (const choice of value.offered_choices) {
    exact(choice, ['decision', 'persistence']);
    assert(['accept', 'decline'].includes(choice.decision) && ['once', 'session', 'always'].includes(choice.persistence), 'Unsupported browser permission choice');
    assert(request.approval_scope !== 'browser_history_read' || choice.persistence !== 'session', 'History permission cannot use session persistence');
    const key = canonical(choice); assert(!seen.has(key), 'Duplicate browser permission choice'); seen.add(key);
  }
  return value;
}
function subject(request) {
  if (request.approval_scope === 'browser_file_upload') {
    const origin = request.upload.top_level_origin, parsed = new URL(origin);
    assert(['http:', 'https:'].includes(parsed.protocol) && parsed.origin === origin, 'Browser upload origin is not canonical');
    return { type: 'upload', origin };
  }
  assert(request.approval_scope === 'browser_history_read', 'Unknown browser permission subject');
  reference(request.history.source);
  assert(/^[a-f0-9]{64}$/.test(request.history.source_payload_digest), 'History source digest is invalid');
  return { type: 'history', source: request.history.source, source_payload_digest: request.history.source_payload_digest };
}
function session(request) { text(request.original.run); return { run: request.original.run, client_interface: offer(request).client_interface }; }
function response(request, value) {
  exact(value, value.decision === 'cancel' ? ['request_id', 'call_id', 'decision'] : ['request_id', 'call_id', 'decision', 'selected_persistence']);
  assert(value.request_id === request.request_id && value.call_id === request.call_id, 'Browser permission response lost its original request');
  if (value.decision === 'cancel') return;
  assert(offer(request).offered_choices.some(choice => choice.decision === value.decision && choice.persistence === value.selected_persistence), 'Browser permission response selected an unoffered choice');
}

function createBrowserPermissions({ hostRoot, maxBytes, maxRecords }) {
  const directory = path.join(hostRoot, 'user-permissions'), file = path.join(directory, 'preferences.json');
  let writing = Promise.resolve();
  assert(Number.isSafeInteger(maxBytes) && maxBytes > 0 && Number.isSafeInteger(maxRecords) && maxRecords > 0, 'Browser preference bounds are absent');
  async function root() {
    const owner = await fs.lstat(hostRoot);
    assert(owner.isDirectory() && !owner.isSymbolicLink() && owner.uid === process.getuid() && (owner.mode & 0o777) === 0o700 && await fs.realpath(hostRoot) === hostRoot, 'Browser host root changed');
    await fs.mkdir(directory, { mode: 0o700 }).catch(error => { if (error.code !== 'EEXIST') throw error; });
    const stat = await fs.lstat(directory);
    assert(stat.isDirectory() && !stat.isSymbolicLink() && stat.uid === process.getuid() && (stat.mode & 0o777) === 0o700 && await fs.realpath(directory) === directory, 'Browser preference owner directory changed');
  }
  function validate(record) {
    exact(record, ['id', 'subject', 'session', 'response', 'source_artifact', 'source_movement_seq']);
    reference(record.source_artifact);
    assert(Number.isSafeInteger(record.source_movement_seq) && record.source_movement_seq > 0, 'Browser decision has no admitted movement');
    const value = { ...record }; delete value.id; assert(record.id === hash(value), 'Browser preference record changed');
    assert(['accept', 'decline'].includes(record.response.decision) && ['session', 'always'].includes(record.response.selected_persistence), 'Browser preference is not a saved decision');
    exact(record.response, ['request_id', 'call_id', 'decision', 'selected_persistence']);
    text(record.response.request_id); text(record.response.call_id);
    if (record.response.selected_persistence === 'session') { exact(record.session, ['run', 'client_interface']); text(record.session.run); text(record.session.client_interface); }
    else assert(record.session === null, 'Always choice cannot carry session scope');
    if (record.subject.type === 'upload') { exact(record.subject, ['type', 'origin']); const parsed = new URL(record.subject.origin); assert(['http:', 'https:'].includes(parsed.protocol) && parsed.origin === record.subject.origin, 'Saved upload origin changed'); }
    else { exact(record.subject, ['type', 'source', 'source_payload_digest']); assert(record.subject.type === 'history' && record.response.selected_persistence === 'always' && /^[a-f0-9]{64}$/.test(record.subject.source_payload_digest), 'Saved history subject changed'); reference(record.subject.source); }
  }
  async function read() {
    await root();
    let handle;
    try { handle = await fs.open(file, constants.O_RDONLY | constants.O_NOFOLLOW); }
    catch (error) { if (error.code === 'ENOENT') return []; throw error; }
    try {
      const stat = await handle.stat(); assert(stat.isFile() && stat.uid === process.getuid() && stat.nlink === 1 && (stat.mode & 0o777) === 0o600 && stat.size <= maxBytes, 'Browser preference file ownership or bound changed');
      const bytes = await handle.readFile(); assert(bytes.length <= maxBytes, 'Browser preference file exceeds bound');
      const value = JSON.parse(bytes); exact(value, ['schema_version', 'records']);
      assert(value.schema_version === 1 && Array.isArray(value.records) && value.records.length <= maxRecords, 'Browser preference format is invalid');
      const keys = new Set(); for (const record of value.records) { validate(record); const key = canonical([record.subject, record.session]); assert(!keys.has(key), 'Duplicate browser permission subject/scope'); keys.add(key); }
      return value.records;
    } finally { await handle.close(); }
  }
  async function write(records, revalidate) {
    const bytes = Buffer.from(JSON.stringify({ schema_version: 1, records })); assert(bytes.length <= maxBytes && records.length <= maxRecords, 'Browser preference storage is full');
    const temporary = path.join(directory, `${crypto.randomUUID()}.tmp`);
    const handle = await fs.open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    try {
      try { await handle.writeFile(bytes); await handle.sync(); } finally { await handle.close(); }
      if (revalidate) await revalidate();
      await fs.rename(temporary, file); const owner = await fs.open(directory, constants.O_RDONLY); try { await owner.sync(); } finally { await owner.close(); }
    }
    finally { await fs.unlink(temporary).catch(error => { if (error.code !== 'ENOENT') throw error; }); }
  }
  async function lookup(request) {
    const offered = offer(request), key = canonical(subject(request)), coordinate = canonical(session(request));
    const records = (await read()).filter(record => canonical(record.subject) === key && (record.session === null || canonical(record.session) === coordinate));
    records.sort((a, b) => Number(b.session !== null) - Number(a.session !== null));
    const denied = records.find(record => record.response.decision === 'decline');
    if (denied) throw Object.assign(new Error('Saved browser permission denies this request'), { code: 'browser_permission_denied', permission_evidence: denied });
    return records.find(record => offered.offered_choices.some(choice => choice.decision === record.response.decision && choice.persistence === record.response.selected_persistence)) ?? null;
  }
  async function remember(request, body, params) {
    response(request, params.response); reference(params.source_artifact);
    assert(Buffer.from(params.source_artifact.content).toString('hex') === body.artifact_content_hash, 'Remembered response differs from authenticated admitted artifact');
    if (params.response.decision === 'cancel' || params.response.selected_persistence === 'once') return { status: 'not_saved' };
    const value = { subject: subject(request), session: params.response.selected_persistence === 'session' ? session(request) : null,
      response: structuredClone(params.response), source_artifact: structuredClone(params.source_artifact), source_movement_seq: body.source_movement_seq };
    const record = { id: hash(value), ...value }; validate(record);
    const action = writing.then(async () => { const records = (await read()).filter(item => canonical([item.subject, item.session]) !== canonical([record.subject, record.session])); records.push(record); await write(records); return { status: 'saved', evidence: record }; });
    writing = action.then(() => {}, () => {}); return action;
  }
  async function requireSaved(request, id, decision) { const record = await lookup(request); assert(record && record.id === id && record.response.decision === decision, 'Saved permission changed before native dispatch'); return record; }
  function settingsEntries(records) {
    return records.filter(record => record.response.selected_persistence === 'always').map(record => ({
      id: record.id, subject: structuredClone(record.subject), decision: record.response.decision,
    }));
  }
  async function list(revalidate) {
    assert(typeof revalidate === 'function', 'Browser settings require their current client owner');
    await revalidate(); const records = await read(); await revalidate(); return { entries: settingsEntries(records) };
  }
  async function remove(id, revalidate) {
    assert(typeof id === 'string' && /^[a-f0-9]{64}$/.test(id) && typeof revalidate === 'function', 'Browser settings removal requires its exact saved entry and client owner');
    const action = writing.then(async () => {
      await revalidate(); const records = await read(); await revalidate();
      const index = records.findIndex(record => record.id === id && record.response.selected_persistence === 'always');
      if (index < 0) return { removed: false, entries: settingsEntries(records) };
      records.splice(index, 1); await write(records, revalidate); await revalidate();
      return { removed: true, entries: settingsEntries(records) };
    });
    writing = action.then(() => {}, () => {}); return action;
  }
  return { lookup, remember, requireSaved, list, remove };
}
module.exports = { createBrowserPermissions, validatePermissionOffer: offer };
