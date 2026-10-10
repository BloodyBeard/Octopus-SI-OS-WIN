'use strict';

const crypto = require('node:crypto');
const { startWaitObservation } = require('./browser-load-wait.cjs');
const IDENTITY_FIELDS = ['host_generation', 'worker_environment_id', 'formation_pin', 'browser_context_id', 'target_id', 'debugger_epoch', 'start_source_movement_seq', 'start_artifact_content_hash'];
const HANDLE_FIELDS = ['wait_id', ...IDENTITY_FIELDS, 'condition_digest'];
const ERROR_BYTES = 4096;
const TERMINAL_RESERVE = ERROR_BYTES + 1024;
const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]`
  : value !== null && typeof value === 'object' ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value);
const digest = value => crypto.createHash('sha256').update(canonical(value)).digest('hex');
const fail = (code, message) => Object.assign(new Error(message), { code });
function requireValue(value, message, code = 'invalid_waiter') { if (!value) throw fail(code, message); }
function exact(value, keys) {
  requireValue(value && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key)), 'Waiter object fields differ from the declared schema');
}
function identity(value) {
  exact(value, IDENTITY_FIELDS);
  for (const key of ['host_generation', 'formation_pin', 'start_artifact_content_hash']) {
    requireValue(typeof value[key] === 'string' && new RegExp(`^[a-f0-9]{${key === 'host_generation' ? 32 : 64}}$`).test(value[key]), 'Waiter lost its source digest or generation');
  }
  requireValue(typeof value.worker_environment_id === 'string' && value.worker_environment_id.length > 0 && value.worker_environment_id.length <= 2048
    && value.worker_environment_id.trim() === value.worker_environment_id && !/[\x00-\x1f\x7f]/.test(value.worker_environment_id), 'Waiter lost its exact room');
  for (const key of ['browser_context_id', 'target_id']) requireValue(typeof value[key] === 'string' && /^[A-Za-z0-9_-]{1,256}$/.test(value[key]), 'Waiter lost its owned target or context');
  requireValue(Number.isSafeInteger(value.debugger_epoch) && value.debugger_epoch >= 0
    && Number.isSafeInteger(value.start_source_movement_seq) && value.start_source_movement_seq > 0, 'Waiter source counters are invalid');
  return structuredClone(value);
}
function condition(args) {
  requireValue(args && typeof args === 'object' && !Array.isArray(args), 'Wait start requires its original arguments');
  const keys = ['phase', 'target_id', 'url_pattern', 'wait_until', 'load_state', 'timeout_ms'];
  requireValue(Object.keys(args).every(key => keys.includes(key)) && args.phase === 'start'
    && Object.hasOwn(args, 'url_pattern') !== Object.hasOwn(args, 'load_state'), 'Wait start requires exactly one supported source condition');
  if (Object.hasOwn(args, 'url_pattern')) requireValue(typeof args.url_pattern === 'string' && args.url_pattern.length > 0
    && (!Object.hasOwn(args, 'wait_until') || ['commit', 'load', 'domcontentloaded'].includes(args.wait_until)), 'Invalid exact URL wait condition');
  else requireValue(['load', 'domcontentloaded'].includes(args.load_state) && !Object.hasOwn(args, 'wait_until'), 'Invalid exact load wait condition');
  if (Object.hasOwn(args, 'timeout_ms')) requireValue(Number.isSafeInteger(args.timeout_ms) && args.timeout_ms >= 0 && args.timeout_ms <= 300000, 'Invalid source wait deadline');
  const { phase, target_id, ...value } = args;
  return value;
}
function boundedError(error) {
  const code = typeof error?.code === 'string' && /^[a-z0-9_]{1,128}$/.test(error.code) ? error.code : 'wait_observation_failed';
  return { code, message: Buffer.from(String(error?.message ?? error)).subarray(0, ERROR_BYTES).toString('utf8') };
}
function createBrowserWaiters({ maxWaiters, maxBytes }) {
  requireValue(Number.isSafeInteger(maxWaiters) && maxWaiters > 0 && maxWaiters <= 16 && Number.isSafeInteger(maxBytes) && maxBytes >= TERMINAL_RESERVE && maxBytes <= 8 * 1024 * 1024, 'Waiter owner requires declared finite resource bounds');
  const records = new Map();
  let retainedBytes = 0, closed = false;
  function find(handle) {
    requireValue(!closed, 'Waiter owner has ended', 'formation_withdrawn');
    exact(handle, HANDLE_FIELDS);
    const record = records.get(handle.wait_id);
    requireValue(record && canonical(record.handle) === canonical(handle), 'Waiter is not the exact owned source record', 'waiter_unavailable');
    return record;
  }
  function settle(record, outcome) {
    if (record.outcome !== null) return;
    let value = outcome;
    let size = Buffer.byteLength(JSON.stringify(value));
    if (retainedBytes - record.cost + record.baseCost + Math.max(TERMINAL_RESERVE, size) > maxBytes) {
      value = { status: 'failed', error: { code: 'waiter_capacity', message: 'Wait observation exceeded its declared aggregate evidence bound' } };
      size = Buffer.byteLength(JSON.stringify(value));
    }
    retainedBytes -= record.cost;
    record.cost = record.baseCost + Math.max(TERMINAL_RESERVE, size);
    retainedBytes += record.cost;
    record.outcome = value;
    record.resolve(value);
  }
  return Object.freeze({
    async start({ pin, creator, arguments: args, cdp, validateSource }) {
      requireValue(!closed, 'Waiter owner has ended', 'formation_withdrawn');
      const source = identity(pin), value = condition(args);
      requireValue(args.target_id === source.target_id && typeof creator === 'string' && creator.length > 0 && creator.length <= 2048
        && typeof validateSource === 'function', 'Waiter requires the exact creator and source lifetime');
      await validateSource();
      requireValue(!closed && records.size < maxWaiters, 'Waiter owner reached its declared retained-record bound', 'waiter_capacity');
      const handle = Object.freeze({ wait_id: crypto.randomBytes(16).toString('hex'), ...source, condition_digest: digest(value) });
      const baseCost = Buffer.byteLength(JSON.stringify({ handle, condition: value, creator }));
      const cost = baseCost + TERMINAL_RESERVE;
      requireValue(retainedBytes + cost <= maxBytes, 'Waiter owner reached its declared aggregate evidence bound', 'waiter_capacity');
      let resolve;
      const done = new Promise(finish => { resolve = finish; });
      const record = { handle, creator, baseCost, cost, done, resolve, outcome: null, reader: null, controller: new AbortController() };
      records.set(handle.wait_id, record); retainedBytes += cost;
      const passive = {
        call: async (tabId, method, params) => {
          requireValue(tabId === source.target_id && ['Page.enable', 'Page.getFrameTree', 'Runtime.evaluate'].includes(method), 'Retained wait attempted a non-observation command');
          if (record.controller.signal.aborted) throw record.controller.signal.reason;
          await validateSource();
          const result = await cdp.call(tabId, method, params);
          if (record.controller.signal.aborted) throw record.controller.signal.reason;
          await validateSource(); return result;
        },
        addListener: (name, listener) => cdp.addListener(name, listener),
        removeListener: (name, listener) => cdp.removeListener(name, listener),
      };
      const observation = startWaitObservation(passive, source.target_id, value, record.controller.signal);
      observation.outcome.then(result => settle(record, { status: 'satisfied', observation: result }), error => {
        const detail = boundedError(error);
        const status = detail.code === 'wait_cancelled' ? 'cancelled' : ['wait_source_lost', 'formation_withdrawn'].includes(detail.code) ? 'source_lost' : 'failed';
        settle(record, { status, error: detail });
      });
      await observation.ready;
      return { waiter: structuredClone(handle), status: record.outcome?.status ?? 'pending', effective_timeout_ms: observation.effective_timeout_ms };
    },
    async complete(handle, { reader, signal, validate }) {
      requireValue(typeof reader === 'string' && reader.length > 0 && reader.length <= 2048 && typeof validate === 'function', 'Wait completion requires its own admitted invocation');
      const record = find(handle);
      requireValue(record.reader === null, 'Waiter already has a completion reservation', 'waiter_already_claimed');
      await validate();
      requireValue(find(handle) === record && record.reader === null, 'Waiter completion reservation changed', 'waiter_already_claimed');
      record.reader = reader;
      let abort;
      const cancelled = new Promise((_, reject) => {
        abort = () => reject(fail('execution_uncertain', 'Wait completion connection ended; observation was not restarted'));
        signal?.addEventListener('abort', abort, { once: true }); if (signal?.aborted) abort();
      });
      try {
        const outcome = await Promise.race([record.done, cancelled]);
        await validate();
        requireValue(find(handle) === record && record.reader === reader, 'Wait completion reservation ended', 'execution_uncertain');
        return { waiter: structuredClone(record.handle), ...structuredClone(outcome) };
      } catch (error) { if (record.reader === reader) record.reader = null; throw error; }
      finally { signal?.removeEventListener('abort', abort); }
    },
    capture(handle, reader) {
      const record = find(handle);
      requireValue(record.reader === reader && record.outcome !== null, 'Only an admitted terminal completion can capture a waiter');
      records.delete(handle.wait_id); retainedBytes -= record.cost;
      return { captured: true };
    },
    cancel(handle) {
      const record = find(handle);
      if (record.outcome === null) record.controller.abort(fail('wait_cancelled', 'Browser wait explicitly cancelled'));
      return { waiter: structuredClone(record.handle), status: record.outcome?.status ?? 'cancelling' };
    },
    releaseReader(reader) { for (const record of records.values()) if (record.reader === reader) record.reader = null; },
    releaseCreator(creator) { for (const record of records.values()) if (record.creator === creator && record.outcome === null) record.controller.abort(fail('wait_source_lost', 'The admitted wait creator connection ended')); },
    releaseTarget(targetId, debuggerEpoch) { for (const record of records.values()) if (record.handle.target_id === targetId && record.handle.debugger_epoch === debuggerEpoch && record.outcome === null) record.controller.abort(fail('wait_source_lost', 'The exact native wait target or debugger ended')); },
    stop() {
      if (closed) return;
      closed = true;
      for (const record of records.values()) {
        record.controller.abort(fail('formation_withdrawn', 'The formed wait owner ended'));
        settle(record, { status: 'source_lost', error: { code: 'formation_withdrawn', message: 'The formed wait owner ended' } });
      }
      records.clear(); retainedBytes = 0;
    },
    retained() { return { records: records.size, bytes: retainedBytes }; },
  });
}
module.exports = { createBrowserWaiters };
