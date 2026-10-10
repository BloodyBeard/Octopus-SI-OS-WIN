'use strict';

const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const CHUNK_BYTES = 8 * 1024 * 1024;
const TERMINAL = new Set(['completed', 'cancelled', 'interrupted', 'failed', 'expired', 'imported']);
const METHODS = { download_arm: ['OctopusBrowser.armDownload'], download_wait: ['OctopusBrowser.waitDownload', 'OctopusBrowser.readDownload', 'OctopusBrowser.finishDownload'], download_cancel: ['OctopusBrowser.cancelDownload'] };
function check(condition, message) { if (!condition) throw Object.assign(new Error(message), { code: 'download_contract_failed' }); }
function fields(value, required, optional = []) {
  check(value && typeof value === 'object' && !Array.isArray(value) && required.every(key => Object.hasOwn(value, key)) && Object.keys(value).every(key => required.includes(key) || optional.includes(key)), 'Invalid browser download fields');
}
function positive(value, max = Number.MAX_SAFE_INTEGER) { check(Number.isSafeInteger(value) && value > 0 && value <= max, 'Invalid browser download bound'); return value; }
function header(response, name) { return response.responseHeaders?.find(entry => entry.name.toLowerCase() === name)?.value; }
function attachment(response) { return /^attachment(?:\s*;|\s*$)/i.test(header(response, 'content-disposition') ?? ''); }
function embedded(response) { const type = (header(response, 'content-type') ?? '').split(';')[0].trim().toLowerCase(); return type === 'application/pdf' || type.startsWith('image/') || type.startsWith('video/'); }
function downloadable(response, armed) {
  if (response.resourceType !== 'Document' || [301, 302, 303, 307, 308].includes(response.responseStatusCode) && header(response, 'location')) return false;
  const html = (header(response, 'content-type') ?? '').split(';')[0].trim().toLowerCase() === 'text/html';
  return attachment(response) || armed && embedded(response) || !html && !embedded(response);
}
function overrides(response) {
  if (attachment(response) || !embedded(response)) return {};
  const headers = response.responseHeaders.map(entry => ({ ...entry }));
  const disposition = headers.find(entry => entry.name.toLowerCase() === 'content-disposition');
  if (disposition) disposition.value = disposition.value.replace(/^[^;]*/, 'attachment');
  else headers.push({ name: 'Content-Disposition', value: 'attachment' });
  return { responseCode: response.responseStatusCode, ...(response.responseStatusText ? { responsePhrase: response.responseStatusText } : {}), responseHeaders: headers };
}
function origin(body) { return { source_movement_seq: body.source_movement_seq, artifact_content_hash: body.artifact_content_hash, invocation_nonce: body.invocation_nonce, tool: body.tool.id, tool_descriptor_hash: body.formation.tool_descriptor_hashes[body.tool.id], formation_pin: body.formation.formation_pin }; }

