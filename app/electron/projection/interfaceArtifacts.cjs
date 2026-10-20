module.exports = { interfaceArtifactsProjection };

const crypto = require("node:crypto");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const { ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES, ARTIFACT_PREVIEW_MAX_BYTES, INTERFACE_MEDIA_MAX_BYTES } = require("./constants.cjs");
const { artifactAttachmentBytes, artifactMovementCoordinates, artifactPayloadFilename, artifactPayloadMimeType, compareMovementOrder, indexedUnsignedInteger, interfaceArtifactTransferTargetRow, ledgerRowsForRun, movementKindText, movementRunId, nestedArtifactMedia, normalizeIndexedProjectionMovement, openValidatedLedgerIndex, readStableContentAddressedArtifactBytes } = require("./movementRows.cjs");
const { operatorChatAttachments } = require("./operatorChatEvents.cjs");
const { baseInterfaceRows, interfaceActivationEntries, preserveProjectionEvidenceError } = require("./registryRows.cjs");
const { arrayValue, contentHashHex, dedupeRows, interfaceArtifactTransferProjection, isAdmittedInterfaceArtifactMovement, ledgerArtifactId, ledgerArtifactLabel, limitRows, numberValue, objectOrEmpty, requiredContentHash, requiredString, stringValue, timestampOf } = require("./values.cjs");

