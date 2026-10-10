const fs = require("node:fs");
const fsp = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const readline = require("node:readline");
const { isMainThread, parentPort, Worker, workerData } = require("node:worker_threads");

const LITELLM_RATES_URL = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const RATE_TTL_MS = 24 * 60 * 60 * 1000;
const MTIME_SLACK_MS = 36 * 60 * 60 * 1000;
const CACHE_RETENTION_MS = 92 * 24 * 60 * 60 * 1000;
const CACHE_VERSION = 1;
const EMPTY_TOTALS = Object.freeze({
  uncachedInputTokens: 0,
  cachedInputTokens: 0,
  cacheCreationTokens: 0,
  outputTokens: 0,
  reasoningTokens: 0,
});

function createCodexUsageReader(options = {}) {
  const codexHome = resolveCodexHome(options.codexHome);
  const sessionsRoot = path.join(codexHome, "sessions");
  const cacheRoot = requireAbsolutePath(options.cacheRoot, "cacheRoot");
  const concurrency = positiveInteger(options.concurrency, 8, 16);
  const parserPool = isMainThread ? createTranscriptParserPool(concurrency) : null;
  const fetchImpl = options.fetch === undefined ? globalThis.fetch : options.fetch;
  const scanCachePath = path.join(cacheRoot, "codex-usage-scan-cache-v1.json");
  const ratesCachePath = path.join(cacheRoot, "codex-usage-litellm-rates-v1.json");
  const fileCache = new Map();
  let scanCacheDirty = false;
  let scanCacheLoaded = false;
  let scanCacheLoad = null;
  let rates = new Map();
  let ratesFetchedAtMs = null;
  let pricingStatus = "unavailable";
  let ratesLoaded = false;
  let ratesLoad = null;
  let serial = Promise.resolve();

  return {
    readUsage(request = {}) {
      const work = serial.then(() => readCodexUsageInternal(request));
      serial = work.catch(() => undefined);
      return work;
    },
    get sessionsRoot() {
      return sessionsRoot;
    },
  };

  async function readCodexUsageInternal(request) {
    const startedAtMs = Date.now();
    const input = normalizeRequest(request, startedAtMs);
    await Promise.all([ensureScanCache(), ensureRates()]);
    const source = await scanSource(input);
    await persistScanCache(startedAtMs).catch(() => undefined);
    const buckets = aggregateRecords(source.records, input, rates);
    return {
      schema: "octopus.client_usage",
      schemaVersion: 1,
      provider: "codex",
      readAt: new Date().toISOString(),
      timeZone: input.timeZone,
      sinceDay: input.sinceDay,
      untilDay: input.untilDay,
      buckets,
      source: {
        kind: "codex_session_transcripts",
        resolvedHomePath: codexHome,
        path: sessionsRoot,
        status: source.status,
        scannedFiles: source.scannedFiles,
        cachedFiles: source.cachedFiles,
        skippedFiles: source.skippedFiles,
        failedFiles: source.failedFiles,
        inaccessibleDirectories: source.inaccessibleDirectories,
        malformedRecords: source.malformedRecords,
        unmodeledRecords: source.unmodeledRecords,
        distinctSessions: source.sessions.size,
        message: source.message,
      },
      pricing: {
        status: pricingStatus,
        source: LITELLM_RATES_URL,
        fetchedAt: ratesFetchedAtMs === null ? null : new Date(ratesFetchedAtMs).toISOString(),
        knownModels: rates.size,
      },
      scanDurationMs: Math.max(0, Date.now() - startedAtMs),
    };
  }

  async function scanSource(input) {
    let files;
    try {
      files = await listJsonlFiles(sessionsRoot, input.mtimeCutoffMs);
    } catch (error) {
      if (error && error.code === "ENOENT") return emptySource("missing", "No Codex session directory exists at the resolved home path.");
      return emptySource("failed", boundedError(error));
    }
    const results = await mapLimit(files.entries, concurrency, async (file) => readFileRecords(file, input.timeZone));
    const source = emptySource("ok", null);
    source.inaccessibleDirectories = files.listingFailures;
    let partial = files.listingFailures > 0;
    for (const result of results) {
      if (result.error) {
        source.failedFiles += 1;
        partial = true;
        continue;
      }
      if (result.cached) source.cachedFiles += 1;
      else source.scannedFiles += 1;
      source.malformedRecords += result.malformedRecords;
      source.unmodeledRecords += result.unmodeledRecords;
      if (result.records.length === 0) source.skippedFiles += 1;
      for (const record of result.records) {
        source.records.push(record);
        if (record.day < input.sinceDay || record.day > input.untilDay) continue;
        for (const sessionId of record.sessions) source.sessions.add(sessionId);
      }
    }
    if (partial) {
      source.status = "partial";
      source.message = files.listingFailures > 0
        ? "Part of the Codex session tree could not be listed; totals exclude inaccessible paths."
        : "One or more Codex session files could not be read; totals exclude those files.";
    }
    return source;
  }

  async function readFileRecords(file, timeZone) {
    const cached = fileCache.get(file.path);
    if (cached && cached.size === file.size && cached.mtimeMs === file.mtimeMs && cached.timeZone === timeZone) {
      return { ...cached, cached: true };
    }
    const parsed = await (parserPool
      ? parserPool.parse(file.path, timeZone).catch(() => null)
      : parseCodexTranscript(file.path, timeZone));
    if (parsed === null) return { error: true, cached: false };
    const entry = {
      size: file.size,
      mtimeMs: file.mtimeMs,
      timeZone,
      records: parsed.records,
      malformedRecords: parsed.malformedRecords,
      unmodeledRecords: parsed.unmodeledRecords,
      cached: false,
    };
    fileCache.set(file.path, entry);
    scanCacheDirty = true;
    return entry;
  }

  async function ensureScanCache() {
    if (scanCacheLoaded) return;
    if (scanCacheLoad) return scanCacheLoad;
    scanCacheLoad = (async () => {
      const parsed = await readJson(scanCachePath);
      const restored = decodeScanCache(parsed);
      for (const [filePath, entry] of restored) fileCache.set(filePath, entry);
      scanCacheLoaded = true;
    })().finally(() => {
      scanCacheLoad = null;
    });
    return scanCacheLoad;
  }

  async function persistScanCache(nowMs) {
    if (!scanCacheDirty) return;
    const entries = {};
    for (const [filePath, entry] of fileCache) {
      if (entry.mtimeMs >= nowMs - CACHE_RETENTION_MS) entries[filePath] = entry;
    }
    await writeJsonAtomically(scanCachePath, { version: CACHE_VERSION, files: entries });
    scanCacheDirty = false;
  }

  async function ensureRates() {
    if (!ratesLoaded) {
      if (!ratesLoad) {
        ratesLoad = (async () => {
          const cached = await readJson(ratesCachePath);
          if (isObject(cached) && Number.isFinite(cached.fetchedAtMs)) {
            const parsed = parseRateTable(cached.document);
            if (parsed.size > 0) {
              rates = parsed;
              ratesFetchedAtMs = cached.fetchedAtMs;
              pricingStatus = "cached";
            }
          }
          ratesLoaded = true;
        })().finally(() => {
          ratesLoad = null;
        });
      }
      await ratesLoad;
    }
    const nowMs = Date.now();
    if (ratesFetchedAtMs !== null && nowMs - ratesFetchedAtMs < RATE_TTL_MS) return;
    if (typeof fetchImpl !== "function") return;
    let response;
    try {
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 10_000);
      try {
        response = await fetchImpl(LITELLM_RATES_URL, { signal: controller.signal });
      } finally {
        clearTimeout(timeout);
      }
      if (!response || !response.ok) return;
      const document = await response.json();
      const parsed = parseRateTable(document);
      if (parsed.size === 0) return;
      rates = parsed;
      ratesFetchedAtMs = nowMs;
      pricingStatus = "fresh";
      await writeJsonAtomically(ratesCachePath, { fetchedAtMs: nowMs, document }).catch(() => undefined);
    } catch {}
  }
}

