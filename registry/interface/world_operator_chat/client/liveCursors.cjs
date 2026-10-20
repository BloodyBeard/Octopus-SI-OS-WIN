module.exports = { closeLedgerChangeHub, createLedgerChangeHub, liveCursorIsAhead, liveCursorPath, mergeLiveCursors, mergeOperatorStreamCursors, normalizeLiveCursor, operatorStreamCursors, persistLiveCursor, readPersistedLiveCursor, rememberLiveCursor, watchLedgerChange, withOperatorStreamCursors };

const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const { ledgerChangeHubs, liveCursorWriteTails, liveLedgerCursors } = require("./constants.cjs");
const { syncDirectory } = require("./publication.cjs");
const { laterStreamCursor } = require("./turnStreams.cjs");
const { contentHashHex, isObject, nonNegativeInteger, positiveInteger, randomToken, readJsonIfPresent, requiredObject } = require("./values.cjs");

function watchLedgerChange(movementsPath, waitMs) {
  let hub = ledgerChangeHubs.get(movementsPath);
  if (!hub) {
    hub = createLedgerChangeHub(movementsPath);
    ledgerChangeHubs.set(movementsPath, hub);
  }
  let settled = false;
  let timer = null;
  let resolvePromise;
  const promise = new Promise((resolve) => {
    resolvePromise = resolve;
  });
  const finish = (value) => {
    if (settled) return;
    settled = true;
    if (timer) clearTimeout(timer);
    hub.waiters.delete(finish);
    if (hub.waiters.size === 0) closeLedgerChangeHub(movementsPath, hub);
    resolvePromise(value);
  };
  hub.waiters.add(finish);
  timer = setTimeout(() => finish(false), waitMs);
  return {
    promise,
    cancel: () => finish(false),
  };
}

function createLedgerChangeHub(movementsPath) {
  const hub = {
    closed: false,
    statListener: null,
    statWatcher: null,
    waiters: new Set(),
    watcher: null,
  };
  const wake = () => {
    if (hub.closed) return;
    const waiters = [...hub.waiters];
    closeLedgerChangeHub(movementsPath, hub);
    for (const finish of waiters) finish(true);
  };
  hub.statListener = (current, previous) => {
    if (
      current.dev === previous.dev
      && current.ino === previous.ino
      && current.size === previous.size
      && current.mtimeMs === previous.mtimeMs
      && current.nlink === previous.nlink
    ) return;
    wake();
  };
  hub.statWatcher = fsSync.watchFile(
    movementsPath,
    { interval: 250, persistent: false },
    hub.statListener,
  );
  try {
    const watcher = fsSync.watch(movementsPath, { persistent: false }, wake);
    hub.watcher = watcher;
    watcher.once("error", () => {
      try {
        watcher.close();
      } catch {
      }
      if (hub.watcher === watcher) hub.watcher = null;
    });
  } catch {
  }
  return hub;
}

function closeLedgerChangeHub(movementsPath, hub) {
  if (ledgerChangeHubs.get(movementsPath) === hub) {
    ledgerChangeHubs.delete(movementsPath);
  }
  if (hub.closed) return;
  hub.closed = true;
  try {
    hub.watcher?.close();
  } catch {
  }
  hub.watcher = null;
  if (hub.statListener) {
    fsSync.unwatchFile(movementsPath, hub.statListener);
    hub.statListener = null;
  }
  hub.statWatcher = null;
}

async function readPersistedLiveCursor(octopusHome, interfaceId) {
  const cursorPath = liveCursorPath(octopusHome, interfaceId);
  const document = await readJsonIfPresent(cursorPath);
  if (document === null) return null;
  return normalizeLiveCursor(document);
}

function persistLiveCursor(octopusHome, interfaceId, cursor) {
  if (!cursor || cursor.seq === null) return Promise.resolve();
  const cursorPath = liveCursorPath(octopusHome, interfaceId);
  const previous = liveCursorWriteTails.get(cursorPath) ?? Promise.resolve();
  const write = previous.then(async () => {
    const current = await readPersistedLiveCursor(octopusHome, interfaceId);
    const next = current ? mergeLiveCursors(current, cursor) : cursor;
    if (current && !liveCursorIsAhead(next, current)) return;
    await fs.mkdir(path.dirname(cursorPath), { recursive: true, mode: 0o700 });
    const temporary = path.join(
      path.dirname(cursorPath),
      `.${path.basename(cursorPath)}.${process.pid}.${randomToken(16)}.tmp`,
    );
    let handle;
    try {
      handle = await fs.open(temporary, "wx", 0o600);
      await handle.writeFile(`${JSON.stringify(next, null, 2)}\n`, "utf8");
      await handle.sync();
      await handle.close();
      handle = null;
      await fs.rename(temporary, cursorPath);
      await fs.chmod(cursorPath, 0o600);
      await syncDirectory(path.dirname(cursorPath));
    } finally {
      await handle?.close().catch(() => {});
      await fs.unlink(temporary).catch((error) => {
        if (error?.code !== "ENOENT") throw error;
      });
    }
  });
  liveCursorWriteTails.set(cursorPath, write.then(() => undefined, () => undefined));
  return write;
}

