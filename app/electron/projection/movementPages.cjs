module.exports = { movementPagesProjection };

const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const { compareMovementOrder, escapeSqlLike, indexedUnsignedInteger, matchesMovementPage, movementBaseKind, movementKindText, movementPageRequiresIndex, movementRows, normalizeIndexedProjectionMovement, normalizedMovementKindFilter, openValidatedLedgerIndex, optionalUnsignedInteger, projectMovementRows } = require("./movementRows.cjs");
const { preserveProjectionEvidenceError } = require("./registryRows.cjs");
const { ledgerArtifactLabel, limitRows, numberValue, stringValue } = require("./values.cjs");

function movementPagesProjection(store) {
  async function readMovementPage(projection, params) {
    const indexed = await readIndexedMovementPage(projection, params);
    if (indexed) return indexed;
    if (movementPageRequiresIndex(params)) {
      throw new Error("ledger index is required for cursor paging or search");
    }
    await store.refreshLedgerProjection(projection);
    const movements = await movementRows(projection);
    return {
      movements: limitRows(movements, params),
      total: movements.length,
      next_seq: movements.reduce((next, row) => Math.max(next, row.seq + 1), 0),
    };
  }

  async function filteredMovementRows(projection, params) {
    const kinds = normalizedMovementKindFilter(params);
    if (kinds.length === 0) return null;
    const limit = Math.min(5_000, Math.max(1, numberValue(params.limit, 1_000)));
    const indexed = await readIndexedFilteredMovements(projection, new Set(kinds), limit);
    if (indexed) {
      const movements = projectMovementRows(projection, indexed.rows).slice(0, limit);
      return { movements, total: indexed.total };
    }
    await store.refreshLedgerProjection(projection);
    const rows = projection.ledgerRows.filter((row) => kinds.includes(movementBaseKind(row)));
    const movements = projectMovementRows(projection, rows);
    return { movements: movements.slice(0, limit), total: movements.length };
  }

  async function readIndexedMovementPage(projection, params) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const limit = Math.min(1_000, Math.max(1, numberValue(params?.limit, 500)));
      const beforeSeq = optionalUnsignedInteger(params?.before_seq);
      const afterSeq = optionalUnsignedInteger(params?.after_seq);
      const recordedAfter = optionalUnsignedInteger(params?.recorded_after_unix_ms);
      const recordedBefore = optionalUnsignedInteger(params?.recorded_before_unix_ms);
      const query = stringValue(params?.query).toLocaleLowerCase();
      const kinds = normalizedMovementKindFilter(params);
      if (query.length > 512) throw new Error("movement query exceeds 512 characters");
      if (beforeSeq !== null && afterSeq !== null && beforeSeq <= afterSeq) {
        throw new Error("movement cursor before_seq must be greater than after_seq");
      }
      const predicates = ["seq < ?"];
      const values = [Math.min(beforeSeq ?? index.watermark, index.watermark)];
      if (afterSeq !== null) {
        predicates.push("seq > ?");
        values.push(afterSeq);
      }
      if (recordedAfter !== null) {
        predicates.push("recorded_at_unix_ms >= ?");
        values.push(recordedAfter);
      }
      if (recordedBefore !== null) {
        predicates.push("recorded_at_unix_ms <= ?");
        values.push(recordedBefore);
      }
      if (kinds.length > 0) {
        predicates.push(`kind IN (${kinds.map(() => "?").join(", ")})`);
        values.push(...kinds);
      }
      if (query) {
        const like = `%${escapeSqlLike(query)}%`;
        predicates.push(`(
          LOWER(kind) LIKE ? ESCAPE '\\'
          OR LOWER(trace) LIKE ? ESCAPE '\\'
          OR LOWER(COALESCE(artifact_id, '')) LIKE ? ESCAPE '\\'
          OR LOWER(COALESCE(artifact_label, '')) LIKE ? ESCAPE '\\'
          OR LOWER(COALESCE(from_id, '')) LIKE ? ESCAPE '\\'
          OR LOWER(COALESCE(to_id, '')) LIKE ? ESCAPE '\\'
        )`);
        values.push(like, like, like, like, like, like);
      }
      const where = predicates.join("\nAND ");
      const select = `SELECT
        seq, lamport, recorded_at_unix_ms, trace, kind, mark,
        from_kind, from_id, to_kind, to_id,
        artifact_id, artifact_content,
        runtime_run, runtime_checkpoint, runtime_interface
        FROM movements
        WHERE ${where}
        ORDER BY seq DESC
        LIMIT ?`;
      const indexedRows = index.database.prepare(select)
        .all(...values, limit)
        .map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const indexedCount = indexedUnsignedInteger(index.database.prepare(
        `SELECT COUNT(*) AS count FROM movements WHERE ${where}`,
      ).get(...values)?.count);
      if (indexedCount === null) return null;
      const tail = await readMovementLedgerTailPage(index.ledgerPath, index.durableEndOffset, index.watermark, {
        afterSeq,
        beforeSeq,
        kinds: new Set(kinds),
        query,
        recordedAfter,
        recordedBefore,
        limit,
      });
      if (tail === null) return null;
      const rows = projectMovementRows(projection, [...indexedRows, ...tail.rows])
        .slice(0, limit);
      const total = indexedCount + tail.total;
      const oldest = rows.reduce((value, row) => {
        const seq = indexedUnsignedInteger(row.seq);
        return seq === null ? value : value === null ? seq : Math.min(value, seq);
      }, null);
      return {
        movements: rows,
        total,
        next_seq: tail.nextSeq,
        ...(oldest !== null && rows.length < total ? { next_before_seq: oldest } : {}),
      };
    } finally {
      index.database.close();
    }
  }

  async function readMovementLedgerTailPage(ledgerPath, startOffset, expectedSeq, filter) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return { rows: [], total: 0, nextSeq: expectedSeq };
    const rows = [];
    let total = 0;
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset, end: stat.size - 1 })) {
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
        if (!matchesMovementPage(movement, seq, filter)) continue;
        total += 1;
        rows.push({
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        });
        if (rows.length >= filter.limit * 2) rows.splice(0, rows.length - filter.limit);
      }
      pending = bytes.subarray(recordStart);
    }
    return { rows: rows.slice(-filter.limit), total, nextSeq: expectedSeq };
  }

  async function readIndexedFilteredMovements(projection, kinds, limit) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const kindList = [...kinds];
      const placeholders = kindList.map(() => "?").join(", ");
      const indexedRows = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE kind IN (${placeholders})
         ORDER BY seq DESC
         LIMIT ?`,
      ).all(...kindList, limit).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const countRow = index.database.prepare(
        `SELECT COUNT(*) AS count
         FROM movements
         WHERE kind IN (${placeholders})`,
      ).get(...kindList);
      const indexedCount = indexedUnsignedInteger(countRow?.count);
      if (indexedCount === null) return null;
      const tail = await readFilteredLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        kinds,
      );
      if (tail === null) return null;
      return {
        rows: [...indexedRows, ...tail.rows],
        total: indexedCount + tail.count,
      };
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readFilteredLedgerTail(ledgerPath, startOffset, expectedSeq, kinds) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return { rows: [], count: 0 };
    const rows = [];
    let count = 0;
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
        if (!kinds.has(movementBaseKind(movement))) continue;
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
    return { rows, count };
  }

  function artifactLabelMatches(value, label, namespace = false) {
    return value === label || (namespace && typeof value === "string" && value.startsWith(`${label}.`));
  }

  async function indexedArtifactLabelRows(projection, label, { namespace = false } = {}) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const indexed = namespace
        ? index.database.prepare(
          `SELECT
             seq, lamport, recorded_at_unix_ms, trace, kind, mark,
             from_kind, from_id, to_kind, to_id,
             artifact_id, artifact_content,
             runtime_run, runtime_checkpoint, runtime_interface
           FROM movements
           WHERE artifact_label = ? OR (artifact_label >= ? AND artifact_label < ?)
           ORDER BY seq ASC`,
        ).all(label, `${label}.`, `${label}/`)
        : index.database.prepare(
          `SELECT
             seq, lamport, recorded_at_unix_ms, trace, kind, mark,
             from_kind, from_id, to_kind, to_id,
             artifact_id, artifact_content,
             runtime_run, runtime_checkpoint, runtime_interface
           FROM movements
           WHERE artifact_label = ?
           ORDER BY seq ASC`,
        ).all(label);
      const rows = indexed.map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const tail = await readArtifactLabelLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        label,
        namespace,
      );
      if (tail === null) return null;
      rows.push(...tail);
      return rows.sort(compareMovementOrder);
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readArtifactLabelLedgerTail(ledgerPath, startOffset, expectedSeq, label, namespace = false) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return [];
    const rows = [];
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
        if (!artifactLabelMatches(ledgerArtifactLabel(movement), label, namespace)) continue;
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
    return rows;
  }

  return { artifactLabelMatches, filteredMovementRows, indexedArtifactLabelRows, readArtifactLabelLedgerTail, readFilteredLedgerTail, readIndexedFilteredMovements, readIndexedMovementPage, readMovementLedgerTailPage, readMovementPage };
}