async function parseCodexTranscript(filePath, timeZone) {
  const state = { model: "", sessionId: "", lastSignature: null };
  const buckets = new Map();
  const dayFor = dayFormatter(timeZone);
  let malformedRecords = 0;
  let unmodeledRecords = 0;
  try {
    const input = fs.createReadStream(filePath, { encoding: "utf8" });
    const lines = readline.createInterface({ input, crlfDelay: Infinity });
    for await (const line of lines) {
      if (!line.includes('"token_count"') && !line.includes('"turn_context"') && !line.includes('"session_meta"')) continue;
      let row;
      try {
        row = JSON.parse(line);
      } catch {
        if (line.includes('"token_count"')) malformedRecords += 1;
        continue;
      }
      if (!isObject(row) || !isObject(row.payload)) continue;
      const payload = row.payload;
      if (row.type === "session_meta") {
        const sessionId = payload.id ?? payload.session_id;
        if (typeof sessionId === "string") state.sessionId = sessionId;
        continue;
      }
      if (row.type === "turn_context") {
        if (typeof payload.model === "string" && payload.model.trim()) state.model = payload.model.trim();
        continue;
      }
      if (payload.type !== "token_count") continue;
      const timestampMs = parseTimestamp(row.timestamp);
      const lastUsage = isObject(payload.info) && isObject(payload.info.last_token_usage) ? payload.info.last_token_usage : null;
      if (timestampMs === null || lastUsage === null) {
        malformedRecords += 1;
        continue;
      }
      if (!state.model) {
        unmodeledRecords += 1;
        continue;
      }
      const signature = JSON.stringify(lastUsage);
      if (signature === state.lastSignature) continue;
      state.lastSignature = signature;
      const inputTokens = nonNegativeInt(lastUsage.input_tokens);
      const cachedInputTokens = nonNegativeInt(lastUsage.cached_input_tokens);
      const cacheCreationTokens = nonNegativeInt(lastUsage.cache_write_input_tokens);
      const outputTokens = nonNegativeInt(lastUsage.output_tokens);
      const totals = {
        uncachedInputTokens: Math.max(0, inputTokens - cachedInputTokens - cacheCreationTokens),
        cachedInputTokens,
        cacheCreationTokens,
        outputTokens,
        reasoningTokens: Math.min(outputTokens, nonNegativeInt(lastUsage.reasoning_output_tokens)),
      };
      if (totalTokens(totals) === 0) continue;
      const day = dayFor(timestampMs);
      const key = `${day}\u0000${state.model}`;
      let bucket = buckets.get(key);
      if (!bucket) {
        bucket = { day, model: state.model, totals: { ...EMPTY_TOTALS }, records: 0, sessions: new Set() };
        buckets.set(key, bucket);
      }
      addTotals(bucket.totals, totals);
      bucket.records += 1;
      if (state.sessionId) bucket.sessions.add(state.sessionId);
    }
  } catch {
    return null;
  }
  return {
    records: [...buckets.values()].map((bucket) => ({
      day: bucket.day,
      model: bucket.model,
      totals: bucket.totals,
      records: bucket.records,
      sessions: [...bucket.sessions],
    })),
    malformedRecords,
    unmodeledRecords,
  };
}

