'use strict';

const { managedErrorForURL, displayPage, mapHistory, createErrorDocument, ignoredLoadFailure, aborted } = require('./browser-error.cjs');
const { createBrowserDownloads } = require('./browser-downloads.cjs');
const { createBrowserFileChoosers } = require('./browser-filechooser.cjs');
const { createHumanDownloads } = require('./browser-human-downloads.cjs');
const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const net = require('node:net');
const crypto = require('node:crypto');
const { once } = require('node:events');
const { createBrowserFamily, roomClipboard, FAMILY, keyCommands } = require('./browser-locators.cjs');
const { clearDomState, invalidatePageWorlds } = require('./browser-primitives.cjs');
const { createBrowserInvocations } = require('./browser-invocations.cjs');
const { createBrowserWaiters } = require('./browser-waiters.cjs');
const { createRoomHistory } = require('./browser-history-control.cjs');
const { createBrowserPermissions } = require('./browser-permissions.cjs');
const { engineInfo } = require('./engine-mode.cjs');
const { createStandardEngine } = require('./standard-engine.cjs');

const PROTOCOL = 'browser.native.v1';
const PRESENTATION_FIELDS = ['attachmentId', 'hostGeneration', 'mountGeneration', 'viewportSequence', 'metricsRevision', 'stageBounds', 'pageBounds', 'visualBounds', 'scale', 'visible'];
const LEASE_TIMEOUT = 60000;
const LINE_LIMIT = 16 * 1024 * 1024;
const RESULT_LIMIT = 96 * 1024 * 1024;
const CHUNK_BYTES = 8 * 1024 * 1024;
const QUEUE_LIMIT = 16;
const COMMAND_TIMEOUT = 300000;
const FORMATION_FIELDS = ['worker_environment_id', 'run', 'ee', 'worker', 'door_interface', 'door_descriptor_hash', 'worker_environment_kind', 'worker_environment_kind_descriptor_hash', 'tool_descriptor_hashes', 'instance_config_hashes', 'formation_pin', 'granted_tools', 'door_root_bindings', 'readable_root_slots', 'writable_root_slots', 'network_ceiling', 'requires_release'];
const OBSERVE = ['Page.enable', 'Runtime.enable', 'Log.enable', 'Network.enable', 'Runtime.evaluate', 'Accessibility.enable', 'Accessibility.getFullAXTree', 'DOM.enable'];
const FRAME_LIMIT = 512;
const SESSION_LIMIT = 128;
const INVENTORY_LIMIT = 65536;
const IFRAME_FILTER = [{ type: 'iframe', exclude: false }];
const READINESS = ['Page.getFrameTree', 'Page.setLifecycleEventsEnabled', 'Target.setAutoAttach', 'DOMSnapshot.captureSnapshot', 'DOM.describeNode', 'DOM.getFrameOwner'];
const ELEMENT = ['Page.createIsolatedWorld', 'DOM.getContentQuads', 'DOM.getNodeForLocation', 'DOM.scrollIntoViewIfNeeded', 'DOM.getBoxModel', 'DOM.resolveNode', 'DOM.describeNode', 'Runtime.callFunctionOn', 'Runtime.releaseObject'];
const TOOL_METHODS = {
  navigate: [...READINESS, 'Page.navigate'],
  snapshot: [...READINESS],
  click: [...READINESS, ...ELEMENT, 'Input.dispatchMouseEvent'],
  type: [...READINESS, ...ELEMENT, 'DOM.focus', 'Input.dispatchKeyEvent', 'Input.insertText'],
  key: [...READINESS, ...ELEMENT, 'Input.dispatchKeyEvent', 'Input.insertText'], scroll: [...READINESS, 'Input.dispatchMouseEvent', 'Input.synthesizeScrollGesture'],
  back: [...READINESS, 'Page.getNavigationHistory', 'Page.navigateToHistoryEntry'],
  forward: [...READINESS, 'Page.getNavigationHistory', 'Page.navigateToHistoryEntry'],
  reload: [...READINESS, 'Page.reload'],
  viewport_set: [], viewport_reset: [], visibility_get: [], visibility_set: [], download_arm: [], download_wait: [], download_cancel: [],
  filechooser_arm: [], filechooser_wait: [], filechooser_set_files: [], filechooser_cancel: [], filechooser_resume: [],
  playwright_dom_snapshot: [...READINESS, ...ELEMENT], dom_snapshot: [...READINESS, ...ELEMENT], dom_click: [...READINESS, ...ELEMENT, 'Input.dispatchMouseEvent'], dom_scroll: [...READINESS, ...ELEMENT, 'Page.getLayoutMetrics', 'Input.synthesizeScrollGesture'],
  locator_query: [...READINESS, ...ELEMENT], locator_fill: [...READINESS, ...ELEMENT, 'Input.insertText'],
  locator_press: [...READINESS, ...ELEMENT, 'Input.dispatchKeyEvent', 'Input.insertText'], locator_select: [...READINESS, ...ELEMENT],
  locator_check: [...READINESS, ...ELEMENT, 'Input.dispatchMouseEvent'], locator_wait: [...READINESS, ...ELEMENT],
  pointer_move: [...READINESS, 'Input.dispatchMouseEvent'], paste: [...READINESS, ...ELEMENT],
  clipboard_read: [], clipboard_write: [], evaluate_readonly: [...READINESS, ...ELEMENT], media_download: [...READINESS, ...ELEMENT],
  eval: ['Runtime.callFunctionOn', 'Runtime.releaseObject'],
  hover: [...READINESS, ...ELEMENT, 'Input.dispatchMouseEvent'],
  select: [...READINESS, ...ELEMENT],
  drag: [...READINESS, ...ELEMENT, 'Input.dispatchMouseEvent'],
  wait: [...READINESS], wait_complete: [], history: [], history_resume: [], images: [], console: [],
  dialog: ['Page.handleJavaScriptDialog'], screenshot: ['Page.getFrameTree', 'Page.getLayoutMetrics', 'Page.captureScreenshot'],
  pdf: ['Page.printToPDF'], cookies: ['Network.getCookies'], cookie_set: ['Network.setCookie'],
  cookie_delete: ['Network.deleteCookies'], tabs: [], tab_new: [], tab_select: [], tab_mark: [], tab_close: [], close: [],
};

