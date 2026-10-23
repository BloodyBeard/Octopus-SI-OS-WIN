module.exports = { ledgerProjection };

const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const { blake3 } = require("@noble/hashes/blake3");
const { artifactReadProjectionForLabel } = require("../boundedJsonProjection.cjs");
const { verifiedArtifactFilesUnchanged } = require("../contentAddressedArtifact.cjs");
const { ARTIFACT_PAYLOAD_READ_CONCURRENCY, ProjectionEvidenceError, RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES, RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_ENTRIES } = require("./constants.cjs");
const { loadLedgerArtifactPayload, movementKindText, movementRunId, validateCanonicalMovement } = require("./movementRows.cjs");
const { contentHashHex, ledgerArtifactId, ledgerArtifactLabel, stringValue } = require("./values.cjs");

function ledgerProjection(store) {
  function resolveDurableRoot() {
    return path.join(store.octopusHome(), "ledger");
  }

  const ledgerCursors = new Map();
  const resolvedMovementGrants = new Map();
  const resolvedArtifactPayloads = new Map();
  let resolvedArtifactPayloadBytes = 0;
  let ledgerProjectionReadTail = Promise.resolve();
  let ledgerProjectionSnapshot = null;

  function readLedgerProjection(durableRoot, warnings = []) {
    const read = ledgerProjectionReadTail.then(() => (
      readLedgerProjectionUnlocked(durableRoot, warnings)
    ));
    ledgerProjectionReadTail = read.then(() => undefined, () => undefined);
    return read;
  }

  async function readLedgerProjectionUnlocked(durableRoot, warnings) {
    const files = [path.join(durableRoot, "movements.jsonl")];
    const cursors = [];
    const revisions = [];
    for (const file of files) {
      const cursor = await advanceLedgerCursor(file, warnings);
      if (cursor) cursors.push(cursor);
      revisions.push([
        file,
        cursor?.dev ?? null,
        cursor?.ino ?? null,
        cursor?.offset ?? null,
        cursor?.lineBase ?? null,
        cursor?.rows.length ?? null,
      ]);
    }
    const revision = JSON.stringify(revisions);
    if (
      ledgerProjectionSnapshot
      && ledgerProjectionSnapshot.durableRoot === durableRoot
      && ledgerProjectionSnapshot.revision === revision
    ) return ledgerProjectionSnapshot;
    const rows = cursors.length === 1 ? cursors[0].rows : cursors.flatMap((cursor) => cursor.rows);
    const rowsByRunSources = cursors.map((cursor) => cursor.rowsByRun);
    const invalidRows = cursors.length === 1
      ? cursors[0].invalidRows
      : cursors.flatMap((cursor) => cursor.invalidRows);
    ledgerProjectionSnapshot = { durableRoot, rows, rowsByRunSources, invalidRows, revision };
    return ledgerProjectionSnapshot;
  }

  async function advanceLedgerCursor(file, warnings = []) {
    let stat;
    try {
      stat = await fs.stat(file);
    } catch (error) {
      if (error?.code === "ENOENT") {
        ledgerCursors.delete(file);
        return null;
      }
      throw error;
    }
    let cursor = ledgerCursors.get(file);
    if (
      !cursor
      || cursor.dev !== stat.dev
      || cursor.ino !== stat.ino
      || stat.size < cursor.offset
    ) {
      cursor = {
        dev: stat.dev,
        ino: stat.ino,
        offset: 0,
        lineBase: 0,
        rows: [],
        rowsByRun: new Map(),
        invalidRows: [],
      };
    }
    if (stat.size === cursor.offset) {
      ledgerCursors.set(file, cursor);
      return cursor;
    }
    let pending = [];
    let pendingLength = 0;
    for await (const chunk of fsSync.createReadStream(file, { start: cursor.offset, end: stat.size - 1 })) {
      let start = 0;
      for (;;) {
        const newline = chunk.indexOf(0x0a, start);
        if (newline < 0) break;
        const fragment = chunk.subarray(start, newline);
        const bytes = pendingLength ? Buffer.concat([...pending, fragment], pendingLength + fragment.length) : fragment;
        const line = bytes.toString("utf8");
        cursor.offset += bytes.length + 1;
        cursor.lineBase += 1;
        start = newline + 1;
        pending = [];
        pendingLength = 0;
        if (line.trim()) parseLedgerLine(line, file, cursor.lineBase, cursor, warnings);
      }
      if (start < chunk.length) {
        const remainder = chunk.subarray(start);
        pending.push(remainder);
        pendingLength += remainder.length;
      }
    }
    ledgerCursors.set(file, cursor);
    return cursor;
  }

  function parseLedgerLine(line, file, lineNumber, cursor, warnings) {
    const index = lineNumber - 1;
    let row;
    try {
      row = parseCanonicalMovement(line, file, index + 1);
    } catch (error) {
      const message = String(error?.message ?? error);
      warnings.push(message);
      cursor.invalidRows.push({ source_path: file, source_line: index + 1, error: message });
      return;
    }
    const kind = movementKindText(row.kind ?? row.movement_class ?? row.type ?? row.event) || "ledger.row";
    const projected = {
      ...row,
      envelope_id: stringValue(row.envelope_id ?? row.id) || `${path.basename(file, ".jsonl")}:${index + 1}`,
      kind,
      source_file: file,
      source_line: index + 1,
    };
    cursor.rows.push(projected);
    const runId = movementRunId(projected);
    if (runId) {
      const runRows = cursor.rowsByRun.get(runId) ?? [];
      runRows.push(projected);
      cursor.rowsByRun.set(runId, runRows);
    }
  }

  function parseCanonicalMovement(source, file, lineNumber) {
    let row;
    try {
      row = JSON.parse(Buffer.isBuffer(source) ? source.toString("utf8") : source);
    } catch (error) {
      throw new ProjectionEvidenceError(`ledger movement JSON is invalid: ${file}:${lineNumber}: ${error.message}`);
    }
    const grantContent = decodeDurableMovementGrant(row, file, lineNumber);
    const movement = validateCanonicalMovement(row, file, lineNumber);
    if (grantContent) {
      delete movement.invocation_metadata.grant;
      movement.invocation_metadata.grant_content = grantContent;
    }
    return movement;
  }

  function decodeDurableMovementGrant(row, file, lineNumber) {
    const metadata = row?.invocation_metadata;
    if (!metadata || typeof metadata !== "object" || Array.isArray(metadata)) return "";
    const hasGrant = Object.prototype.hasOwnProperty.call(metadata, "grant");
    const hasGrantContent = Object.prototype.hasOwnProperty.call(metadata, "grant_content");
    if (!hasGrantContent) return "";
    if (hasGrant) {
      throw new ProjectionEvidenceError(
        `ledger movement metadata carries both inline and content-addressed grant: ${file}:${lineNumber}`,
      );
    }
    const content = metadata.grant_content;
    if (typeof content !== "string" || !/^[0-9a-f]{64}$/.test(content)) {
      throw new ProjectionEvidenceError(
        `ledger movement grant content address is not canonical BLAKE3: ${file}:${lineNumber}`,
      );
    }
    const metadataRoot = path.join(path.dirname(file), "metadata");
    const cacheKey = `${metadataRoot}:${content}`;
    let grant = resolvedMovementGrants.get(cacheKey);
    if (grant === undefined) {
      const grantPath = path.join(metadataRoot, content);
      let bytes;
      try {
        bytes = fsSync.readFileSync(grantPath);
      } catch (error) {
        throw new ProjectionEvidenceError(
          `ledger movement grant content is unreadable: ${grantPath}: ${error.message}`,
        );
      }
      const actual = Buffer.from(blake3(bytes)).toString("hex");
      if (actual !== content) {
        throw new ProjectionEvidenceError(
          `ledger movement grant content integrity mismatch: ${grantPath}: expected ${content}, got ${actual}`,
        );
      }
      try {
        grant = JSON.parse(bytes.toString("utf8"));
      } catch (error) {
        throw new ProjectionEvidenceError(
          `ledger movement grant content JSON is invalid: ${grantPath}: ${error.message}`,
        );
      }
      resolvedMovementGrants.set(cacheKey, grant);
    }
    delete metadata.grant_content;
    metadata.grant = grant;
    return content;
  }


  async function resolveLedgerArtifactPayload(row, readOptions = {}) {
    const hex = contentHashHex(row && row.artifact && row.artifact.content);
    if (!hex || !row.source_file) return null;
    const contract = Object.hasOwn(readOptions, "projection")
      ? artifactReadProjectionForLabel(readOptions.projection, ledgerArtifactLabel(row)) : null;
    const cacheKey = `${row.source_file}:${hex}:${contract ? JSON.stringify([contract, ledgerArtifactId(row)]) : "raw"}`;
    const cached = resolvedArtifactPayloads.get(cacheKey);
    if (cached) {
      await cached.promise;
      if (readOptions.onVerifiedFiles && cached.files
        && !await verifiedArtifactFilesUnchanged(cached.files, path.join(path.dirname(row.source_file), "blobs"))) {
        removeResolvedArtifactPayload(cacheKey, cached);
      } else {
        if (resolvedArtifactPayloads.get(cacheKey) === cached) {
          resolvedArtifactPayloads.delete(cacheKey);
          resolvedArtifactPayloads.set(cacheKey, cached);
        }
        return observedPayload(cached, readOptions);
      }
    }
    const entry = { promise: null, weight: 0, files: null };
    const pending = loadLedgerArtifactPayload(row, hex, false, contract).then(({ payload, cacheable, retainedBytes, files }) => {
      entry.files = cacheable ? files : null;
      if (!cacheable) {
        removeResolvedArtifactPayload(cacheKey, entry);
      } else if (resolvedArtifactPayloads.get(cacheKey) === entry) {
        const weight = Math.max(1_024, retainedBytes * 4);
        if (weight > RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES) {
          removeResolvedArtifactPayload(cacheKey, entry);
        } else {
          entry.weight = weight;
          resolvedArtifactPayloadBytes += weight;
          trimResolvedArtifactPayloads();
        }
      }
      return payload;
    }).catch((error) => {
      removeResolvedArtifactPayload(cacheKey, entry);
      throw error;
    });
    entry.promise = pending;
    const replaced = resolvedArtifactPayloads.get(cacheKey);
    if (replaced) removeResolvedArtifactPayload(cacheKey, replaced);
    resolvedArtifactPayloads.set(cacheKey, entry);
    trimResolvedArtifactPayloads();
    return observedPayload(entry, readOptions);
  }

  function observedPayload(entry, readOptions) {
    if (!readOptions.onVerifiedFiles) return entry.promise;
    return entry.promise.then((payload) => {
      readOptions.onVerifiedFiles(entry.files);
      return payload;
    });
  }

  function removeResolvedArtifactPayload(cacheKey, entry) {
    if (resolvedArtifactPayloads.get(cacheKey) !== entry) return;
    resolvedArtifactPayloads.delete(cacheKey);
    resolvedArtifactPayloadBytes = Math.max(0, resolvedArtifactPayloadBytes - entry.weight);
  }

  function trimResolvedArtifactPayloads() {
    while (
      resolvedArtifactPayloads.size > RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_ENTRIES
      || resolvedArtifactPayloadBytes > RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES
    ) {
      const oldest = resolvedArtifactPayloads.entries().next().value;
      if (!oldest) break;
      removeResolvedArtifactPayload(oldest[0], oldest[1]);
    }
  }

  async function resolveLedgerArtifactPayloads(rows, required = false, readOptions = {}) {
    const unique = [...new Set(rows)];
    const payloads = new Map();
    let next = 0;
    const workers = Array.from({ length: Math.min(ARTIFACT_PAYLOAD_READ_CONCURRENCY, unique.length) }, async () => {
      for (;;) {
        const index = next;
        next += 1;
        if (index >= unique.length) return;
        const row = unique[index];
        if (required) {
          const hash = contentHashHex(row?.artifact?.content);
          if (!hash || !row.source_file) throw new Error(`Activity has no readable artifact receipt: ${ledgerArtifactId(row)}`);
          const contract = Object.hasOwn(readOptions, "projection")
            ? artifactReadProjectionForLabel(readOptions.projection, ledgerArtifactLabel(row)) : null;
          payloads.set(row, (await loadLedgerArtifactPayload(row, hash, true, contract)).payload);
        } else payloads.set(row, await resolveLedgerArtifactPayload(row, readOptions));
      }
    });
    await Promise.all(workers);
    return payloads;
  }

  async function resolveLedgerArtifactPayloadResults(rows, readOptions = {}) {
    const unique = [...new Set(rows)];
    const results = new Map();
    let next = 0;
    const workers = Array.from({ length: Math.min(ARTIFACT_PAYLOAD_READ_CONCURRENCY, unique.length) }, async () => {
      for (;;) {
        const index = next;
        next += 1;
        if (index >= unique.length) return;
        const row = unique[index];
        try {
          results.set(row, { payload: await resolveLedgerArtifactPayload(row, readOptions), error: "" });
        } catch (error) {
          results.set(row, {
            payload: null,
            error: error instanceof Error ? error.message : String(error),
          });
        }
      }
    });
    await Promise.all(workers);
    return results;
  }

  function closeLedgerProjection() {
    ledgerProjectionSnapshot = null;
    ledgerCursors.clear();
    resolvedArtifactPayloads.clear();
    resolvedArtifactPayloadBytes = 0;
  }

  Object.defineProperty(store, "ledgerProjectionSnapshot", { get: () => ledgerProjectionSnapshot });

  return { advanceLedgerCursor, closeLedgerProjection, decodeDurableMovementGrant, parseCanonicalMovement, parseLedgerLine, readLedgerProjection, readLedgerProjectionUnlocked, removeResolvedArtifactPayload, resolveDurableRoot, resolveLedgerArtifactPayload, resolveLedgerArtifactPayloadResults, resolveLedgerArtifactPayloads, trimResolvedArtifactPayloads };
}