function aggregateRecords(records, input, rates) {
  const buckets = new Map();
  for (const record of records) {
    if (record.day < input.sinceDay || record.day > input.untilDay) continue;
    const key = `${record.day}\u0000${record.model}`;
    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = {
        day: record.day,
        model: record.model,
        totals: { ...EMPTY_TOTALS },
        costUsd: 0,
        cacheSavingsUsd: 0,
        records: 0,
        unpricedRecords: 0,
        sessions: new Set(),
      };
      buckets.set(key, bucket);
    }
    addTotals(bucket.totals, record.totals);
    const rate = lookupRate(rates, record.model);
    if (rate) {
      bucket.costUsd += priceTotals(record.totals, rate);
      bucket.cacheSavingsUsd += record.totals.cachedInputTokens * (rate.inputCostPerToken - rate.cacheReadCostPerToken);
    } else {
      bucket.unpricedRecords += record.records;
    }
    bucket.records += record.records;
    for (const sessionId of record.sessions) bucket.sessions.add(sessionId);
  }
  return [...buckets.values()].map((bucket) => ({
    day: bucket.day,
    model: bucket.model,
    ...bucket.totals,
    totalTokens: totalTokens(bucket.totals),
    costUsd: bucket.costUsd,
    cacheSavingsUsd: bucket.cacheSavingsUsd,
    priced: bucket.unpricedRecords !== bucket.records,
    records: bucket.records,
    unpricedRecords: bucket.unpricedRecords,
    sessions: bucket.sessions.size,
  })).sort((left, right) => left.day.localeCompare(right.day) || left.model.localeCompare(right.model));
}

