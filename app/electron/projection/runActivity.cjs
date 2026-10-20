module.exports = { runActivityProjection };

const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const { ProjectionEvidenceError } = require("./constants.cjs");
const { compareMovementOrder, indexedUnsignedInteger, ledgerRowsForRun, movementBelongsToRun, movementBoundaryInterface, movementEndpoint, movementKindText, normalizeIndexedProjectionMovement, openValidatedLedgerIndex, optionalActivityAfterSeq, optionalUnsignedInteger, projectionLedgerSourceCursor } = require("./movementRows.cjs");
const { activityDirection, operatorChatActivityPayload, operatorChatEventData } = require("./operatorChatEvents.cjs");
const { baseInterfaceRows, octopusOsInterfaceSidecars } = require("./registryRows.cjs");
const { arrayValue, contentHashHex, endpointId, ledgerArtifactId, ledgerArtifactLabel, numberValue, objectOrEmpty, rowTimeMs, stringValue } = require("./values.cjs");

function runActivityProjection(store) {
  async function readIndexedRunActivityRows(projection, runId, options) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const limit = Math.min(1_000, Math.max(1, numberValue(options.limit, 100)));
      const afterSeq = optionalUnsignedInteger(options.afterSeq);
      const beforeSeq = optionalUnsignedInteger(options.beforeSeq);
      const fromEnd = options.fromEnd === true;
      if (afterSeq !== null && beforeSeq !== null && beforeSeq <= afterSeq) {
        throw new Error("run activity before_seq must be greater than after_seq");
      }
      const basePredicate = `
        artifact_id IS NOT NULL
        AND (
          runtime_run = ?
        )`;
      const predicates = [basePredicate];
      const values = [runId];
      if (afterSeq !== null) {
        predicates.push("seq > ?");
        values.push(afterSeq);
      }
      if (beforeSeq !== null) {
        predicates.push("seq < ?");
        values.push(beforeSeq);
      }
      const predicate = predicates.join("\nAND ");
      const indexedCount = indexedUnsignedInteger(index.database.prepare(
        `SELECT COUNT(*) AS count FROM movements WHERE ${predicate}`,
      ).get(...values)?.count);
      if (indexedCount === null) return null;
      const descending = fromEnd || beforeSeq !== null;
      const indexedRows = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE ${predicate}
         ORDER BY seq ${descending ? "DESC" : "ASC"}
         LIMIT ?`,
      ).all(...values, limit).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const tail = await readRunActivityLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        runId,
        { afterSeq, beforeSeq },
      );
      if (tail === null) return null;
      const candidates = [...indexedRows, ...tail.rows].sort(compareMovementOrder);
      const rows = descending
        ? candidates.slice(Math.max(0, candidates.length - limit))
        : candidates.slice(0, limit);
      const indexedBounds = index.database.prepare(
        `SELECT MIN(seq) AS oldest, MAX(seq) AS latest FROM movements WHERE ${basePredicate}`,
      ).get(runId);
      const indexedOldest = indexedUnsignedInteger(indexedBounds?.oldest);
      const indexedLatest = indexedUnsignedInteger(indexedBounds?.latest);
      const oldestSeq = rows.length ? numberValue(rows[0].seq, null) : null;
      const latestSeq = rows.length ? numberValue(rows[rows.length - 1].seq, null) : null;
      const allOldest = [indexedOldest, tail.allOldest].filter((value) => value !== null).reduce((current, value) => current === null ? value : Math.min(current, value), null);
      const allLatest = [indexedLatest, tail.allLatest].filter((value) => value !== null).reduce((current, value) => current === null ? value : Math.max(current, value), null);
      return {
        rows,
        total: indexedCount + tail.count,
        hasOlder: oldestSeq !== null && allOldest !== null && allOldest < oldestSeq,
        hasNewer: latestSeq !== null && allLatest !== null && allLatest > latestSeq,
        oldestSeq,
        latestSeq,
      };
    } finally {
      index.database.close();
    }
  }

  async function readRunActivityRows(projection, runId, options) {
    const indexed = await readIndexedRunActivityRows(projection, runId, options);
    if (indexed !== null) return indexed;
    return readLedgerRunActivityRows(projection, runId, options);
  }

  async function readLedgerRunActivityRows(projection, runId, options) {
    const limit = Math.min(1_000, Math.max(1, numberValue(options.limit, 100)));
    const afterSeq = optionalUnsignedInteger(options.afterSeq);
    const beforeSeq = optionalUnsignedInteger(options.beforeSeq);
    const fromEnd = options.fromEnd === true;
    if (afterSeq !== null && beforeSeq !== null && beforeSeq <= afterSeq) {
      throw new Error("run activity before_seq must be greater than after_seq");
    }
    await store.refreshLedgerProjection(projection);
    const allRows = ledgerRowsForRun(projection, runId)
      .filter((row) => ledgerArtifactId(row))
      .sort(compareMovementOrder);
    const candidates = allRows.filter((row) => {
      const seq = indexedUnsignedInteger(row.seq);
      if (seq === null) {
        throw new ProjectionEvidenceError("run activity movement has no valid sequence coordinate");
      }
      return (afterSeq === null || seq > afterSeq)
        && (beforeSeq === null || seq < beforeSeq);
    });
    const descending = fromEnd || beforeSeq !== null;
    const rows = descending
      ? candidates.slice(Math.max(0, candidates.length - limit))
      : candidates.slice(0, limit);
    const oldestSeq = rows.length ? numberValue(rows[0].seq, null) : null;
    const latestSeq = rows.length ? numberValue(rows[rows.length - 1].seq, null) : null;
    const allOldest = allRows.length ? numberValue(allRows[0].seq, null) : null;
    const allLatest = allRows.length ? numberValue(allRows[allRows.length - 1].seq, null) : null;
    return {
      rows,
      total: candidates.length,
      hasOlder: oldestSeq !== null && allOldest !== null && allOldest < oldestSeq,
      hasNewer: latestSeq !== null && allLatest !== null && allLatest > latestSeq,
      oldestSeq,
      latestSeq,
    };
  }

  async function readRunActivityLedgerTail(ledgerPath, startOffset, expectedSeq, runId, options) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return { allLatest: null, allOldest: null, count: 0, rows: [] };
    const rows = [];
    let count = 0;
    let allOldest = null;
    let allLatest = null;
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset })) {
      const bytes = pending.length ? Buffer.concat([pending, chunk]) : chunk;
      let recordStart = 0;
      for (;;) {
        const newline = bytes.indexOf(0x0a, recordStart);
        if (newline < 0) break;
        const end = newline > recordStart && bytes[newline - 1] === 0x0d ? newline - 1 : newline;
        const raw = bytes.subarray(recordStart, end);
        recordStart = newline + 1;
        if (!raw.length) continue;
        const movement = store.parseCanonicalMovement(raw, ledgerPath, expectedSeq + 1);
        const seq = indexedUnsignedInteger(movement?.seq);
        if (seq === null || seq !== expectedSeq) return null;
        expectedSeq += 1;
        if (!ledgerArtifactId(movement) || !movementBelongsToRun(movement, runId)) continue;
        allOldest = allOldest === null ? seq : Math.min(allOldest, seq);
        allLatest = allLatest === null ? seq : Math.max(allLatest, seq);
        if (
          options.afterSeq !== null && seq <= options.afterSeq
          || options.beforeSeq !== null && seq >= options.beforeSeq
        ) continue;
        count += 1;
        rows.push({
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        });
      }
      pending = bytes.subarray(recordStart);
    }
    return { allLatest, allOldest, count, rows };
  }

  async function operatorRunActivity(projection, runId, params) {
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const afterSeq = optionalActivityAfterSeq(params.after_seq);
    const beforeSeq = optionalUnsignedInteger(params.before_seq);
    const requestedLimit = Math.min(1_000, Math.max(1, numberValue(params.limit, 100)));
    const fromEnd = params.from_end === true;
    if (afterSeq !== null && beforeSeq !== null && beforeSeq <= afterSeq) {
      throw new Error("run activity before_seq must be greater than after_seq");
    }
    const indexed = await readRunActivityRows(
      projection,
      runId,
      { afterSeq, beforeSeq, fromEnd, limit: requestedLimit },
    );
    const selected = indexed.rows;
    const resolvedPayloads = await store.resolveLedgerArtifactPayloadResults(selected, { projection: protocol.artifactReadProjection });
    const sidecars = octopusOsInterfaceSidecars(projection);
    const interfaceRoles = new Map(baseInterfaceRows(projection).map((descriptor) => [
      stringValue(descriptor.rule_id ?? descriptor.id),
      stringValue(descriptor.role),
    ]).filter(([interfaceId]) => interfaceId));
    const toolCallOrigins = new Map();
    const activity = [];
    for (const row of selected) {
      const artifactId = ledgerArtifactId(row);
      const label = ledgerArtifactLabel(row);
      if (arrayValue(protocol.hiddenActivityLabels).includes(label)) continue;
      const movementKind = movementKindText(row.kind);
      const from = movementEndpoint(row.from);
      const to = movementEndpoint(row.to);
      const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
      const direction = activityDirection(row, runId);
      const interfaceId = direction === "in"
        ? endpointId(row.from, "Interface") || stringValue(runtime.interface)
        : direction === "out"
          ? endpointId(row.to, "Interface") || stringValue(runtime.interface)
          : endpointId(row.from, "Interface") || endpointId(row.to, "Interface") || stringValue(runtime.interface);
      const resolved = resolvedPayloads.get(row);
      const payload = resolved?.payload ?? null;
      const payloadError = resolved?.error ?? "";
      const projectedActivity = operatorChatActivityPayload(payload, label, protocol);
      const eventKind = projectedActivity.eventKind;
      const eventPayload = projectedActivity.payload;
      const sidecar = sidecars.get(interfaceId);
      const role = direction === "out"
        ? "assistant"
        : direction === "in" && sidecar?.component === "client"
          ? "user"
          : "system";
      const sourceInterfaceId = movementBoundaryInterface(row, "from");
      const callId = stringValue(eventPayload?.call_id ?? eventPayload?.correlation_id);
      let activityOrigin = {
        interface_id: sourceInterfaceId,
        interface_role: interfaceRoles.get(sourceInterfaceId) ?? "",
      };
      if (eventKind === "dynamic_tool" && callId) {
        if (stringValue(eventPayload?.status) === "started") {
          toolCallOrigins.set(callId, activityOrigin);
        } else {
          activityOrigin = toolCallOrigins.get(callId) ?? activityOrigin;
        }
      }
      const payloadAlreadyPresented = sidecar?.component === "client";
      const evidenceOnly = payloadAlreadyPresented
        || ["usage", "item_started", "turn_started", "turn_completed"].includes(eventKind);
      const presentedEventKind = evidenceOnly ? "event" : eventKind;
      const movementSeq = numberValue(row.seq, 0);
      const evidenceText = [
        label,
        `${Object.entries(objectOrEmpty(row.from)).map(([kind, id]) => `${kind}:${stringValue(id)}`).find((value) => !value.endsWith(":")) ?? "unknown"} → ${Object.entries(objectOrEmpty(row.to)).map(([kind, id]) => `${kind}:${stringValue(id)}`).find((value) => !value.endsWith(":")) ?? "unknown"}`,
        `movement ${movementSeq}`,
        artifactId ? `artifact ${artifactId}` : "",
      ].filter(Boolean).join(" · ");
      const text = evidenceOnly
        ? evidenceText
        : typeof payload === "string" ? payload : projectedActivity.text;
      activity.push({
        id: `${runId}:activity:${Number.isSafeInteger(row.seq) ? row.seq : `line-${row.source_line}`}:${movementKind}`,
        run_id: runId,
        role,
        label,
        direction,
        event_kind: presentedEventKind,
        event_data: {
          ...operatorChatEventData(eventPayload, eventKind, row),
          _run_activity: true,
          _activity_origin: activityOrigin,
          ...(payloadAlreadyPresented ? { _movement_evidence: true } : {}),
        },
        text,
        error: payloadAlreadyPresented ? false : Boolean(payloadError) || projectedActivity.error || movementKind.startsWith("VerificationFailed") || movementKind.startsWith("DeliveryRefused"),
        terminal: false,
        created_at_ms: rowTimeMs(row),
        movement_seq: movementSeq,
        movement_kind: movementKind,
        from,
        to,
        verification: stringValue(objectOrEmpty(row.invocation_metadata).verification),
        artifact_id: artifactId,
        content_hash: contentHashHex(row?.artifact?.content),
        interface_id: interfaceId,
        attachments: store.activityAttachments(eventPayload, artifactId, label, rowTimeMs(row)),
        payload_error: payloadError,
      });
    }
    const sourceCursor = await projectionLedgerSourceCursor(
      projection,
      indexed.latestSeq ?? afterSeq ?? -1,
    );
    return {
      run_id: runId,
      total: indexed.total,
      truncated: selected.length < indexed.total,
      has_older: indexed.hasOlder,
      has_newer: indexed.hasNewer,
      oldest_seq: indexed.oldestSeq,
      latest_seq: indexed.latestSeq,
      activity,
      source_cursor: sourceCursor,
    };
  }

  return { operatorRunActivity, readIndexedRunActivityRows, readLedgerRunActivityRows, readRunActivityLedgerTail, readRunActivityRows };
}
