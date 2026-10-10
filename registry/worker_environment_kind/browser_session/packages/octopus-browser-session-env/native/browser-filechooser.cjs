'use strict';

const { validatePermissionOffer } = require('./browser-permissions.cjs');

const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const QUOTA = 268435456;
const CHUNK = 1048576;
const METHODS = new Map([
  ['OctopusBrowser.armFileChooser', 'filechooser_arm'], ['OctopusBrowser.waitFileChooser', 'filechooser_wait'],
  ['OctopusBrowser.inspectFileChooser', 'filechooser_set_files'], ['OctopusBrowser.stageFileChooser', 'filechooser_set_files'],
  ['OctopusBrowser.writeFileChooser', 'filechooser_set_files'], ['OctopusBrowser.prepareFileChooser', 'filechooser_set_files'],
  ['OctopusBrowser.cancelFileChooser', 'filechooser_cancel'], ['OctopusBrowser.applyFileChooser', 'filechooser_resume'],
  ['OctopusBrowser.abandonFileChooser', 'filechooser_set_files'],
]);
function fail(message, code = 'file_chooser_invalid') { throw Object.assign(new Error(message), { code }); }
function exact(value, keys, required = keys) { if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(key => !keys.includes(key)) || required.some(key => !Object.hasOwn(value, key))) fail('File chooser fields differ from the declared operation'); return value; }
function id(value) { if (typeof value !== 'string' || !/^[a-f0-9]{32}$/.test(value)) fail('File chooser identity must be exact'); return value; }
function canonical(value) { return Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value && typeof value === 'object' ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value); }
function sha(value) { return crypto.createHash('sha256').update(value).digest('hex'); }
function digest(value) { if (typeof value !== 'string' || !/^[a-f0-9]{64}$/.test(value)) fail('File payload requires an exact SHA256'); return value; }
function filename(value) { if (typeof value !== 'string' || !value || ['.', '..'].includes(value) || /[\x00/\\]/.test(value) || Buffer.byteLength(value) > 255) fail('Upload filename must be one bounded basename'); return value; }
function ref(value) { exact(value, ['id', 'content']); if (typeof value.id !== 'string' || !value.id || value.id.includes('\0') || !Array.isArray(value.content) || value.content.length !== 32 || value.content.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) fail('Upload artifact reference is malformed'); }
async function directory(root, parts) {
  let current = root;
  for (const part of ['', ...parts]) {
    if (part) { current = path.join(current, part); try { await fs.mkdir(current, { mode: 0o700 }); } catch (error) { if (error.code !== 'EEXIST') throw error; } }
    const stat = await fs.lstat(current);
    if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o700 || await fs.realpath(current) !== current) fail('Upload storage must be canonical owned mode0700 directories');
  }
  return current;
}
function createBrowserFileChoosers({ hostRoot, hostGeneration, requireTarget, ensureDebugger, permissions }) {
  const records = new Map();
  const retained = new Map();
  function current(room, reservation) { const record = records.get(id(reservation)); if (!record || record.room !== room) fail('File chooser reservation is not owned by this room'); return record; }
  function live(record) { requireTarget(record.room, record.target.id); if (record.epoch !== record.target.debuggerEpoch || ['cancelled', 'failed'].includes(record.status)) fail(record.error || 'File chooser document or debugger is unavailable'); }
  async function call(record, method, params, operation) {
    operation.validate(); live(record); const epoch = record.target.debuggerEpoch; let timer; let abort;
    const result = await Promise.race([record.target.contents.debugger.sendCommand(method, params ?? {}), new Promise((_, reject) => {
      abort = () => reject(Object.assign(new Error('File chooser connection ended after dispatch; outcome is uncertain'), { code: 'execution_uncertain' }));
      operation.signal?.addEventListener('abort', abort, { once: true });
      timer = setTimeout(abort, 300000); if (operation.signal?.aborted) abort();
    })]).finally(() => { clearTimeout(timer); operation.signal?.removeEventListener('abort', abort); });
    operation.validate(); live(record); if (epoch !== record.target.debuggerEpoch) fail('File chooser debugger changed after dispatch', 'execution_uncertain'); return result;
  }
  function receipt(record) { return { reservation_id: record.id, file_chooser_id: record.chooserId ?? null, status: record.status, target_id: record.target.id, browser_context_id: record.room.contextId, host_generation: hostGeneration(), is_multiple: record.multiple ?? null, ...(record.error ? { error: record.error } : {}) }; }
  function changed(record) { for (const wake of record.waiters) wake(); record.waiters.clear(); }
  async function disable(record) { if (!record.intercepted) return; record.intercepted = false; if (!record.target.contents.isDestroyed()) await record.target.contents.debugger.sendCommand('Page.setInterceptFileChooserDialog', { enabled: false }); }
  async function discard(record, reason) {
    if (['applied', 'uncertain', 'applying'].includes(record.status)) return;
    record.status = 'cancelled'; record.error = reason; changed(record);
    await disable(record);
    if (record.directory) { await fs.rm(record.directory, { recursive: true }); record.directory = null; }
    if (record.reservedBytes !== undefined) { retained.set(record.room, retained.get(record.room) - record.reservedBytes); delete record.reservedBytes; }
  }
  async function inspect(record, operation) {
    live(record);
    if (!record.chooserId || !['opened', 'staging', 'prepared', 'applying'].includes(record.status)) fail('File chooser has no current unapplied native event');
    const tree = await call(record, 'Page.getFrameTree', {}, operation);
    const nodes = [tree.frameTree];
    for (let index = 0; index < nodes.length; index++) { if (index >= 512 || !nodes[index]?.frame) fail('File chooser frame tree is invalid'); nodes.push(...(nodes[index].childFrames ?? [])); }
    const frame = nodes.find(node => node.frame.id === record.frameId)?.frame;
    const root = tree.frameTree.frame;
    if (!frame || typeof root.loaderId !== 'string' || typeof frame.loaderId !== 'string') fail('File chooser document has no current frame-loader identity');
    let origin; try { const url = new URL(root.url); if (!['https:', 'http:'].includes(url.protocol)) fail('File upload requires the current HTTP(S) top-level origin'); origin = url.origin; } catch { fail('File upload requires the current HTTP(S) top-level origin'); }
    const resolved = await call(record, 'DOM.resolveNode', { backendNodeId: record.nodeId }, operation);
    const objectId = resolved.object?.objectId;
    if (typeof objectId !== 'string') fail('File chooser node no longer resolves');
    try {
      const value = await call(record, 'Runtime.callFunctionOn', { objectId, functionDeclaration: 'function(){ return { connected: this.isConnected, file: this.localName === "input" && this.type === "file", documentURL: this.ownerDocument.URL }; }', returnByValue: true }, operation);
      if (value.exceptionDetails || value.result?.value?.connected !== true || value.result.value.file !== true) fail('File chooser node is no longer a connected file input');
    } finally { await call(record, 'Runtime.releaseObject', { objectId }, operation); }
    const document = { main_frame: root.id, main_loader: root.loaderId, frame: frame.id, loader: frame.loaderId, epoch: record.epoch };
    if (record.document && canonical(record.document) !== canonical(document)) fail('File chooser document changed');
    record.document ??= document;
    return { ...receipt(record), document_token: record.documentToken, backend_node_id: record.nodeId, top_level_origin: origin };
  }
  function validateRequest(record, request, body) {
    const required = ['schema_version', 'request_id', 'call_id', 'original', 'scope', 'approval_scope', 'upload', 'resume'];
    exact(request, [...required, 'permission_offer'], required);
    exact(request.scope, ['worker_environment_id', 'formation_pin', 'host_generation']);
    exact(request.upload, ['browser_context_id', 'target_id', 'reservation_id', 'file_chooser_id', 'document_token', 'backend_node_id', 'top_level_origin', 'files']);
    const content = { schema_version: request.schema_version, original: request.original, scope: request.scope, approval_scope: request.approval_scope, upload: request.upload };
    if (Object.hasOwn(request, 'permission_offer')) { validatePermissionOffer(request); content.permission_offer = request.permission_offer; }
    const commitment = sha(canonical(content));
    if (request.schema_version !== 1 || request.approval_scope !== 'browser_file_upload' || request.request_id !== `sha256:${commitment}` || request.resume.correlation_id !== `browser-upload-resume:${commitment}`
      || request.call_id !== request.original.correlation_id || typeof request.call_id !== 'string' || !request.call_id
      || request.scope.host_generation !== hostGeneration() || request.scope.worker_environment_id !== body.formation.worker_environment_id || request.scope.formation_pin !== body.formation.formation_pin
      || request.original.run !== body.formation.run || request.original.ee !== body.formation.ee || request.original.worker !== body.formation.worker || request.original.door_interface !== body.formation.door_interface
      || request.original.tool !== body.tool.id || request.original.source_movement_seq !== body.source_movement_seq || Buffer.from(request.original.source_artifact.content).toString('hex') !== body.artifact_content_hash
      || canonical(request.original.arguments) !== canonical(body.arguments) || !body.formation.granted_tools.includes(request.resume.tool)
      || request.upload.reservation_id !== record.id || request.upload.file_chooser_id !== record.chooserId || request.upload.target_id !== record.target.id || request.upload.browser_context_id !== record.room.contextId
      || request.upload.document_token !== record.documentToken || request.upload.backend_node_id !== record.nodeId) fail('Upload approval commitment changed its admitted call or native chooser');
    return commitment;
  }
  async function verifyFiles(record) {
    for (const file of record.files) {
      if (file.offset !== file.metadata.size_bytes) fail('Upload file is incomplete');
      const handle = await fs.open(file.path, constants.O_RDONLY | constants.O_NOFOLLOW);
      try {
        const stat = await handle.stat(); if (!stat.isFile() || stat.uid !== process.getuid() || stat.nlink !== 1 || (stat.mode & 0o777) !== 0o600 || stat.size !== file.metadata.size_bytes) fail('Upload file ownership or byte count changed');
        const hash = crypto.createHash('sha256'), buffer = Buffer.alloc(CHUNK); let offset = 0;
        while (offset < stat.size) { const { bytesRead } = await handle.read(buffer, 0, Math.min(buffer.length, stat.size - offset), offset); if (!bytesRead) fail('Upload file ended before its declared length'); hash.update(buffer.subarray(0, bytesRead)); offset += bytesRead; }
        if (hash.digest('hex') !== file.metadata.payload_sha256) fail('Upload file content changed');
      } finally { await handle.close(); }
    }
  }
  async function command(room, target, body, method, params, signal, validate) {
    validate(); const operation = { signal, validate };
    if (METHODS.get(method) !== body.tool.operation && !(method === 'OctopusBrowser.abandonFileChooser' && body.tool.operation === 'filechooser_resume') && !(method === 'OctopusBrowser.applyFileChooser' && body.tool.operation === 'filechooser_set_files' && Object.hasOwn(params, 'permission_entry_id'))) fail('Tool cannot perform this file chooser operation');
    if (method === 'OctopusBrowser.armFileChooser') {
      exact(params, []); exact(body.arguments, ['target_id'], []);
      if (!target || [...records.values()].some(record => record.target === target && ['arming', 'armed', 'opened', 'staging', 'prepared'].includes(record.status))) fail('Page already has an outstanding file chooser reservation');
      ensureDebugger(target);
      const record = { id: crypto.randomBytes(16).toString('hex'), room, target, epoch: target.debuggerEpoch, status: 'arming', documentToken: crypto.randomBytes(16).toString('hex'), waiters: new Set(), intercepted: false };
      records.set(record.id, record);
      try { await call(record, 'Page.enable', {}, operation); record.status = 'armed'; record.intercepted = true; await call(record, 'Page.setInterceptFileChooserDialog', { enabled: true }, operation); }
      catch (error) { await discard(record, String(error.message)); throw error; }
      return receipt(record);
    }
    const record = current(room, params.reservation_id);
    if (body.tool.operation !== 'filechooser_resume' && params.reservation_id !== body.arguments.reservation_id) fail('File chooser differs from the original tool arguments');
    if (target && target !== record.target) fail('File chooser belongs to another page');
    if (method === 'OctopusBrowser.abandonFileChooser') {
      exact(params, ['reservation_id', 'request_id']);
      if (params.request_id !== record.request?.request_id || (body.tool.operation === 'filechooser_resume' ? body.arguments.request_id !== params.request_id || body.tool.id !== record.request.resume.tool || body.source_movement_seq <= record.request.original.source_movement_seq : record.invocationNonce !== body.invocation_nonce)) fail('Upload cleanup lost its original or approved request');
      await discard(record, 'Upload preparation or current admission failed'); return receipt(record);
    }
    if (method === 'OctopusBrowser.cancelFileChooser') { exact(params, ['reservation_id']); exact(body.arguments, ['reservation_id']); await discard(record, 'File chooser cancelled explicitly'); return receipt(record); }
    if (method === 'OctopusBrowser.waitFileChooser') {
      exact(params, ['reservation_id', 'timeout_ms']); exact(body.arguments, ['reservation_id', 'timeout_ms'], ['reservation_id']);
      if (!Number.isSafeInteger(params.timeout_ms) || params.timeout_ms < 0 || params.timeout_ms > 3000 || params.timeout_ms !== (body.arguments.timeout_ms ?? 3000)) fail('Invalid file chooser wait timeout');
      if (record.status === 'armed') await new Promise((resolve, reject) => { let timer; const finish = error => { clearTimeout(timer); record.waiters.delete(wake); signal?.removeEventListener('abort', abort); error ? reject(error) : resolve(); }; const wake = () => finish(); const abort = () => finish(new Error('File chooser wait connection ended')); record.waiters.add(wake); signal?.addEventListener('abort', abort, { once: true }); timer = setTimeout(wake, params.timeout_ms); if (signal?.aborted) abort(); });
      if (record.status === 'armed') { await discard(record, 'File chooser observation deadline reached'); return { ...receipt(record), observation: 'deadline' }; }
      validate(); return receipt(record);
    }
    if (method === 'OctopusBrowser.inspectFileChooser') { exact(params, ['reservation_id', 'file_chooser_id']); if (params.file_chooser_id !== body.arguments.file_chooser_id || params.file_chooser_id !== record.chooserId) fail('File chooser event changed'); return inspect(record, operation); }
    if (method === 'OctopusBrowser.stageFileChooser') {
      exact(params, ['reservation_id', 'request', 'max_staged_bytes']); exact(body.arguments, ['reservation_id', 'file_chooser_id', 'files']);
      if (record.status !== 'opened' || params.max_staged_bytes !== QUOTA || params.request.upload.file_chooser_id !== body.arguments.file_chooser_id) fail('File chooser is no longer available for staging');
      const request = params.request; validateRequest(record, request, body);
      const observed = await inspect(record, operation);
      if (observed.top_level_origin !== request.upload.top_level_origin) fail('Upload page origin changed');
      const files = request.upload.files;
      if (!Array.isArray(files) || !files.length || !Array.isArray(body.arguments.files) || files.length !== body.arguments.files.length || !record.multiple && files.length > 1) fail('File chooser selection violates native multiplicity');
      let total = 0;
      files.forEach((file, index) => { exact(file, ['artifact_ref', 'filename', 'payload_sha256', 'size_bytes']); ref(file.artifact_ref); filename(file.filename); digest(file.payload_sha256); exact(body.arguments.files[index], ['artifact_ref', 'filename']); if (canonical(file.artifact_ref) !== canonical(body.arguments.files[index].artifact_ref) || file.filename !== body.arguments.files[index].filename || !Number.isSafeInteger(file.size_bytes) || file.size_bytes < 0) fail('Staged file differs from selected artifact'); total += file.size_bytes; });
      if (!Number.isSafeInteger(total) || (retained.get(room) ?? 0) + total > QUOTA) fail('Room upload staging quota is exhausted; applied and uncertain files remain counted');
      record.status = 'staging'; record.reservedBytes = total; retained.set(room, (retained.get(room) ?? 0) + total); record.request = request; record.invocationNonce = body.invocation_nonce; record.files = [];
      try {
        record.directory = await directory(hostRoot, ['uploads', hostGeneration(), room.contextId, record.id]);
        for (const [index, metadata] of files.entries()) { live(record); const owner = await directory(record.directory, [String(index)]), file = path.join(owner, metadata.filename); const handle = await fs.open(file, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600); await handle.close(); record.files.push({ metadata, path: file, offset: 0 }); }
      } catch (error) { await discard(record, String(error.message)); throw error; }
      return receipt(record);
    }
    if (method === 'OctopusBrowser.writeFileChooser') {
      exact(params, ['reservation_id', 'request_id', 'index', 'offset', 'payload_base64']);
      if (record.status !== 'staging' || record.invocationNonce !== body.invocation_nonce || params.request_id !== record.request.request_id || !Number.isSafeInteger(params.index) || params.index < 0) fail('Upload chunk is outside its original admitted staging invocation');
      const file = record.files[params.index];
      if (!file || params.offset !== file.offset || typeof params.payload_base64 !== 'string' || params.payload_base64.length > Math.ceil(CHUNK / 3) * 4) fail('Upload chunk ordering or byte bound changed');
      const bytes = Buffer.from(params.payload_base64, 'base64');
      if (!bytes.length || bytes.length > CHUNK || bytes.toString('base64') !== params.payload_base64 || file.offset + bytes.length > file.metadata.size_bytes) fail('Upload chunk differs from declared bytes');
      live(record); const handle = await fs.open(file.path, constants.O_WRONLY | constants.O_NOFOLLOW);
      try { const stat = await handle.stat(); if (!stat.isFile() || stat.uid !== process.getuid() || stat.nlink !== 1 || (stat.mode & 0o777) !== 0o600 || stat.size !== file.offset) fail('Upload staging file changed'); let written = 0; while (written < bytes.length) { const result = await handle.write(bytes, written, bytes.length - written, file.offset + written); if (!result.bytesWritten) fail('Upload staging write stopped'); written += result.bytesWritten; } await handle.sync(); }
      finally { await handle.close(); }
      validate(); live(record); file.offset += bytes.length;
      return { ...receipt(record), index: params.index, offset: file.offset };
    }
    if (method === 'OctopusBrowser.prepareFileChooser') {
      exact(params, ['reservation_id', 'request_id']);
      if (record.status !== 'staging' || record.invocationNonce !== body.invocation_nonce || params.request_id !== record.request.request_id) fail('Upload preparation differs from its admitted invocation');
      const observed = await inspect(record, operation); if (observed.top_level_origin !== record.request.upload.top_level_origin) fail('Upload origin changed during staging');
      await verifyFiles(record); validate(); live(record); record.status = 'prepared'; return { ...receipt(record), request_id: record.request.request_id };
    }
    if (method === 'OctopusBrowser.applyFileChooser') {
      const cached = Object.hasOwn(params, 'permission_entry_id');
      exact(params, cached ? ['reservation_id', 'request_id', 'decision', 'permission_entry_id'] : ['reservation_id', 'request_id', 'decision']);
      permissionRequest(room, body, params.request_id);
      if (cached) {
        if (body.tool.operation !== 'filechooser_set_files') fail('Saved choice must continue its original upload');
        record.permissionReuse = await permissions.requireSaved(record.request, params.permission_entry_id, params.decision);
      } else {
        exact(body.arguments, ['request_id']);
        if (body.tool.operation !== 'filechooser_resume' || body.arguments.request_id !== params.request_id || !['accept', 'decline', 'cancel'].includes(params.decision)) fail('Upload response differs from its original request');
      }
      if (record.status !== 'prepared') fail('Upload approval was already consumed or its document is unavailable');
      if (params.decision !== 'accept') { await discard(record, params.decision === 'cancel' ? 'Upload cancelled by the operator' : 'Upload declined by the operator'); return receipt(record); }
      const observed = await inspect(record, operation); if (observed.top_level_origin !== record.request.upload.top_level_origin) fail('Approved upload origin changed');
      await verifyFiles(record); validate(); live(record);
      if (record.status !== 'prepared') fail('Upload reservation changed before dispatch');
      record.status = 'applying'; record.operatorSource = { source_movement_seq: body.source_movement_seq, artifact_content_hash: body.artifact_content_hash };
      try { await call(record, 'DOM.setFileInputFiles', { backendNodeId: record.nodeId, files: record.files.map(file => file.path) }, operation); record.status = 'applied'; }
      catch (error) { record.status = 'uncertain'; record.error = `File input dispatch may have completed and will not be repeated: ${error.message}`; }
      changed(record); return { ...receipt(record), request_id: params.request_id, effect_performed: record.status === 'applied', operator_source: record.operatorSource, ...(record.permissionReuse ? { permission_reuse: record.permissionReuse } : {}) };
    }
    fail('Unknown file chooser method');
  }
  function permissionRequest(room, body, requestId) {
    const matches = [...records.values()].filter(record => record.room === room && record.request?.request_id === requestId);
    if (matches.length !== 1) fail('Permission has no exact native chooser request');
    const record = matches[0]; live(record);
    if (record.status !== 'prepared') fail('Permission chooser is no longer prepared');
    if (body.tool.operation === 'filechooser_set_files') {
      if (record.invocationNonce !== body.invocation_nonce || record.request.original.tool !== body.tool.id || record.request.original.source_movement_seq !== body.source_movement_seq) fail('Permission lost its original upload invocation');
    } else if (body.tool.operation !== 'filechooser_resume' || record.request.resume.tool !== body.tool.id || body.arguments.request_id !== requestId || body.source_movement_seq <= record.request.original.source_movement_seq) fail('Permission lost its admitted upload continuation');
    return record.request;
  }
  function event(target, method, params, sessionId) {
    for (const record of records.values()) {
      if (record.target !== target || ['cancelled', 'failed', 'applied', 'uncertain', 'applying'].includes(record.status)) continue;
      if (method === 'Page.fileChooserOpened' && record.status === 'armed') {
        if (sessionId !== null || !Number.isSafeInteger(params.backendNodeId) || params.backendNodeId < 1 || typeof params.frameId !== 'string') { record.status = 'failed'; record.error = 'File chooser event has no supported in-process node/frame'; }
        else { record.nodeId = params.backendNodeId; record.frameId = params.frameId; record.multiple = params.mode === 'selectMultiple'; record.chooserId = crypto.randomBytes(16).toString('hex'); record.status = 'opened'; }
        disable(record).catch(error => { record.status = 'failed'; record.error = `Cannot end chooser interception: ${error.message}`; changed(record); }); changed(record);
      } else if (sessionId === null && (method === 'Runtime.executionContextsCleared' || method === 'Page.frameDetached' && params.frameId === record.frameId || method === 'Page.frameNavigated' && record.document && [record.document.main_frame, record.document.frame].includes(params.frame?.id))) {
        discard(record, 'File chooser document ended').catch(error => { record.error = String(error.message); });
      }
    }
  }
  function releaseTarget(target, reason) { for (const record of records.values()) if (record.target === target) discard(record, reason).catch(error => { record.error = String(error.message); }); }
  async function releaseRoom(room) {
    for (const [key, record] of records) if (record.room === room) { record.status = 'cancelled'; changed(record); if (record.directory) await fs.rm(record.directory, { recursive: true }); records.delete(key); }
    retained.delete(room);
  }
  return { permissionRequest, owns: method => METHODS.has(method), command, event, releaseTarget, releaseRoom };
}
module.exports = { createBrowserFileChoosers };