function createBrowserDownloads({ hostRoot, hostGeneration, requireTarget, emit, humanDownloads, isModelActive }) {
  check(typeof isModelActive === 'function', 'Downloads require the exact native page control state');
  const records = new Map();
  const manual = new Map();
  const setup = new WeakMap();
  function current(target) { return requireTarget(target.room, target.id); }
  function retained(target) {
    const record = records.get(target.id);
    check(record?.target === target && target.room.active, 'Download owner is no longer admitted in this room');
    if (!record.item) current(target);
    return target;
  }
  function view(record) { return { download_id: record.id, target_id: record.target.id, browser_context_id: record.target.room.contextId, host_generation: hostGeneration(), status: record.status, max_bytes: record.maxBytes, filename: record.filename ?? null, mime_type: record.mime ?? null, url: record.url ?? null, url_chain: record.urlChain ?? [], bytes: record.bytes ?? 0, sha256: record.hash ?? null, arm: record.arm, trigger: record.trigger ?? null, response: record.response ?? null, native_event: record.nativeEvent ?? null, error: record.error ?? null }; }
  function publish(record) { try { if (humanDownloads) humanDownloads.observeModel(record.target.room, view(record), record.item); } catch (error) { emit({ type: 'download-history-error', error: { code: 'download_history_unavailable', message: error.message } }); } for (const wake of record.waiters) wake(); record.waiters.clear(); emit({ type: 'downloads', workerEnvironmentId: record.target.room.formation.worker_environment_id, targetId: record.target.id, download: view(record) }); }
  function stop(record, status, message) {
    if (TERMINAL.has(record.status)) return;
    clearTimeout(record.timer); clearTimeout(record.allowTimer); record.status = status; record.error = message;
    if (record.item && !record.nativeDone) record.item.cancel();
    publish(record);
  }
  async function ready(target) {
    current(target);
    const old = setup.get(target.contents);
    if (old?.epoch === target.debuggerEpoch) return old.promise;
    const entry = { epoch: target.debuggerEpoch, promise: target.contents.debugger.sendCommand('Fetch.enable', { patterns: [{ requestStage: 'Response', resourceType: 'Document' }] }) };
    setup.set(target.contents, entry);
    try { await entry.promise; current(target); check(target.debuggerEpoch === entry.epoch, 'Browser debugger changed during download setup'); }
    catch (error) { if (setup.get(target.contents) === entry) setup.delete(target.contents); throw error; }
  }
  function observe(target, body, method) {
    if (['navigate', 'click', 'type', 'key', 'scroll', 'back', 'forward', 'reload', 'eval', 'hover', 'select', 'drag', 'locator_fill', 'locator_press', 'locator_select', 'locator_check', 'dom_click', 'dom_scroll', 'paste', 'media_download'].includes(body.tool.operation) && ['Input.dispatchMouseEvent', 'Input.dispatchKeyEvent', 'Input.insertText', 'Page.navigate', 'Page.navigateToHistoryEntry', 'Page.reload', 'Runtime.evaluate', 'Runtime.callFunctionOn'].includes(method)) {
      const record = records.get(target.id);
      if (record?.status === 'armed' && body.source_movement_seq > record.arm.source_movement_seq) record.trigger = origin(body);
    }
  }
  async function response(target, params) {
    current(target);
    const send = (method, data) => target.contents.debugger.sendCommand(method, { requestId: params.requestId, ...data });
    if (params.responseErrorReason != null) { await send('Fetch.continueRequest', {}); return; }
    const record = records.get(target.id);
    const armed = record?.status === 'armed' && record.trigger && record.trigger.source_movement_seq > record.arm.source_movement_seq;
    if (!downloadable(params, !!armed)) { await send('Fetch.continueResponse', {}); return; }
    if (!armed) {
      if (!isModelActive(target)) await send('Fetch.continueResponse', {});
      else await send('Fetch.failRequest', { errorReason: 'BlockedByClient' });
      return;
    }
    check(typeof params.requestId === 'string' && typeof params.request?.url === 'string', 'Download response has no native request identity');
    clearTimeout(record.timer);
    record.response = { request_id: params.requestId, url: params.request.url, status_code: params.responseStatusCode };
    record.url = params.request.url; record.status = 'allowed'; record.allowUntil = Date.now() + 10000;
    record.allowTimer = setTimeout(() => stop(record, 'expired', 'Native download did not start during its one-use response grant'), 10000);
    publish(record);
    try { await send('Fetch.continueResponse', overrides(params)); }
    catch (error) { stop(record, 'failed', `Download response continuation is uncertain: ${error.message}`); throw error; }
  }
  function event(target, method, params, sessionId) {
    if (method !== 'Fetch.requestPaused' || sessionId != null || params.resourceType !== 'Document' || params.responseStatusCode == null && params.responseErrorReason == null) return false;
    void response(target, params).catch(error => { const record = records.get(target.id); if (record) stop(record, 'failed', error.message); emit({ type: 'download-error', targetId: target.id, error: error.message }); });
    return true;
  }
  async function file(record) {
    const handle = await fs.open(record.path, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const stat = await handle.stat();
      check(stat.isFile() && stat.nlink === 1 && stat.uid === process.getuid() && stat.size <= record.maxBytes, 'Native download file does not satisfy its admitted bound');
      return { handle, stat };
    } catch (error) { await handle.close(); throw error; }
  }
  function willDownload(room, event, item, contents) {
    const target = [...room.targets.values()].find(target => target.contents === contents);
    if (!target || !room.active) { event.preventDefault(); return; }
    const record = records.get(target.id);
    const urls = item.getURLChain();
    const allowed = record?.status === 'allowed' && record.allowUntil >= Date.now() && record.response && (item.getURL() === record.response.url || urls.includes(record.response.url));
    if (!allowed) {
      if (isModelActive(target)) { event.preventDefault(); item.cancel(); }
      else {
        try {
          if (humanDownloads) humanDownloads.start(room, target, item);
          else {
            if (!manual.has(room)) manual.set(room, new Set());
            manual.get(room).add(item);
            const ended = () => { manual.get(room)?.delete(item); if (!manual.get(room)?.size) manual.delete(room); };
            item.once('done', ended); item.once('destroyed', ended);
          }
          emit({ type: 'human-download', workerEnvironmentId: room.formation.worker_environment_id, targetId: target.id, filename: item.getFilename(), url: item.getURL(), status: 'started' });
        }
        catch (error) { event.preventDefault(); emit({ type: 'download-error', workerEnvironmentId: room.formation.worker_environment_id, targetId: target.id, error: error.message }); }
      }
      return;
    }
    clearTimeout(record.allowTimer);
    record.status = 'progressing'; record.item = item; record.filename = item.getFilename(); record.mime = item.getMimeType(); record.url = item.getURL(); record.urlChain = urls;
    record.nativeEvent = { web_contents_id: contents.id, start_time: item.getStartTime(), state: 'progressing' };
    item.setSavePath(record.path);
    item.once('destroyed', () => {
      if (record.nativeDone) return;
      record.nativeDone = true;
      stop(record, 'cancelled', 'Native download item was destroyed before completion');
    });
    item.on('updated', (_event, state) => {
      record.bytes = item.getReceivedBytes();
      if (record.bytes > record.maxBytes || item.getTotalBytes() > record.maxBytes) stop(record, 'failed', 'Download exceeds the explicitly admitted max_bytes');
      else if (!TERMINAL.has(record.status) && record.status !== 'cancelling') {
        if (state === 'interrupted' && !item.canResume()) { record.status = 'failed'; record.error = 'Native download was interrupted and cannot resume'; }
        else record.status = state === 'interrupted' || item.isPaused() ? 'paused' : 'progressing';
        publish(record);
      }
    });
    item.once('done', (_event, state) => {
      record.nativeDone = true; record.nativeEvent = { ...record.nativeEvent, state, end_time: item.getEndTime() }; record.bytes = item.getReceivedBytes();
      if (state !== 'completed') { if (!TERMINAL.has(record.status)) record.status = state; publish(record); return; }
      if (record.status === 'failed' || record.status === 'cancelled') { publish(record); return; }
      record.status = 'verifying';
      void (async () => {
        const opened = await file(record);
        try {
          check(record.bytes === opened.stat.size, 'Native terminal byte count differs from the saved download');
          const hash = crypto.createHash('sha256');
          for await (const bytes of opened.handle.createReadStream({ autoClose: false })) hash.update(bytes);
          check(records.get(target.id) === record && room.active, 'Download owner withdrew before terminal verification');
          record.hash = hash.digest('hex'); record.status = 'completed'; publish(record);
        } finally { await opened.handle.close(); }
      })().catch(error => stop(record, 'failed', error.message));
    });
    if (item.getTotalBytes() > record.maxBytes) stop(record, 'failed', 'Download exceeds the explicitly admitted max_bytes');
    else publish(record);
  }
  async function command(target, body, method, params, signal) {
    if (method === 'OctopusBrowser.armDownload') current(target); else retained(target);
    check(METHODS[body.tool.operation]?.includes(method), 'Download method is outside the signed tool operation');
    if (method === 'OctopusBrowser.armDownload') {
      fields(body.arguments, ['max_bytes', 'timeout_ms'], ['target_id']); fields(params, ['max_bytes', 'timeout_ms']);
      check(params.max_bytes === body.arguments.max_bytes && params.timeout_ms === body.arguments.timeout_ms, 'Download arm differs from signed arguments');
      positive(params.max_bytes); positive(params.timeout_ms, 300000);
      const previous = records.get(target.id);
      check(!previous || ['imported', 'cancelled', 'interrupted', 'failed', 'expired'].includes(previous.status), 'This target already owns an uncollected or pending download');
      if (previous?.path) await fs.unlink(previous.path).catch(error => { if (error.code !== 'ENOENT') throw error; });
      const root = path.join(hostRoot, 'downloads', hostGeneration(), target.room.handle);
      await fs.mkdir(root, { recursive: true, mode: 0o700 });
      for (const name of [path.join(hostRoot, 'downloads'), path.dirname(root), root]) {
        const stat = await fs.lstat(name); check(stat.isDirectory() && !stat.isSymbolicLink() && stat.uid === process.getuid() && (stat.mode & 0o777) === 0o700, 'Download storage is not private to the native browser owner');
      }
      const id = crypto.randomBytes(16).toString('hex'), filename = path.join(root, id);
      const handle = await fs.open(filename, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600); await handle.close();
      const record = { id, target, path: filename, maxBytes: params.max_bytes, status: 'armed', arm: origin(body), waiters: new Set() };
      record.timer = setTimeout(() => stop(record, 'expired', 'No matching download response arrived before the declared timeout'), params.timeout_ms);
      records.set(target.id, record); publish(record); return view(record);
    }
    fields(body.arguments, ['download_id'], body.tool.operation === 'download_wait' ? ['timeout_ms'] : []);
    const record = records.get(target.id);
    check(record && record.id === body.arguments.download_id && params.download_id === record.id && body.source_movement_seq > record.arm.source_movement_seq, 'Download does not belong to this target and admitted invocation');
    if (method === 'OctopusBrowser.cancelDownload') {
      fields(params, ['download_id']);
      if (!TERMINAL.has(record.status) && record.item && !record.nativeDone) {
        clearTimeout(record.timer); clearTimeout(record.allowTimer); record.status = 'cancelling'; record.item.cancel(); publish(record);
      } else stop(record, 'cancelled', 'Download arm cancelled explicitly');
      return view(record);
    }
    if (method === 'OctopusBrowser.waitDownload') {
      fields(params, ['download_id', 'timeout_ms']); positive(params.timeout_ms, 30000);
      const requested = body.arguments.timeout_ms ?? 1000; positive(requested, 30000);
      check(params.timeout_ms <= requested, 'Download wait exceeds signed arguments');
      if (!TERMINAL.has(record.status)) await new Promise((resolve, reject) => {
        let timer;
        const cleanup = () => { clearTimeout(timer); record.waiters.delete(done); signal?.removeEventListener('abort', abort); };
        const done = () => { cleanup(); resolve(); };
        const abort = () => { cleanup(); reject(Object.assign(new Error('Download observation connection ended; download outcome must be observed again'), { code: 'execution_uncertain' })); };
        if (signal?.aborted) { abort(); return; }
        signal?.addEventListener('abort', abort, { once: true }); record.waiters.add(done); timer = setTimeout(done, params.timeout_ms);
      });
      retained(target); return view(record);
    }
    check(record.status === 'completed', 'Only a native completed download can be imported');
    if (method === 'OctopusBrowser.readDownload') {
      fields(params, ['download_id', 'offset', 'length']);
      check(Number.isSafeInteger(params.offset) && params.offset >= 0 && params.offset <= record.bytes, 'Invalid download offset'); positive(params.length, CHUNK_BYTES);
      const opened = await file(record);
      try {
        check(opened.stat.size === record.bytes, 'Completed download size changed');
        const bytes = Buffer.alloc(Math.min(params.length, record.bytes - params.offset));
        const read = await opened.handle.read(bytes, 0, bytes.length, params.offset);
        check(read.bytesRead === bytes.length, 'Completed download read was incomplete');
        return { download_id: record.id, target_id: target.id, browser_context_id: target.room.contextId, offset: params.offset, bytes: bytes.length, payload_base64: bytes.toString('base64'), sha256: record.hash };
      } finally { await opened.handle.close(); }
    }
    fields(params, ['download_id', 'sha256']); check(params.sha256 === record.hash, 'Imported download does not match native completed bytes');
    await fs.unlink(record.path); record.status = 'imported'; publish(record); return view(record);
  }
  function targetFor(room, id) {
    check(typeof id === 'string' && /^[a-f0-9]{32}$/.test(id), 'Invalid native download identity');
    const record = [...records.values()].find(record => record.id === id && record.target.room === room);
    check(record && room.active, 'Download is not retained in this exact room');
    return retained(record.target);
  }
  function discard(record, reason) {
    stop(record, 'cancelled', reason); clearTimeout(record.timer); clearTimeout(record.allowTimer); records.delete(record.target.id);
    const unlink = () => { void fs.unlink(record.path).catch(() => {}); };
    if (record.item && !record.nativeDone) { record.item.once('done', unlink); record.item.once('destroyed', unlink); }
    else unlink();
  }
  function release(target, reason) {
    const record = records.get(target.id); if (!record) return;
    if (record.item && target.room.active) return;
    discard(record, reason);
  }
  function releaseRoom(room, reason) {
    for (const item of manual.get(room) ?? []) item.cancel();
    manual.delete(room);
    for (const record of records.values()) if (record.target.room === room) discard(record, reason);
  }
  return { ready, observe, event, willDownload, command, targetFor, release, releaseRoom, list: target => { const value = records.get(target.id); return value ? [view(value)] : []; }, owns: method => Object.values(METHODS).some(methods => methods.includes(method)) };
}
module.exports = { createBrowserDownloads };
