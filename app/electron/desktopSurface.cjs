const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const { isDeepStrictEqual } = require("node:util");
const path = require("node:path");
const { randomUUID, createHash } = require("node:crypto");

function createDesktopSurfaceHost(options) {
  if (typeof options?.resolveSurface !== "function" || typeof options?.homePath !== "function") throw new Error("Desktop surface requires exact projected surface and Home resolvers");
  const views = new Map();
  const closedOwners = new Set();
  const ownerEpochs = new Map();

  async function request(raw, ownerId) {
    const input = validateInput(raw);
    if (!Number.isSafeInteger(ownerId) || ownerId <= 0 || closedOwners.has(ownerId)) throw new Error("Desktop viewer owner is unavailable");
    let record = views.get(input.viewId);
    if (record && (record.ownerId !== ownerId || !sameTarget(record.input, input))) throw new Error("Desktop view belongs to another owner or room");
    if (input.operation === "stop") {
      if (record) record.controlEpoch += 1;
      const epoch = ownerEpochs.get(ownerId);
      const requireOwner = () => {
        if (closedOwners.has(ownerId) || ownerEpochs.get(ownerId) !== epoch) throw new Error("Desktop owner changed during Stop admission");
        const currentView = views.get(input.viewId);
        if (currentView && (currentView.ownerId !== ownerId || !sameTarget(currentView.input, input))) throw new Error("Desktop view belongs to another owner or room");
      };
      const connection = await admit(input);
      await current(connection, input);
      requireOwner();
      const evidence = await exchange(connection, { wireId: randomUUID() }, "stop", input);
      await current(connection, input);
      requireOwner();
      if (evidence.control.mode !== "stopped") throw new Error("Desktop Stop response did not confirm stopped control");
      return project(input, evidence);
    }
    if (input.operation === "pause_view") {
      if (!record) throw new Error("Desktop view is not attached to this owner");
      return pause(record, true);
    }
    if (record?.closed) throw new Error("Desktop view attachment was closed; attach a new view identity");
    if (!record) {
      if (input.operation !== "view") throw new Error("Inspect the desktop before requesting control");
      record = { input, ownerId, wireId: randomUUID(), closed: false, started: false, connection: null, pausePromise: null, controlEpoch: 0 };
      views.set(input.viewId, record);
      record.ready = admit(input).then((connection) => { record.connection = connection; return connection; });
    }
    const controlEpoch = record.controlEpoch;
    const requireCurrentControl = () => {
      if (input.operation !== "view" && record.controlEpoch !== controlEpoch) throw new Error("Desktop control request superseded by Stop");
    };
    try {
      const connection = await record.ready;
      if (record.closed || closedOwners.has(ownerId)) throw new Error("Desktop viewer owner closed during attachment");
      await current(connection, input);
      if (record.closed) throw new Error("Desktop view attachment was closed");
      requireCurrentControl();
      const type = input.operation === "view" && !input.screenId ? "status" : input.operation;
      record.started = true;
      const evidence = await exchange(connection, record, type, input);
      await current(connection, input);
      if (record.closed) throw new Error("Desktop view attachment was closed during request");
      requireCurrentControl();
      return project(input, evidence);
    } catch (error) {
      try { await pause(record); } catch (cleanup) { throw new AggregateError([error, cleanup], "Desktop request failed and viewer cleanup failed"); }
      throw error;
    }
  }

  async function closeOwner(ownerId) {
    closedOwners.add(ownerId);
    await pauseOwner(ownerId);
    for (const [id, record] of views) if (record.ownerId === ownerId) views.delete(id);
  }

  async function pauseOwner(ownerId) {
    ownerEpochs.set(ownerId, (ownerEpochs.get(ownerId) || 0) + 1);
    const records = [...views.values()].filter((record) => record.ownerId === ownerId);
    const results = await Promise.allSettled(records.map((record) => pause(record, true)));
    const failures = results.filter((result) => result.status === "rejected").map((result) => result.reason);
    if (failures.length) throw new AggregateError(failures, "Desktop owner viewer detach failed");
  }

  function pause(record, retry = false) {
    record.closed = true;
    if (retry && record.pauseFailed) { record.pausePromise = null; record.pauseFailed = false; }
    if (record.pausePromise) return record.pausePromise;
    record.pausePromise = (async () => {
      const receipt = { workerEnvironmentId: record.input.workerEnvironmentId, surfaceId: record.input.surfaceId, runId: record.input.runId, paused: true };
      try { await record.ready; } catch { record.ready = Promise.resolve(null); return receipt; }
      if (!record.started || !record.connection) { record.connection = null; record.ready = Promise.resolve(null); return receipt; }
      if (record.connection.native) await validateNativeConnection(record.connection);
      await exchange(record.connection, record, "pause_view", record.input);
      record.started = false;
      record.connection = null;
      record.ready = Promise.resolve(null);
      return receipt;
    })().catch((error) => { record.pauseFailed = true; throw error; });
    return record.pausePromise;
  }

  async function admit(input) {
    const descriptor = await options.resolveSurface(input.workerEnvironmentId, input.surfaceId);
    validateSurface(descriptor, input);
    const door = required(descriptor.doorInterfaceId, "projected desktop door");
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(door)) throw new Error("Invalid projected desktop door");
    const home = path.resolve(options.homePath());
    const configPath = path.join(home, "registry", "interface", door, "desktop.json");
    const configGeneration = await fileGeneration([configPath]);
    const config = await readJson(configPath);
    if (config.schema_version !== 1 || config.auth_header !== "Authorization" || config.auth_value_prefix !== "Bearer ") throw new Error("Desktop connection differs from its exact descriptor");
    if (!Number.isSafeInteger(config.request_timeout_ms) || config.request_timeout_ms <= 0 || config.request_timeout_ms > 600000) throw new Error("Desktop request timeout is not declared within its supported bound");
    if (config.host != null) {
      if (typeof config.host !== "object" || Array.isArray(config.host) || config.host.connection_file !== "desktop-host/connection.json"
        || Object.hasOwn(config, "auth_env_key") || !config.headers || typeof config.headers !== "object" || Array.isArray(config.headers) || Object.keys(config.headers).length
        || config.action_url !== "http://127.0.0.1:0/v1/action" || config.viewer_url !== "http://127.0.0.1:0/view") throw new Error("Desktop native host connection is not declared exactly");
      const privateRoot = path.join(home, "interfaces", door);
      const hostRoot = path.join(privateRoot, "desktop-host");
      const connectionPath = path.join(hostRoot, "connection.json");
      const paths = [configPath, connectionPath];
      const generation = await fileGeneration(paths);
      await requirePrivateDirectory(privateRoot); await requirePrivateDirectory(hostRoot);
      const manifest = await readNativeConnection(connectionPath);
      const allowed = ["schema_version", "interface", "host_generation", "action_url", "viewer_url", "token"];
      if (Object.keys(manifest).some((key) => !allowed.includes(key)) || manifest.schema_version !== 1 || manifest.interface !== door
        || typeof manifest.host_generation !== "string" || typeof manifest.token !== "string" || !/^[a-f0-9]{32}$/.test(manifest.host_generation) || !/^[a-f0-9]{64}$/.test(manifest.token)) throw new Error("Desktop private connection identity is invalid");
      const base = nativeUrl(manifest.action_url, "/v1/action"), viewer = nativeUrl(manifest.viewer_url, "/view");
      if (viewer.origin !== base.origin || descriptor.viewerUrl !== manifest.viewer_url || descriptor.data?.host_generation !== manifest.host_generation) throw new Error("Desktop native host differs from the projected generation or origin");
      const connection = { descriptor, home, base, secret: manifest.token, native: true, generation, paths, privateDirectories: [privateRoot, hostRoot], timeout: config.request_timeout_ms, responseLimit: config.max_response_bytes, imageSha256: null };
      await validateNativeConnection(connection);
      if (configGeneration !== await fileGeneration([configPath])) throw new Error("Desktop native connection changed during admission");
      return connection;
    }
    const privatePath = path.join(home, "interfaces", door, "environment.json");
    const paths = [configPath, path.dirname(privatePath), privatePath];
    const generation = await fileGeneration(paths);
    if (config.viewer_url !== descriptor.viewerUrl) throw new Error("Desktop connection differs from its exact descriptor");
    const base = new URL(config.action_url), viewer = new URL(config.viewer_url);
    if (!["127.0.0.1", "[::1]"].includes(base.hostname) || base.protocol !== "http:" || base.pathname !== "/v1/action" || base.username || base.password || base.search || base.hash || viewer.origin !== base.origin) throw new Error("Native desktop service must use its declared loopback origin");
    const key = required(config.auth_env_key, "desktop authentication key");
    if (!/^[A-Z_][A-Z0-9_]*$/i.test(key)) throw new Error("Invalid desktop authentication key");
    let privateValue;
    const privateStat = await fs.lstat(privatePath).catch((error) => { if (error.code === "ENOENT") return null; throw error; });
    if (privateStat) {
      const directory = await fs.lstat(path.dirname(privatePath));
      if (!directory.isDirectory() || directory.isSymbolicLink() || directory.mode & 0o077 || !privateStat.isFile() || privateStat.isSymbolicLink() || privateStat.mode & 0o077 || privateStat.size > 1024 * 1024 || (process.getuid && (privateStat.uid !== process.getuid() || directory.uid !== process.getuid()))) throw new Error("Desktop private environment must be an owner-only regular file in its private directory");
      const value = await readJson(privatePath);
      if (value.schema_version !== 1 || value.interface !== door || !value.values || typeof value.values !== "object" || Array.isArray(value.values)) throw new Error("Desktop private environment identity is invalid");
      privateValue = value.values[key];
    }
    const ambient = environment()[key];
    if (ambient !== undefined && privateValue !== undefined && ambient !== privateValue) throw new Error("Desktop ambient and private authentication differ");
    const secret = privateValue ?? ambient;
    if (typeof secret !== "string" || secret.length < 32 || /[\r\n\0]/.test(secret)) throw new Error("Desktop authentication is not configured");
    if (configGeneration !== await fileGeneration([configPath]) || generation !== await fileGeneration(paths)) throw new Error("Desktop connection files changed during admission");
    return { descriptor, home, base, secret, key, ambient, generation, paths, timeout: config.request_timeout_ms, imageSha256: null };
  }

  function environment() { return options.environment ? options.environment() : process.env; }
  async function current(connection, input) {
    const descriptor = await options.resolveSurface(input.workerEnvironmentId, input.surfaceId);
    validateSurface(descriptor, input);
    if (surfaceIdentity(descriptor) !== surfaceIdentity(connection.descriptor) || path.resolve(options.homePath()) !== connection.home) throw new Error("Desktop surface changed; attach a new view");
    if (descriptor.readOnly && !["view", "pause_view"].includes(input.operation)) throw new Error("Desktop surface is read-only");
    if (connection.native) await validateNativeConnection(connection);
    else if (connection.ambient !== environment()[connection.key] || connection.generation !== await fileGeneration(connection.paths)) throw new Error("Desktop connection configuration changed; attach a new view");
  }

  async function exchange(connection, record, type, input) {
    const body = { schema_version: 2, worker_environment_id: input.workerEnvironmentId, request_id: randomUUID(), tool: "operator.desktop", action: { type, view_id: record.wireId, ...(input.screenId ? { screen_id: input.screenId } : {}), ...(["take_control", "release_control"].includes(type) ? { control_generation: input.controlGeneration } : {}), ...(type === "view" && connection.imageSha256 ? { known_image_sha256: connection.imageSha256 } : {}) } };
    const url = new URL(type === "view" || type === "status" ? "/v1/view" : "/v1/operator", connection.base);
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(new Error("Desktop service exceeded its declared request timeout")), connection.timeout);
    try {
      const response = await (options.fetch || fetch)(url, { method: "POST", redirect: "error", signal: abort.signal, headers: { "Content-Type": "application/json", Authorization: `Bearer ${connection.secret}` }, body: JSON.stringify(body) });
      if (!response.ok) throw new Error(`Desktop service returned HTTP ${response.status}`);
      const reader = response.body?.getReader();
      if (!reader) throw new Error("Desktop service returned no body");
      const parts = []; let length = 0;
      try {
        while (true) {
          const chunk = await reader.read(); if (chunk.done) break;
          length += chunk.value.byteLength;
          if (length > 48 * 1024 * 1024) throw new Error("Desktop response exceeded its evidence limit");
          parts.push(Buffer.from(chunk.value));
        }
      } finally { await reader.cancel().catch(() => {}); }
      const raw = Buffer.concat(parts).toString("utf8");
      if (raw.includes(connection.secret)) throw new Error("Desktop service reflected authentication material");
      const value = JSON.parse(raw);
      if (value.schema_version !== 2 || value.worker_environment_id !== body.worker_environment_id || value.request_id !== body.request_id || value.operation !== type || typeof value.accepted !== "boolean") throw new Error("Desktop service returned mismatched room/request identity");
      if (!value.accepted) throw new Error(`${value.error?.code || "desktop_refused"}: ${value.error?.message || "No failure evidence"}`);
      if (value.error != null) throw new Error("Desktop service accepted an action while reporting an error");
      const evidence = validateEvidence(value.evidence);
      const observation = evidence.observation;
      if (type === "view" && observation?.screen.id !== input.screenId) throw new Error("Desktop viewer returned another display");
      if (evidence.view_image_ref && (type !== "view" || evidence.view_image_ref.sha256 !== connection.imageSha256)) throw new Error("Desktop image reference is not pinned to this attachment");
      if (type === "view") connection.imageSha256 = observation?.image?.sha256 || evidence.view_image_ref?.sha256 || null;
      return evidence;
    } finally { clearTimeout(timer); }
  }
  async function preparePresentationSource(raw) {
    const fields = ['workerEnvironmentId', 'surfaceId', 'runId', 'appInstanceId', 'sourceWindowId'];
    if (!raw || typeof raw !== 'object' || Array.isArray(raw) || Object.keys(raw).length !== fields.length || Object.keys(raw).some(key => !fields.includes(key))) throw new Error('Invalid native presentation source request');
    for (const field of fields) required(raw[field], field);
    const input = { ...raw, operation: 'view' }, connection = await admit(input);
    if (!connection.native) throw new Error('Native presentation requires the exact private native host');
    const captured = presentationSource(connection.descriptor, raw);
    const check = async () => {
      await current(connection, input);
      const descriptor = await options.resolveSurface(raw.workerEnvironmentId, raw.surfaceId);
      const actual = presentationSource(descriptor, raw);
      if (JSON.stringify(actual.identity) !== JSON.stringify(captured.identity)) throw new Error('Native presentation source was withdrawn or replaced');
    };
    await check();
    const complete = async (publication, signal) => {
      await check();
      if (!publication || typeof publication.presentationId !== 'string' || !publication.presentationId
        || !Number.isSafeInteger(publication.streamGeneration) || publication.streamGeneration <= 0
        || !/^[a-f0-9]{64}$/.test(publication.receiverInstanceNonce || '')) throw new Error('Native presentation completion identity is invalid');
      const body = { schema_version: 2, worker_environment_id: raw.workerEnvironmentId, request_id: randomUUID(), tool: 'operator.desktop',
        action: { type: 'presentation_complete', scope: captured.scope, source_window_id: raw.sourceWindowId,
          presentation_id: publication.presentationId, stream_generation: publication.streamGeneration, receiver_instance_nonce: publication.receiverInstanceNonce } };
      const abort = new AbortController(), cancel = () => abort.abort(new Error('Native presentation completion source was withdrawn'));
      const timer = setTimeout(() => abort.abort(new Error('Native presentation completion outcome is uncertain after its declared timeout')), connection.timeout);
      signal?.addEventListener('abort', cancel, { once: true }); if (signal?.aborted) cancel();
      try {
        const response = await (options.fetch || fetch)(new URL('/v1/operator', connection.base), { method: 'POST', redirect: 'error', signal: abort.signal,
          headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${connection.secret}` }, body: JSON.stringify(body) });
        if (!response.ok || !response.body) throw new Error('Native presentation completion private host response failed');
        const reader = response.body.getReader(), chunks = []; let length = 0;
        try { for (;;) { const chunk = await reader.read(); if (chunk.done) break; length += chunk.value.byteLength;
          if (length > 128 * 1024) throw new Error('Native presentation completion exceeds its fixed control bound'); chunks.push(Buffer.from(chunk.value)); }
        } finally { await reader.cancel().catch(() => {}); }
        const text = Buffer.concat(chunks).toString('utf8');
        if (text.includes(connection.secret)) throw new Error('Native presentation completion reflected the private credential');
        const value = JSON.parse(text);
        if (value.schema_version !== 2 || value.worker_environment_id !== body.worker_environment_id || value.request_id !== body.request_id
          || value.operation !== 'presentation_complete' || value.accepted !== true || value.error != null || value.receiver_bootstrap != null) throw new Error('Native presentation completion did not admit the exact source request');
        const result = value.presentation_result, expected = { ...body.action }; delete expected.type;
        if (!result || result.schema_version !== 1 || !['ended', 'absent'].includes(result.status)
          || !result.target || Object.keys(result.target).length !== Object.keys(expected).length
          || Object.keys(expected).some(key => key === 'scope'
            ? !result.target.scope || Object.keys(result.target.scope).length !== Object.keys(expected.scope).length
              || Object.keys(expected.scope).some(field => result.target.scope[field] !== expected.scope[field])
            : result.target[key] !== expected[key])) throw new Error('Native completion acknowledgement changed its accepted source identity');
        await check(); if (abort.signal.aborted) throw abort.signal.reason;
        return result;
      } finally { clearTimeout(timer); signal?.removeEventListener('abort', cancel); }
    };
    return { home: connection.home, timeout: connection.timeout, ...captured, current: check, complete,
      bootstrap: async (pending, signal) => {
        await check();
        const body = { schema_version: 2, worker_environment_id: raw.workerEnvironmentId, request_id: pending.requestId,
          tool: 'operator.desktop', action: { type: 'receiver_bootstrap', scope: captured.scope, source_window_id: raw.sourceWindowId,
            receiver_pid: pending.receiverPid, receiver_nonce: pending.receiverNonce, receiver_instance_nonce: pending.receiverInstanceNonce } };
        const abort = new AbortController(), cancel = () => abort.abort(new Error('Native presentation source was withdrawn'));
        const timer = setTimeout(() => abort.abort(new Error('Native presentation exceeded its declared request timeout')), connection.timeout);
        signal?.addEventListener('abort', cancel, { once: true }); if (signal?.aborted) cancel();
        let key;
        try {
          const response = await (options.fetch || fetch)(new URL('/v1/operator', connection.base), { method: 'POST', redirect: 'error', signal: abort.signal,
            headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${connection.secret}` }, body: JSON.stringify(body) });
          if (!response.ok || !response.body) throw new Error('Native presentation private host response failed');
          const reader = response.body.getReader(), chunks = []; let length = 0;
          try { for (;;) { const chunk = await reader.read(); if (chunk.done) break; length += chunk.value.byteLength;
            if (length > 128 * 1024) throw new Error('Native presentation bootstrap exceeds its fixed control bound'); chunks.push(Buffer.from(chunk.value)); }
          } finally { await reader.cancel().catch(() => {}); }
          const bytes = Buffer.concat(chunks), text = bytes.toString('utf8');
          if (text.includes(connection.secret)) throw new Error('Native presentation response reflected the private host credential');
          let value; try { value = JSON.parse(text); } catch { throw new Error('Native presentation response is malformed'); }
          if (value.schema_version !== 2 || value.request_id !== body.request_id || value.worker_environment_id !== body.worker_environment_id || value.operation !== 'receiver_bootstrap'
            || value.accepted !== true || value.error != null || value.evidence != null || value.application_result != null) throw new Error('Native presentation response did not admit the exact source request');
          const result = value.receiver_bootstrap;
          if (!result || Object.keys(result).sort().join(',') !== 'binding_bytes_base64,bootstrap_id,bootstrap_key_base64,schema_version' || result.schema_version !== 1 || !/^[a-f0-9]{32}$/.test(result.bootstrap_id || '')) throw new Error('Native presentation bootstrap identity is invalid');
          const decode = (encoded, maximum) => { if (typeof encoded !== 'string') throw new Error('Native presentation bootstrap is malformed'); const decoded = Buffer.from(encoded, 'base64');
            if (!decoded.length || decoded.length > maximum || decoded.toString('base64') !== encoded) throw new Error('Native presentation private bytes are invalid'); return decoded; };
          key = decode(result.bootstrap_key_base64, 32); if (key.length !== 32) throw new Error('Native presentation key length differs');
          const bindingBytes = decode(result.binding_bytes_base64, 65536);
          let binding; try { binding = JSON.parse(bindingBytes.toString('utf8')); } catch { throw new Error('Native presentation binding is malformed'); }
          if (binding.bootstrap_id !== result.bootstrap_id || binding.request_id !== body.request_id) throw new Error('Native presentation response changed its private binding identity');
          await check(); if (abort.signal.aborted) throw abort.signal.reason;
          return { key, bindingBytes };
        } catch (error) { key?.fill(0); throw error; }
        finally { clearTimeout(timer); signal?.removeEventListener('abort', cancel); }
      },
    };
  }
  async function preparePermissionSource(raw) {
    const input = permissionSurfaceInput(raw), connection = await admit({ ...input, operation: 'view' });
    if (!connection.native) throw new Error('macOS permissions require this room’s exact native helper');
    const hostGeneration = connection.descriptor.data.host_generation;
    const check = async () => current(connection, { ...input, operation: 'view' });
    let snapshot = null;
    const readSettingsSource = async (requestId, permission, signal) => {
      await check(); if (signal.aborted) throw signal.reason;
      const body = { schema_version: 2, worker_environment_id: input.workerEnvironmentId, request_id: randomUUID(), tool: 'operator.desktop',
        action: { type: 'permission_settings_source', permission_request_id: requestId } };
      const response = await (options.fetch || fetch)(new URL('/v1/operator', connection.base), { method: 'POST', redirect: 'error', signal,
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${connection.secret}` }, body: JSON.stringify(body) });
      if (!response.ok || !response.body) throw new Error('Native Settings source observation is unavailable');
      const reader = response.body.getReader(), parts = []; let count = 0;
      try { for (;;) { const item = await reader.read(); if (item.done) break; count += item.value.byteLength;
        if (count > 128 * 1024) throw new Error('Native Settings source exceeded its control bound'); parts.push(Buffer.from(item.value)); }
      } finally { await reader.cancel().catch(() => {}); }
      const text = Buffer.concat(parts).toString('utf8'); if (text.includes(connection.secret)) throw new Error('Native Settings source reflected private credentials');
      const value = JSON.parse(text);
      if (!value || typeof value !== 'object' || Array.isArray(value)
        || Object.keys(value).some(key => !['schema_version', 'worker_environment_id', 'request_id', 'operation', 'accepted', 'evidence', 'error', 'permission_settings_source'].includes(key))
        || value.schema_version !== 2 || value.worker_environment_id !== body.worker_environment_id || value.request_id !== body.request_id
        || value.operation !== 'permission_settings_source' || value.accepted !== true || value.error != null || value.evidence != null) throw new Error('Native Settings source changed its private request or result carrier');
      const source = validatePermissionSettingsSource(value.permission_settings_source, hostGeneration, requestId, permission);
      await check(); if (signal.aborted) throw signal.reason;
      return source;
    };
    const exchangePermission = async (operation, permission, signal, settingsObserver) => {
      await check();
      if (!['status', 'request', 'open_settings'].includes(operation) || (operation === 'status' ? permission !== null : typeof permission !== 'string')) throw new Error('Invalid human permission operation');
      if (settingsObserver && (typeof settingsObserver.update !== 'function' || typeof settingsObserver.unavailable !== 'function')) throw new Error('Settings source observation requires its private presentation owner');
      if (operation !== 'status') {
        const row = snapshot?.permissions.find(row => row.id === permission);
        if (!row || !row[operation === 'request' ? 'can_request' : 'can_open_settings']) throw new Error('This native permission action is not currently available');
      }
      const body = { schema_version: 2, worker_environment_id: input.workerEnvironmentId, request_id: randomUUID(), tool: 'operator.desktop', action: { type: 'permissions', operation, permission } };
      const abort = new AbortController(), cancel = () => abort.abort(new Error('Permission window or source was closed'));
      const timer = setTimeout(() => abort.abort(new Error(operation === 'status' ? 'Permission status exceeded the declared timeout' : 'Permission request outcome is unknown after the declared timeout; refresh status before retrying')), connection.timeout);
      signal?.addEventListener('abort', cancel, { once: true }); if (signal?.aborted) cancel();
      const watch = new AbortController(), stopWatch = () => watch.abort(new Error('The original Settings operation settled'));
      abort.signal.addEventListener('abort', stopWatch, { once: true }); if (abort.signal.aborted) stopWatch();
      let watcher = Promise.resolve();
      if (operation === 'open_settings' && settingsObserver) {
        watcher = (async () => {
          let seen = null, last = null;
          while (!watch.signal.aborted) {
            await new Promise((resolve, reject) => {
              const cancelWait = () => { clearTimeout(tick); watch.signal.removeEventListener('abort', cancelWait); reject(watch.signal.reason); };
              const tick = setTimeout(() => { watch.signal.removeEventListener('abort', cancelWait); resolve(); }, 100);
              watch.signal.addEventListener('abort', cancelWait, { once: true }); if (watch.signal.aborted) cancelWait();
            });
            const source = await readSettingsSource(body.request_id, permission, watch.signal);
            if (source.state === 'observed') {
              if (seen && (!isDeepStrictEqual(source.settings, seen.settings) || source.window.id !== seen.window.id
                || source.window.observation_sequence < seen.window.observation_sequence
                || source.window.observation_sequence === seen.window.observation_sequence && !isDeepStrictEqual(source.window, seen.window))) throw new Error('Native Settings source changed its retained process or window');
              seen = source;
            }
            if (!isDeepStrictEqual(source, last)) { await settingsObserver.update(source); last = source; }
            if (source.state === 'ended' && (seen || source.permission !== null)) return;
          }
        })().catch(error => { if (!watch.signal.aborted) return settingsObserver.unavailable(error); });
      }
      try {
        const response = await (options.fetch || fetch)(new URL('/v1/operator', connection.base), { method: 'POST', redirect: 'error', signal: abort.signal,
          headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${connection.secret}` }, body: JSON.stringify(body) });
        if (!response.ok || !response.body) throw new Error('Native permission helper did not return a response');
        const reader = response.body.getReader(), parts = []; let count = 0;
        try { for (;;) { const item = await reader.read(); if (item.done) break; count += item.value.byteLength;
          if (count > 128 * 1024) throw new Error('Native permission status exceeded its control bound'); parts.push(Buffer.from(item.value)); }
        } finally { await reader.cancel().catch(() => {}); }
        const text = Buffer.concat(parts).toString('utf8'); if (text.includes(connection.secret)) throw new Error('Native permission helper reflected private credentials');
        const value = JSON.parse(text);
        if (value.schema_version !== 2 || value.worker_environment_id !== body.worker_environment_id || value.request_id !== body.request_id
          || value.operation !== 'permissions' || typeof value.accepted !== 'boolean') throw new Error('Native permission response changed its room or request');
        if (!value.accepted) throw new Error(value.error?.message || 'Native permission request was refused');
        if (value.error != null || value.evidence != null || value.application_result != null || value.receiver_bootstrap != null || value.presentation_result != null || value.permission_settings_source != null) throw new Error('Native permission response mixed unrelated authority or evidence');
        const next = validatePermissionSnapshot(value.permission_result, hostGeneration);
        if (snapshot && !isDeepStrictEqual(next.helper, snapshot.helper)) throw new Error('The native permission helper restarted; reopen permissions');
        await check(); if (abort.signal.aborted) throw abort.signal.reason;
        snapshot = next; return structuredClone(next);
      } finally {
        stopWatch(); clearTimeout(timer); abort.signal.removeEventListener('abort', stopWatch); signal?.removeEventListener('abort', cancel);
        await watcher;
      }
    };
    return { home: connection.home, hostGeneration, current: check, exchange: exchangePermission };
  }
  async function preferenceStore(door) {
    if (typeof door !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(door)) throw new Error('Application preference owner has no exact descriptor');
    const home = path.resolve(options.homePath()), directory = path.join(home, 'registry', 'interface', door);
    const paths = [path.join(home, 'host.json'), path.join(directory, 'interface.json'), path.join(directory, 'desktop.json')];
    const before = await fileGeneration(paths);
    const [machine, descriptor, config] = await Promise.all(paths.map(readJson));
    if (descriptor.id !== door || typeof descriptor.interface_driver_ref !== 'string' || !descriptor.interface_driver_ref) throw new Error('Application preference descriptor identity changed');
    const selected = config.host?.application_preferences;
    if (selected == null) return null;
    if (!selected || typeof selected !== 'object' || Array.isArray(selected) || Object.keys(selected).length !== 1
      || typeof selected.root_slot !== 'string' || !selected.root_slot) throw new Error('Application preferences have no exact configured store');
    const slots = descriptor.interface_driver_config?.host_slots, slot = selected.root_slot;
    const root = slots && Object.hasOwn(slots, slot) ? slots[slot]?.path : machine.root_refs?.[slot];
    if (machine.host_manifest_version !== 1 || typeof root !== 'string' || !path.isAbsolute(root) || path.resolve(root) !== root) throw new Error('Application preference store is not an explicit canonical path');
    await requirePrivateDirectory(root);
    const stat = await fs.lstat(root, { bigint: true });
    if (await fs.realpath(root) !== root || before !== await fileGeneration(paths) || path.resolve(options.homePath()) !== home) throw new Error('Application preference configuration changed during resolution');
    return { home, root, driver: descriptor.interface_driver_ref, generation: before,
      identity: JSON.stringify([home, root, String(stat.dev), String(stat.ino), String(stat.uid), String(stat.mode)]) };
  }
  async function defaultPreferenceStore(revalidate) {
    return options.withDefaultPreferenceDriver(async door => {
      await revalidate(); const selected = await preferenceStore(door);
      if (!selected) throw new Error('Computer Use has no declared preference store');
      return { root: selected.root, identity: selected.identity, generation: selected.generation, current: async () => options.withDefaultPreferenceDriver(async nextDoor => {
        await revalidate(); if (nextDoor !== door || !isDeepStrictEqual(await preferenceStore(door), selected)) throw new Error('Computer Use preference store changed');
        await revalidate();
      }) };
    });
  }
  async function stopConfiguredApplications(ownerId, revalidate) {
    return options.withDefaultPreferenceDriver(async door => {
      await revalidate(); const selected = await preferenceStore(door);
      if (!selected) throw new Error('Computer Use has no declared preference store');
      const rows = await options.resolvePreferenceEnvironments();
      if (!Array.isArray(rows)) throw new Error('Computer Use host inventory is unavailable');
      for (const row of rows) {
        if (row.lifecycle !== 'active' || row.worker_environment_kind !== 'desktop_session') continue;
        const store = await preferenceStore(row.door);
        if (!store || store.root !== selected.root || store.driver !== selected.driver) continue;
        for (const surface of row.client_surfaces ?? []) if (surface.protocol === 'desktop_session' && surface.mode === 'direct') {
          await revalidate(); if (!isDeepStrictEqual(await preferenceStore(door), selected)) throw new Error('Computer Use preference store changed during Stop');
          await request({ workerEnvironmentId: row.worker_environment_id, surfaceId: surface.id, runId: row.run_id, viewId: randomUUID(), operation: 'stop' }, ownerId);
          await revalidate();
        }
      }
    });
  }
  async function defaultApplicationPermissions(raw, revalidate) {
    const keys = raw?.operation === 'list' ? ['operation'] : ['operation','sourceId','bundleId'];
    if (!raw || typeof raw !== 'object' || Array.isArray(raw) || Object.keys(raw).length !== keys.length || keys.some(key => !Object.hasOwn(raw,key))
      || !['list','remove'].includes(raw.operation) || typeof revalidate !== 'function' || typeof options.resolvePreferenceEnvironments !== 'function'
      || typeof options.withDefaultPreferenceDriver !== 'function') throw new Error('Saved application Settings require their configured client owner');
    return options.withDefaultPreferenceDriver(async (defaultDoor) => {
      await revalidate(); const selected = await preferenceStore(defaultDoor);
      if (!selected) throw new Error('Saved application permissions are not configured');
      const rows = await options.resolvePreferenceEnvironments();
      if (!Array.isArray(rows)) throw new Error('Application preference host inventory is unavailable');
      const owners = new Map();
      for (const row of rows) {
        if (!['active','failed'].includes(row.lifecycle) || row.worker_environment_kind !== 'desktop_session' || !Array.isArray(row.client_surfaces)) continue;
        const surfaces = row.client_surfaces.filter(surface => surface?.protocol === 'desktop_session' && surface.mode === 'direct');
        if (!surfaces.length) continue;
        const store = await preferenceStore(row.door);
        if (!store || store.driver !== selected.driver || store.identity !== selected.identity) continue;
        for (const surface of surfaces) {
          if (!Number.isSafeInteger(row.opened_at_movement_seq) || row.opened_at_movement_seq <= 0) throw new Error('Application preference host has no current formation observation');
          const input = permissionSurfaceInput({workerEnvironmentId:row.worker_environment_id,surfaceId:surface.id,runId:row.run_id});
          const connection = await admit({...input,operation:'view'});
          if (!connection.native || connection.descriptor.doorInterfaceId !== row.door || connection.descriptor.data.host_generation !== surface.data?.host_generation) throw new Error('Application preference host differs from its current projection');
          const ownerId = JSON.stringify([row.door,connection.generation,connection.base.origin,connection.descriptor.data.host_generation]);
          const candidate = {input,connection,store,opened:row.opened_at_movement_seq};
          const previous = owners.get(ownerId);
          if (!previous || candidate.opened > previous.opened) owners.set(ownerId,candidate);
          else if (candidate.opened === previous.opened && !isDeepStrictEqual(candidate.input,previous.input)) throw new Error('Application preference host has ambiguous current formation evidence');
        }
      }
      if (!owners.size) throw new Error('Saved application permissions are unavailable until their configured Computer Use host is running');
      const ordered = [...owners.entries()].sort(([leftId,left],[rightId,right]) =>
        Number(right.connection.descriptor.doorInterfaceId === defaultDoor) - Number(left.connection.descriptor.doorInterfaceId === defaultDoor)
          || (leftId < rightId ? -1 : leftId > rightId ? 1 : 0));
      const owner = ordered[0][1];
      const sourceId = createHash('sha256').update(JSON.stringify([selected,owner.input,owner.connection.generation,owner.opened])).digest('hex');
      if (raw.operation === 'remove' && raw.sourceId !== sourceId) throw new Error('Saved application permission source changed; refresh before removing');
      const check = async () => {
        await revalidate();
        const [defaultStore, ownerStore] = await Promise.all([preferenceStore(defaultDoor),preferenceStore(owner.connection.descriptor.doorInterfaceId)]);
        if (!isDeepStrictEqual(defaultStore,selected) || !isDeepStrictEqual(ownerStore,owner.store)) throw new Error('Saved application preference store changed');
        await current(owner.connection,{...owner.input,operation:'view'}); await revalidate();
      };
      await check();
      const result = await applicationPermissions({...owner.input,hostGeneration:owner.connection.descriptor.data.host_generation,operation:raw.operation,
        ...(raw.operation==='remove'?{bundleId:raw.bundleId}:{})},check);
      await check(); return {sourceId,applications:result.applications,removed:result.removed};
    });
  }
  async function applicationPermissions(raw, revalidate) {
    const fields = raw?.operation === 'list' ? ['workerEnvironmentId','surfaceId','runId','hostGeneration','operation'] : ['workerEnvironmentId','surfaceId','runId','hostGeneration','operation','bundleId'];
    if (!raw || typeof raw !== 'object' || Array.isArray(raw) || Object.keys(raw).length !== fields.length || fields.some(key => !Object.hasOwn(raw,key))
      || !['list','remove'].includes(raw.operation) || typeof revalidate !== 'function') throw new Error('Invalid saved application permission request');
    const input = permissionSurfaceInput({ workerEnvironmentId: raw.workerEnvironmentId, surfaceId: raw.surfaceId, runId: raw.runId });
    if (raw.operation === 'remove' && (typeof raw.bundleId !== 'string' || !raw.bundleId || raw.bundleId.trim() !== raw.bundleId || Buffer.byteLength(raw.bundleId) > 4096 || raw.bundleId.includes('\0'))) throw new Error('Saved application permission has no exact bundle identity');
    await revalidate(); const connection = await admit({ ...input, operation: 'view' });
    if (!connection.native || connection.descriptor.data.host_generation !== raw.hostGeneration) throw new Error('Saved application settings require the exact current native host');
    if (!Number.isSafeInteger(connection.responseLimit) || connection.responseLimit < 1024 || connection.responseLimit > 97871192) throw new Error('Saved application permissions have no valid declared response limit');
    const check = async () => { await revalidate(); await current(connection, { ...input, operation: 'view' }); await revalidate(); };
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(new Error('Saved application permission outcome is unknown after its declared timeout; refresh before retrying')), connection.timeout);
    const body = { schema_version: 2, worker_environment_id: input.workerEnvironmentId, request_id: randomUUID(), tool: 'operator.desktop',
      action: { type: 'application_permissions', operation: raw.operation, host_generation: raw.hostGeneration, ...(raw.operation === 'remove' ? { bundle_id: raw.bundleId } : {}) } };
    try {
      await check();
      const response = await (options.fetch || fetch)(new URL('/v1/operator', connection.base), { method: 'POST', redirect: 'error', signal: abort.signal,
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${connection.secret}` }, body: JSON.stringify(body) });
      if (!response.ok || !response.body) throw new Error('Saved application permission host is unavailable');
      const reader = response.body.getReader(), parts = []; let count = 0;
      try { for (;;) { const item = await reader.read(); if (item.done) break; count += item.value.byteLength;
        if (count > connection.responseLimit) throw new Error('Saved application permission response exceeded its declared bound'); parts.push(Buffer.from(item.value)); }
      } finally { await reader.cancel().catch(() => {}); }
      const text = Buffer.concat(parts).toString('utf8'); if (text.includes(connection.secret)) throw new Error('Saved application permission host reflected credentials');
      const value = JSON.parse(text);
      if (!value || typeof value !== 'object' || Array.isArray(value)
        || Object.keys(value).some(key => !['schema_version','worker_environment_id','request_id','operation','accepted','evidence','error','application_result','receiver_bootstrap','presentation_result','permission_result','permission_settings_source'].includes(key))
        || value.schema_version !== 2 || value.worker_environment_id !== body.worker_environment_id
        || value.request_id !== body.request_id || value.operation !== 'application_permissions' || typeof value.accepted !== 'boolean') throw new Error('Saved application permission response lost its exact request');
      if (!value.accepted) throw new Error(value.error?.message || 'Saved application permission request was refused');
      if (value.error != null || value.evidence != null || value.receiver_bootstrap != null || value.presentation_result != null || value.permission_result != null || value.permission_settings_source != null) throw new Error('Saved application permissions mixed unrelated results');
      const result = value.application_result;
      if (!result || Object.keys(result).length !== 4 || Object.keys(result).some(key => !['type','host_generation','applications','removed'].includes(key))
        || result.type !== 'application_permissions' || result.host_generation !== raw.hostGeneration || typeof result.removed !== 'boolean' || !Array.isArray(result.applications) || result.applications.length > 1024
        || raw.operation === 'list' && result.removed) throw new Error('Saved application permission result changed its host or operation');
      const ids = new Set();
      for (const row of result.applications) {
        if (!row || Object.keys(row).length !== 2 || !['bundle_id','display_name'].every(key => typeof row[key] === 'string' && row[key] && Buffer.byteLength(row[key]) <= 4096 && !row[key].includes('\0')) || ids.has(row.bundle_id)) throw new Error('Saved application permission entry is invalid');
        ids.add(row.bundle_id);
      }
      await check(); if (abort.signal.aborted) throw abort.signal.reason;
      return { home: connection.home, hostGeneration: raw.hostGeneration, applications: result.applications, removed: result.removed };
    } finally { clearTimeout(timer); }
  }
  return { request, pauseOwner, closeOwner, preparePresentationSource, preparePermissionSource, applicationPermissions, defaultApplicationPermissions, defaultPreferenceStore, stopConfiguredApplications };
}

function validateInput(input) {
  const allowed = new Set(["workerEnvironmentId", "surfaceId", "runId", "operation", "screenId", "viewId", "controlGeneration"]);
  if (!input || typeof input !== "object" || Array.isArray(input) || Object.keys(input).some((key) => !allowed.has(key))) throw new Error("Invalid desktop surface request");
  for (const key of ["workerEnvironmentId", "surfaceId", "runId", "viewId"]) required(input[key], key);
  if (!["view", "pause_view", "take_control", "release_control", "stop"].includes(input.operation)) throw new Error("Unsupported desktop surface action");
  if (input.screenId !== undefined) required(input.screenId, "screenId");
  if (["take_control", "release_control"].includes(input.operation)) {
    if (!Number.isSafeInteger(input.controlGeneration) || input.controlGeneration < 0) throw new Error("Desktop control requires its observed generation");
  } else if (input.controlGeneration !== undefined) throw new Error("Desktop control generation is not valid for this operation");
  return { ...input };
}
function sameTarget(a, b) {
  return a.workerEnvironmentId === b.workerEnvironmentId && a.surfaceId === b.surfaceId && a.runId === b.runId && (b.operation !== "view" || a.screenId === b.screenId);
}
function surfaceIdentity(value) { return JSON.stringify([value.runId, value.environmentKind, value.protocol, value.mode, value.doorInterfaceId, value.viewerUrl, value.formationRevision, value.readOnly, value.data?.host_generation]); }
function project(input, evidence) { return { workerEnvironmentId: input.workerEnvironmentId, surfaceId: input.surfaceId, runId: input.runId, evidence }; }
async function fileGeneration(paths) {
  const values = await Promise.all(paths.map(async (file) => {
    try { const stat = await fs.lstat(file, { bigint: true }); return [stat.dev, stat.ino, stat.size, stat.mtimeNs, stat.ctimeNs, stat.mode, stat.uid, stat.nlink].map(String); }
    catch (error) { if (error.code === "ENOENT") return null; throw error; }
  }));
  return JSON.stringify(values);
}

function validateSurface(descriptor, request) {
  if (!descriptor || descriptor.runId !== request.runId || descriptor.environmentKind !== "desktop_session" || descriptor.protocol !== "desktop_session" || !["direct", "evidence"].includes(descriptor.mode)) throw new Error("Desktop surface is no longer the exact declared room");
}

function validateEvidence(evidence) {
  if (!evidence || !["ready", "permission_required", "disabled", "unavailable", "stopped"].includes(evidence.status) || typeof evidence.permissions?.screen_capture !== "boolean" || typeof evidence.permissions?.accessibility !== "boolean" || !["idle", "model", "human", "stopped"].includes(evidence.control?.mode) || !Number.isSafeInteger(evidence.control.generation) || evidence.control.generation < 0 || (evidence.control.mode === "model") !== (typeof evidence.control.owner === "string") || !Array.isArray(evidence.screens)) throw new Error("Desktop service returned invalid readiness/control evidence");
  if (evidence.status === "ready" && (!evidence.permissions.screen_capture || !evidence.permissions.accessibility)) throw new Error("Desktop readiness is unsupported by permission evidence");
  for (const screen of evidence.screens) {
    if (!screen || typeof screen.id !== "string" || ![screen.x, screen.y, screen.width, screen.height].every(Number.isFinite) || screen.width <= 0 || screen.height <= 0 || ![screen.pixel_width, screen.pixel_height].every((value) => Number.isSafeInteger(value) && value > 0)) throw new Error("Desktop screen coordinate domain is invalid");
  }
  const observation = evidence.observation;
  if (evidence.view_image_ref != null && (!observation || observation.image != null || !/^[a-f0-9]{64}$/.test(evidence.view_image_ref.sha256))) throw new Error("Desktop passive image reference is invalid");
  if (observation) {
    if (!observation.id || !observation.input_revision || !Number.isSafeInteger(observation.captured_at_unix_ms) || !evidence.screens.some((screen) => JSON.stringify(screen) === JSON.stringify(observation.screen))) throw new Error("Desktop observation identity is invalid");
    const image = observation.image;
    if (image) {
      if (!["image/png", "image/jpeg"].includes(image.media_type) || typeof image.data_base64 !== "string" || !Number.isSafeInteger(image.byte_length) || image.byte_length <= 0 || image.byte_length > 32 * 1024 * 1024 || image.width !== observation.screen.pixel_width || image.height !== observation.screen.pixel_height) throw new Error("Desktop frame geometry/carrier is invalid");
      const bytes = Buffer.from(image.data_base64, "base64");
      if (bytes.length !== image.byte_length || bytes.toString("base64") !== image.data_base64 || createHash("sha256").update(bytes).digest("hex") !== image.sha256) throw new Error("Desktop frame failed integrity verification");
    }
  }
  return evidence;
}

function required(value, name) {
  if (typeof value !== "string" || !value.trim() || value.length > 512 || value.includes("\0")) throw new Error(`${name} is required`);
  return value;
}

async function readJson(file) {
  const stat = await fs.lstat(file);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 1024 * 1024) throw new Error("Desktop configuration must be a bounded regular file");
  const value = JSON.parse(await fs.readFile(file, "utf8"));
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Desktop configuration must be an object");
  return value;
}

async function requirePrivateDirectory(directory) {
  const stat = await fs.lstat(directory);
  if (!process.getuid || !stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o700 || await fs.realpath(directory) !== directory) throw new Error("Desktop native private directory must be canonical, owned and mode 0700");
}

async function validateNativeConnection(connection) {
  for (const directory of connection.privateDirectories) await requirePrivateDirectory(directory);
  if (connection.generation !== await fileGeneration(connection.paths)) throw new Error("Desktop native connection changed; attach a new view");
}

async function readNativeConnection(file) {
  if (!process.getuid || !Number.isInteger(constants.O_NOFOLLOW)) throw new Error("Desktop native private connections require no-follow file access");
  const handle = await fs.open(file, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o600 || stat.nlink !== 1 || stat.size > 4096) throw new Error("Desktop private connection must be an owned single-link mode 0600 file within 4096 bytes");
    const bytes = Buffer.alloc(4097); let length = 0;
    while (length < bytes.length) {
      const { bytesRead } = await handle.read(bytes, length, bytes.length - length, null);
      if (!bytesRead) break;
      length += bytesRead;
    }
    if (length > 4096) throw new Error("Desktop private connection exceeds 4096 bytes");
    let value;
    try { value = JSON.parse(bytes.subarray(0, length).toString("utf8")); }
    catch { throw new Error("Desktop private connection is malformed"); }
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Desktop private connection must be an object");
    return value;
  } finally { await handle.close(); }
}

function nativeUrl(value, route) {
  let url;
  try { url = new URL(value); } catch { throw new Error("Desktop private connection URL is invalid"); }
  if (typeof value !== "string" || url.href !== value || url.protocol !== "http:" || url.hostname !== "127.0.0.1" || !url.port || Number(url.port) <= 0
    || url.pathname !== route || url.username || url.password || url.search || url.hash) throw new Error("Desktop private connection must name its exact loopback endpoint");
  return url;
}

function createDesktopPresentationHost(options) {
  for (const field of ['getNativeHost', 'prepareSource', 'requireAccess', 'homePath']) {
    if (typeof options?.[field] !== 'function') throw new Error(`Native desktop presentation requires ${field}`);
  }
  const sources = new Map(), completedBatches = new Map(), automationWaits = new Map(), views = new Map();
  let native = null, nativePromise = null, closed = false, home = null;
  const snapshot = () => [...sources.values()].map(record => ({
    worker_environment_id: record.input.workerEnvironmentId, run_id: record.input.runId,
    source_id: createHash('sha256').update(JSON.stringify([record.id,record.nativeIdentity])).digest('hex'),
    app_instance_id: record.input.appInstanceId, source_window_id: record.input.sourceWindowId,
    display_name: record.source.application.display_name,
    window_title: record.source.windows.find(window => window.window_id === record.input.sourceWindowId)?.title ?? null,
    phase: record.phase, batch_movement_seq: record.batch, completion_status: record.completionStatus || null, error: record.error || null,
  }));
  function changed() { options.onChange?.(snapshot()); }
  function current(record) {
    if (closed || sources.get(record.id) !== record || record.abort.signal.aborted || options.homePath() !== home) throw new Error('Native presentation source or Home was withdrawn');
    options.requireAccess();
  }
  function withdraw(record) {
    if (sources.get(record.id) !== record) return;
    closeSourceViews(record); sources.delete(record.id); record.abort.abort();
    const requestId = record.permissionRequestId || record.requestId;
    for (const pending of automationWaits.values()) if (pending.requestId === requestId) pending.finish(new Error('Native Automation source was withdrawn'));
    if (requestId && native) native.cancelSource({ requestId });
  }
  function closeView(view) {
    if (views.get(view.id) !== view) return;
    views.delete(view.id); native?.detachEmbeddedSource({ viewId: view.id, viewGeneration: view.generation });
  }
  function closeSourceViews(record) { for (const view of [...views.values()]) if (view.source === record) closeView(view); }
  function closeViewsOwner(ownerId) { for (const view of [...views.values()]) if (view.owner.id === ownerId) closeView(view); }
  async function applicationView(raw, owner) {
    const fields = ['workerEnvironmentId','surfaceId','runId','viewId','operation','appInstanceId','sourceWindowId','rect'];
    if (!raw || typeof raw !== 'object' || Array.isArray(raw) || Object.keys(raw).some(key => !fields.includes(key))
      || !['attach','detach','hide'].includes(raw.operation)) throw new Error('Invalid native application view request');
    for (const key of fields.slice(0, 4)) required(raw[key], key);
    owner.current(); options.requireAccess();
    const previous = views.get(raw.viewId);
    if (previous && (previous.owner.id !== owner.id || !sameTarget(previous.input, raw))) throw new Error('Native application view belongs to another owner or room');
    if (raw.operation === 'detach' || raw.operation === 'hide') {
      if (Object.keys(raw).length !== 5) throw new Error('Native detach requires only its exact view identity');
      if (previous) {
        if (raw.operation === 'detach') closeView(previous);
        else { ++previous.revision; previous.visible = false; native?.hideEmbeddedSource({ viewId: previous.id, viewGeneration: previous.generation }); }
      }
      return { attached: false };
    }
    if (Object.keys(raw).length !== fields.length || !raw.rect || Object.keys(raw.rect).length !== 4
      || !['x','y','width','height'].every(key => Number.isFinite(raw.rect[key])) || raw.rect.x < 0 || raw.rect.y < 0
      || raw.rect.width <= 0 || raw.rect.height <= 0 || Object.values(raw.rect).some(value => value > 16384)) throw new Error('Native application view requires its visible rectangle');
    required(raw.appInstanceId,'appInstanceId'); required(raw.sourceWindowId,'sourceWindowId');
    const source = [...sources.values()].find(record => sameTarget(record.input, raw) && record.input.appInstanceId === raw.appInstanceId
      && record.input.sourceWindowId === raw.sourceWindowId && record.phase === 'published' && !record.completionStatus);
    if (!source?.authenticated || !source.requestId) throw new Error('Application preview is not currently published');
    if (previous && previous.source !== source) throw new Error('Detach the previous application before replacing the view');
    const view = previous || { id: raw.viewId, generation: randomUUID(), owner, input: raw, source, revision: 0, visible: false };
    const revision = ++view.revision; view.visible = true;
    views.set(view.id, view);
    const check = () => { owner.current(); current(source); if (views.get(view.id) !== view || view.revision !== revision || !view.visible || source.phase !== 'published' || source.completionStatus) throw new Error('Native application view was withdrawn'); };
    try {
      check(); await source.preparedSource.current(); check();
      const host = await getNative(); check(); options.syncHosts?.(host);
      const hostIdentity = owner.hostIdentity();
      host.embedSource({ requestId: source.requestId, viewId: view.id, viewGeneration: view.generation,
        hostId: hostIdentity.hostId, hostGeneration: hostIdentity.generation, windowHandle: owner.windowHandle(), rect: raw.rect });
      check(); return { attached: true };
    } catch (error) { if (view.revision === revision) closeView(view); throw error; }
  }
  async function getNative() {
    if (!nativePromise) nativePromise = Promise.resolve().then(() => options.getNativeHost(onEvent)).then(value => {
      for (const method of ['beginSource', 'installSource', 'cancelSource', 'completeGroup', 'clearCompletedGroup','embedSource','detachEmbeddedSource','hideEmbeddedSource']) {
        if (typeof value?.[method] !== 'function') throw new Error('Client native presentation API is incomplete');
      }
      if (closed) throw new Error('Native presentation client was closed during startup');
      native = value; return value;
    });
    return nativePromise;
  }
  async function start(record) {
    let key = null;
    try {
      current(record);
      const source = await options.prepareSource(record.input); current(record);
      if (source.home !== home || JSON.stringify(source.source.source) !== JSON.stringify(record.source.source)) throw new Error('Native presentation observation changed before preparation');
      const host = await getNative(); current(record); await source.current(); current(record);
      record.preparedSource = source;
      record.requestId = randomUUID();
      const pending = host.beginSource({ requestId: record.requestId, scope: source.scope, source: source.source.source,
        sourceWindowId: record.input.sourceWindowId, groupId: record.input.runId, batchId: String(record.batch), timeoutMs: source.timeout });
      if (pending?.requestId !== record.requestId || pending.receiverPid !== process.pid || !/^[a-f0-9]{64}$/.test(pending.receiverNonce || '') || !/^[a-f0-9]{64}$/.test(pending.receiverInstanceNonce || '')) throw new Error('Native presentation preparation returned another receiver');
      record.receiverInstanceNonce = pending.receiverInstanceNonce;
      record.nonce = pending.receiverNonce; record.phase = 'authenticating'; changed();
      const response = await source.bootstrap({ requestId: record.requestId, ...pending }, record.abort.signal); key = response.key;
      current(record); await source.current(); current(record);
      const binding = JSON.parse(response.bindingBytes.toString('utf8'));
      record.nativeIdentity = { hostGeneration: binding.host_generation, presentationId: binding.presentation_id, streamGeneration: binding.stream_generation };
      host.installSource({ requestId: record.requestId, bindingBytes: response.bindingBytes, key });
      record.permissionRequestId = record.requestId;
    } catch (error) {
      if (record.requestId && native) native.cancelSource({ requestId: record.requestId });
      if (sources.get(record.id) === record && !record.abort.signal.aborted) {
        record.phase = 'failed'; record.error = error instanceof Error ? error.message : 'Native presentation failed'; changed();
      }
    } finally { key?.fill(0); }
  }
  async function completeSource(record) {
    record.completionStatus = 'requested';
    try {
      current(record); await record.preparedSource.current(); current(record);
      const identity = record.nativeIdentity;
      if (!record.authenticated || !identity || typeof record.preparedSource.complete !== 'function') throw new Error('Authenticated native publication is unavailable for completion');
      record.completionEvidence = await record.preparedSource.complete({ presentationId: identity.presentationId,
        streamGeneration: identity.streamGeneration, receiverInstanceNonce: record.receiverInstanceNonce }, record.abort.signal);
      current(record); record.completionStatus = record.completionEvidence.status; changed();
    } catch (error) {
      if (sources.get(record.id) === record && !record.abort.signal.aborted) {
        record.completionStatus = 'uncertain'; record.error = error instanceof Error ? error.message : 'Native completion outcome is uncertain'; changed();
      }
    }
  }
  function onEvent(event) {
    if (closed || !event || typeof event.type !== 'string') return;
    if (event.type === 'automation-permission-result') {
      const pending = automationWaits.get(event.operationId);
      if (pending && pending.requestId === event.requestId) pending.finish(event.error === null ? null : new Error(typeof event.error === 'string' ? event.error : 'Native Automation response failed'), event.requirement);
      return;
    }
    if (event.requestId) {
      const record = [...sources.values()].find(candidate => candidate.requestId === event.requestId && candidate.nonce === event.receiverNonce);
      if (!record) return;
      if (event.type === 'source-authenticated') { record.authenticated = true; if (record.phase !== 'published') record.phase = 'authenticated'; }
      else if (['source-failed', 'source-timeout', 'source-disconnected', 'source-withdrawn', 'source-cancelled', 'source-permission-required'].includes(event.type)) {
        closeSourceViews(record);
        const reconnect = event.type === 'source-disconnected' && record.authenticated === true;
        record.permissionRequired = event.type === 'source-permission-required';
        record.phase = reconnect ? 'reconnecting' : 'failed'; record.error = event.type === 'source-permission-required' ? 'macOS Automation permission for the client’s native presentation connection has not been granted' : 'Native presentation source is unavailable';
        record.authenticated = false; record.nativeIdentity = null;
        record.abort.abort(); record.requestId = null; record.nonce = null;
      }
      changed(); return;
    }
    if (event.identity) {
      const record = [...sources.values()].find(value => value.nativeIdentity && JSON.stringify(value.nativeIdentity) === JSON.stringify(event.identity));
      if (record && event.type === 'published') { record.phase = 'published'; changed(); }
      if (record && event.type === 'invalidated') { closeSourceViews(record); record.phase = 'ended'; changed(); }
    }
    options.onNativeEvent?.(event);
  }
  async function reconcile(candidates, turns) {
    if (closed) return;
    options.requireAccess();
    for (const view of [...views.values()]) { try { view.owner.current(); } catch { closeView(view); } }
    const currentHome = options.homePath();
    if (home !== null && home !== currentHome) {
      for (const record of [...sources.values()]) withdraw(record);
      throw new Error('Native presentation belongs to another Home; restart the client');
    }
    home = currentHome;
    if (!Array.isArray(candidates) || candidates.length > 1024 || !Array.isArray(turns)) throw new Error('Native presentation source inventory is invalid');
    const byRun = new Map(turns.map(turn => [turn.run_id, turn])), admitted = new Set();
    for (const candidate of candidates) {
      const turn = byRun.get(candidate.input.runId);
      if (!turn || !Number.isSafeInteger(turn.turn_opened_movement_seq) || turn.turn_opened_movement_seq < 0) continue;
      const batch = turn.turn_opened_movement_seq;
      const key = JSON.stringify([candidate.input.workerEnvironmentId, candidate.source.scope.formation_pin, candidate.source.grant_request_id, candidate.input.appInstanceId, candidate.input.sourceWindowId, batch]);
      const existing = sources.get(key);
      if (existing && JSON.stringify(existing.identity) !== JSON.stringify(candidate.identity)) { withdraw(existing); }
      admitted.add(key);
      if (!turn.turn_open) {
        if (existing) closeSourceViews(existing);
        if (existing && !existing.authenticated) withdraw(existing);
        else if (existing && !existing.completionStatus) void completeSource(existing);
        if (native && completedBatches.get(turn.run_id) !== batch) { native.completeGroup({ groupId: turn.run_id }); completedBatches.set(turn.run_id, batch); }
        continue;
      }
      if (candidate.source.source.source_movement_seq < batch) continue;
      if (completedBatches.has(turn.run_id)) {
        native?.clearCompletedGroup({ groupId: turn.run_id }); completedBatches.delete(turn.run_id);
      }
      let record = sources.get(key);
      if (record && (record.phase === 'reconnecting' || record.phase === 'failed' && record.source.source.source_movement_seq !== candidate.source.source.source_movement_seq)) { withdraw(record); record = null; }
      if (!record) {
        record = { ...candidate, id: key, batch, phase: 'preparing', requestId: null, nonce: null, abort: new AbortController(), error: null };
        sources.set(key, record); void start(record);
      }
    }
    for (const record of [...sources.values()]) if (!admitted.has(record.id)) withdraw(record);
    if (native) options.syncHosts?.(native);
  }
  async function permissionSources(input, helper) {
    const matches = [...sources.values()].filter(record => record.input.workerEnvironmentId === input.workerEnvironmentId
      && record.input.runId === input.runId && record.permissionRequestId && !record.completionStatus);
    if (matches.length && (typeof native?.prepareAutomationPermission !== 'function' || typeof native?.beginAutomationPermission !== 'function' || typeof native?.cancelAutomationSettings !== 'function')) throw new Error('Client native Automation permission API is incomplete');
    const result = [], groups = new Map();
    for (const record of matches) {
      const requestId = record.permissionRequestId;
      const check = async () => {
        if (closed || sources.get(record.id) !== record || options.homePath() !== home || record.completionStatus || record.permissionRequestId !== requestId) throw new Error('Native Automation source is no longer desired');
        options.requireAccess(); const source = await options.prepareSource(record.input);
        if (source.home !== home || !isDeepStrictEqual(source.scope, record.preparedSource.scope) || !isDeepStrictEqual(source.source.source, record.source.source)) throw new Error('Native Automation source grant or observation changed');
        await source.current(); options.requireAccess();
        if (sources.get(record.id) !== record || options.homePath() !== home || record.completionStatus) throw new Error('Native Automation source was withdrawn');
        return source;
      };
      await check();
      const initial = validateAutomationRequirement(native.prepareAutomationPermission({ requestId }), helper);
      const member = { record, requestId, check };
      if (groups.has(initial.id)) { groups.get(initial.id).push(member); continue; }
      const members = [member]; groups.set(initial.id, members);
      let latest = initial;
      const exchange = async (operation, signal) => {
        const source = await check();
        if (signal?.aborted) throw signal.reason;
        if (!['status', 'request', 'open_settings'].includes(operation)) throw new Error('Invalid native Automation operation');
        if (operation !== 'status' && !latest[operation === 'request' ? 'can_request' : 'can_open_settings']) throw new Error('Native Automation operation is not currently available');
        if (automationWaits.size >= 1024) throw new Error('Native permission operation bound reached');
        const operationId = randomUUID();
        const requirement = await new Promise((resolve, reject) => {
          const cancel = () => finish(signal.reason || new Error('Permission window closed'));
          const timer = setTimeout(() => finish(new Error(operation === 'status' ? 'Native Automation status timed out' : 'Native Automation request outcome is uncertain; refresh status without repeating the request')), source.timeout);
          const finish = (error, value) => {
            if (!automationWaits.delete(operationId)) return;
            clearTimeout(timer); signal?.removeEventListener('abort', cancel);
            if (error && operation === 'open_settings') {
              try { native.cancelAutomationSettings({ requestId, operationId }); }
              catch (cleanup) { error = new AggregateError([error, cleanup], 'Settings observation outcome and native cleanup are uncertain'); }
            }
            error ? reject(error) : resolve(value);
          };
          automationWaits.set(operationId, { requestId, finish });
          signal?.addEventListener('abort', cancel, { once: true }); if (signal?.aborted) { cancel(); return; }
          try { native.beginAutomationPermission({ requestId, operationId, operation, timeoutMs: source.timeout }); } catch (error) { finish(error); }
        });
        await check(); if (signal?.aborted) throw signal.reason;
        const next = validateAutomationRequirement(requirement, helper);
        if (next.id !== initial.id || !isDeepStrictEqual(next.provenance, initial.provenance)) throw new Error('Native Automation process incarnation changed');
        latest = next; return next;
      };
      const resume = async () => {
        if (latest.status !== 'granted') return false;
        let resumed = false;
        for (const member of members) {
          const sourceRecord = member.record;
          if (sources.get(sourceRecord.id) !== sourceRecord || !sourceRecord.permissionRequired) continue;
          await member.check();
          if (native.prepareAutomationPermission({ requestId: member.requestId }).id !== initial.id) throw new Error('Native Automation recipient changed before reconnect');
          const replacement = { input: sourceRecord.input, source: sourceRecord.source, identity: sourceRecord.identity, id: sourceRecord.id, batch: sourceRecord.batch,
            phase: 'preparing', requestId: null, nonce: null, abort: new AbortController(), error: null };
          withdraw(sourceRecord); sources.set(replacement.id, replacement); await start(replacement); resumed = true;
        }
        return resumed;
      };
      result.push({ id: initial.id, current: check, exchange, resume });
    }
    return result;
  }
  function revokeAll() { const hadSources = sources.size > 0; for (const record of [...sources.values()]) withdraw(record); completedBatches.clear(); if (hadSources) changed(); }
  function stop() { if (closed) return; revokeAll(); closed = true; }
  return { reconcile, onEvent, snapshot, permissionSources, applicationView, closeViewsOwner, revokeAll, stop };
}

function presentationSource(descriptor, request) {
  validateSurface(descriptor, request);
  if (descriptor.lifecycle !== 'active' || !descriptor.formationPin || !/^[a-f0-9]{32}$/.test(descriptor.data?.host_generation || '')) throw new Error('Native presentation requires its exact Active room and host generation');
  const source = descriptor.data?.desktop?.applications?.find(value => value?.application?.app?.app_instance_id === request.appInstanceId);
  if (!source || source.scope?.worker_environment_id !== request.workerEnvironmentId || source.scope.formation_pin !== descriptor.formationPin || source.scope.host_generation !== descriptor.data.host_generation
    || source.application.app.host_generation !== source.scope.host_generation || typeof source.grant_request_id !== 'string') throw new Error('Native presentation app is outside the current exact room');
  const window = source.windows?.find(value => value?.window_id === request.sourceWindowId);
  if (!window || !Number.isSafeInteger(window.cg_window_id) || window.cg_window_id <= 0 || !window.source_image) throw new Error('Native presentation window lacks current observation evidence');
  const identity = { scope: source.scope, grant_request_id: source.grant_request_id, application: source.application, source_window_id: window.window_id, cg_window_id: window.cg_window_id };
  return { source: structuredClone(source), identity, scope: { ...source.scope, app_instance_id: request.appInstanceId, grant_request_id: source.grant_request_id } };
}


function permissionSurfaceInput(raw) {
  const fields = ['workerEnvironmentId', 'surfaceId', 'runId'];
  if (!raw || typeof raw !== 'object' || Array.isArray(raw) || Object.keys(raw).length !== fields.length || Object.keys(raw).some(key => !fields.includes(key))) throw new Error('Invalid permission surface identity');
  for (const field of fields) required(raw[field], field);
  return { ...raw };
}
function validatePermissionSnapshot(value, hostGeneration) {
  const exact = (object, fields) => object && typeof object === 'object' && !Array.isArray(object) && Object.keys(object).length === fields.length && Object.keys(object).every(key => fields.includes(key));
  const text = value => typeof value === 'string' && value.length > 0 && value.length <= 4096 && !value.includes('\0');
  if (!exact(value, ['schema_version', 'host_generation', 'helper', 'permissions']) || value.schema_version !== 1 || value.host_generation !== hostGeneration
    || !exact(value.helper, ['executable_path', 'display_name', 'process_id', 'bundle', 'responsible_app'])
    || !['executable_path', 'display_name'].every(key => text(value.helper[key])) || !path.isAbsolute(value.helper.executable_path)
    || ![value.helper.bundle, value.helper.responsible_app].every(bundle => bundle === null || exact(bundle, ['identifier', 'path']) && text(bundle.identifier) && text(bundle.path) && path.isAbsolute(bundle.path))
    || !Number.isSafeInteger(value.helper.process_id) || value.helper.process_id <= 0 || value.helper.process_id > 2147483647 || !Array.isArray(value.permissions) || value.permissions.length > 64) throw new Error('Invalid native permission helper status');
  const ids = new Set();
  for (const row of value.permissions) {
    if (!row || Object.keys(row).some(key => !['id', 'kind', 'title', 'status', 'can_request', 'can_open_settings', 'detail', 'provenance'].includes(key))
      || !text(row.id) || ids.has(row.id) || !text(row.title) || !['screen_capture', 'accessibility'].includes(row.kind)
      || !['granted', 'not_granted', 'not_determined', 'denied', 'restricted', 'unavailable'].includes(row.status)
      || typeof row.can_request !== 'boolean' || typeof row.can_open_settings !== 'boolean' || (row.detail != null && !text(row.detail))
      || !exact(row.provenance, ['process_id', 'api']) || row.provenance.process_id !== value.helper.process_id
      || row.provenance.api !== ({ screen_capture: 'CGPreflightScreenCaptureAccess', accessibility: 'AXIsProcessTrusted' })[row.kind]) throw new Error('Invalid native permission requirement');
    ids.add(row.id);
  }
  return structuredClone(value);
}
function validatePermissionSettingsSource(value, hostGeneration, requestId, permission) {
  const exact = (object, fields) => object && typeof object === 'object' && !Array.isArray(object) && Object.keys(object).length === fields.length && Object.keys(object).every(key => fields.includes(key));
  if (!exact(value, ['schema_version', 'host_generation', 'permission_request_id', 'permission', 'state', 'settings', 'window'])
    || value.schema_version !== 1 || value.host_generation !== hostGeneration || value.permission_request_id !== requestId
    || !['waiting', 'observed', 'ended'].includes(value.state)
    || (value.permission !== permission && !(value.state === 'ended' && value.permission === null))) throw new Error('Native Settings source changed its original human permission request');
  if (value.state !== 'observed') {
    if (value.settings !== null || value.window !== null) throw new Error('An unavailable Settings source must not publish a window');
  } else {
    const process = value.settings, window = value.window;
    if (!exact(process, ['process_id', 'executable_path', 'display_name', 'bundle', 'responsible_app', 'start_seconds', 'start_microseconds'])
      || !/^[1-9][0-9]{0,19}$/.test(process.start_seconds) || !/^(0|[1-9][0-9]{0,5})$/.test(process.start_microseconds)) throw new Error('Native Settings process incarnation is invalid');
    const { start_seconds, start_microseconds, ...helper } = process;
    validatePermissionSnapshot({ schema_version: 1, host_generation: hostGeneration, helper, permissions: [] }, hostGeneration);
    if (!exact(window, ['id', 'observation_sequence', 'bounds']) || !Number.isSafeInteger(window.id) || window.id <= 0 || window.id > 4294967295
      || !Number.isSafeInteger(window.observation_sequence) || window.observation_sequence <= 0
      || !exact(window.bounds, ['x', 'y', 'width', 'height']) || !Object.values(window.bounds).every(Number.isFinite)
      || window.bounds.width <= 0 || window.bounds.height <= 0) throw new Error('Native Settings window identity or geometry is invalid');
  }
  return structuredClone(value);
}
function validateAutomationRequirement(row, helper) {
  const exact = (value, fields) => value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length === fields.length && Object.keys(value).every(key => fields.includes(key));
  const text = value => typeof value === 'string' && value.length > 0 && value.length <= 4096 && !value.includes('\0');
  const processIdentity = value => {
    if (!exact(value, ['process_id', 'executable_path', 'display_name', 'bundle', 'responsible_app', 'start_seconds', 'start_microseconds'])
      || !/^[1-9][0-9]{0,19}$/.test(value.start_seconds) || !/^(0|[1-9][0-9]{0,5})$/.test(value.start_microseconds)) return false;
    const { start_seconds, start_microseconds, ...identity } = value;
    try { validatePermissionSnapshot({ schema_version: 1, host_generation: 'native', helper: identity, permissions: [] }, 'native'); return true; } catch { return false; }
  };
  if (!exact(row, ['id', 'kind', 'title', 'status', 'can_request', 'can_open_settings', 'detail', 'provenance']) || !text(row.id) || row.kind !== 'automation' || !text(row.title)
    || !['granted', 'denied', 'not_determined', 'unavailable'].includes(row.status) || typeof row.can_request !== 'boolean' || typeof row.can_open_settings !== 'boolean' || !text(row.detail)
    || !exact(row.provenance, ['api', 'event_class', 'event_id', 'sender', 'recipient']) || row.provenance.api !== 'AEDeterminePermissionToAutomateTarget'
    || row.provenance.event_class !== 'OcPi' || row.provenance.event_id !== 'BtV1' || !processIdentity(row.provenance.sender) || !processIdentity(row.provenance.recipient)
    || row.provenance.sender.process_id !== process.pid || row.provenance.recipient.process_id !== helper.process_id || row.provenance.recipient.executable_path !== helper.executable_path
    || (row.can_request && row.status !== 'not_determined')) throw new Error('Native Automation requirement does not identify the actual sender and bound recipient');
  return structuredClone(row);
}
function createDesktopPermissionHost(options) {
  for (const field of ['prepareSource', 'automationSources', 'getNativeHost', 'homePath']) if (typeof options?.[field] !== 'function') throw new Error(`Permission presentation requires ${field}`);
  const records = new Map(), byOwner = new Map(); let closed = false;
  function current(record) {
    if (closed || record.abort.signal.aborted || records.get(record.id) !== record || options.homePath() !== record.home) throw new Error('Permission presentation owner or Home changed');
    record.owner.current();
  }
  function remove(record) {
    if (records.get(record.id) !== record) return;
    records.delete(record.id); if (byOwner.get(record.owner.id) === record) byOwner.delete(record.owner.id);
    record.abort.abort(); record.native?.closePermissions({ presentationId: record.id, generation: record.generation });
    if (record.input.setup === true) record.source?.close?.();
  }
  const presentation = record => ({ ...record.snapshot, automation: record.automationRows });
  async function refresh(record) {
    record.snapshot = await record.source.exchange('status', null, record.abort.signal); current(record);
    const refs = record.input.setup === true ? [] : await options.automationSources(record.input, record.snapshot.helper); current(record);
    if (!Array.isArray(refs) || refs.length + record.snapshot.permissions.length > 64) throw new Error('Native Automation requirement list exceeds its bound');
    const map = new Map(), rows = [];
    for (const ref of refs) {
      if (map.has(ref.id) || record.snapshot.permissions.some(row => row.id === ref.id)) throw new Error('Native Automation requirement identity is duplicated');
      const row = validateAutomationRequirement(await ref.exchange('status', record.abort.signal), record.snapshot.helper); current(record);
      if (row.id !== ref.id) throw new Error('Native Automation requirement changed its source');
      map.set(row.id, ref); rows.push(row);
    }
    record.automations = map; record.automationRows = rows;
  }
  async function open(raw, owner) {
    const input = raw?.setup === true && Object.keys(raw).length === 1 ? { setup: true } : permissionSurfaceInput(raw);
    if (!owner || !Number.isSafeInteger(owner.id) || owner.id <= 0 || typeof owner.current !== 'function' || typeof owner.windowHandle !== 'function') throw new Error('Permission presentation requires its actual client owner');
    owner.current(); if (closed) throw new Error('Permission presentation client is closed');
    const old = byOwner.get(owner.id); if (old) remove(old);
    const record = { id: randomUUID(), generation: randomUUID(), input, owner, home: options.homePath(), abort: new AbortController(), sequence: 0, busy: false, snapshot: null, automations: new Map(), automationRows: [], native: null, source: null };
    records.set(record.id, record); byOwner.set(owner.id, record);
    try {
      const source = await options.prepareSource(input, owner); current(record);
      if (source.home !== record.home) throw new Error('Permission helper belongs to another Home'); record.source = source;
      await refresh(record); await source.current(); current(record);
      const native = await options.getNativeHost(); current(record);
      for (const method of ['showPermissions', 'updatePermissions', 'updatePermissionSettings', 'closePermissions']) if (typeof native?.[method] !== 'function') throw new Error('Packaged native permission UI is unavailable');
      record.native = native;
      native.showPermissions({ presentationId: record.id, generation: record.generation, windowHandle: owner.windowHandle(), snapshot: presentation(record) });
      return { opened: true };
    } catch (error) { remove(record); throw error; }
  }
  async function onEvent(event) {
    if (event?.type !== 'permission-action') return;
    const record = records.get(event.presentationId); if (!record || event.generation !== record.generation) return;
    if (event.operation === 'dismiss') { remove(record); return; }
    if (!Number.isSafeInteger(event.sequence) || event.sequence !== record.sequence + 1 || record.busy
      || !['status', 'request', 'open_settings'].includes(event.operation) || (event.operation === 'status' ? event.permission !== null : typeof event.permission !== 'string')) { remove(record); return; }
    record.sequence = event.sequence; record.busy = true;
    try { current(record); await record.source.current(); current(record); }
    catch { remove(record); return; }
    let failure = null, presentationFailure = null;
    const settingsObserver = {
      update: async source => {
        current(record); await record.source.current(); current(record);
        record.native.updatePermissionSettings({ presentationId: record.id, generation: record.generation, sequence: record.sequence, source });
      },
      unavailable: error => {
        presentationFailure = error instanceof Error ? error.message : 'Settings accessory observation failed';
        try { current(record); record.native.updatePermissionSettings({ presentationId: record.id, generation: record.generation, sequence: record.sequence, source: null }); } catch {}
      },
    };
    try {
      await refresh(record);
      let resumed = false;
      if (event.operation !== 'status') {
        const automation = record.automations.get(event.permission);
        if (automation) {
          const currentRow = record.automationRows.find(row => row.id === event.permission);
          const row = event.operation === 'request' && currentRow?.status === 'granted' ? currentRow : validateAutomationRequirement(await automation.exchange(event.operation, record.abort.signal), record.snapshot.helper); current(record);
          record.automationRows = record.automationRows.map(old => old.id === row.id ? row : old);
          if (row.status === 'granted') resumed = await automation.resume();
        } else {
          const row = record.snapshot.permissions.find(row => row.id === event.permission);
          if (!row) throw new Error('The native permission target is no longer current');
          if (event.operation !== 'request' || row.status !== 'granted') record.snapshot = await record.source.exchange(event.operation, event.permission, record.abort.signal, settingsObserver);
        }
      } else {
        for (const row of record.automationRows) if (row.status === 'granted') resumed = await record.automations.get(row.id).resume() || resumed;
      }
      if (resumed) await refresh(record);
    } catch (error) { failure = error instanceof Error ? error.message : 'The native permission action failed'; }
    try {
      current(record); await record.source.current(); current(record); record.busy = false;
      record.native.updatePermissions({ presentationId: record.id, generation: record.generation, sequence: record.sequence, snapshot: presentation(record), error: failure || (presentationFailure ? `Settings accessory unavailable: ${presentationFailure}` : null) });
    } catch { remove(record); }
  }
  function closeOwner(ownerId) { const record = byOwner.get(ownerId); if (record) remove(record); }
  function stop() { if (closed) return; closed = true; for (const record of [...records.values()]) remove(record); }
  return { open, onEvent, closeOwner, stop };
}

module.exports = { createDesktopSurfaceHost, createDesktopPresentationHost, createDesktopPermissionHost, presentationSource, validateEvidence, validatePermissionSnapshot, validatePermissionSettingsSource };
