module.exports = { clientSurfacesProjection };

const crypto = require("node:crypto");
const { isDeepStrictEqual } = require("node:util");
const { ProjectionEvidenceError } = require("./constants.cjs");
const { immutableClientAction, isClientSurfaceDocumentPayload, ledgerRowsForRun, movementRunId } = require("./movementRows.cjs");
const { isAdmittedInterfaceArtifactMovement, ledgerArtifactId, ledgerArtifactLabel, objectOrEmpty, stringValue, timestampOf } = require("./values.cjs");

const objectValue = value => value !== null && typeof value === "object" && !Array.isArray(value);

function clientSurfacesProjection(store) {
  async function clientSurfaceDocumentSnapshot(projection) {
    const documents = [];
    const errors = [];
    const indexedRows = await store.indexedArtifactLabelRows(projection, "client.surface.document");
    if (indexedRows === null) await store.refreshLedgerProjection(projection);
    for (const row of indexedRows ?? projection.ledgerRows) {
      const artifactId = ledgerArtifactId(row);
      if (!isAdmittedInterfaceArtifactMovement(row)) continue;
      if (ledgerArtifactLabel(row) !== "client.surface.document") continue;
      if (!artifactId) continue;
      const runId = movementRunId(row);
      const interfaceId = stringValue(objectOrEmpty(row.to).Interface);
      const fromInterfaceId = stringValue(objectOrEmpty(row.from).Interface);
      if (!runId || !interfaceId || !fromInterfaceId || !Number.isSafeInteger(row.seq) || row.seq < 0) {
        errors.push(`${artifactId}: client surface delivery requires an exact run, source and recipient interfaces, and movement sequence`);
        continue;
      }
      let payload;
      try {
        payload = await store.resolveLedgerArtifactPayload(row);
      } catch {
        documents.push({
          path: `artifact:${artifactId}`,
          digest: `sha256:${crypto.createHash("sha256").update("").digest("hex")}`,
          bytes: 0,
          modifiedAt: timestampOf(row),
          text: "",
          artifactId,
          runId,
          interfaceId,
          fromInterfaceId,
          sequence: row.seq,
        });
        continue;
      }
      if (!isClientSurfaceDocumentPayload(payload)) {
        errors.push(`${artifactId}: client.surface.document does not satisfy octopus.client_surface@1`);
        continue;
      }
      const text = JSON.stringify(payload);
      const bytes = Buffer.byteLength(text, "utf8");
      const digest = crypto.createHash("sha256").update(text).digest("hex");
      documents.push({
        path: `artifact:${artifactId}`,
        digest: `sha256:${digest}`,
        bytes,
        modifiedAt: timestampOf(row),
        text,
        artifactId,
        runId,
        interfaceId,
        fromInterfaceId,
        sequence: row.seq,
      });
    }
    return {
      generation: store.projectionGeneration,
      root: "artifact://ledger",
      documents,
      errors,
    };
  }

  async function clientActionRows(projection, params) {
    const runId = params.run_id;
    const interfaceId = params.interface_id;
    const afterSeq = params.after_seq ?? -1;
    const labels = new Set(["client.action"]);
    const indexed = await store.readIndexedInterfaceArtifactRows(
      projection, runId, interfaceId, labels, afterSeq, true,
    );
    if (indexed === null) {
      await store.refreshLedgerProjection(projection);
      if (projection.invalidLedgerRows.length) {
        throw new ProjectionEvidenceError("Client actions cannot be consumed from malformed ledger evidence.");
      }
    }
    const movements = indexed ?? ledgerRowsForRun(projection, runId);
    const page = movements.filter((row) => store.matchesInterfaceArtifactMovement(
      row, runId, interfaceId, labels, afterSeq, true,
    ))
      .sort((left, right) => left.seq - right.seq)
      .slice(0, params.limit ?? 128);
    const closures = page.length ? await clientActionTurnClosures(projection, runId, interfaceId, page[0].seq) : [];
    const results = page.length ? await clientActionResults(projection, runId, interfaceId, page) : new Map();
    return Promise.all(page.map(async (row) => {
      const identity = {
        artifact_id: row.artifact.id, run_id: runId, seq: row.seq,
        movement_kind: "InterfaceOutbound", from: row.from, to: row.to,
      };
      try {
        const request = await immutableClientAction(row);
        const receipts = results.get(row.artifact.id) ?? [];
        for (const { payload, row: receipt } of receipts) {
          if (payload.schema !== "octopus.client_action_result" || payload.schema_version !== 1
            || payload.phase !== "completed" || typeof payload.ok !== "boolean"
            || receipt.seq <= row.seq || payload.request_seq !== row.seq || payload.request_id !== request.action.request_id
            || payload.action !== request.action.action || !isDeepStrictEqual(payload.tool_call, request.toolCall)
            || (payload.ok ? !Array.isArray(payload.widgets) && !objectValue(payload.client) && !objectValue(payload.relaunch) : typeof payload.error !== "string")
            || !isDeepStrictEqual(payload, receipts[0].payload)) {
            throw new Error("Recorded client result conflicts with its exact request.");
          }
        }
        return { ...identity, action: request.action, tool_call: request.toolCall,
          ...(receipts.length ? { completed_result: { artifact_id: receipts[0].row.artifact.id, seq: receipts[0].row.seq } } : {}),
          expires_at_ms: row.recorded_at_unix_ms + 60_000,
          turn_closure: await clientActionTurnClosure(closures, row.seq) };
      } catch (error) {
        return { ...identity, error: String(error?.message ?? error) };
      }
    }));
  }

  async function clientActionResults(projection, runId, interfaceId, actions) {
    const labels = new Set(["client.action.result"]);
    const after = actions[0].seq;
    const indexed = await store.readIndexedInterfaceArtifactRows(projection, runId, interfaceId, labels, after);
    if (indexed === null) await store.refreshLedgerProjection(projection);
    const requests = new Set(actions.map(row => row.artifact.id));
    const results = new Map();
    for (const row of indexed ?? ledgerRowsForRun(projection, runId)) {
      if (row.kind !== "InterfaceArrival" || row.from?.Interface !== interfaceId
        || !store.matchesInterfaceArtifactMovement(row, runId, interfaceId, labels, after)) continue;
      const payload = objectOrEmpty(await store.resolveLedgerArtifactPayload(row));
      if (!requests.has(payload.request_artifact)) continue;
      const previous = results.get(payload.request_artifact) ?? [];
      previous.push({ row, payload });
      results.set(payload.request_artifact, previous);
    }
    return results;
  }

  async function clientActionTurnClosures(projection, runId, interfaceId, afterSeq) {
    const labels = new Set(["turn.completed", "turn.interrupted.result", "turn.control.acknowledged"]);
    const indexed = await store.readIndexedInterfaceArtifactRows(projection, runId, interfaceId, labels, afterSeq, true);
    if (indexed === null) await store.refreshLedgerProjection(projection);
    return (indexed ?? ledgerRowsForRun(projection, runId))
      .filter((row) => store.matchesInterfaceArtifactMovement(row, runId, interfaceId, labels, afterSeq, true))
      .sort((left, right) => left.seq - right.seq);
  }

  async function clientActionTurnClosure(closures, actionSeq) {
    for (const row of closures) {
      if (row.seq <= actionSeq) continue;
      const label = ledgerArtifactLabel(row);
      if (label === "turn.interrupted.result") return "stopped";
      const payload = objectOrEmpty(await store.resolveLedgerArtifactPayload(row));
      if (label === "turn.control.acknowledged") {
        if (["stop", "interrupt"].includes(stringValue(payload.action))) return "stopped";
        continue;
      }
      return stringValue(objectOrEmpty(payload.turn).status).toLowerCase() === "completed" ? "completed" : "stopped";
    }
    return null;
  }

  return { clientActionRows, clientActionTurnClosure, clientActionTurnClosures, clientSurfaceDocumentSnapshot };
}