function rememberLiveCursor(key, cursor) {
  const current = liveLedgerCursors.get(key);
  liveLedgerCursors.set(key, current ? mergeLiveCursors(current, cursor) : cursor);
}

function operatorStreamCursors(cursor) {
  const cursors = new Map(Object.entries(cursor?.streams ?? {}));
  if (cursor?.stream) cursors.set(cursor.stream.stream_id,
    laterStreamCursor(cursors.get(cursor.stream.stream_id), cursor.stream));
  return cursors;
}

function mergeOperatorStreamCursors(current, candidate) {
  const merged = new Map(current);
  for (const [id, cursor] of candidate) merged.set(id, laterStreamCursor(merged.get(id), cursor));
  return merged;
}

function withOperatorStreamCursors(ledger, cursors) {
  const entries = [...cursors.entries()];
  return { offset: ledger.offset, seq: ledger.seq,
    ...(entries.length === 1 ? { stream: entries[0][1] } : {}),
    ...(entries.length > 1 ? { streams: Object.fromEntries(entries) } : {}),
  };
}

function liveCursorIsAhead(candidate, current) {
  if (candidate.seq !== null && current.seq !== null && candidate.seq !== current.seq) {
    return candidate.seq > current.seq;
  }
  if (candidate.offset !== current.offset) return candidate.offset > current.offset;
  const prior = operatorStreamCursors(current);
  return [...operatorStreamCursors(candidate)].some(([id, cursor]) => {
    const before = prior.get(id);
    return !before || cursor.seq > before.seq || cursor.seq === before.seq && cursor.offset > before.offset;
  });
}

function mergeLiveCursors(current, candidate) {
  const ledger = liveCursorIsAhead(
    { offset: candidate.offset, seq: candidate.seq },
    { offset: current.offset, seq: current.seq },
  ) ? candidate : current;
  return withOperatorStreamCursors(ledger, mergeOperatorStreamCursors(
    operatorStreamCursors(current), operatorStreamCursors(candidate),
  ));
}

function liveCursorPath(octopusHome, interfaceId) {
  return path.join(
    octopusHome,
    "interfaces",
    interfaceId,
    "ledger-cursor.client.json",
  );
}

function normalizeLiveCursor(value) {
  if (value === undefined || value === null) return null;
  if (!isObject(value)) throw new Error("operator chat cursor must be an object");
  const unknown = Object.keys(value).filter((key) => !["offset", "seq", "stream", "streams"].includes(key));
  if (unknown.length > 0) {
    throw new Error(`operator chat cursor contains unknown fields: ${unknown.join(", ")}`);
  }
  const offset = nonNegativeInteger(value.offset, "operator chat cursor offset");
  const seq = value.seq === null || value.seq === undefined
    ? null
    : nonNegativeInteger(value.seq, "operator chat cursor seq");
  const normalizeStream = (input) => {
    const row = requiredObject(input, "operator chat stream cursor");
    const streamUnknown = Object.keys(row).filter((key) => ![
      "stream_id",
      "offset",
      "seq",
      "recorded_at_unix_ms",
      "source_movement_seq",
      "sealed",
    ].includes(key));
    if (streamUnknown.length > 0) {
      throw new Error(`operator chat stream cursor contains unknown fields: ${streamUnknown.join(", ")}`);
    }
    return {
      stream_id: contentHashHex(row.stream_id, "operator chat stream cursor stream_id"),
      offset: nonNegativeInteger(row.offset, "operator chat stream cursor offset"),
      seq: nonNegativeInteger(row.seq, "operator chat stream cursor seq"),
      recorded_at_unix_ms: positiveInteger(
        row.recorded_at_unix_ms,
        "operator chat stream cursor recorded_at_unix_ms",
      ),
      source_movement_seq: nonNegativeInteger(
        row.source_movement_seq,
        "operator chat stream cursor source_movement_seq",
      ),
      ...(row.sealed === true ? { sealed: true } : {}),
    };
  };
  const cursors = new Map();
  if (value.streams !== undefined) {
    for (const [id, input] of Object.entries(requiredObject(value.streams, "operator chat stream cursors"))) {
      const cursor = normalizeStream(input);
      if (id !== cursor.stream_id) throw new Error("operator chat stream cursor key differs from its identity");
      cursors.set(id, cursor);
    }
  }
  if (value.stream !== undefined && value.stream !== null) {
    const cursor = normalizeStream(value.stream);
    if (cursors.has(cursor.stream_id)) throw new Error("operator chat cursor repeats a stream identity");
    cursors.set(cursor.stream_id, cursor);
  }
  return withOperatorStreamCursors({ offset, seq }, cursors);
}