function interfaceArtifactsProjection(store) {
  async function interfaceArtifactPayloadRows(projection, params) {
    const runId = stringValue(params.run_id);
    const interfaceId = stringValue(params.interface_id);
    const labels = new Set(arrayValue(params.labels).map(stringValue).filter(Boolean));
    const afterSeq = numberValue(params.after_seq, -1);
    if (params.order !== undefined && params.order !== "oldest" && params.order !== "newest") {
      throw new Error("interface artifact order must be oldest or newest");
    }
    const newest = params.order === "newest";
    const indexed = await readIndexedInterfaceArtifactRows(
      projection,
      runId,
      interfaceId,
      labels,
      afterSeq,
    );
    if (indexed === null) await store.refreshLedgerProjection(projection);
    const seen = new Set();
    const selected = [];
    const movements = indexed ?? (runId ? ledgerRowsForRun(projection, runId) : projection.ledgerRows);
    for (const row of movements) {
      if (!matchesInterfaceArtifactMovement(row, runId, interfaceId, labels, afterSeq)) continue;
      const artifactId = ledgerArtifactId(row);
      if (seen.has(artifactId)) continue;
      seen.add(artifactId);
      selected.push(row);
    }
    const ordered = selected.sort((left, right) => numberValue(left.seq, -1) - numberValue(right.seq, -1));
    const page = limitRows(newest ? ordered.reverse() : ordered, params);
    const payloads = await store.resolveLedgerArtifactPayloadResults(page);
    const artifacts = [];
    for (const row of page) {
      const artifactId = ledgerArtifactId(row);
      const label = ledgerArtifactLabel(row);
      const seq = numberValue(row.seq, -1);
      const coordinates = artifactMovementCoordinates(row);
      const resolved = payloads.get(row);
      if (resolved?.error) {
        artifacts.push({
          artifact_id: artifactId,
          label,
          seq,
          run_id: coordinates.runId,
          interface_id: coordinates.interfaceId,
          error: resolved.error,
        });
        continue;
      }
      artifacts.push({
        artifact_id: artifactId,
        label,
        seq,
        run_id: coordinates.runId,
        interface_id: coordinates.interfaceId,
        movement_kind: stringValue(row.kind),
        created_at: timestampOf(row),
        payload: resolved?.payload ?? null,
      });
    }
    return artifacts.sort((left, right) => newest ? right.seq - left.seq : left.seq - right.seq);
  }

  function matchesInterfaceArtifactMovement(row, runId, interfaceId, labels, afterSeq, outboundRecipientOnly = false) {
    if (outboundRecipientOnly ? row.kind !== "InterfaceOutbound" : !isAdmittedInterfaceArtifactMovement(row)) return false;
    const artifactId = ledgerArtifactId(row);
    const label = outboundRecipientOnly ? row.artifact?.id?.split(":")[0] : ledgerArtifactLabel(row);
    const seq = numberValue(row.seq, -1);
    if (!artifactId || seq <= afterSeq) return false;
    if (labels.size > 0 && !labels.has(label)) return false;
    if (outboundRecipientOnly) {
      return row.invocation_metadata?.runtime?.run === runId && row.to?.Interface === interfaceId;
    }
    const coordinates = artifactMovementCoordinates(row);
    if (runId && coordinates.runId !== runId) return false;
    return !interfaceId || coordinates.interfaceId === interfaceId
      || row.from?.Interface === interfaceId || row.to?.Interface === interfaceId;
  }

  async function admittedArtifactMovement(projection, artifactId, predicate) {
    const indexed = await readIndexedArtifactMovementRows(projection, artifactId);
    if (indexed !== null) return indexed.find(predicate) ?? null;
    await store.refreshLedgerProjection(projection);
    return projection.ledgerRows.find((row) => (
      ledgerArtifactId(row) === artifactId
      && isAdmittedInterfaceArtifactMovement(row)
      && predicate(row)
    )) ?? null;
  }

  async function readIndexedArtifactMovementRows(projection, artifactId) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const indexedRows = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE artifact_id = ?
         ORDER BY seq ASC`,
      ).all(artifactId).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const tail = await readArtifactLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        artifactId,
      );
      if (tail === null) return null;
      return [...indexedRows, ...tail]
        .filter(isAdmittedInterfaceArtifactMovement)
        .sort(compareMovementOrder);
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readArtifactLedgerTail(ledgerPath, startOffset, expectedSeq, artifactId) {
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
        if (
          ledgerArtifactId(movement) !== artifactId
          || !isAdmittedInterfaceArtifactMovement(movement)
        ) continue;
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

  async function readIndexedInterfaceArtifactRows(
    projection,
    runId,
    interfaceId,
    labels,
    afterSeq,
    outboundRecipientOnly = false,
  ) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const predicates = [
        "artifact_id IS NOT NULL",
        outboundRecipientOnly ? "kind = 'InterfaceOutbound'" : "kind IN ('InterfaceArrival', 'InterfaceOutbound')",
        "seq > ?",
      ];
      const values = [afterSeq];
      if (labels.size > 0) {
        predicates.push(`artifact_label IN (${[...labels].map(() => "?").join(", ")})`);
        values.push(...labels);
      }
      if (runId) {
        predicates.push(`(
          runtime_run = ?
        )`);
        values.push(runId);
      }
      if (interfaceId && outboundRecipientOnly) {
        predicates.push("to_kind = 'interface' AND to_id = ?");
        values.push(interfaceId);
      } else if (interfaceId) {
        predicates.push(`(
          runtime_interface = ?
          OR (from_kind = 'interface' AND from_id = ?)
          OR (to_kind = 'interface' AND to_id = ?)
        )`);
        values.push(interfaceId, interfaceId, interfaceId);
      }
      const indexedRows = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE ${predicates.join("\nAND ")}
         ORDER BY seq ASC`,
      ).all(...values).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const tail = await readInterfaceArtifactLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        runId,
        interfaceId,
        labels,
        afterSeq,
        outboundRecipientOnly,
      );
      if (tail === null) return null;
      const rows = [...indexedRows, ...tail].sort(compareMovementOrder);
      return rows;
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readInterfaceArtifactLedgerTail(
    ledgerPath,
    startOffset,
    expectedSeq,
    runId,
    interfaceId,
    labels,
    afterSeq,
    outboundRecipientOnly = false,
  ) {
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
        const projected = {
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        };
        if (matchesInterfaceArtifactMovement(
          projected,
          runId,
          interfaceId,
          labels,
          afterSeq,
          outboundRecipientOnly,
        )) rows.push(projected);
      }
      pending = bytes.subarray(recordStart);
    }
    return rows;
  }

  function activityAttachments(payload, artifactId, label, createdAtMs) {
    const attachments = operatorChatAttachments(payload);
    const media = objectOrEmpty(payload.media);
    const mimeType = stringValue(media.mime_type ?? payload.mime_type);
    if (!mimeType) return attachments;
    const mediaAttachment = {
      id: artifactId,
      name: stringValue(media.name ?? payload.name) || label || "Artifact",
      size: numberValue(media.size ?? payload.size, 0),
      type: mimeType,
      label: label || "artifact",
      last_modified: createdAtMs,
    };
    return attachments.some((attachment) => attachment.id === artifactId)
      ? attachments
      : [...attachments, mediaAttachment];
  }

  async function interfaceArtifactMedia(projection, params) {
    const artifactId = requiredString(params.artifact_id, "artifact_id");
    const runId = requiredString(params.run_id, "run_id");
    const interfaceId = requiredString(params.interface_id, "interface_id");
    const row = await admittedArtifactMovement(projection, artifactId, (candidate) => {
      if (ledgerArtifactLabel(candidate) !== "realtime.session.audio") return false;
      const coordinates = artifactMovementCoordinates(candidate);
      return coordinates.runId === runId && coordinates.interfaceId === interfaceId;
    });
    if (!row) {
      throw new Error("The requested realtime audio artifact is not present in the ledger projection.");
    }
    const { runId: rowRunId, interfaceId: rowInterfaceId } = artifactMovementCoordinates(row);
    const payload = await store.resolveLedgerArtifactPayload(row);
    if (!payload || payload.schema !== "octopus.voice_audio" || payload.schema_version !== 1) {
      throw new Error("The realtime audio artifact does not satisfy octopus.voice_audio@1.");
    }
    const media = objectOrEmpty(payload.media);
    const mediaPath = requiredString(media.path, "media.path");
    const mediaType = requiredString(media.mime_type, "media.mime_type");
    const expectedHash = requiredString(media.content_hash, "media.content_hash").toLowerCase();
    const expectedSize = numberValue(media.size, -1);
    if (!path.isAbsolute(mediaPath)) throw new Error("Realtime audio media.path must be absolute.");
    if (!/^audio\/[A-Za-z0-9.+-]+(?:[;][ -~]*)?$/.test(mediaType)) throw new Error("Realtime audio media.mime_type must be an audio MIME type.");
    if (!/^[0-9a-f]{64}$/.test(expectedHash)) throw new Error("Realtime audio media.content_hash must be a SHA-256 hex digest.");
    const stat = await fs.stat(mediaPath);
    if (!stat.isFile()) throw new Error("Realtime audio media.path is not a regular file.");
    if (stat.size <= 0 || stat.size > INTERFACE_MEDIA_MAX_BYTES) {
      throw new Error(`Realtime audio media size must be between 1 and ${INTERFACE_MEDIA_MAX_BYTES} bytes.`);
    }
    if (expectedSize !== stat.size) throw new Error("Realtime audio media.size does not match the admitted file.");
    const bytes = await fs.readFile(mediaPath);
    const actualHash = crypto.createHash("sha256").update(bytes).digest("hex");
    if (actualHash !== expectedHash) throw new Error("Realtime audio media.content_hash does not match the admitted file.");
    return {
      artifact_id: artifactId,
      run_id: rowRunId,
      interface_id: rowInterfaceId,
      correlation_id: stringValue(payload.correlation_id),
      mime_type: mediaType,
      content_hash: actualHash,
      size: bytes.byteLength,
      base64: bytes.toString("base64"),
    };
  }

  async function admittedArtifactBytes(projection, params) {
    const runId = requiredString(params.run_id, "run_id");
    const byCall = Object.hasOwn(params, "call_id");
    const row = byCall
      ? await admittedToolResultMovement(projection, runId, requiredString(params.call_id, "call_id"))
      : await admittedArtifactMovement(projection, requiredString(params.artifact_id, "artifact_id"), (candidate) => {
        const coordinates = artifactMovementCoordinates(candidate);
        return coordinates.runId === runId && coordinates.interfaceId === requiredString(params.interface_id, "interface_id");
      });
    if (!row) {
      throw new Error(byCall
        ? "The tool result for this call is not admitted in this run."
        : "The requested artifact is not admitted for this run and interface.");
    }
    const artifactId = ledgerArtifactId(row);
    const interfaceId = artifactMovementCoordinates(row).interfaceId;
    const manifestHash = contentHashHex(row?.artifact?.content);
    if (!manifestHash || !row.source_file) throw new Error("The admitted artifact has no content-addressed manifest.");
    const blobRoot = path.join(path.dirname(row.source_file), "blobs");
    const manifestBytes = await readStableContentAddressedArtifactBytes(
      path.join(blobRoot, manifestHash),
      manifestHash,
      ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES,
      "artifact manifest",
    );
    let manifest;
    try {
      manifest = JSON.parse(manifestBytes.toString("utf8"));
    } catch {
      throw new Error("The admitted artifact manifest is not valid JSON.");
    }
    if (!manifest || typeof manifest !== "object" || Array.isArray(manifest)) {
      throw new Error("The admitted artifact manifest is not a JSON object.");
    }
    const label = requiredString(manifest.label, "artifact manifest label");
    if (label !== ledgerArtifactLabel(row) || artifactId !== `${label}:${manifestHash}`) {
      throw new Error("The admitted artifact identity does not match its content-addressed manifest.");
    }
    const payloadHash = requiredContentHash(manifest.payload_content_hash, "artifact manifest payload_content_hash");
    const payloadPath = requiredString(manifest.payload_path, "artifact manifest payload_path");
    if (!path.isAbsolute(payloadPath)) throw new Error("The admitted artifact payload_path must be absolute.");
    const payloadBytes = await readStableContentAddressedArtifactBytes(
      path.join(blobRoot, payloadHash),
      payloadHash,
      ARTIFACT_PREVIEW_MAX_BYTES,
      "artifact payload blob",
    );
    const nested = Object.hasOwn(params, "attachment_id")
      ? await artifactAttachmentBytes(payloadBytes, params.attachment_id)
      : await nestedArtifactMedia(payloadBytes);
    const selected = nested ?? {
      bytes: payloadBytes,
      contentHash: payloadHash,
      filename: artifactPayloadFilename(label, payloadPath, payloadBytes),
      mimeType: artifactPayloadMimeType(payloadBytes),
    };
    return {
      artifact_id: artifactId,
      run_id: runId,
      interface_id: interfaceId,
      ...(Object.hasOwn(params, "attachment_id") ? { attachment_id: params.attachment_id } : {}),
      filename: selected.filename,
      mime_type: selected.mimeType,
      content_hash: selected.contentHash,
      size: selected.bytes.byteLength,
      base64: selected.bytes.toString("base64"),
    };
  }

  async function admittedToolResultMovement(projection, runId, callId) {
    const index = await openValidatedLedgerIndex(projection);
    let rows;
    if (index) {
      try {
        rows = index.database.prepare(
          `SELECT
             seq, lamport, recorded_at_unix_ms, trace, kind, mark,
             from_kind, from_id, to_kind, to_id,
             artifact_id, artifact_content,
             runtime_run, runtime_checkpoint, runtime_interface
           FROM movements
           WHERE runtime_run = ? AND kind = 'InterfaceOutbound' AND artifact_id LIKE '%.result:%'
           ORDER BY seq ASC`,
        ).all(runId).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      } finally {
        index.database.close();
      }
    } else {
      await store.refreshLedgerProjection(projection);
      rows = projection.ledgerRows.filter((row) => (
        movementRunId(row) === runId
        && stringValue(row.kind) === "InterfaceOutbound"
        && ledgerArtifactLabel(row).endsWith(".result")
      ));
    }
    for (const row of rows.filter(isAdmittedInterfaceArtifactMovement)) {
      const payload = objectOrEmpty(await store.resolveLedgerArtifactPayload(row));
      if (stringValue(payload.call_id ?? payload.correlation_id) === callId) return row;
    }
    return null;
  }

  async function attachInterfaceArtifactTransferProjection(row, readinessCache) {
    const transfer = await interfaceArtifactTransferProjection(store.octopusHome(), row, readinessCache);
    return {
      ...row,
      artifact_transfer: transfer,
      artifact_transfer_surface_status: transfer.status,
      artifact_transfer_path: stringValue(transfer.path),
      artifact_transfer_reason: stringValue(transfer.reason),
      activation_transport_refs: transfer.activation_refs,
    };
  }

  function interfaceArtifactTransferTargetRows(projection) {
    return interfaceArtifactTransferTargetRowsFor(baseInterfaceRows(projection));
  }

  function interfaceArtifactTransferTargetRowsFor(interfaces) {
    const rows = [];
    for (const iface of interfaces) {
      const interfaceId = stringValue(iface.rule_id ?? iface.id);
      if (!interfaceId) continue;
      const transfer = objectOrEmpty(iface.artifact_transfer);
      const activations = interfaceActivationEntries(iface.activation_sidecars);
      const transferPath = stringValue(transfer.path);
      const transferStatus = stringValue(transfer.status) || "unknown";
      const transferReason = stringValue(transfer.reason);
      const filesystemActivations = activations.filter((activation) => activation.ref === "app_server_transport");
      if (filesystemActivations.length === 0) {
        rows.push(interfaceArtifactTransferTargetRow({
          interfaceId,
          iface,
          activation: {},
          transferPath,
          status: "not_declared",
          reason: transferReason || "no activation declares an artifact hand-in socket",
        }));
        continue;
      }
      for (const activation of filesystemActivations) {
        if (transferStatus !== "ready") {
          rows.push(interfaceArtifactTransferTargetRow({
            interfaceId,
            iface,
            activation,
            transferPath,
            status: transferStatus === "not_declared" ? "not_declared" : "unavailable",
            reason: transferReason || "artifact hand-in socket is not ready",
          }));
          continue;
        }
        rows.push(interfaceArtifactTransferTargetRow({
          interfaceId,
          iface,
          activation,
          transferPath,
          status: "ready",
          reason: "declared artifact hand-in socket is ready",
        }));
      }
    }
    return dedupeRows(rows, "target_id");
  }

  return { activityAttachments, admittedArtifactBytes, admittedArtifactMovement, admittedToolResultMovement, attachInterfaceArtifactTransferProjection, interfaceArtifactMedia, interfaceArtifactPayloadRows, interfaceArtifactTransferTargetRows, interfaceArtifactTransferTargetRowsFor, matchesInterfaceArtifactMovement, readArtifactLedgerTail, readIndexedArtifactMovementRows, readIndexedInterfaceArtifactRows, readInterfaceArtifactLedgerTail };
}