async function listJsonlFiles(root, sinceMs) {
  const files = [];
  let listingFailures = 0;
  async function walk(directory, isRoot) {
    let entries;
    try {
      entries = await fsp.readdir(directory, { withFileTypes: true });
    } catch (error) {
      if (isRoot) throw error;
      listingFailures += 1;
      return;
    }
    for (const entry of entries) {
      const child = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        await walk(child, false);
        continue;
      }
      if (!entry.isFile() || !entry.name.endsWith(".jsonl")) continue;
      try {
        const stat = await fsp.stat(child);
        if (stat.mtimeMs >= sinceMs) files.push({ path: child, size: stat.size, mtimeMs: stat.mtimeMs });
      } catch {}
    }
  }
  await walk(root, true);
  return { entries: files, listingFailures };
}

function normalizeRequest(request, nowMs) {
  if (!isObject(request)) throw new TypeError("Codex usage request must be an object.");
  const windowDays = request.windowDays;
  if (windowDays !== 7 && windowDays !== 30 && windowDays !== 90) {
    throw new RangeError("Codex usage windowDays must be one of 7, 30, or 90.");
  }
  if (typeof request.timeZone !== "string" || !request.timeZone.trim()) {
    throw new TypeError("Codex usage timeZone must be a non-empty IANA time zone.");
  }
  const timeZone = request.timeZone.trim();
  let untilDay;
  try {
    untilDay = dayFormatter(timeZone)(nowMs);
  } catch {
    throw new RangeError(`Codex usage timeZone is invalid: ${timeZone}`);
  }
  const sinceDay = shiftDay(untilDay, -(windowDays - 1));
  return {
    windowDays,
    timeZone,
    sinceDay,
    untilDay,
    mtimeCutoffMs: Date.parse(`${sinceDay}T00:00:00.000Z`) - MTIME_SLACK_MS,
  };
}

function dayFormatter(timeZone) {
  const formatter = new Intl.DateTimeFormat("en-US", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  });
  return (timestampMs) => {
    const parts = formatter.formatToParts(new Date(timestampMs));
    const values = Object.fromEntries(parts.filter((part) => part.type !== "literal").map((part) => [part.type, part.value]));
    return `${values.year}-${values.month}-${values.day}`;
  };
}

function shiftDay(day, days) {
  const value = new Date(`${day}T00:00:00.000Z`);
  value.setUTCDate(value.getUTCDate() + days);
  return value.toISOString().slice(0, 10);
}

function parseRateTable(document) {
  const table = new Map();
  if (!isObject(document)) return table;
  for (const [name, raw] of Object.entries(document)) {
    if (!isObject(raw)) continue;
    const inputCostPerToken = finiteNumber(raw.input_cost_per_token);
    const outputCostPerToken = finiteNumber(raw.output_cost_per_token);
    if (inputCostPerToken === null || outputCostPerToken === null) continue;
    table.set(normalizeModelName(name), {
      inputCostPerToken,
      outputCostPerToken,
      cacheReadCostPerToken: finiteNumber(raw.cache_read_input_token_cost) ?? inputCostPerToken,
      cacheCreationCostPerToken: finiteNumber(raw.cache_creation_input_token_cost) ?? inputCostPerToken,
    });
  }
  return table;
}

