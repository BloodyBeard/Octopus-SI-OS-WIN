module.exports = { clientSurfacesProjection };

const crypto = require("node:crypto");
const { ProjectionEvidenceError } = require("./constants.cjs");
const { immutableClientAction, isClientSurfaceDocumentPayload, ledgerRowsForRun, movementRunId } = require("./movementRows.cjs");
const { isAdmittedInterfaceArtifactMovement, ledgerArtifactId, ledgerArtifactLabel, objectOrEmpty, stringValue, timestampOf } = require("./values.cjs");

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
    return Promise.all(page.map(async (row) => {
      const identity = {
        artifact_id: row.artifact.id, run_id: runId, seq: row.seq,
        movement_kind: "InterfaceOutbound", from: row.from, to: row.to,
      };
      try {
        return { ...identity, action: await immutableClientAction(row), turn_closure: await clientActionTurnClosure(closures, row.seq) };
      } catch (error) {
        return { ...identity, error: String(error?.message ?? error) };
      }
    }));
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