function failure(code, message) { return Object.assign(new Error(message), { code }); }
function assert(condition, message, code = 'invalid_request') { if (!condition) throw failure(code, message); }
function object(value, keys) {
  assert(value !== null && typeof value === 'object' && !Array.isArray(value), 'Expected JSON object');
  if (keys) assert(Object.keys(value).every(key => keys.includes(key)) && keys.every(key => Object.hasOwn(value, key)), 'Object fields do not match native browser schema');
  return value;
}
function identity(value) { assert(typeof value === 'string' && value.length > 0 && value.length <= 2048 && value.trim() === value && !/[\x00-\x1f\x7f]/.test(value), 'Invalid identity'); return value; }
function integer(value) { assert(Number.isSafeInteger(value) && value > 0, 'Expected positive safe integer'); return value; }
function hex(value, length) { assert(typeof value === 'string' && value.length === length && /^[0-9a-f]+$/.test(value), 'Invalid hexadecimal value'); return value; }
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value !== null && typeof value === 'object') return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`;
  return JSON.stringify(value);
}
function digest(value) { return crypto.createHash('sha256').update(canonical(value)).digest('hex'); }
function randomId() { return crypto.randomBytes(16).toString('hex'); }
function hmac(key, bytes) { return crypto.createHmac('sha256', key).update(bytes).digest('hex'); }
function urlAllowed(value, resource = false) {
  if (value === 'about:blank') return true;
  try { const url = new URL(value); return ['http:', 'https:'].includes(url.protocol) || (resource && ['data:', 'blob:'].includes(url.protocol)); } catch { return false; }
}
function validateUrl(value) { assert(typeof value === 'string' && urlAllowed(value), 'Browser navigation requires http, https, or about:blank', 'navigation_denied'); return value; }
function form(value) {
  object(value, FORMATION_FIELDS);
  for (const field of ['worker_environment_id', 'run', 'ee', 'worker', 'door_interface']) identity(value[field]);
  for (const field of ['door_descriptor_hash', 'worker_environment_kind_descriptor_hash', 'formation_pin']) hex(value[field], 64);
  assert(value.worker_environment_kind === 'browser_session', 'Formation does not name this browser kind', 'authority_denied');
  assert(['denied', 'restricted', 'full'].includes(value.network_ceiling), 'Invalid network ceiling');
  assert(typeof value.requires_release === 'boolean', 'Invalid release declaration');
  for (const field of ['granted_tools', 'readable_root_slots', 'writable_root_slots']) {
    assert(Array.isArray(value[field]) && value[field].every(entry => typeof entry === 'string'), 'Invalid formed list');
    value[field].forEach(identity);
    assert(canonical(value[field]) === canonical([...new Set(value[field])].sort()), 'Formed list must be sorted and unique');
  }
  object(value.tool_descriptor_hashes); object(value.instance_config_hashes); object(value.door_root_bindings);
  assert(canonical(Object.keys(value.tool_descriptor_hashes).sort()) === canonical(value.granted_tools), 'Tool hashes differ from granted tools');
  Object.values(value.tool_descriptor_hashes).forEach(value => hex(value, 64));
  Object.values(value.instance_config_hashes).forEach(value => hex(value, 64));
  for (const [slot, root] of Object.entries(value.door_root_bindings)) { identity(slot); assert(typeof root === 'string' && path.isAbsolute(root) && path.normalize(root) === root, 'Invalid formed root'); }
  for (const slot of [...value.readable_root_slots, ...value.writable_root_slots]) assert(Object.hasOwn(value.door_root_bindings, slot), 'Missing formed root binding');
  return value;
}
async function privateRoot(root) {
  assert(typeof root === 'string' && path.isAbsolute(root) && path.normalize(root) === root, 'Host root must be an absolute canonical path');
  const parts = root.split(path.sep).filter(Boolean);
  let current = path.parse(root).root;
  for (let index = 0; index < parts.length; index++) {
    current = path.join(current, parts[index]);
    let stat;
    try { stat = await fs.lstat(current); } catch (error) {
      if (error.code !== 'ENOENT' || index !== parts.length - 1) throw error;
      await fs.mkdir(current, { mode: 0o700 }); stat = await fs.lstat(current);
    }
    assert(stat.isDirectory() && !stat.isSymbolicLink(), 'Host root traverses a non-directory or symlink', 'host_path_denied');
    if (index === parts.length - 1) assert(stat.uid === process.getuid() && (stat.mode & 0o777) === 0o700, 'Host root must be owner-only mode 0700', 'host_path_denied');
  }
}
async function privateWrite(filename, bytes) {
  try {
    const stat = await fs.lstat(filename);
    assert(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1 && stat.uid === process.getuid() && (stat.mode & 0o777) === 0o600, 'Existing host file is not a private regular file', 'host_path_denied');
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  const temporary = `${filename}.${randomId()}`;
  const handle = await fs.open(temporary, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW, 0o600);
  try { await handle.writeFile(bytes); await handle.sync(); } finally { await handle.close(); }
  await fs.rename(temporary, filename);
}
async function unlinkOwned(filename) {
  try { await fs.unlink(filename); } catch (error) { if (error.code !== 'ENOENT') throw error; }
}
async function removeStaleSocket(filename) {
  let stat;
  try { stat = await fs.lstat(filename); } catch (error) { if (error.code === 'ENOENT') return; throw error; }
  assert(stat.isSocket() && stat.uid === process.getuid() && (stat.mode & 0o777) === 0o600, 'Existing native socket is not owned by this host', 'host_path_denied');
  await new Promise((resolve, reject) => {
    const socket = net.createConnection(filename);
    socket.setTimeout(1000, () => { socket.destroy(); reject(failure('host_already_present', 'Existing native browser socket did not establish its state')); });
    socket.once('connect', () => { socket.destroy(); reject(failure('host_already_present', 'Native browser host is already listening')); });
    socket.once('error', error => { socket.destroy(); if (error.code === 'ECONNREFUSED') resolve(); else reject(error); });
  });
  const current = await fs.lstat(filename);
  assert(current.ino === stat.ino && current.dev === stat.dev, 'Native browser socket changed during recovery', 'host_path_denied');
  await fs.unlink(filename);
}
function timed(promise, message, signal) {
  let timer;
  let abort;
  return Promise.race([promise, new Promise((_, reject) => {
    abort = () => reject(failure('execution_uncertain', 'Native browser connection ended after dispatch; the effect may have completed'));
    if (signal?.aborted) { abort(); return; }
    signal?.addEventListener('abort', abort, { once: true });
    timer = setTimeout(() => reject(failure('execution_uncertain', message)), COMMAND_TIMEOUT);
  })]).finally(() => { clearTimeout(timer); signal?.removeEventListener('abort', abort); });
}
function safePreferences(session) {
  return { session, sandbox: true, nodeIntegration: false, nodeIntegrationInWorker: false, nodeIntegrationInSubFrames: false, contextIsolation: true, webSecurity: true, allowRunningInsecureContent: false, webviewTag: false, plugins: false, navigateOnDragDrop: false, focusOnNavigation: false, backgroundThrottling: false };
}

function createBrowserEngineHost({ electron, hostRoot, engineProof, engineMode = 'owned', descriptorHash, onEvent = () => {} }) {
  const engine = engineInfo(engineMode);
  const standard = engineMode === 'standard' ? createStandardEngine(electron) : null;
  const unavailable = capability => failure('engine_capability_unavailable', `${capability} is unavailable in ${engineMode} browser mode`);
  const downloadSnapshot = () => ({ available: false, sequence: 0, downloads: [], unacknowledgedIds: [], preferences: null,
    error: unavailable('Persistent download history').message });
  function paintLease(contents, enabled) { if (!standard && engine.capabilities.hiddenPageCapture) contents.setOwnedGuestPaintLease(enabled); }
  function destroyContents(contents) { return standard ? standard.destroy(contents) : contents.destroy(); }
  function presentationAttached(attachment) { return standard ? standard.attached(attachment) : attachment.target.contents.hostWebContents === attachment.sender; }
  const permissions = createBrowserPermissions({ hostRoot, maxBytes: LINE_LIMIT, maxRecords: INVENTORY_LIMIT });
  const rooms = new Map();
  const contexts = new Map();
  const targets = new Map();
  const guestTargets = new WeakMap();
  const attachments = new Map();
  const destinations = new Map();
  const connections = new Set();
  let generation;
  let key;
  let server;
  let mountGeneration = 0;
  let cursorSequence = 0;
  let stopped = false;
  let started = false;
  let socketPath;

  const invocations = createBrowserInvocations({
    descriptorHash, generation: () => generation, rooms, createRoom, ensureLive, form, methods: TOOL_METHODS,
    ownerEnded: (room, creator) => room.waiters?.releaseCreator(creator),
    invocationEnded: (room, reader) => room.waiters?.releaseReader(reader),
  });
  function createRoom(formation, formationDigest) {
    const room = { formation, formationDigest, active: true, handle: randomId(), contextId: null, session: null, targets: new Map(), presentationIntentId: 0, openIntent: null, pendingViewport: null, viewportIntent: null, seq: 0, queued: 0, chain: Promise.resolve() };
    room.history = engine.capabilities.navigationHistory ? createRoomHistory({ electron, hostRoot, engineProof, engineMode, room, generation: () => generation, ensureLive, pendingLimit: QUEUE_LIMIT, permissions,
      onError: error => emit({ type: 'history-collection-error', workerEnvironmentId: room.formation.worker_environment_id, formationPin: room.formation.formation_pin,
        error: { code: error.code || 'history_collection_failed', message: error.message } }) }) : null;
    rooms.set(formation.worker_environment_id, room);
    return room;
  }
  function emit(event) { onEvent({ ...event, hostGeneration: generation }); }
  const humanDownloads = engine.capabilities.persistentDownloadHistory ? createHumanDownloads({ hostRoot, electron, emit,
    historyBackend: engineMode === 'standard' ? require('./standard-download-history.cjs').createStandardDownloadHistory() : undefined }) : null;
  const downloads = createBrowserDownloads({ hostRoot, hostGeneration: () => generation, requireTarget, emit, humanDownloads, isModelActive: modelActive });
  const fileChoosers = createBrowserFileChoosers({ hostRoot, hostGeneration: () => generation, requireTarget, ensureDebugger, permissions });
  async function ensureDownloads(target) { ensureDebugger(target); await downloads.ready(target); }
  function ensureLive(room) { assert(!stopped && room.active && rooms.get(room.formation.worker_environment_id) === room, 'Browser formation has been withdrawn', 'formation_withdrawn'); }
  function requireTarget(room, id) {
    ensureLive(room);
    const target = targets.get(id);
    assert(target && target.room === room && !target.contents.isDestroyed() && target.contents.session === room.session, 'Exact browser target is unavailable in this formed room', 'target_unavailable');
    return target;
  }
  function sessionChain(target, sessionId) {
    const chain = [];
    const seen = new Set();
    for (let id = sessionId; id !== null; ) {
      assert(!seen.has(id) && seen.size < SESSION_LIMIT, 'Invalid iframe ancestry', 'session_unavailable');
      seen.add(id);
      const entry = target.frameSessions.get(id);
      assert(entry && entry.parent_session_id !== id, 'Iframe session is no longer owned by this target', 'session_unavailable');
      chain.push(entry); id = entry.parent_session_id;
    }
    return chain;
  }
  function verifyChain(target, chain) {
    requireTarget(target.room, target.id);
    assert(chain.every(entry => target.frameSessions.get(entry.session_id) === entry), 'Iframe attachment changed after dispatch; effect may have completed', 'execution_uncertain');
    sessionChain(target, chain[0]?.session_id ?? null);
  }
  function forgetSession(target, id) {
    const removed = new Set([id]);
    for (const entry of target.frameSessions.values()) if (removed.has(entry.parent_session_id)) removed.add(entry.session_id);
    for (const child of removed) target.frameSessions.delete(child);
    for (const [key, context] of target.frameContexts) if (removed.has(context.sessionId)) target.frameContexts.delete(key);
  }
  function frameInventory(target) {
    const sessions = [...target.frameSessions.values()];
    for (const entry of sessions) sessionChain(target, entry.session_id);
    const inventory = { target_id: target.id, sessions };
    assert(sessions.length <= SESSION_LIMIT && Buffer.byteLength(JSON.stringify(inventory)) <= INVENTORY_LIMIT, 'Iframe inventory exceeds its boundary', 'session_unavailable');
    return inventory;
  }
  function frameEvent(target, method, params, source) {
    if (source !== null) sessionChain(target, source);
    if (method === 'Target.attachedToTarget') {
      if (params.targetInfo?.type !== 'iframe') return false;
      identity(params.sessionId); identity(params.targetInfo.targetId);
      assert(params.sessionId !== source, 'Iframe attachment cannot parent itself', 'session_unavailable');
      const existing = target.frameSessions.get(params.sessionId);
      if (existing) assert(existing.target_id === params.targetInfo.targetId && existing.parent_session_id === source, 'Iframe attachment identity changed', 'session_unavailable');
      else {
        assert(target.frameSessions.size < SESSION_LIMIT, 'Too many iframe attachments', 'session_unavailable');
        integer(++target.attachmentSeq);
        target.frameSessions.set(params.sessionId, { session_id: params.sessionId, parent_session_id: source, target_id: params.targetInfo.targetId, attachment_seq: target.attachmentSeq });
        frameInventory(target);
      }
    } else if (method === 'Target.detachedFromTarget') {
      const entry = target.frameSessions.get(params.sessionId);
      if (!entry || entry.parent_session_id !== source) return false;
      forgetSession(target, params.sessionId);
    }
    if (method === 'Runtime.executionContextsCleared' || method === 'Page.frameNavigated' || method === 'Page.frameDetached') {
      const frameId = params.frame?.id ?? params.frameId;
      for (const [key, context] of target.frameContexts) if (context.sessionId === source && (method === 'Runtime.executionContextsCleared' || context.frameId === frameId)) target.frameContexts.delete(key);
    }
    return true;
  }
  function ensureDebugger(target) {
    requireTarget(target.room, target.id);
    if (!target.contents.debugger.isAttached()) { target.contents.debugger.attach('1.3'); integer(++target.debuggerEpoch); }
    else if (target.debuggerEpoch === 0) target.debuggerEpoch = 1;
    return target.debuggerEpoch;
  }
  function viewportIntent(body, viewport) {
    return { revision: body.source_movement_seq, viewport, artifact_content_hash: body.artifact_content_hash, tool_id: body.tool.id, tool_descriptor_hash: body.formation.tool_descriptor_hashes[body.tool.id], formation_pin: body.formation.formation_pin };
  }
  function viewportValue(body) {
    const keys = Object.hasOwn(body.arguments, 'target_id') ? ['target_id'] : [];
    if (keys.length) identity(body.arguments.target_id);
    if (body.tool.operation === 'viewport_reset') { object(body.arguments, keys); return null; }
    assert(body.tool.operation === 'viewport_set', 'Tool cannot change model viewport', 'authority_denied');
    object(body.arguments, ['width', 'height', ...keys]);
    for (const value of [body.arguments.width, body.arguments.height]) assert(Number.isInteger(value) && value > 0, 'Viewport dimensions must be positive integers');
    return { width: Math.min(4096, Math.max(240, body.arguments.width)), height: Math.min(4096, Math.max(160, body.arguments.height)) };
  }
  function viewportState(room, target) {
    ensureLive(room);
    if (target) requireTarget(room, target.id);
    return { schema_version: 1, worker_environment_id: room.formation.worker_environment_id, formation_pin: room.formation.formation_pin, host_generation: generation, engine_handle: room.handle, browser_context_id: room.contextId, target_id: target?.id ?? null,
      last_intent: room.viewportIntent, pending: room.pendingViewport, model_viewport: target?.viewportKnown ? target.modelViewport : null, application: target?.viewportApplication ?? null, debugger_epoch: target?.debuggerEpoch ?? null };
  }
  function withholdViewport(target) {
    target.cursorWaiter?.finish('unpresented');
    for (const attachment of [target.attachment, target.pendingAttachment]) {
      if (!attachment || attachments.get(attachment.id) !== attachment) continue;
      attachment.visible = false;
      if (attachment.phase !== 'prepared') {
        attachment.phase = 'attached';
        try { nativeVisibility(attachment, false); } catch (error) { endAttachment(attachment, { code: error.code || 'attachment_unavailable', message: error.message }); }
      }
    }
  }
  async function applyViewport(target, intent, connection, validate) {
    const room = target.room;
    requireTarget(room, target.id);
    assert(target.viewportApplication?.intent.revision !== intent.revision, 'Viewport intent has already been dispatched; use readback', 'replay_rejected');
    if (target.viewportKnown && canonical(target.modelViewport.emulatedSize) === canonical(intent.viewport)) return;
    const epoch = ensureDebugger(target);
    withholdViewport(target);
    if (target.viewportKnown) publish(target);
    target.viewportKnown = false;
    const application = { intent, status: 'applying', debugger_epoch: epoch };
    target.viewportApplication = application;
    try {
      assert(!connection.socket.destroyed, 'Viewport connection ended before dispatch', 'execution_uncertain');
      const method = intent.viewport === null ? 'Emulation.clearDeviceMetricsOverride' : 'Emulation.setDeviceMetricsOverride';
      const params = intent.viewport === null ? {} : { ...intent.viewport, deviceScaleFactor: 1, mobile: false };
      await timed(standard ? standard.setViewport(target.contents, intent.viewport, () => {
        validate(); requireTarget(room, target.id);
        assert(target.viewportApplication === application && target.debuggerEpoch === epoch && target.contents.debugger.isAttached() && !connection.socket.destroyed && !connection.abort.signal.aborted, 'Viewport authority changed before dispatch', 'execution_uncertain');
      }) : target.contents.debugger.sendCommand(method, params), 'Viewport metrics dispatch outcome is uncertain', connection.abort.signal);
      requireTarget(room, target.id);
      assert(target.viewportApplication === application && target.debuggerEpoch === epoch && target.contents.debugger.isAttached() && !connection.socket.destroyed, 'Viewport target/debugger changed after dispatch', 'execution_uncertain');
      application.status = 'applied';
      target.modelViewport = Object.freeze({ revision: intent.revision, emulatedSize: intent.viewport === null ? null : Object.freeze({ ...intent.viewport }) });
      target.viewportKnown = true;
      publish(target);
    } catch (error) {
      if (target.viewportApplication === application) { application.status = 'uncertain'; target.viewportKnown = false; }
      for (const attachment of [target.attachment, target.pendingAttachment]) if (attachment) emit({ type: 'error', attachmentId: attachment.id, mountGeneration: attachment.mountGeneration, error: { code: 'viewport_unconfirmed', message: 'Viewport application is unconfirmed; no metrics command was replayed' } });
      throw error;
    }
  }
  function reloadPage(target) {
    requireTarget(target.room, target.id);
    if (target.loadError) return target.contents.loadURL(validateUrl(target.loadError.failedUrl));
    target.contents.reload();
  }
  function rendererUnavailable(target) {
    if (targets.get(target.id) !== target || target.contents.isDestroyed() || target.failedRendererVersion === target.navigationVersion) return;
    target.failedRendererVersion = target.navigationVersion;
    target.frameSessions.clear(); target.frameContexts.clear();
    for (const connection of connections) if (connection.targetId === target.id) connection.socket.destroy();
    const url = target.loadError?.failedUrl || target.pendingUrl || target.contents.getURL();
    if (urlAllowed(url) && url !== 'about:blank') showPageError(target, { kind: 'crash', failedUrl: url, errorCode: 0, errorDescription: 'PAGE_CRASHED' });
    else publishTargets(target.room);
  }
  function showPageError(target, details) {
    if (targets.get(target.id) !== target || target.contents.isDestroyed()) return;
    if (!engine.capabilities.diagnosticHistoryReplacement) {
      target.loadError = { ...details, title: details.kind === 'crash' ? 'This page crashed' : "This site can't be reached" }; target.pendingUrl = details.failedUrl;
      publish(target); publishTargets(target.room); return;
    }
    const wc = target.contents;
    const history = wc.navigationHistory.getAllEntries();
    for (const [url, error] of target.errorPages) if (!history.some(entry => managedErrorForURL({ loadError: error, errorPages: new Map() }, entry.url))) target.errorPages.delete(url);
    const error = createErrorDocument(details); target.errorPages.set(error.pageUrl, error); target.loadError = error;
    target.pendingUrl = error.failedUrl; const version = target.navigationVersion;
    clearTimeout(target.errorTimer); publish(target); publishTargets(target.room);
    const current = () => targets.get(target.id) === target && target.room.active && !wc.isDestroyed() && target.contents === wc && target.loadError === error && target.navigationVersion === version;
    const load = (attempt = 0) => {
      if (!current()) return;
      if (wc.isLoading() && attempt < 100) { target.errorTimer = setTimeout(() => load(attempt + 1), 50); target.errorTimer.unref(); return; }
      const failed = failure => {
        if (!current() || aborted(failure)) return;
        if (String(failure?.message).includes('navigation is already pending') && attempt < 100) {
          target.errorTimer = setTimeout(() => load(attempt + 1), 50); target.errorTimer.unref(); return;
        }
        for (const attachment of [target.attachment, target.pendingAttachment]) if (attachment) emit({ type: 'error', attachmentId: attachment.id, mountGeneration: attachment.mountGeneration, error: { code: 'error_page_unavailable', message: 'The browser could not display its error document' } });
      };
      try { wc.loadURL(error.pageUrl, { replaceCurrentEntry: true }).catch(failed); } catch (error) { failed(error); }
    };
    load();
  }
  function modelActive(target) {
    return target.room.modelTurn?.open === true && target.modelActivitySeq > (target.room.modelTurn.openingMovementSeq ?? 0);
  }
  function claimModelTarget(target, body) {
    target.modelActivitySeq = body.source_movement_seq;
    if (target.room.modelTurn?.open === false && body.source_movement_seq <= target.room.modelTurn.observedMovementSeq) return;
    target.modelControlled = true;
    paintLease(target.contents, true);
  }
  function tabOwnership(target) {
    return { lifetime: target.lifetime, model_controlled: target.modelControlled, disposition: target.disposition ? { ...target.disposition } : null };
  }
  async function completeModelTurn(room, turn) {
    const current = () => room.active && room.seq <= turn.observedMovementSeq && room.modelTurn?.open === false && room.modelTurn.openingMovementSeq === turn.openingMovementSeq;
    if (!current() || (room.modelTurn.lastToolSeq ?? 0) <= turn.openingMovementSeq || room.completedTurn?.openingMovementSeq === turn.openingMovementSeq) return;
    room.completedTurn = { ...turn, status: 'completing', errors: [] };
    try { await room.pageClipboard?.cleanupPageClipboards(); } catch (error) { room.completedTurn.errors.push({ code: 'clipboard_cleanup_failed', message: error.message }); }
    for (const target of [...room.targets.values()]) {
      if (!current()) break;
      if (!target.modelControlled || target.contents.isDestroyed()) continue;
      const disposition = target.disposition?.source_movement_seq > turn.openingMovementSeq && target.disposition.source_movement_seq <= turn.observedMovementSeq ? target.disposition.status : null;
      target.cursorWaiter?.finish('unpresented'); target.cursor = null;
      paintLease(target.contents, false);
      for (const connection of connections) if (connection.room === room && connection.targetId === target.id) connection.socket.destroy();
      target.frameSessions.clear(); target.frameContexts.clear();
      if (!disposition && target.lifetime === 'temporary') { destroyTarget(target, 'Temporary browser page completed its model turn'); continue; }
      const handoff = disposition === 'handoff' && target.lifetime === 'temporary';
      target.disposition = null;
      if (!handoff) { target.modelControlled = false; target.lifetime = 'persistent'; }
      try {
        const wc = target.contents;
        if (wc.debugger.isAttached()) await timed(wc.debugger.sendCommand('Emulation.setFocusEmulationEnabled', { enabled: false }), 'Browser turn focus cleanup is unconfirmed');
        if (!current() || targets.get(target.id) !== target || wc.isDestroyed()) break;
        if (!handoff && (!target.viewportKnown || target.modelViewport.emulatedSize !== null)) {
          withholdViewport(target); target.viewportKnown = false;
          const epoch = ensureDebugger(target);
          await timed(standard ? standard.setViewport(wc, null, () => {
            assert(current() && targets.get(target.id) === target && target.debuggerEpoch === epoch && wc.debugger.isAttached(), 'Browser target changed before viewport cleanup', 'execution_uncertain');
          }) : wc.debugger.sendCommand('Emulation.clearDeviceMetricsOverride', {}), 'Released browser viewport cleanup is unconfirmed');
          assert(current() && targets.get(target.id) === target && target.debuggerEpoch === epoch && wc.debugger.isAttached(), 'Browser target changed during turn cleanup', 'execution_uncertain');
          target.modelViewport = Object.freeze({ revision: turn.observedMovementSeq, emulatedSize: null });
          target.viewportApplication = null; target.viewportKnown = true;
        }
        publish(target);
      } catch (error) {
        room.completedTurn.errors.push({ target_id: target.id, code: error.code || 'turn_cleanup_failed', message: error.message });
        emit({ type: 'turn-cleanup-error', workerEnvironmentId: room.formation.worker_environment_id, targetId: target.id, error: { code: error.code || 'turn_cleanup_failed', message: error.message } });
      }
    }
    room.completedTurn.status = room.completedTurn.errors.length ? 'unconfirmed' : 'completed';
    publishTargets(room);
  }
  async function moveCursor(target, body, params, connection) {
    object(params, ['x', 'y', 'waitForArrival']);
    assert(Number.isFinite(params.x) && Number.isFinite(params.y) && typeof params.waitForArrival === 'boolean', 'Invalid browser cursor point');
    target.cursorWaiter?.finish('superseded');
    assert(cursorSequence < Number.MAX_SAFE_INTEGER, 'Browser cursor sequence exhausted');
    const moveSequence = ++cursorSequence;
    claimModelTarget(target, body);
    target.cursor = { moveSequence, x: params.x, y: params.y, metricsRevision: target.modelViewport.revision, sourceMovementSeq: body.source_movement_seq };
    const receipt = status => ({ target_id: target.id, browser_context_id: target.room.contextId, move_sequence: moveSequence, status });
    const attachment = target.attachment;
    if (!engine.capabilities.cursorOverlay || !attachment || attachment.phase !== 'ready' || !attachment.visible || !windowVisible(attachment) || !modelActive(target)) {
      publish(target); return receipt('unpresented');
    }
    if (!params.waitForArrival) { publish(target); return receipt('moving'); }
    return new Promise(resolve => {
      let timer;
      const abort = () => finish('withdrawn');
      const finish = status => {
        if (target.cursorWaiter !== waiter) return;
        target.cursorWaiter = null; clearTimeout(timer); connection.abort.signal.removeEventListener('abort', abort);
        resolve(receipt(status));
      };
      const waiter = { attachment, moveSequence, metricsRevision: target.modelViewport.revision, finish };
      target.cursorWaiter = waiter;
      timer = setTimeout(() => finish('timeout'), 1500); timer.unref();
      connection.abort.signal.addEventListener('abort', abort, { once: true });
      if (connection.abort.signal.aborted) abort(); else publish(target);
    });
  }
  function targetInfo(target) {
    return { targetId: target.id, type: 'page', ...displayPage(target), ...tabOwnership(target), attached: target.contents.debugger.isAttached(), browserContextId: target.room.contextId, ...(target.openerId ? { openerId: target.openerId } : {}) };
  }
  function publishTargets(room) {
    if (room.active && !stopped) emit({ type: 'targets', workerEnvironmentId: room.formation.worker_environment_id, formationPin: room.formation.formation_pin, hostGeneration: generation, engineHandle: room.handle, browserContextId: room.contextId });
  }
  function state(attachment) {
    const target = requireTarget(attachment.room, attachment.target.id);
    const wc = target.contents;
    assert(target.viewportKnown, 'Applied browser viewport is unconfirmed', 'viewport_unconfirmed');
    return { attachmentId: attachment.id, mountGeneration: attachment.mountGeneration, surfaceId: attachment.surfaceId, protocol: PROTOCOL, presentationProtocol: engine.presentationProtocol, engine, presentationPhase: attachment.phase, viewportSequence: attachment.viewportSequence, modelViewport: { ...target.modelViewport }, modelActive: modelActive(target), ...tabOwnership(target), modelTurnKey: String(target.room.modelTurn?.openingMovementSeq ?? 0), cursor: engine.capabilities.cursorOverlay && target.cursor?.metricsRevision === target.modelViewport.revision ? { ...target.cursor } : null, engineHandle: attachment.room.handle, hostGeneration: generation, targetId: target.id, browserContextId: attachment.room.contextId, workerEnvironmentId: attachment.room.formation.worker_environment_id, run: attachment.room.formation.run, ee: attachment.room.formation.ee, connected: true, visible: attachment.visible && windowVisible(attachment), readOnly: false, loading: wc.isLoading(), ...displayPage(target), ...(target.loadError ? { loadError: { kind: target.loadError.kind, failedUrl: target.loadError.failedUrl, errorCode: target.loadError.errorCode, errorDescription: target.loadError.errorDescription } } : {}), canGoBack: wc.navigationHistory.canGoBack(), canGoForward: wc.navigationHistory.canGoForward(), zoomFactor: wc.getZoomFactor(), ...(attachment.findResult || {}) };
  }
  function publish(target) {
    if (target.contents.isDestroyed() || !target.room.active || !target.viewportKnown) return;
    for (const attachment of [target.attachment, target.pendingAttachment]) {
      if (attachment && attachments.get(attachment.id) === attachment) emit({ type: 'state', attachmentId: attachment.id, mountGeneration: attachment.mountGeneration, state: state(attachment) });
    }
  }
  function endAttachment(attachment, error) {
    if (attachments.get(attachment.id) !== attachment) return;
    if (attachment.target.cursorWaiter?.attachment === attachment) attachment.target.cursorWaiter.finish('unpresented');
    attachments.delete(attachment.id);
    clearTimeout(attachment.timer);
    attachment.sender.removeListener('destroyed', attachment.onDestroyed);
    attachment.sender.removeListener('render-process-gone', attachment.onDestroyed);
    attachment.sender.removeListener('did-start-navigation', attachment.onNavigation);
    attachment.window.removeListener('closed', attachment.onDestroyed);
    for (const name of ['show', 'hide', 'minimize', 'restore']) attachment.window.removeListener(name, attachment.onVisibility);
    const frames = destinations.get(attachment.sender);
    if (frames?.get(attachment.frameToken) === attachment) frames.delete(attachment.frameToken);
    if (frames?.size === 0) destinations.delete(attachment.sender);
    if (attachment.target.attachment === attachment) attachment.target.attachment = null;
    if (attachment.target.pendingAttachment === attachment) attachment.target.pendingAttachment = null;
    if (!attachment.target.attachment && !attachment.target.pendingAttachment) attachment.target.focusIntentId = null;
    const wc = attachment.target.contents;
    try {
      if (standard) standard.release(attachment);
      else if (!wc.isDestroyed()) {
        wc.cancelOwnedGuestLease(attachment.adoptionToken);
        wc.releaseOwnedGuest(attachment.mountGeneration);
      }
    } finally {
      emit({ type: 'closed', attachmentId: attachment.id, mountGeneration: attachment.mountGeneration, error: error || { code: 'viewer_detached', message: 'Browser presentation detached' } });
    }
  }
  function destroyTarget(target, reason) {
    if (targets.get(target.id) !== target) return target.destruction;
    clearTimeout(target.errorTimer);
    target.room.waiters?.releaseTarget(target.id, target.debuggerEpoch);
    for (const cleanup of target.room.clipboardCleanup ?? []) Promise.resolve().then(() => cleanup(target.contents.id)).catch(() => {});
    clearDomState(target.contents.id);
    for (const listener of target.room.clipboardListeners ?? []) listener({ type: 'tabDetached', value: target.contents.id });
    downloads.release(target, reason);
    fileChoosers.releaseTarget(target, reason);
    targets.delete(target.id); guestTargets.delete(target.contents);
    target.room.targets.delete(target.id);
    if (target.room.openIntent?.targetId === target.id) target.room.openIntent = null;
    if (target.pendingAttachment) endAttachment(target.pendingAttachment, { code: 'target_unavailable', message: reason });
    if (target.attachment) endAttachment(target.attachment, { code: 'target_unavailable', message: reason });
    for (const connection of connections) {
      if (connection.targetId === target.id) { connection.targetId = null; if (!connection.releasing) connection.socket.destroy(); }
    }
    if (!target.contents.isDestroyed()) {
      paintLease(target.contents, false);
      target.destruction = destroyContents(target.contents);
      target.destruction?.catch(error => emit({ type: 'error', targetId: target.id, error: { code: 'release_incomplete', message: error.message } }));
    }
    publishTargets(target.room);
    return target.destruction;
  }
  async function revoke(room, reason) {
    room.active = false;
    const cleanup = room.releaseCleanup ??= { complete: false, completed: new Set(), targets: new Set(), task: null };
    if (cleanup.complete) return;
    if (cleanup.task) return cleanup.task;
    const step = async (name, action) => {
      if (cleanup.completed.has(name)) return;
      await action(); cleanup.completed.add(name);
    };
    cleanup.task = Promise.resolve().then(async () => {
      await step('authority', () => { invocations.revoke(room); room.waiters?.stop(); });
      await step('commands', async () => {
        for (const connection of connections) if (connection.room === room && !connection.releasing) connection.socket.destroy();
        await room.chain;
        for (const target of room.targets.values()) cleanup.targets.add(target);
      });
      await step('clipboard', () => { room.pageClipboard?.dispose(); room.clipboardListeners?.clear(); room.clipboardCleanup?.clear(); });
      await step('downloads', () => downloads.releaseRoom(room, reason));
      if (humanDownloads) await step('humanDownloads', () => humanDownloads.releaseRoom(room));
      await step('fileChoosers', () => fileChoosers.releaseRoom(room));
      if (room.history) await step('navigationHistory', () => room.history.close());
      await step('targets', async () => {
        for (const target of cleanup.targets) {
          await destroyTarget(target, reason);
          if (!target.contents.isDestroyed()) { paintLease(target.contents, false); await destroyContents(target.contents); }
          assert(target.contents.isDestroyed(), 'Native target destruction is unconfirmed', 'release_incomplete');
          cleanup.targets.delete(target);
        }
      });
      await step('connections', () => room.session?.closeAllConnections());
      await step('storage', () => room.session?.clearStorageData());
      contexts.delete(room.contextId); cleanup.complete = true;
    }).finally(() => { cleanup.task = null; });
    return cleanup.task;
  }
  function configureSession(session, room) {
    session.setPermissionCheckHandler((_contents, permission) => permission === 'clipboard-sanitized-write');
    session.setPermissionRequestHandler((_contents, permission, callback) => callback(permission === 'clipboard-sanitized-write'));
    session.setDevicePermissionHandler(() => false);
    session.on('will-download', (event, item, contents) => downloads.willDownload(room, event, item, contents));
    session.webRequest.onBeforeRequest((details, callback) => {
      const resource = !['mainFrame', 'subFrame'].includes(details.resourceType);
      const page = Number.isSafeInteger(details.webContentsId) ? [...room.targets.values()].find(target => target.contents.id === details.webContentsId) : null;
      const diagnostic = details.resourceType === 'mainFrame' && page && managedErrorForURL(page, details.url);
      callback({ cancel: !room.active || (!urlAllowed(details.url, resource) && !diagnostic) });
    });
  }
  function createTarget(room, openerId, openIntent) {
    ensureLive(room);
    if (standard) return registerTarget(room, standard.create(safePreferences(room.session)), openerId, openIntent);
    assert(typeof electron.webContents?.createOwnedGuest === 'function', 'Owned browser guest engine is unavailable', 'engine_unavailable');
    let contents;
    try { contents = electron.webContents.createOwnedGuest({ ...safePreferences(room.session), initialSurfaceSize: { width: 1280, height: 720 } }); }
    catch (error) { throw failure('engine_unavailable', `Owned browser guest creation failed: ${error.message}`); }
    return registerTarget(room, contents, openerId, openIntent);
  }
  function registerTarget(room, contents, openerId, openIntent) {
    ensureLive(room);
    assert(contents && (standard ? standard.owns(contents) : typeof contents.isOwnedGuest === 'function' && contents.isOwnedGuest() && ['createOwnedGuestLease', 'cancelOwnedGuestLease', 'releaseOwnedGuest', 'setOwnedGuestVisibility', 'setOwnedGuestPaintLease', 'destroy'].every(name => typeof contents[name] === 'function')), 'Declared browser engine is unavailable', 'engine_unavailable');
    assert(!contents.isDestroyed() && contents.session === room.session && !guestTargets.has(contents), 'Created target has a foreign session or existing owner', 'authority_denied');
    try { room.history?.observe(contents); } catch (error) { destroyContents(contents); throw error; }
    const target = { id: randomId(), room, contents, openerId, attachment: null, pendingAttachment: null, modelViewport: Object.freeze({ revision: 0, emulatedSize: null }), viewportKnown: true, viewportApplication: null, debuggerEpoch: 0, frameSessions: new Map(), frameContexts: new Map(), attachmentSeq: 0, navigationVersion: 0, pendingUrl: '', loadError: null, errorPages: new Map(), errorTimer: null, failedRendererVersion: null, modelActivitySeq: 0, modelControlled: false, lifetime: openIntent ? 'persistent' : 'temporary', disposition: null, cursor: null, cursorWaiter: null };
    targets.set(target.id, target); room.targets.set(target.id, target); guestTargets.set(contents, target);
    const wc = target.contents;
    if (room.pendingVisibility) { target.visibilityIntent = room.pendingVisibility; room.pendingVisibility = null; if (target.visibilityIntent.visible) openIntent = { source: 'tool', activate: false }; }
    paintLease(wc, false);
    wc.setBackgroundThrottling(false);
    if (!standard) wc.on('did-attach', () => {
      const attachment = target.pendingAttachment;
      if (!attachment) return;
      try {
        checkAttachment(attachment);
        assert(wc.hostWebContents === attachment.sender && wc.setOwnedGuestVisibility(attachment.mountGeneration, false) === true, 'Native browser adoption did not commit this generation', 'attachment_unavailable');
        checkAttachment(attachment);
        if (target.attachment) endAttachment(target.attachment);
        checkAttachment(attachment);
        target.attachment = attachment; target.pendingAttachment = null;
        attachment.phase = 'attached'; attachment.visible = false;
        clearTimeout(attachment.timer);
        publish(target);
      } catch (error) { endAttachment(attachment, { code: error.code || 'attachment_unavailable', message: error.message }); }
    });
    if (!standard) wc.on('-owned-guest-released', (_event, releasedGeneration) => {
      for (const attachment of [target.attachment, target.pendingAttachment]) if (attachment?.mountGeneration === releasedGeneration) endAttachment(attachment);
    });
    for (const name of ['did-start-loading', 'did-stop-loading', 'did-navigate', 'did-navigate-in-page', 'page-title-updated']) wc.on(name, () => { publish(target); publishTargets(room); });
    const rejectNavigation = (event, url, isMainFrame) => {
      event.preventDefault();
      if (!room.active || targets.get(target.id) !== target) return;
      const blocked = { type: 'event', value: { method: 'Page.navigationBlocked', params: { url, resourceType: isMainFrame ? 'mainFrame' : 'subFrame' }, source: { tabId: wc.id } } };
      for (const listener of target.familyListeners ?? []) listener(blocked);
    };
    wc.on('will-navigate', (event, url) => { if (!urlAllowed(url) && !managedErrorForURL(target, url)) rejectNavigation(event, url, true); });
    wc.on('will-frame-navigate', event => { if (!urlAllowed(event.url) && !(event.isMainFrame && managedErrorForURL(target, event.url))) rejectNavigation(event, event.url, event.isMainFrame); });
    wc.on('will-attach-webview', event => event.preventDefault());
    wc.on('context-menu', (_event, params) => {
      const attachment = target.attachment;
      if (!attachment || attachment.phase !== 'ready' || !room.active || wc.isDestroyed()) return;
      const act = action => () => {
        if (!room.active || wc.isDestroyed() || target.attachment !== attachment || attachment.phase !== 'ready' || targets.get(target.id) !== target) return;
        const failed = error => emit({ type: 'error', attachmentId: attachment.id, mountGeneration: attachment.mountGeneration, error: { code: error.code || 'action_failed', message: error.message } });
        try {
          checkAttachment(attachment);
          const result = action();
          if (result && typeof result.then === 'function') result.catch(failed);
        } catch (error) { failed(error); }
      };
      const items = [];
      if (params.linkURL && urlAllowed(params.linkURL)) items.push({ label: 'Open link in new tab', click: act(() => {
        const page = createTarget(room, target.id, { source: 'context-menu', activate: true });
        page.contents.loadURL(params.linkURL).catch(error => emit({ type: 'error', attachmentId: attachment.id, mountGeneration: attachment.mountGeneration, error: { code: 'navigation_failed', message: error.message } }));
      }) });
      if (params.linkURL) items.push({ label: 'Copy link address', click: act(() => electron.clipboard.writeText(params.linkURL)) });
      if (params.isEditable) items.push({ label: 'Cut', enabled: params.editFlags.canCut, click: act(() => wc.cut()) });
      if (params.isEditable || params.selectionText?.trim()) items.push({ label: 'Copy', enabled: params.editFlags.canCopy, click: act(() => wc.copy()) });
      if (params.isEditable) items.push({ label: 'Paste', enabled: params.editFlags.canPaste, click: act(() => wc.paste()) });
      if (items.length) items.push({ type: 'separator' });
      if (!params.isEditable && !params.linkURL && !params.srcURL && !params.selectionText?.trim() && params.mediaType === 'none') items.push(
        { label: 'Back', enabled: wc.navigationHistory.canGoBack(), click: act(() => wc.navigationHistory.goBack()) },
        { label: 'Forward', enabled: wc.navigationHistory.canGoForward(), click: act(() => wc.navigationHistory.goForward()) },
        { label: 'Reload', click: act(() => reloadPage(target)) },
        { type: 'separator' },
      );
      items.push({ label: 'Inspect', click: act(() => wc.inspectElement(params.x, params.y)) });
      electron.Menu.buildFromTemplate(items).popup({ window: attachment.window });
    });
    wc.on('before-input-event', (event, input) => {
      const attachment = target.attachment;
      const modifier = process.platform === 'darwin' ? input.meta === true : input.control === true;
      if (!attachment || attachment.phase !== 'ready' || input.type !== 'keyDown' || !modifier || input.alt || input.shift || input.isAutoRepeat) return;
      const command = input.key?.toLowerCase() === 'l' ? 'focus-address' : input.key?.toLowerCase() === 'f' ? 'find' : null;
      if (command) {
        event.preventDefault();
        emit({ type: 'command', attachmentId: attachment.id, mountGeneration: attachment.mountGeneration, command });
      }
    });
    wc.on('found-in-page', (_event, result) => {
      const attachment = target.attachment;
      if (attachment && attachment.findRequestId === result.requestId) {
        attachment.findResult = { findFound: result.matches > 0, findMatches: result.matches, findActiveOrdinal: result.activeMatchOrdinal };
        publish(target);
      }
    });
    wc.on('did-start-navigation', (_event, url, isInPlace, isMainFrame) => {
      if (!isMainFrame || isInPlace) return;
      const diagnostic = managedErrorForURL(target, url);
      if (diagnostic !== target.loadError || !diagnostic) {
        integer(++target.navigationVersion); clearTimeout(target.errorTimer); target.failedRendererVersion = null;
      }
      target.loadError = diagnostic; target.pendingUrl = diagnostic?.failedUrl || url;
    });
    wc.on('did-navigate', (_event, url) => { target.loadError = managedErrorForURL(target, url); target.pendingUrl = ''; publish(target); publishTargets(room); });
    wc.on('did-fail-load', (_event, code, description, url, isMainFrame) => {
      if (!ignoredLoadFailure(code, description, isMainFrame) && url !== 'about:blank' && urlAllowed(url) && !managedErrorForURL(target, url)) showPageError(target, { kind: 'load', failedUrl: url, errorCode: code, errorDescription: description });
    });
    wc.on('render-process-gone', (_event, details = {}) => {
      if (details.reason === 'clean-exit') return;
      rendererUnavailable(target);
    });
    wc.on('unresponsive', () => rendererUnavailable(target));
    wc.on('destroyed', () => destroyTarget(target, 'Browser target closed'));
    wc.setWindowOpenHandler(details => {
      if (!room.active || !targets.has(target.id) || !urlAllowed(details.url)) return { action: 'deny' };
      return { action: 'allow', outlivesOpener: true, overrideBrowserWindowOptions: { webPreferences: safePreferences(room.session) }, createWindow: options => {
        requireTarget(room, target.id);
        const register = contents => registerTarget(room, contents, target.id, { source: 'popup', activate: details.disposition !== 'background-tab' });
        if (!standard) return register(options.webContents).contents;
        let child;
        return standard.createPopup(safePreferences(room.session), options, details, contents => { child = register(contents); }, error => {
          if (targets.get(child.id) === child && !aborted(error)) emit({ type: 'error', targetId: child.id, error: { code: 'navigation_failed', message: error.message } });
        });
      } };
    });
    wc.debugger.on('message', (_event, method, params, sessionId) => {
      if (!room.active || !targets.has(target.id)) return;
      fileChoosers.event(target, method, params, sessionId || null);
      if (downloads.event(target, method, params, sessionId || null)) return;
      try { if (!frameEvent(target, method, params, sessionId || null)) return; }
      catch { target.frameSessions.clear(); target.frameContexts.clear(); for (const connection of connections) if (connection.targetId === target.id) connection.socket.destroy(); return; }
      if (['Runtime.executionContextsCleared', 'Runtime.executionContextDestroyed', 'Page.frameNavigated', 'Page.frameDetached', 'Target.detachedFromTarget'].includes(method)) invalidatePageWorlds(wc.id);
      const event = { type: 'event', value: { method, params, source: { tabId: wc.id, ...(sessionId ? { sessionId, targetId: target.frameSessions.get(sessionId)?.target_id } : {}) } } };
      for (const listener of target.familyListeners ?? []) listener(event);
      target.clipboardResponseContext = method === 'Runtime.bindingCalled' && Number.isSafeInteger(params.executionContextId) && params.executionContextId > 0 ? canonical([sessionId || null, params.executionContextId]) : null;
      try { for (const listener of room.clipboardListeners ?? []) listener(event); }
      finally { target.clipboardResponseContext = null; }
      const bytes = Buffer.from(JSON.stringify({ method, params, ...(sessionId ? { sessionId } : {}) }) + '\n');
      for (const connection of connections) if (connection.room === room && connection.targetId === target.id) {
        if (bytes.length > LINE_LIMIT || connection.socket.writableLength + bytes.length > RESULT_LIMIT) connection.socket.destroy();
        else if (connection.writingResponse) {
          if (connection.eventBytes + bytes.length > LINE_LIMIT) connection.socket.destroy();
          else { connection.events.push(bytes); connection.eventBytes += bytes.length; }
        } else connection.socket.write(bytes);
      }
    });
    wc.debugger.on('detach', () => {
      room.waiters?.releaseTarget(target.id, target.debuggerEpoch);
      invalidatePageWorlds(wc.id);
      target.frameSessions.clear(); target.frameContexts.clear();
      integer(++target.debuggerEpoch); withholdViewport(target); target.viewportKnown = !engine.capabilities.modelViewport;
      if (target.viewportApplication) target.viewportApplication.status = 'uncertain';
      for (const connection of connections) if (connection.targetId === target.id) connection.socket.destroy();
    });
    if (openIntent) {
      room.presentationIntentId = (room.presentationIntentId || 0) + 1;
      assert(Number.isSafeInteger(room.presentationIntentId), 'Native browser presentation intent counter exhausted');
      room.openIntent = { id: room.presentationIntentId, targetId: target.id, ...openIntent };
    }
    publishTargets(room);
    return target;
  }
  async function dispatch(body, room, connection, lease) {
    invocations.validate(lease);
    const { target_id: targetId, session_id: sessionId, method, params } = body.operation;
    const operation = body.tool.operation;
    if (!engine.capabilities.navigationHistory && (['history', 'history_resume'].includes(operation) || method === 'OctopusBrowser.selectHistoryCollection')) throw unavailable('Navigation history');
    if (!engine.capabilities.modelViewport && ['viewport_set', 'viewport_reset'].includes(operation)) throw unavailable('Model viewport overrides');
    if (['OctopusBrowser.lookupPermission', 'OctopusBrowser.rememberPermission'].includes(method)) {
      assert(targetId === null && sessionId === null && connection.targetId === null, 'Permission cannot target a debugger session', 'authority_denied');
      const remembering = method === 'OctopusBrowser.rememberPermission';
      object(params, remembering ? ['request_id', 'response', 'source_artifact'] : ['request_id']);
      assert((remembering ? ['filechooser_resume', 'history_resume'] : ['filechooser_set_files', 'history']).includes(operation), 'Tool cannot operate browser preferences', 'method_denied');
      const request = operation === 'history' || operation === 'history_resume' ? room.history.permissionRequest(body, params.request_id)
        : fileChoosers.permissionRequest(room, body, params.request_id);
      const result = remembering ? await permissions.remember(request, body, params) : await permissions.lookup(request);
      invocations.validate(lease); return result;
    }
    const historyMethods = ['OctopusBrowser.prepareHistorySource', 'OctopusBrowser.stageHistoryRequest', 'OctopusBrowser.applyHistoryRequest'];
    if (operation === 'history') assert(historyMethods.includes(method), 'History source invocation cannot operate a page or Session', 'method_denied');
    if (operation === 'history_resume') assert(method === 'OctopusBrowser.applyHistoryRequest', 'History resume may only consume its exact admitted source request', 'method_denied');
    if ([...historyMethods, 'OctopusBrowser.applyHistoryRequest', 'OctopusBrowser.selectHistoryCollection'].includes(method)) {
      assert(targetId === null && sessionId === null && connection.targetId === null, 'History control cannot target a page or debugger session', 'authority_denied');
      const validate = () => invocations.validate(lease);
      if (method === 'OctopusBrowser.prepareHistorySource') return room.history.prepare(body, params, validate);
      if (method === 'OctopusBrowser.stageHistoryRequest') return room.history.stage(body, params);
      if (method === 'OctopusBrowser.applyHistoryRequest') return room.history.apply(body, params, connection.abort.signal, validate);
      return room.history.selectCollection(body, params, validate);
    }
    if (operation === 'wait_complete') assert(method === 'OctopusBrowser.completeWait', 'Completion executor may only read its exact stored waiter', 'method_denied');
    if (method === 'OctopusBrowser.startWait') {
      assert(operation === 'wait' && body.arguments.phase === 'start' && targetId !== null && sessionId === null, 'Wait start requires its exact root page', 'method_denied');
      object(params, ['arguments', 'limits']); object(params.limits, ['max_waiters', 'max_event_bytes']);
      assert(canonical(params.arguments) === canonical(body.arguments), 'Wait start changed its signed arguments', 'authority_denied');
      const limits = { maxWaiters: params.limits.max_waiters, maxBytes: params.limits.max_event_bytes };
      if (room.waiterLimits) assert(canonical(room.waiterLimits) === canonical(limits), 'Waiter limits changed in the formed room', 'authority_denied');
      else { room.waiters = createBrowserWaiters(limits); room.waiterLimits = limits; }
      const target = requireTarget(room, targetId);
      assert(connection.targetId === null || connection.targetId === targetId, 'Wait connection cannot retarget', 'authority_denied');
      connection.targetId = targetId; ensureDebugger(target);
      const epoch = target.debuggerEpoch, creator = `${lease.owner.key}:${lease.active.instance}`;
      const validateSource = () => {
        invocations.validateCreator(room, lease.owner, lease.active.instance);
        assert(requireTarget(room, targetId) === target && target.debuggerEpoch === epoch, 'Wait source page or debugger was replaced', 'wait_source_lost');
      };
      const listeners = new Map();
      const cdp = {
        call: async (id, nestedMethod, nestedParams) => {
          assert(id === targetId && ['Page.enable', 'Page.getFrameTree', 'Runtime.evaluate'].includes(nestedMethod), 'Wait observation escaped its admitted source', 'method_denied');
          validateSource(); const result = await timed(target.contents.debugger.sendCommand(nestedMethod, nestedParams), 'Wait source observation did not complete'); validateSource(); return result;
        },
        addListener: (name, listener) => {
          assert(name === 'event' && !listeners.has(listener), 'Invalid wait subscription');
          const forward = (_event, method, params, sessionId) => { if (!sessionId) listener({ source: { tabId: targetId }, method, params }); };
          listeners.set(listener, forward); target.contents.debugger.on('message', forward);
        },
        removeListener: (name, listener) => { assert(name === 'event', 'Invalid wait subscription'); const forward = listeners.get(listener); if (forward) { target.contents.debugger.removeListener('message', forward); listeners.delete(listener); } },
      };
      return room.waiters.start({ pin: { host_generation: generation, worker_environment_id: room.formation.worker_environment_id, formation_pin: room.formation.formation_pin, browser_context_id: room.contextId, target_id: targetId, debugger_epoch: epoch, start_source_movement_seq: body.source_movement_seq, start_artifact_content_hash: body.artifact_content_hash }, creator, arguments: body.arguments, cdp, validateSource });
    }
    if (method === 'OctopusBrowser.completeWait' || method === 'OctopusBrowser.cancelWait') {
      assert(targetId === null && sessionId === null && connection.targetId === null && room.waiters, 'Wait observation requires its existing room control source', 'waiter_unavailable');
      object(params, ['waiter']);
      object(body.arguments, method === 'OctopusBrowser.completeWait' ? ['waiter'] : ['phase', 'waiter']);
      assert(canonical(params.waiter) === canonical(body.arguments.waiter), 'Wait handle differs from its signed arguments', 'authority_denied');
      if (method === 'OctopusBrowser.cancelWait') { assert(operation === 'wait' && body.arguments.phase === 'cancel', 'Wait cancellation requires its declared operation', 'method_denied'); return room.waiters.cancel(params.waiter); }
      assert(operation === 'wait_complete', 'Wait completion requires its declared observer', 'method_denied');
      connection.readerHandle = lease.active.handle;
      const result = await room.waiters.complete(params.waiter, { reader: lease.active.handle, signal: connection.abort.signal, validate: () => invocations.validate(lease) });
      lease.active.captures.set(result.waiter.wait_id, { waiter: result.waiter, digest: digest(result) });
      return result;
    }
    if (targetId !== null && Object.hasOwn(body.arguments, 'target_id')) {
      assert(identity(body.arguments.target_id) === targetId, 'Page command differs from the signed retained target', 'authority_denied');
    }
    if (method === 'OctopusBrowser.roomOperation') {
      assert(['clipboard_read', 'clipboard_write', 'visibility_get', 'visibility_set'].includes(operation) && targetId === null && sessionId === null && connection.targetId === null, 'Browser room operation requires its exact control connection', 'authority_denied');
      object(params, ['arguments', 'target_id', 'max_clipboard_bytes']);
      assert(canonical(params.arguments) === canonical(body.arguments), 'Room operation changed its signed arguments', 'authority_denied');
      const args = body.arguments, keys = Object.hasOwn(args, 'target_id') ? ['target_id'] : [];
      if (keys.length) assert(args.target_id === params.target_id, 'Room operation changed its retained target', 'authority_denied');
      const page = params.target_id === null ? null : requireTarget(room, identity(params.target_id));
      let value;
      if (operation.startsWith('clipboard_')) {
        object(args, [...keys, ...(operation === 'clipboard_write' ? ['items'] : [])]);
        roomClipboard(room, params.max_clipboard_bytes);
        if (operation === 'clipboard_write') { assert(Number.isSafeInteger(params.max_clipboard_bytes) && params.max_clipboard_bytes > 0 && params.max_clipboard_bytes <= LINE_LIMIT && Buffer.byteLength(JSON.stringify(args.items)) <= params.max_clipboard_bytes, 'Virtual clipboard exceeds the admitted byte bound'); room.pageClipboard.write(args.items, body.tool.id); value = { written: true }; }
        else value = { items: room.pageClipboard.read() };
      } else {
        object(args, [...keys, ...(operation === 'visibility_set' ? ['visible'] : [])]);
        if (operation === 'visibility_set') {
          assert(typeof args.visible === 'boolean', 'Browser visibility must be boolean');
          const intent = { visible: args.visible, source_movement_seq: body.source_movement_seq, artifact_content_hash: body.artifact_content_hash };
          if (page) {
            page.visibilityIntent = intent;
            if (args.visible) { integer(++room.presentationIntentId); room.openIntent = { id: room.presentationIntentId, targetId: page.id, source: 'tool', activate: false }; if (page.attachment?.phase === 'ready' && page.attachment.packet) { const visible = packetVisible(page.attachment, page.attachment.packet); nativeVisibility(page.attachment, visible); page.attachment.visible = visible; } }
            else if (page.attachment) { nativeVisibility(page.attachment, false); page.attachment.visible = false; }
            publish(page); publishTargets(room);
          } else room.pendingVisibility = intent;
        }
        value = { visible: page ? Boolean(page.attachment?.visible && windowVisible(page.attachment)) : false, intent: page?.visibilityIntent ?? room.pendingVisibility ?? null };
      }
      return { target_id: page?.id ?? null, browser_context_id: room.contextId, value };
    }
    if (method === 'OctopusBrowser.perform') {
      assert((FAMILY.has(operation) || ['key', 'click', 'scroll', 'drag'].includes(operation)) && targetId !== null && sessionId === null, 'Typed browser operation requires its exact page', 'method_denied');
      object(params, ['arguments', 'limits', 'page_editing_profile']); object(params.limits, ['max_type_chars', 'max_eval_chars']);
      object(params.page_editing_profile, ['rich_text_paste', 'plain_text_documents']);
      assert(typeof params.page_editing_profile.rich_text_paste === 'boolean' && Array.isArray(params.page_editing_profile.plain_text_documents), 'Invalid admitted page editing profile');
      for (const document of params.page_editing_profile.plain_text_documents) {
        object(document, ['host', 'first_path_segment']); identity(document.host); identity(document.first_path_segment);
        assert(new URL(`https://${document.host}`).host === document.host && !document.first_path_segment.includes('/'), 'Page editing profile requires an exact host and path segment');
      }
      const profileDigest = digest(params.page_editing_profile);
      assert(room.pageEditingProfileDigest === undefined || room.pageEditingProfileDigest === profileDigest, 'Page editing profile changed within the formed room', 'authority_denied');
      room.pageEditingProfileDigest = profileDigest;
      assert(canonical(params.arguments) === canonical(body.arguments), 'Typed browser operation changed the signed original arguments', 'authority_denied');
      for (const value of Object.values(params.limits)) assert(Number.isSafeInteger(value) && value > 0 && value <= LINE_LIMIT, 'Invalid admitted browser limit');
      const target = requireTarget(room, identity(targetId));
      assert(connection.targetId === null || connection.targetId === targetId, 'Typed browser connection cannot retarget', 'authority_denied');
      connection.targetId = targetId; claimModelTarget(target, body); await ensureDownloads(target);
      if (['locator_fill', 'locator_press', 'locator_select', 'locator_check', 'dom_click', 'paste', 'key', 'click', 'drag', 'scroll', 'pointer_move', 'dom_scroll'].includes(operation)) await timed(target.contents.debugger.sendCommand('Emulation.setFocusEmulationEnabled', { enabled: true }), 'Browser input focus emulation did not complete', connection.abort.signal);
      let operationFailure = null;
      const validate = () => {
        if (operationFailure) throw operationFailure;
        requireTarget(room, targetId);
        invocations.validate(lease);
      };
      const owner = createBrowserFamily({
        target, limits: params.limits, pageEditingProfile: params.page_editing_profile, pageUrl: () => displayPage(target).url, validate,
        onPageEvent: listener => { target.familyListeners ??= new Set(); target.familyListeners.add(listener); return () => target.familyListeners.delete(listener); },
        inventory: () => frameInventory(target).sessions,
        cursor: (x, y, waitForArrival) => { validate(); return moveCursor(target, body, { x, y, waitForArrival }, connection); },
        call: async (child, nestedMethod, nestedParams, options = {}) => {
          validate();
          const deadline = options.deadlineMs === undefined ? COMMAND_TIMEOUT : options.deadlineMs - Date.now();
          const milliseconds = Math.min(COMMAND_TIMEOUT, options.timeoutMs ?? COMMAND_TIMEOUT, deadline);
          assert(Number.isFinite(milliseconds) && milliseconds > 0, 'Browser operation deadline reached before dispatch', 'operation_timeout');
          connection.seenSessions = new Map(frameInventory(target).sessions.map(entry => [entry.session_id, entry.attachment_seq]));
          let timer;
          try {
            return await Promise.race([
              dispatch({ ...body, operation: { target_id: targetId, session_id: child, method: nestedMethod, params: nestedParams } }, room, connection, lease),
              new Promise((_, reject) => { timer = setTimeout(() => { operationFailure = failure('execution_uncertain', 'Browser command deadline reached after dispatch; effect may have completed and will not be replayed'); reject(operationFailure); }, milliseconds); }),
            ]);
          } finally { clearTimeout(timer); }
        },
        clipboardCommand: async (route, clipboardMethod, clipboardParams = {}) => {
          ensureLive(room);
          const members = [...room.targets.values()].filter(page => page.contents.id === route.tabId);
          assert(members.length === 1 && route.targetId === undefined, 'Clipboard bridge lost its exact room page', 'authority_denied');
          const page = requireTarget(room, members[0].id), child = route.sessionId ?? null, chain = sessionChain(page, child);
          assert(['Runtime.addBinding', 'Runtime.removeBinding', 'Runtime.evaluate', 'Page.addScriptToEvaluateOnNewDocument', 'Page.removeScriptToEvaluateOnNewDocument'].includes(clipboardMethod), 'Clipboard bridge method is outside its installed lifecycle', 'method_denied');
          if (clipboardParams.contextId !== undefined) assert(page.clipboardResponseContext === canonical([child, clipboardParams.contextId]), 'Clipboard response context was not observed on this exact page', 'authority_denied');
          const result = await timed(page.contents.debugger.sendCommand(clipboardMethod, clipboardParams, child ?? undefined), 'Page clipboard lifecycle did not complete');
          requireTarget(room, page.id); verifyChain(page, chain); validate(); return result;
        },
        onClipboardEvent: listener => { room.clipboardListeners ??= new Set(); room.clipboardListeners.add(listener); },
        registerClipboardCleanup: listener => { room.clipboardCleanup ??= new Set(); room.clipboardCleanup.add(listener); return () => room.clipboardCleanup.delete(listener); },
      });
      try { const result = await owner.perform(operation, params.arguments, params.limits); validate(); return { target_id: targetId, browser_context_id: room.contextId, ...displayPage(target), ...result }; }
      finally { owner.dispose(); }
    }
    if (method === 'OctopusBrowser.moveCursor') {
      assert(['click', 'hover', 'drag', 'scroll'].includes(operation) && targetId !== null && sessionId === null, 'Cursor accompaniment is outside this exact page operation', 'method_denied');
      assert(connection.targetId === null || connection.targetId === targetId, 'Cursor connection cannot retarget', 'authority_denied');
      const target = requireTarget(room, identity(targetId)); connection.targetId = targetId;
      return moveCursor(target, body, params, connection);
    }
    if (method === 'OctopusBrowser.markTarget') {
      assert(operation === 'tab_mark' && targetId === null && sessionId === null && connection.targetId === null, 'Tab disposition requires the exact room control connection', 'authority_denied');
      object(body.arguments, ['target_id', 'status']); object(params, ['target_id', 'status']);
      assert(canonical(body.arguments) === canonical(params) && ['handoff', 'deliverable'].includes(params.status), 'Tab disposition differs from its signed arguments', 'authority_denied');
      const target = requireTarget(room, identity(params.target_id));
      assert(!(room.modelTurn?.open === false && body.source_movement_seq <= room.modelTurn.observedMovementSeq), 'The tab disposition belongs to a completed turn', 'turn_completed');
      target.disposition = { status: params.status, source_movement_seq: body.source_movement_seq, artifact_content_hash: body.artifact_content_hash };
      publish(target); publishTargets(room);
      return { target_id: target.id, browser_context_id: room.contextId, ...tabOwnership(target) };
    }
    if (fileChoosers.owns(method)) {
      assert(sessionId === null, 'File chooser control cannot address an iframe session', 'method_denied');
      const target = targetId === null ? null : requireTarget(room, identity(targetId));
      assert(connection.targetId === null || target && connection.targetId === target.id, 'File chooser connection cannot retarget', 'authority_denied');
      if (target) { connection.targetId = target.id; claimModelTarget(target, body); }
      return fileChoosers.command(room, target, body, method, params, connection.abort.signal, () => invocations.validate(lease));
    }
    if (downloads.owns(method)) {
      assert(sessionId === null, 'Download command cannot address an iframe session', 'authority_denied');
      if (operation === 'download_arm') {
        assert(targetId !== null && (connection.targetId === null || connection.targetId === targetId), 'Download arm requires this exact root target', 'authority_denied');
        const target = requireTarget(room, identity(targetId)); connection.targetId = targetId;
        claimModelTarget(target, body);
        await ensureDownloads(target);
        return downloads.command(target, body, method, params, connection.abort.signal);
      }
      assert(targetId === null && connection.targetId === null, 'Download collection is scoped by its original room-owned identity', 'authority_denied');
      const target = downloads.targetFor(room, body.arguments.download_id);
      return downloads.command(target, body, method, params, connection.abort.signal);
    }
    if (method === 'OctopusBrowser.setViewport' || method === 'OctopusBrowser.getViewportState') {
      assert(sessionId === null, 'Viewport is a root-page operation', 'authority_denied');
      assert(connection.targetId === null || connection.targetId === targetId, 'Viewport connection cannot retarget', 'authority_denied');
      const target = targetId === null ? null : requireTarget(room, identity(targetId));
      if (method === 'OctopusBrowser.getViewportState') { object(params, []); if (target) connection.targetId = target.id; return viewportState(room, target); }
      object(params, ['viewport']);
      const viewport = viewportValue(body);
      assert(canonical(params.viewport) === canonical(viewport), 'Viewport differs from the signed tool arguments', 'authority_denied');
      assert(!room.viewportIntent || body.source_movement_seq > room.viewportIntent.revision, 'Viewport intent is already recorded; use readback', 'replay_rejected');
      assert(target || room.targets.size === 0 && !Object.hasOwn(body.arguments, 'target_id'), 'Pending viewport requires an empty owned context without an explicit target', 'target_unavailable');
      const intent = viewportIntent(body, viewport);
      room.viewportIntent = intent;
      if (target) { connection.targetId = target.id; claimModelTarget(target, body); await applyViewport(target, intent, connection, () => invocations.validate(lease)); }
      else room.pendingViewport = viewport === null ? null : intent;
      return viewportState(room, target);
    }
    if (targetId === null) {
      assert(connection.targetId === null, 'Page connection cannot become browser control', 'authority_denied');
      if (method === 'Target.createBrowserContext') {
        assert(['navigate', 'tab_new', 'viewport_set', 'viewport_reset', 'visibility_get', 'visibility_set', 'clipboard_read', 'clipboard_write'].includes(operation) && !Object.hasOwn(body.arguments, 'target_id'), 'Tool cannot create a browser context for an existing target', 'authority_denied');
        assert(Object.keys(params).every(key => key === 'disposeOnDetach') && params.disposeOnDetach !== true, 'Unsupported browser context option');
        assert(body.effective_allow_network && room.formation.network_ceiling !== 'denied', 'Native context creation requires its actual effective network permission', 'network_denied');
        if (!room.session) {
          const contextId = randomId();
          const session = electron.session.fromPartition(`browser-native:${generation}:${room.handle}`);
          room.session = session; room.contextId = contextId; room.contextReady = false;
          configureSession(session, room); room.contextReady = true; contexts.set(contextId, room);
        }
        assert(room.contextReady, 'Native context configuration did not complete', 'engine_unavailable');
        return { browserContextId: room.contextId, engine_handle: room.handle, host_generation: generation };
      }
      if (method === 'Target.getBrowserContexts') { object(params, []); return { browserContextIds: room.contextReady ? [room.contextId] : [] }; }
      if (method === 'Target.getTargets') { object(params, []); return { targetInfos: [...room.targets.values()].map(targetInfo) }; }
      if (method === 'Target.createTarget') {
        object(params, ['browserContextId', 'url']);
        assert(['navigate', 'tab_new'].includes(operation) && params.browserContextId === room.contextId && !Object.hasOwn(body.arguments, 'target_id'), 'Target creation is outside the formed context or retained-target invocation', 'authority_denied');
        assert(body.effective_allow_network && room.session && room.contextReady && room.contextId, 'Native page creation requires its actual admitted context and network authority', 'network_denied');
        validateUrl(params.url);
        const target = createTarget(room);
        claimModelTarget(target, body);
        const pending = room.pendingViewport; room.pendingViewport = null;
        try {
          await ensureDownloads(target);
          await timed(target.contents.loadURL(pending ? 'about:blank' : params.url), 'Target creation timed out; loading outcome is uncertain', connection.abort.signal);
          requireTarget(room, target.id);
          if (pending) {
            await applyViewport(target, pending, connection, () => invocations.validate(lease));
            if (params.url !== 'about:blank') await timed(target.contents.loadURL(params.url), 'Target navigation outcome is uncertain', connection.abort.signal);
          }
          requireTarget(room, target.id); return { targetId: target.id };
        }
        catch (error) { destroyTarget(target, 'Target creation failed'); throw error; }
      }
      if (method === 'Target.activateTarget') {
        object(params, ['targetId']); assert(operation === 'tab_select', 'Tool cannot select a tab', 'authority_denied');
        requireTarget(room, params.targetId); return {};
      }
      if (method === 'Target.closeTarget') {
        object(params, ['targetId']); assert(['tab_close', 'close'].includes(operation), 'Tool cannot close a tab', 'authority_denied');
        const target = requireTarget(room, params.targetId);
        await Promise.allSettled([...(room.clipboardCleanup ?? [])].map(cleanup => cleanup(target.contents.id)));
        await destroyTarget(target, 'Browser tool closed the target'); return { success: true };
      }
      throw failure('method_denied', 'Unsupported browser-level command');
    }
    identity(targetId);
    const target = requireTarget(room, targetId);
    if (standard && (method.startsWith('Input.') || method === 'Page.captureScreenshot')) assert(target.viewportKnown, 'Applied browser viewport is unconfirmed', 'viewport_unconfirmed');
    if (!engine.capabilities.hiddenPageCapture && method === 'Page.captureScreenshot') {
      const attachment = target.attachment;
      assert(attachment?.phase === 'ready' && attachment.visible && windowVisible(attachment), 'Standard browser screenshots require a visible attached page', 'engine_capability_unavailable');
    }
    assert(connection.targetId === null || connection.targetId === targetId, 'Page connection cannot retarget', 'authority_denied');
    assert(OBSERVE.includes(method) || TOOL_METHODS[operation].includes(method), 'CDP method is outside this browser tool operation', 'method_denied');
    assert(!Object.hasOwn(params, 'browserContextId') && !Object.hasOwn(params, 'sessionId'), 'Page command cannot choose a browser context or protocol session', 'authority_denied');
    const chain = sessionChain(target, sessionId);
    assert(chain.every(entry => connection.seenSessions.get(entry.session_id) === entry.attachment_seq), 'Iframe attachment has not been observed by this connection', 'session_unavailable');
    if (sessionId !== null) assert(READINESS.includes(method) || ELEMENT.includes(method) || OBSERVE.includes(method) || method.startsWith('Input.'), 'Child session command is outside the frame slice', 'method_denied');
    if (method === 'Target.setAutoAttach') {
      object(params, ['autoAttach', 'flatten', 'waitForDebuggerOnStart', 'filter']);
      assert(params.autoAttach === true && params.flatten === true && params.waitForDebuggerOnStart === false && canonical(params.filter) === canonical(IFRAME_FILTER), 'Only owned iframe auto-attachment is permitted', 'authority_denied');
    }
    if (method === 'Page.navigate') validateUrl(params.url);
    if (method === 'Page.printToPDF') assert(params.transferMode === 'ReturnAsBase64', 'PDF must return bounded bytes');
    if (method === 'Input.dispatchKeyEvent') assert(keyCommands(params) && (!params.commands || ['key', 'locator_press'].includes(operation)), 'Only declared page editing commands belong to this input operation', 'method_denied');
    if (method.startsWith('Network.') && params.url) validateUrl(params.url);
    connection.targetId = targetId;
    if (target.modelActivitySeq !== body.source_movement_seq || !target.modelControlled) { claimModelTarget(target, body); publish(target); }
    await ensureDownloads(target);
    const contextId = method === 'DOM.resolveNode' ? params.executionContextId : method === 'Runtime.evaluate' ? params.contextId : method === 'Runtime.callFunctionOn' ? params.executionContextId : undefined;
    if (contextId !== undefined) {
      integer(contextId);
      assert(target.frameContexts.has(canonical([sessionId, contextId])), 'Execution context was not created in this owned frame route', 'authority_denied');
    }
    if (method === 'Page.createIsolatedWorld') {
      object(params, ['frameId', 'worldName', 'grantUniveralAccess']); identity(params.frameId);
      assert(params.grantUniveralAccess === false && params.worldName === 'octopus.browser.actions', 'Isolated frame world cannot grant universal access', 'authority_denied');
      const tree = await timed(target.contents.debugger.sendCommand('Page.getFrameTree', {}, sessionId ?? undefined), 'Frame validation timed out', connection.abort.signal);
      verifyChain(target, chain); invocations.validate(lease);
      const queue = [tree.frameTree]; const frames = new Set();
      for (let i = 0; i < queue.length; i++) {
        assert(i < FRAME_LIMIT && queue[i]?.frame && !frames.has(queue[i].frame.id), 'Invalid owned frame tree', 'session_unavailable');
        frames.add(queue[i].frame.id); queue.push(...(queue[i].childFrames ?? []));
      }
      assert(frames.has(params.frameId), 'Isolated world frame is outside this debugger route', 'authority_denied');
    }
    verifyChain(target, chain);
    if (method.startsWith('Input.')) {
      await timed(target.contents.debugger.sendCommand('Emulation.setFocusEmulationEnabled', { enabled: true }, sessionId ?? undefined), 'Browser input focus emulation timed out', connection.abort.signal);
      verifyChain(target, chain); invocations.validate(lease);
      assert(!connection.socket.destroyed, 'Native browser connection ended before input dispatch', 'execution_uncertain');
    }
    invocations.validate(lease); downloads.observe(target, body, method);
    const result = await timed(standard ? standard.command(target.contents, method, params, sessionId ?? undefined, () => {
      requireTarget(room, targetId); verifyChain(target, chain); invocations.validate(lease);
      assert(!connection.socket.destroyed && !connection.abort.signal.aborted, 'Browser connection ended before queued dispatch', 'execution_uncertain');
      if (method.startsWith('Input.') || method === 'Page.captureScreenshot') assert(target.viewportKnown, 'Applied browser viewport is unconfirmed', 'viewport_unconfirmed');
    }) : target.contents.debugger.sendCommand(method, params, sessionId ?? undefined), 'Browser command timed out after dispatch; the effect may have completed', connection.abort.signal);
    verifyChain(target, chain);
    if (method === 'Page.createIsolatedWorld') {
      integer(result.executionContextId);
      for (const [key, context] of target.frameContexts) if (context.sessionId === sessionId && context.frameId === params.frameId) target.frameContexts.delete(key);
      assert(target.frameContexts.size < FRAME_LIMIT, 'Isolated frame context inventory exceeds its boundary', 'session_unavailable');
      target.frameContexts.set(canonical([sessionId, result.executionContextId]), { sessionId, frameId: params.frameId });
    }
    return method === 'Page.getNavigationHistory' ? mapHistory(target, result) : result;
  }
  async function send(connection, message) {
    const bytes = Buffer.from(JSON.stringify(message));
    assert(bytes.length <= RESULT_LIMIT, 'Native browser response exceeds the aggregate byte limit', 'response_too_large');
    const total = bytes.length < LINE_LIMIT ? 1 : Math.ceil(bytes.length / CHUNK_BYTES);
    connection.writingResponse = true;
    try {
      for (let index = 0; index < total; index++) {
        const frame = total === 1 ? bytes : Buffer.from(JSON.stringify({ type: 'response_chunk', id: message.id, index, total, data: bytes.subarray(index * CHUNK_BYTES, (index + 1) * CHUNK_BYTES).toString('base64') }));
        assert(frame.length + 1 <= LINE_LIMIT, 'Native browser response frame exceeds line limit', 'response_too_large');
        assert(!connection.socket.destroyed, 'Native browser connection ended', 'execution_uncertain');
        if (!connection.socket.write(Buffer.concat([frame, Buffer.from('\n')]))) await timed(once(connection.socket, 'drain', { signal: connection.abort.signal }), 'Native browser response could not be delivered; effect may have completed', connection.abort.signal);
      }
    } finally {
      connection.writingResponse = false;
      for (const event of connection.events.splice(0)) {
        if (connection.socket.destroyed) break;
        if (connection.socket.writableLength + event.length > RESULT_LIMIT) { connection.socket.destroy(); break; }
        connection.socket.write(event);
      }
      connection.eventBytes = 0;
    }
  }
  function accept(socket) {
    const connection = { socket, abort: new AbortController(), challenge: randomId(), nextId: 1, room: null, targetId: null, seenSessions: new Map(), pending: 0, buffer: Buffer.alloc(0), chain: Promise.resolve(), writingResponse: false, events: [], eventBytes: 0 };
    connections.add(connection);
    socket.on('error', () => {});
    socket.on('close', () => { connection.abort.abort(); invocations.disconnect(connection); for (const member of connections) if (connection.executorRole === 'owner' && member !== connection && member.executionOwner === connection.executionOwner && member.executorInstance === connection.executorInstance) member.socket.destroy(); connections.delete(connection); connection.buffer = Buffer.alloc(0); connection.events = []; connection.eventBytes = 0; });
    socket.write(JSON.stringify({ type: 'challenge', schema_version: 1, host_generation: generation, challenge: connection.challenge, mac: hmac(key, `${PROTOCOL}.challenge\n${generation}\n${connection.challenge}`) }) + '\n');
    socket.on('data', bytes => {
      if (connection.buffer.length + bytes.length > LINE_LIMIT) { socket.destroy(); return; }
      connection.buffer = Buffer.concat([connection.buffer, bytes]);
      let end;
      while ((end = connection.buffer.indexOf(10)) !== -1) {
        const line = connection.buffer.subarray(0, end); connection.buffer = connection.buffer.subarray(end + 1);
        if (++connection.pending > QUEUE_LIMIT) { socket.destroy(); return; }
        connection.chain = connection.chain.then(async () => {
          if (socket.destroyed) return;
          let body;
          try {
            const wrapper = object(JSON.parse(line.toString('utf8')), ['payload', 'mac']);
            assert(typeof wrapper.payload === 'string' && wrapper.payload.length > 0, 'Missing signed native payload', 'authentication_failed');
            const payload = Buffer.from(wrapper.payload, 'base64');
            hex(wrapper.mac, 64);
            assert(payload.toString('base64') === wrapper.payload && crypto.timingSafeEqual(Buffer.from(wrapper.mac, 'hex'), Buffer.from(hmac(key, payload), 'hex')), 'Invalid native browser signature', 'authentication_failed');
            body = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(payload));
          } catch { socket.destroy(); return; }
          try {
            const lease = invocations.accept(body, connection), room = lease.room;
            let result;
            if (lease.kind === 'release') {
              if (room) await revoke(room, 'Exact broker release withdrew the formed room');
              result = { status: lease.status };
            } else if (lease.kind === 'begin') {
              const observed = room.modelTurn?.observedMovementSeq ?? 0;
              room.modelTurn = { ...room.modelTurn, ...(body.source_movement_seq > observed ? { open: true } : {}), lastToolSeq: Math.max(room.modelTurn?.lastToolSeq ?? 0, body.source_movement_seq) };
              result = { invocation_handle: lease.active.handle };
            } else if (lease.kind === 'end') {
              assert(lease.active.pending.count === 0, 'Invocation still has admitted work in flight', 'invocation_busy');
              const params = body.operation.params;
              assert(Object.keys(params).every(key => key === 'captured_wait'), 'Invalid invocation end fields');
              if (params.captured_wait) {
                object(params.captured_wait, ['wait_id', 'outcome_digest']);
                const capture = lease.active.captures.get(params.captured_wait.wait_id);
                assert(capture && capture.digest === params.captured_wait.outcome_digest, 'Terminal observation capture differs from the delivered result', 'authority_denied');
                room.waiters.capture(capture.waiter, lease.active.handle); lease.active.captures.delete(params.captured_wait.wait_id);
              }
              result = invocations.end(lease);
            } else {
              const target = body.operation.target_id === null ? null : requireTarget(room, body.operation.target_id);
              const epoch = target?.debuggerEpoch;
              assert(room.queued < QUEUE_LIMIT, 'Native room command queue is full', 'queue_capacity'); room.queued++; lease.active.pending.count++;
              const run = async () => {
                invocations.validate(lease);
                const result = await dispatch(body, room, connection, lease);
                invocations.validate(lease);
                if (target) assert(requireTarget(room, target.id) === target && target.debuggerEpoch === epoch, 'Native source changed after dispatch; its effect may have completed', 'execution_uncertain');
                return result;
              };
              try {
                const task = body.operation.method === 'OctopusBrowser.completeWait' ? run() : room.chain.then(run);
                if (body.operation.method !== 'OctopusBrowser.completeWait') room.chain = task.catch(() => {});
                result = await task;
              } finally { room.queued--; lease.active.pending.count--; }
            }
            const frame_inventory = lease.kind !== 'command' || body.operation.target_id === null ? undefined : frameInventory(requireTarget(room, body.operation.target_id));
            connection.seenSessions = new Map((frame_inventory?.sessions ?? []).map(entry => [entry.session_id, entry.attachment_seq]));
            await send(connection, { id: body.request_id, result, ...(body.operation?.session_id ? { sessionId: body.operation.session_id } : {}), ...(frame_inventory ? { frame_inventory } : {}) });
            if (lease.kind === 'release') socket.end();
          } catch (error) {
            if (!socket.destroyed) await send(connection, { id: Number.isSafeInteger(body?.request_id) ? body.request_id : null, ...(body?.operation?.session_id ? { sessionId: body.operation.session_id } : {}), error: { code: error.code || 'execution_failed', message: error.message } });
          }
        }).catch(() => socket.destroy()).finally(() => { connection.pending--; });
      }
    });
  }
  function resolveRoom(projection) {
    object(projection);
    assert(started && !stopped && projection.protocol === PROTOCOL && projection.host_generation === generation, 'Native browser host generation is unavailable', 'host_unavailable');
    const room = contexts.get(projection.browser_context_id);
    assert(room && room.handle === projection.engine_handle && room.formation.worker_environment_id === projection.worker_environment_id && room.formation.formation_pin === projection.formation_pin, 'Viewer projection does not identify an owned formation', 'authority_denied');
    ensureLive(room);
    return room;
  }
  const historyOwner = room => ({ workerEnvironmentId: room.formation.worker_environment_id, formationPin: room.formation.formation_pin });
  function resolveHistory(projection) {
    object(projection, ['protocol', 'worker_environment_id', 'formation_pin', 'host_generation']);
    assert(started && !stopped && projection.protocol === 'browser.native.downloads.v1' && projection.host_generation === generation, 'Native browser history generation is unavailable', 'host_unavailable');
    identity(projection.worker_environment_id);
    assert(typeof projection.formation_pin === 'string' && /^[a-f0-9]{64}$/.test(projection.formation_pin), 'Native browser history requires an exact formation pin');
    return { workerEnvironmentId: projection.worker_environment_id, formationPin: projection.formation_pin };
  }
  function historyValidation(projection, validateCurrent) {
    assert(typeof validateCurrent === 'function', 'Native browser history requires its current client source', 'authority_denied');
    return async () => { await validateCurrent(); resolveHistory(projection); };
  }
  function resolve(projection) { return requireTarget(resolveRoom(projection), projection.target_id); }
  function checkAttachment(attachment, projection = attachment.projection) {
    assert(attachments.get(attachment.id) === attachment && (attachment.target.attachment === attachment || attachment.target.pendingAttachment === attachment), 'Native browser presentation has been withdrawn', 'attachment_unavailable');
    assert(!attachment.sender.isDestroyed() && !attachment.window.isDestroyed() && attachment.window.webContents === attachment.sender && attachment.sender.mainFrame === attachment.mainFrame, 'Native browser embedder changed', 'attachment_unavailable');
    assert(destinations.get(attachment.sender)?.get(attachment.frameToken) === attachment && canonical(projection) === canonical(attachment.projection) && resolve(projection) === attachment.target, 'Native browser presentation no longer identifies this exact target', 'attachment_unavailable');
  }
  function owned(request, sender) {
    identity(request.attachmentId); identity(request.hostGeneration); integer(request.mountGeneration);
    assert(request.mountGeneration <= 2147483647 && request.hostGeneration === generation, 'Native browser host generation is unavailable', 'host_unavailable');
    const attachment = attachments.get(request.attachmentId);
    assert(attachment && attachment.sender === sender && attachment.mountGeneration === request.mountGeneration, 'Native browser attachment is unavailable to this sender or generation', 'attachment_unavailable');
    checkAttachment(attachment);
    return attachment;
  }
  function windowVisible(attachment) { return !attachment.window.isDestroyed() && attachment.window.isVisible() && !attachment.window.isMinimized(); }
  function applyRequestedFocus(attachment) {
    const target = attachment.target, intent = target.room.openIntent;
    if (!target.focusIntentId || intent?.id !== target.focusIntentId || intent.targetId !== target.id || intent.source !== 'pip'
      || attachment.phase !== 'ready' || !attachment.visible || !windowVisible(attachment)) return;
    checkAttachment(attachment); target.focusIntentId = null;
    attachment.window.focus(); target.contents.focus();
  }
  function packetVisible(attachment, packet) {
    return !!packet && packet.visible && windowVisible(attachment) && ['stageBounds', 'pageBounds', 'visualBounds'].every(name => packet[name].width > 0 && packet[name].height > 0);
  }
  function nativeVisibility(attachment, visible) {
    const wc = attachment.target.contents;
    if (standard) {
      assert(attachment.target.attachment === attachment && attachment.phase !== 'prepared', 'Standard browser presentation is not attached', 'attachment_unavailable');
      standard.visible(attachment, visible);
    } else {
      assert(attachment.target.attachment === attachment && attachment.phase !== 'prepared' && wc.hostWebContents === attachment.sender && wc.setOwnedGuestVisibility(attachment.mountGeneration, visible) === true, 'Native browser adoption is unavailable for this generation', 'attachment_unavailable');
    }
    checkAttachment(attachment);
    if (visible && attachment.target.lifetime === 'temporary') {
      attachment.target.lifetime = 'persistent';
      publishTargets(attachment.room);
    }
  }
  function presentationPacket(packet) {
    object(packet, PRESENTATION_FIELDS);
    identity(packet.attachmentId); identity(packet.hostGeneration); integer(packet.mountGeneration); integer(packet.viewportSequence);
    assert(Number.isSafeInteger(packet.metricsRevision) && packet.metricsRevision >= 0, 'Invalid browser metrics revision');
    for (const name of ['stageBounds', 'pageBounds', 'visualBounds']) {
      const rect = object(packet[name], ['x', 'y', 'width', 'height']);
      assert(Object.values(rect).every(value => typeof value === 'number' && Number.isFinite(value)) && rect.width >= 0 && rect.height >= 0 && Number.isFinite(rect.x + rect.width) && Number.isFinite(rect.y + rect.height), 'Invalid browser presentation CSS rectangle');
    }
    assert(Number.isFinite(packet.scale) && packet.scale > 0 && typeof packet.visible === 'boolean', 'Invalid browser presentation scale or visibility');
    return { ...packet, stageBounds: { ...packet.stageBounds }, pageBounds: { ...packet.pageBounds }, visualBounds: { ...packet.visualBounds } };
  }
  function layoutPresentation({ projection, sender, ...request }, acknowledge) {
    const packet = presentationPacket(request);
    const attachment = owned(packet, sender);
    const exactProjection = { ...projection };
    checkAttachment(attachment, exactProjection);
    const task = attachment.layout.then(async () => {
      checkAttachment(attachment, exactProjection);
      await attachment.room.chain;
      checkAttachment(attachment, exactProjection);
      assert(attachment.target.viewportKnown, 'Applied browser viewport is unconfirmed', 'viewport_unconfirmed');
      assert(packet.metricsRevision === attachment.target.modelViewport.revision, 'Presentation does not reference the applied model viewport revision', 'metrics_revision_stale');
      assert(packet.viewportSequence >= attachment.viewportSequence, 'Browser presentation layout is stale', 'viewport_sequence_stale');
      const repeated = packet.viewportSequence === attachment.viewportSequence;
      if (repeated) {
        assert(canonical(packet) === canonical(attachment.packet), 'Browser presentation layout changed under an existing sequence', 'viewport_sequence_conflict');
        if (!acknowledge || attachment.phase === 'ready') return state(attachment);
      }
      if (acknowledge) nativeVisibility(attachment, false);
      if (standard) { attachment.visible = false; await standard.layout(attachment, packet, () => {
        checkAttachment(attachment, exactProjection);
        assert(attachment.target.viewportKnown && packet.metricsRevision === attachment.target.modelViewport.revision, 'Presentation viewport changed before layout dispatch', 'metrics_revision_stale');
      }); }
      const ready = acknowledge || attachment.phase === 'ready';
      const visible = ready && attachment.target.visibilityIntent?.visible !== false && packetVisible(attachment, packet);
      if (attachment.phase !== 'prepared') nativeVisibility(attachment, visible);
      checkAttachment(attachment, exactProjection);
      if (acknowledge) { attachment.phase = 'ready'; clearTimeout(attachment.timer); }
      attachment.packet = packet; attachment.viewportSequence = packet.viewportSequence; attachment.visible = visible;
      applyRequestedFocus(attachment);
      if (!visible) attachment.target.cursorWaiter?.finish('unpresented');
      publish(attachment.target);
      return state(attachment);
    });
    attachment.layout = task.then(() => {}, () => {});
    return task;
  }
  return {
    getEngineInfo() { return engine; },
    async start() {
      assert(!started && !stopped, 'Native browser host already started or stopped');
      await privateRoot(hostRoot);
      if (humanDownloads) await humanDownloads.initialize();
      generation = randomId(); key = crypto.randomBytes(32);
      socketPath = path.join(hostRoot, 'engine.sock');
      assert(Buffer.byteLength(socketPath) < 104, 'Native browser socket path exceeds macOS UDS length');
      await removeStaleSocket(socketPath);
      server = net.createServer(accept);
      await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socketPath, () => { server.removeListener('error', reject); resolve(); }); });
      await fs.chmod(socketPath, 0o600);
      try {
        await privateWrite(path.join(hostRoot, 'auth.key'), key);
        const descriptor = { schema_version: 1, host_generation: generation, socket_path: socketPath };
        await privateWrite(path.join(hostRoot, 'host.json'), JSON.stringify(descriptor));
        started = true; return descriptor;
      } catch (error) { for (const connection of connections) connection.socket.destroy(); await new Promise(resolve => server.close(resolve)); await unlinkOwned(socketPath); throw error; }
    },
    async savedPermissions(request, revalidate) {
      assert(typeof revalidate === 'function', 'Browser settings require their current client owner');
      object(request, request.operation === 'list' ? ['operation'] : ['operation', 'hostGeneration', 'id']);
      assert(['list', 'remove'].includes(request.operation), 'Unknown browser settings action');
      if (request.operation === 'remove') assert(request.hostGeneration === generation, 'Browser settings host was replaced');
      const current = async () => {
        assert(started && !stopped && server?.listening, 'Browser settings host is unavailable');
        await revalidate();
        assert(started && !stopped && server?.listening, 'Browser settings host stopped during the request');
      };
      const result = request.operation === 'list' ? await permissions.list(current) : await permissions.remove(request.id, current);
      await current(); return { hostGeneration: generation, ...result };
    },
    getTarget(projection) { const target = resolve(projection); return { engine, ...targetInfo(target), engine_handle: target.room.handle, host_generation: generation, viewport_confirmed: target.viewportKnown }; },
    listTargetsForProjection(projection) {
      object(projection, ['protocol', 'worker_environment_id', 'formation_pin', 'engine_handle', 'host_generation', 'browser_context_id']);
      const room = resolveRoom(projection);
      return { engine, targets: [...room.targets.values()].map(target => {
        requireTarget(room, target.id);
        return { target_id: target.id, browser_context_id: room.contextId, downloads: downloads.list(target), ...displayPage(target), ...tabOwnership(target), ...(target.openerId ? { opener_id: target.openerId } : {}) };
      }), openIntent: room.openIntent ? { ...room.openIntent } : null, downloads: humanDownloads ? humanDownloads.snapshot(historyOwner(room)) : downloadSnapshot() };
    },
    async listDownloadHistoryForProjection(projection) {
      object(projection, ['worker_environment_id', 'formation_pin']);
      const owner = resolveHistory({ ...projection, protocol: 'browser.native.downloads.v1', host_generation: generation });
      if (!humanDownloads) return { engine, host_generation: generation, downloads: downloadSnapshot() };
      await humanDownloads.ready(owner);
      resolveHistory({ ...projection, protocol: 'browser.native.downloads.v1', host_generation: generation });
      return { host_generation: generation, downloads: humanDownloads.snapshot(owner) };
    },
    async acceptDownloadArtifact(projection, body, validateCurrent) {
      const owner = resolveHistory(projection), validate = historyValidation(projection, validateCurrent);
      if (!humanDownloads) throw unavailable('Persistent download history');
      const result = await humanDownloads.acceptArtifact(owner, structuredClone(body), validate); await validate(); return result;
    },
    async setDownloadPreferences(projection, changes, validateCurrent) {
      const owner = resolveHistory(projection), validate = historyValidation(projection, validateCurrent);
      if (!humanDownloads) throw unavailable('Persistent download history');
      const snapshot = await humanDownloads.setPreferences(owner, changes, validate);
      await validate(); return snapshot;
    },
    async downloadAction(projection, action, id, query, validateCurrent) {
      const owner = resolveHistory(projection), validate = historyValidation(projection, validateCurrent);
      if (!humanDownloads) throw unavailable('Persistent download history');
      const snapshot = await humanDownloads.action(owner, action, id, structuredClone(query), validate);
      await validate(); return snapshot;
    },
    syncTurn(projection, turn) {
      const room = resolveRoom(projection);
      object(turn, ['open', 'openingMovementSeq', 'observedMovementSeq']);
      assert(typeof turn.open === 'boolean' && Number.isSafeInteger(turn.openingMovementSeq) && turn.openingMovementSeq >= 0 && Number.isSafeInteger(turn.observedMovementSeq) && turn.observedMovementSeq >= 0, 'Invalid browser turn projection');
      if (turn.observedMovementSeq < room.seq || turn.observedMovementSeq < (room.modelTurn?.observedMovementSeq ?? 0)) return;
      const changed = canonical(room.modelTurn) !== canonical({ ...room.modelTurn, ...turn });
      room.modelTurn = { ...room.modelTurn, ...turn };
      if (changed) for (const target of room.targets.values()) {
        if (!turn.open) { target.cursorWaiter?.finish('unpresented'); target.cursor = null; }
        publish(target);
      }
      if (!turn.open) {
        const completion = room.chain.then(() => completeModelTurn(room, turn));
        room.chain = completion.catch(() => {});
        return completion;
      }
    },
    cursorArrived({ projection, sender, ...request }) {
      object(request, ['attachmentId', 'hostGeneration', 'mountGeneration', 'moveSequence', 'metricsRevision']);
      integer(request.moveSequence);
      assert(Number.isSafeInteger(request.metricsRevision) && request.metricsRevision >= 0, 'Invalid cursor metrics revision');
      const attachment = owned(request, sender); checkAttachment(attachment, projection);
      const target = attachment.target, waiter = target.cursorWaiter;
      if (!waiter || waiter.attachment !== attachment || waiter.moveSequence !== request.moveSequence || waiter.metricsRevision !== request.metricsRevision
        || request.metricsRevision !== target.modelViewport.revision || !modelActive(target) || attachment.phase !== 'ready' || !attachment.visible || !windowVisible(attachment)) return { accepted: false };
      waiter.finish('arrived'); return { accepted: true };
    },
    requestPageFocus(projection) {
      const target = resolve(projection), room = target.room;
      assert(Number.isSafeInteger(room.presentationIntentId + 1), 'Browser presentation intent sequence exhausted');
      room.presentationIntentId += 1;
      room.openIntent = { id: room.presentationIntentId, targetId: target.id, source: 'pip', activate: true };
      target.focusIntentId = room.presentationIntentId;
      if (target.attachment?.phase === 'ready' && target.attachment.packet?.visible) {
        target.attachment.window.show(); applyRequestedFocus(target.attachment);
      }
      publishTargets(room);
    },
    async closeTarget(projection) { const target = resolve(projection); await Promise.allSettled([...(target.room.clipboardCleanup ?? [])].map(cleanup => cleanup(target.contents.id))); resolve(projection); await destroyTarget(target, 'Browser page closed by the user'); },
    preparePresentation({ attachmentId, projection, window, sender, surfaceId, frameToken }) {
      identity(attachmentId); identity(surfaceId); identity(frameToken);
      object(projection, ['protocol', 'worker_environment_id', 'formation_pin', 'engine_handle', 'host_generation', 'browser_context_id', 'target_id']);
      assert(!attachments.has(attachmentId), 'Native browser attachment already exists');
      assert(window && !window.isDestroyed() && window.webContents === sender && !sender.isDestroyed(), 'Native browser sender does not own its window', 'authority_denied');
      assert(sender.mainFrame && !sender.mainFrame.detached, 'Native browser embedder frame is unavailable', 'attachment_unavailable');
      const target = resolve(projection);
      assert(!destinations.get(sender)?.has(frameToken), 'Native browser destination frame already has a presentation owner', 'presentation_owned');
      assert(mountGeneration < 2147483647, 'Native browser mount generation exhausted', 'host_unavailable');
      const attachment = { id: attachmentId, room: target.room, target, window, sender, mainFrame: sender.mainFrame, projection: { ...projection }, mountGeneration: ++mountGeneration, surfaceId, frameToken, phase: 'prepared', visible: false, viewportSequence: 0, packet: null, layout: Promise.resolve(), adoptionToken: null };
      attachment.onDestroyed = () => endAttachment(attachment);
      attachment.onNavigation = (_event, _url, isInPlace, isMainFrame) => { if (isMainFrame && !isInPlace) endAttachment(attachment); };
      attachment.onVisibility = () => {
        try {
          checkAttachment(attachment);
          const visible = attachment.phase === 'ready' && attachment.target.visibilityIntent?.visible !== false && packetVisible(attachment, attachment.packet);
          if (attachment.phase !== 'prepared') nativeVisibility(attachment, visible);
          attachment.visible = visible;
          applyRequestedFocus(attachment);
          publish(target);
        } catch (error) { endAttachment(attachment, { code: error.code || 'attachment_unavailable', message: error.message }); }
      };
      try {
        if (standard) {
          if (target.pendingAttachment) endAttachment(target.pendingAttachment);
          if (target.attachment) endAttachment(target.attachment);
        } else {
          attachment.adoptionToken = target.contents.createOwnedGuestLease(sender, frameToken, attachment.mountGeneration);
          assert(typeof attachment.adoptionToken === 'string' && attachment.adoptionToken.length > 0, 'Native engine refused the destination frame lease', 'attachment_unavailable');
        }
        const previous = target.pendingAttachment;
        attachments.set(attachmentId, attachment); target.pendingAttachment = attachment;
        if (standard) {
          standard.mount(attachment);
          target.pendingAttachment = null; target.attachment = attachment; attachment.phase = 'attached';
        }
        if (!destinations.has(sender)) destinations.set(sender, new Map());
        destinations.get(sender).set(frameToken, attachment);
        sender.once('destroyed', attachment.onDestroyed); sender.on('render-process-gone', attachment.onDestroyed); sender.on('did-start-navigation', attachment.onNavigation);
        window.once('closed', attachment.onDestroyed);
        for (const name of ['show', 'hide', 'minimize', 'restore']) window.on(name, attachment.onVisibility);
        if (previous) endAttachment(previous);
        checkAttachment(attachment);
        attachment.timer = setTimeout(() => endAttachment(attachment, { code: 'attachment_expired', message: 'Native browser presentation lease expired before attachment' }), LEASE_TIMEOUT);
        attachment.timer.unref();
        return { adoptionToken: attachment.adoptionToken, hostGeneration: generation, mountGeneration: attachment.mountGeneration, state: state(attachment) };
      } catch (error) {
        if (attachments.get(attachmentId) === attachment) endAttachment(attachment);
        else if (!standard && attachment.adoptionToken && !target.contents.isDestroyed()) {
          target.contents.cancelOwnedGuestLease(attachment.adoptionToken);
          target.contents.releaseOwnedGuest(attachment.mountGeneration);
        }
        throw error;
      }
    },
    validatePreparedGuest({ sender, guest }) {
      if (standard) return false;
      const target = guestTargets.get(guest);
      const attachment = target?.pendingAttachment;
      if (!attachment || attachment.sender !== sender || attachment.phase !== 'prepared') return false;
      try { checkAttachment(attachment); return guest.isOwnedGuest() === true; } catch { return false; }
    },
    acknowledgePresentation(request) { return layoutPresentation(request, true); },
    syncPresentation(request) { return layoutPresentation(request, false); },
    releasePresentation({ attachmentId, sender, hostGeneration, mountGeneration }) {
      identity(attachmentId); identity(hostGeneration); integer(mountGeneration);
      assert(mountGeneration <= 2147483647, 'Invalid native browser mount generation');
      if (hostGeneration !== generation) return;
      const attachment = attachments.get(attachmentId);
      if (!attachment || attachment.mountGeneration !== mountGeneration) return;
      assert(attachment.sender === sender, 'Native browser presentation belongs to another sender', 'attachment_unavailable');
      endAttachment(attachment);
    },
    async viewerAction({ attachmentId, sender, hostGeneration, mountGeneration, action, params = {} }) {
      const epochs = { attachmentId, hostGeneration, mountGeneration };
      const attachment = owned(epochs, sender); object(params);
      assert(attachment.phase === 'ready' && attachment.target.attachment === attachment && presentationAttached(attachment), 'Native browser presentation has not acknowledged its viewport', 'attachment_unavailable');
      const wc = attachment.target.contents;
      if (action === 'find') {
        object(params, ['query', 'backwards', 'matchCase']);
        assert(typeof params.query === 'string' && params.query.length <= 10000 && typeof params.backwards === 'boolean' && typeof params.matchCase === 'boolean', 'Invalid browser find request');
        if (params.query) attachment.findRequestId = wc.findInPage(params.query, { forward: !params.backwards, matchCase: params.matchCase });
        else { wc.stopFindInPage('clearSelection'); attachment.findRequestId = null; attachment.findResult = {}; }
      }
      else {
        switch (action) {
          case 'navigate': object(params, ['url']); await wc.loadURL(validateUrl(params.url)); break;
          case 'reload': object(params, []); await reloadPage(attachment.target); break;
          case 'back': object(params, []); if (wc.navigationHistory.canGoBack()) wc.navigationHistory.goBack(); break;
          case 'forward': object(params, []); if (wc.navigationHistory.canGoForward()) wc.navigationHistory.goForward(); break;
          case 'stop': object(params, []); wc.stop(); break;
          case 'focus': object(params, []); wc.focus(); break;
          case 'zoom': object(params, ['zoomFactor']); assert(Number.isFinite(params.zoomFactor) && params.zoomFactor >= 0.25 && params.zoomFactor <= 5, 'Invalid browser zoom'); wc.setZoomFactor(params.zoomFactor); break;
          default: throw failure('action_denied', 'Unsupported manual browser action');
        }
      }
      assert(owned(epochs, sender) === attachment, 'Native browser presentation changed after the action; effect may have completed', 'execution_uncertain');
      publish(attachment.target); return state(attachment);
    },
    async revokeFormation({ worker_environment_id, formation_pin, reason }) {
      const room = rooms.get(worker_environment_id);
      assert(room && room.formation.formation_pin === formation_pin, 'Native browser formation is unavailable', 'formation_unavailable');
      await revoke(room, identity(reason));
    },
    async stop() {
      if (stopped) return;
      stopped = true;
      const results = await Promise.allSettled([...rooms.values()].map(room => revoke(room, 'Native browser host stopped')));
      if (standard) standard.stop();
      for (const connection of connections) connection.socket.destroy();
      if (humanDownloads) await humanDownloads.stop();
      if (server?.listening) await new Promise(resolve => server.close(resolve));
      if (started) { await unlinkOwned(socketPath); await unlinkOwned(path.join(hostRoot, 'host.json')); await unlinkOwned(path.join(hostRoot, 'auth.key')); }
      if (key) key.fill(0);
      const rejected = results.find(result => result.status === 'rejected');
      if (rejected) throw rejected.reason;
    },
  };
}

module.exports = { createBrowserEngineHost };