function lookupRate(table, model) {
  const normalized = normalizeModelName(model);
  if (!normalized || new Set(["<synthetic>", "synthetic", "opus", "sonnet", "haiku", "fable"]).has(normalized)) return null;
  return table.get(normalized) ?? null;
}

function normalizeModelName(model) {
  const normalized = String(model).trim().toLowerCase();
  const separator = normalized.lastIndexOf("/");
  return separator === -1 ? normalized : normalized.slice(separator + 1);
}

function priceTotals(totals, rate) {
  return totals.uncachedInputTokens * rate.inputCostPerToken
    + totals.cachedInputTokens * rate.cacheReadCostPerToken
    + totals.cacheCreationTokens * rate.cacheCreationCostPerToken
    + totals.outputTokens * rate.outputCostPerToken;
}

function addTotals(target, source) {
  target.uncachedInputTokens += source.uncachedInputTokens;
  target.cachedInputTokens += source.cachedInputTokens;
  target.cacheCreationTokens += source.cacheCreationTokens;
  target.outputTokens += source.outputTokens;
  target.reasoningTokens += source.reasoningTokens;
}

function totalTokens(totals) {
  return totals.uncachedInputTokens + totals.cachedInputTokens + totals.cacheCreationTokens + totals.outputTokens;
}

function decodeScanCache(document) {
  const cache = new Map();
  if (!isObject(document) || document.version !== CACHE_VERSION || !isObject(document.files)) return cache;
  for (const [filePath, raw] of Object.entries(document.files)) {
    if (!path.isAbsolute(filePath) || !isObject(raw) || !Number.isFinite(raw.size) || !Number.isFinite(raw.mtimeMs) || typeof raw.timeZone !== "string" || !Array.isArray(raw.records)) continue;
    const records = [];
    let valid = true;
    for (const record of raw.records) {
      if (!isRecord(record)) {
        valid = false;
        break;
      }
      records.push(record);
    }
    if (!valid || !Number.isSafeInteger(raw.malformedRecords) || raw.malformedRecords < 0 || !Number.isSafeInteger(raw.unmodeledRecords) || raw.unmodeledRecords < 0) continue;
    cache.set(filePath, {
      size: raw.size,
      mtimeMs: raw.mtimeMs,
      timeZone: raw.timeZone,
      records,
      malformedRecords: raw.malformedRecords,
      unmodeledRecords: raw.unmodeledRecords,
      cached: false,
    });
  }
  return cache;
}

function isRecord(value) {
  return isObject(value)
    && typeof value.day === "string"
    && /^\d{4}-\d{2}-\d{2}$/.test(value.day)
    && typeof value.model === "string"
    && Array.isArray(value.sessions)
    && value.sessions.every((sessionId) => typeof sessionId === "string")
    && Number.isSafeInteger(value.records)
    && value.records > 0
    && isObject(value.totals)
    && ["uncachedInputTokens", "cachedInputTokens", "cacheCreationTokens", "outputTokens", "reasoningTokens"].every((key) => Number.isSafeInteger(value.totals[key]) && value.totals[key] >= 0);
}

function emptySource(status, message) {
  return {
    status,
    message,
    records: [],
    sessions: new Set(),
    scannedFiles: 0,
    cachedFiles: 0,
    skippedFiles: 0,
    failedFiles: 0,
    inaccessibleDirectories: 0,
    malformedRecords: 0,
    unmodeledRecords: 0,
  };
}

