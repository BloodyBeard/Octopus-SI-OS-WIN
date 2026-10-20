module.exports = { assertExactOperatorClientInterface, normalizeIndexedOperatorRow, readExactInterfaceRole, readLedgerOperatorRows, readOperatorChatArtifactBatch, readSpotlightOperatorRows };

const fs = require("node:fs/promises");
const path = require("node:path");
const { readCompleteLedgerRecords } = require("./chatArtifacts.cjs");
const { LIVE_STREAM_ARTIFACT_LABEL } = require("./constants.cjs");
const { arrayValue, artifactLabel, contentHashHex, databaseInteger, decimalSafeInteger, exactOutboundRuntime, isObject, nonNegativeInteger, readJsonIfPresent, requiredString, safeSegment, stringValue } = require("./values.cjs");

async function readExactInterfaceRole(octopusHome, interfaceId) {
  const descriptor = await readJsonIfPresent(path.join(
    octopusHome,
    "registry",
    "interface",
    safeSegment(interfaceId, "source interface id"),
    "interface.json",
  ));
  if (!isObject(descriptor) || descriptor.id !== interfaceId) {
    throw new Error(`source interface '${interfaceId}' has no exact descriptor`);
  }
  return requiredString(descriptor.role, `source interface '${interfaceId}' role`).toLowerCase();
}

async function assertExactOperatorClientInterface(octopusHome, interfaceId) {
  const interfaceRoot = path.join(
    octopusHome,
    "registry",
    "interface",
    interfaceId,
  );
  const descriptor = await readJsonIfPresent(path.join(interfaceRoot, "interface.json"));
  const sidecar = await readJsonIfPresent(path.join(interfaceRoot, "octopus_os.json"));
  if (
    !isObject(descriptor)
    || descriptor.id !== interfaceId
    || stringValue(descriptor.interface_type).toLowerCase() !== "world"
    || stringValue(descriptor.role).toLowerCase() !== "user"
    || !arrayValue(descriptor.source_names).includes("operator.chat")
  ) {
    throw new Error(`interface '${interfaceId}' is not an exact operator chat interface`);
  }
  if (
    !isObject(sidecar)
    || sidecar.interface !== interfaceId
    || sidecar.owner_interface !== interfaceId
    || sidecar.component !== "client"
    || sidecar.workspace !== "octopus_os"
  ) {
    throw new Error(`interface '${interfaceId}' has no exact Octopus OS client ownership`);
  }
}

async function readOperatorChatArtifactBatch(input) {
  const requestedCursor = input.cursor;
  if (requestedCursor) {
    return readLedgerOperatorRows({
      afterSeq: input.afterSeq,
      cursor: requestedCursor,
      interfaceId: input.interfaceId,
      limit: input.limit,
      movementsPath: input.movementsPath,
      runId: input.runId,
    });
  }
  const indexed = await readSpotlightOperatorRows(input);
  if (!indexed) {
    return readLedgerOperatorRows({
      afterSeq: input.afterSeq,
      cursor: { offset: 0, seq: null },
      interfaceId: input.interfaceId,
      limit: input.limit,
      movementsPath: input.movementsPath,
      runId: input.runId,
    });
  }
  const tail = await readLedgerOperatorRows({
    afterSeq: input.afterSeq,
    cursor: indexed.cursor,
    interfaceId: input.interfaceId,
    limit: Math.max(0, input.limit - indexed.rows.length),
    movementsPath: input.movementsPath,
    runId: input.runId,
  });
  const rows = [...indexed.rows, ...tail.rows]
    .sort((left, right) => left.seq - right.seq)
    .filter((row, index, values) => index === 0 || row.seq !== values[index - 1].seq)
    .slice(0, input.limit);
  return { rows, cursor: tail.cursor };
}