function createTranscriptParserPool(size) {
  const queue = [];
  const workers = [];
  let nextTaskId = 1;

  const pump = () => {
    for (const state of workers) {
      if (state.task || queue.length === 0) continue;
      const task = queue.shift();
      state.task = task;
      state.worker.ref();
      state.worker.postMessage({ id: task.id, filePath: task.filePath, timeZone: task.timeZone });
    }
  };

  const spawnWorker = () => {
    const state = {
      worker: new Worker(__filename, { workerData: { octopusCodexUsageParser: true } }),
      task: null,
      closed: false,
    };
    workers.push(state);
    state.worker.on("message", (message) => {
      const task = state.task;
      if (!task || !isObject(message) || message.id !== task.id) return;
      state.task = null;
      state.worker.unref();
      if (typeof message.error === "string") task.reject(new Error(message.error));
      else task.resolve(message.result ?? null);
      pump();
    });
    const fail = (error) => {
      if (state.closed) return;
      state.closed = true;
      const task = state.task;
      state.task = null;
      if (task) task.reject(error instanceof Error ? error : new Error(String(error)));
      const index = workers.indexOf(state);
      if (index >= 0) workers.splice(index, 1);
      spawnWorker();
      pump();
    };
    state.worker.once("error", fail);
    state.worker.once("exit", (code) => {
      if (!state.closed) fail(new Error(`Codex usage parser worker exited with code ${code}`));
    });
    state.worker.unref();
  };

  for (let index = 0; index < size; index += 1) spawnWorker();

  return {
    parse(filePath, timeZone) {
      return new Promise((resolve, reject) => {
        queue.push({ id: nextTaskId, filePath, timeZone, resolve, reject });
        nextTaskId += 1;
        pump();
      });
    },
  };
}

async function mapLimit(items, limit, mapper) {
  const results = new Array(items.length);
  let cursor = 0;
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (true) {
      const index = cursor;
      cursor += 1;
      if (index >= items.length) return;
      results[index] = await mapper(items[index]);
    }
  }));
  return results;
}

async function readJson(filePath) {
  try {
    return JSON.parse(await fsp.readFile(filePath, "utf8"));
  } catch {
    return null;
  }
}

async function writeJsonAtomically(filePath, value) {
  await fsp.mkdir(path.dirname(filePath), { recursive: true, mode: 0o700 });
  const temporary = `${filePath}.${process.pid}.${Math.random().toString(16).slice(2)}.tmp`;
  await fsp.writeFile(temporary, JSON.stringify(value), { encoding: "utf8", mode: 0o600, flag: "wx" });
  await fsp.rename(temporary, filePath);
  await fsp.chmod(filePath, 0o600).catch(() => undefined);
}

function resolveCodexHome(explicitHome) {
  const configured = explicitHome === undefined ? process.env.CODEX_HOME : explicitHome;
  if (configured === undefined || configured === "") return path.join(os.homedir(), ".codex");
  return requireAbsolutePath(configured, "CODEX_HOME");
}

function requireAbsolutePath(value, label) {
  if (typeof value !== "string" || !value.trim() || !path.isAbsolute(value)) throw new TypeError(`${label} must be an absolute path.`);
  return path.resolve(value);
}

function positiveInteger(value, fallback, maximum) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value < 1 || value > maximum) throw new RangeError(`concurrency must be an integer from 1 to ${maximum}.`);
  return value;
}

function nonNegativeInt(value) {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.trunc(value) : 0;
}

function parseTimestamp(value) {
  if (typeof value !== "string") return null;
  const timestampMs = Date.parse(value);
  return Number.isFinite(timestampMs) ? timestampMs : null;
}

function finiteNumber(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function isObject(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function boundedError(error) {
  const message = error instanceof Error ? error.message : String(error);
  return `Codex session directory could not be scanned: ${message.slice(0, 240)}`;
}

if (!isMainThread && workerData?.octopusCodexUsageParser === true && parentPort) {
  parentPort.on("message", async (task) => {
    if (!isObject(task) || !Number.isSafeInteger(task.id) || typeof task.filePath !== "string" || typeof task.timeZone !== "string") return;
    try {
      parentPort.postMessage({ id: task.id, result: await parseCodexTranscript(task.filePath, task.timeZone) });
    } catch (error) {
      parentPort.postMessage({ id: task.id, error: boundedError(error) });
    }
  });
}

module.exports = {
  createCodexUsageReader,
  parseCodexTranscript,
  parseRateTable,
  totalTokens,
};