async function readSpotlightOperatorRows(input) {
  let DatabaseSync;
  try {
    ({ DatabaseSync } = require("node:sqlite"));
  } catch {
    return null;
  }
  const indexPath = path.join(input.octopusHome, "ledger", "ledger-index.sqlite");
  let database;
  try {
    database = new DatabaseSync(indexPath, { readOnly: true });
    database.exec("PRAGMA query_only = ON");
    const meta = database.prepare(
      "SELECT key, value FROM index_meta WHERE key IN ('schema_version', 'watermark', 'durable_end_offset')",
    ).all();
    const values = new Map(meta.map((row) => [String(row.key), String(row.value)]));
    if (values.get("schema_version") !== "3") return null;
    const watermark = decimalSafeInteger(values.get("watermark"), "ledger index watermark");
    const durableEndOffset = decimalSafeInteger(
      values.get("durable_end_offset"),
      "ledger index durable_end_offset",
    );
    const ledgerStat = await fs.stat(input.movementsPath);
    if (durableEndOffset > ledgerStat.size) return null;
    const rows = database.prepare(
      `SELECT
        seq, recorded_at_unix_ms, from_kind, from_id, to_kind, to_id,
        artifact_id, artifact_label, artifact_content,
        runtime_run, runtime_checkpoint, runtime_interface
      FROM movements
      WHERE seq > ?
        AND seq < ?
        AND kind = 'InterfaceOutbound'
        AND from_kind = 'interface'
        AND to_kind = 'interface'
        AND to_id = ?
        AND runtime_interface = ?
        AND runtime_run = ?
      ORDER BY seq ASC
      LIMIT ?`,
    ).all(
      input.afterSeq,
      watermark,
      input.interfaceId,
      input.interfaceId,
      input.runId,
      input.limit + 1,
    ).map((row) => normalizeIndexedOperatorRow(row, input.interfaceId, input.runId));
    if (rows.length > input.limit) return null;
    return {
      rows,
      cursor: {
        offset: durableEndOffset,
        seq: watermark,
      },
    };
  } catch {
    return null;
  } finally {
    database?.close();
  }
}

function normalizeIndexedOperatorRow(row, interfaceId, runId) {
  const seq = databaseInteger(row.seq, "indexed movement seq");
  const recordedAt = databaseInteger(
    row.recorded_at_unix_ms,
    "indexed movement recorded_at_unix_ms",
  );
  const artifactId = requiredString(row.artifact_id, "indexed movement artifact_id");
  const label = requiredString(
    row.artifact_label || artifactLabel(artifactId),
    "indexed movement artifact_label",
  );
  const content = contentHashHex(
    row.artifact_content,
    "indexed movement artifact_content",
  );
  if (
    row.from_kind !== "interface"
    || row.to_kind !== "interface"
    || row.to_id !== interfaceId
    || row.runtime_interface !== interfaceId
    || row.runtime_run !== runId
  ) {
    throw new Error(`indexed outbound movement ${seq} does not match its exact interface coordinates`);
  }
  return {
    seq,
    recorded_at_unix_ms: recordedAt,
    from: { Interface: requiredString(row.from_id, "indexed movement from_id") },
    to: { Interface: interfaceId },
    artifact: {
      id: artifactId,
      content,
    },
    runtime: {
      run: runId,
      checkpoint: requiredString(
        row.runtime_checkpoint,
        "indexed movement runtime_checkpoint",
      ),
      interface: interfaceId,
    },
    label,
  };
}

async function readLedgerOperatorRows(input) {
  if (input.limit === 0) return { rows: [], cursor: input.cursor };
  let expectedSeq = input.cursor.seq;
  let cursor = input.cursor;
  const rows = [];
  let index = 0;
  for await (const record of readCompleteLedgerRecords(input.movementsPath, input.cursor.offset)) {
    if (!record.text.length) continue;
    index += 1;
    const cursorBefore = cursor;
    let movement;
    try {
      movement = JSON.parse(record.text);
    } catch (error) {
      throw new Error(`movement ledger contains malformed complete JSON at tail line ${index}: ${error.message}`);
    }
    const seq = nonNegativeInteger(movement?.seq, "movement seq");
    if (expectedSeq !== null && seq !== expectedSeq) {
      throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
    }
    expectedSeq = seq + 1;
    cursor = {
      offset: record.nextOffset,
      seq: expectedSeq,
    };
    if (seq <= input.afterSeq) continue;
    const runtime = exactOutboundRuntime(movement, input.interfaceId);
    if (!runtime || runtime.run !== input.runId) continue;
    if (!isObject(movement.artifact)) {
      throw new Error(`outbound movement ${seq} carries no artifact`);
    }
    rows.push({
      ...movement,
      runtime,
      __ledger_cursor_before: cursorBefore,
      __ledger_cursor_after: cursor,
    });
    if (artifactLabel(requiredString(movement.artifact.id, "outbound artifact.id")) === LIVE_STREAM_ARTIFACT_LABEL) break;
    if (rows.length >= input.limit) break;
  }
  return {
    rows,
    cursor,
  };
}
