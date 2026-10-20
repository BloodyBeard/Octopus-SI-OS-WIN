module.exports = { topologyRowsProjection };

const { artifactMovementCoordinates, compareArtifactMovementNewest, indexedUnsignedInteger, recentMovementRows, summarizeInterfaceArtifactTransferTargets } = require("./movementRows.cjs");
const { baseInterfaceRows, bindingRows, descriptorRows, eeInstanceRows, envelopeRows, markRows, projectionMemo, projectionSnapshot, recordCheckpointRowsFor, requiredVerifiedArtifactOutcomeIndex, runCheckpointRows, runRows, withoutRawProjectionEvidence, workerEnvironmentRows } = require("./registryRows.cjs");
const { arrayValue, dedupeRows, filterByString, groupRowsBy, isAdmittedInterfaceArtifactMovement, ledgerArtifactId, ledgerArtifactLabel, limitRows, numberValue, objectOrEmpty, stringValue, timestampOf, topologyArtifactRow } = require("./values.cjs");

function topologyRowsProjection(store) {
  function cachedTopologySnapshot(projection, params) {
    const key = JSON.stringify([
      projection.generation,
      params.include_ledger !== false,
      [...new Set(arrayValue(params.focused_run_ids).map(stringValue).filter(Boolean))].sort(),
    ]);
    const cached = projection.topologySnapshotCache;
    if (cached?.key === key) return cached.promise;
    const promise = topologySnapshot(projection, params);
    projection.topologySnapshotCache = { key, promise };
    promise.catch(() => {
      if (projection.topologySnapshotCache?.promise === promise) projection.topologySnapshotCache = null;
    });
    return promise;
  }

  async function topologySnapshot(projection, params) {
    const includeLedger = params.include_ledger !== false;
    const runs = limitRows(runRows(projection, includeLedger), { limit: 1_000 });
    const outcomeIndex = includeLedger ? requiredVerifiedArtifactOutcomeIndex(projection) : new Map();
    const recordByRun = new Map(arrayValue(projection.runRecords).map((record) => [record.run_id, record]));
    const bindings = bindingRows(projection);
    const artifacts = includeLedger ? artifactRows(projection).map(topologyArtifactRow) : [];
    const marks = includeLedger ? markRows(projection).map(withoutRawProjectionEvidence) : [];
    const envelopes = includeLedger ? envelopeRows(projection) : [];
    const bindingsByRun = groupRowsBy(bindings, "run_id");
    const artifactsByRun = groupRowsBy(artifacts, "run_id");
    const marksByRun = groupRowsBy(marks, "run_id");
    const envelopesByRun = new Map();
    for (const row of envelopes) {
      const runId = stringValue(row.run_id ?? row.meta?.run_id ?? row.payload?.run_id);
      if (!runId) continue;
      const group = envelopesByRun.get(runId) ?? [];
      group.push(row);
      envelopesByRun.set(runId, group);
    }
    const requestedRunIds = new Set(arrayValue(params.focused_run_ids).map(stringValue).filter(Boolean));
    for (const run of runs.slice(0, 12)) requestedRunIds.add(stringValue(run.run_id));
    const runDetails = {};
    for (const runId of requestedRunIds) {
      const run = runs.find((row) => stringValue(row.run_id) === runId);
      if (!run) continue;
      const workerEnvironments = workerEnvironmentRows(projection)
        .filter((row) => stringValue(row.run_id) === runId);
      runDetails[runId] = {
        detail: {
          run,
          bindings: bindingsByRun.get(runId) ?? [],
          artifacts: artifactsByRun.get(runId) ?? [],
          marks: marksByRun.get(runId) ?? [],
          checkpoints: recordCheckpointRowsFor(projection, runId, outcomeIndex, recordByRun)
            ?? (includeLedger ? runCheckpointRows(projection, runId) : []),
          worker_environments: workerEnvironments,
          ...(workerEnvironments.length === 1 ? { worker_environment: workerEnvironments[0] } : {}),
        },
        trace: includeLedger
          ? { events: limitRows(envelopesByRun.get(runId) ?? [], { limit: 80 }) }
          : { envelopes: [] },
      };
    }
    return {
      generation: projection.generation,
      snapshotRaw: projectionSnapshot(projection),
      eeRaw: { ee_instances: limitRows(eeInstanceRows(projection, includeLedger), { limit: 300 }) },
      runsRaw: { runs, invalid_run_records: projection.invalidRunRecords },
      runCheckpointsRaw: {
        run_checkpoints: runs.map((run) => ({
          run_id: stringValue(run.run_id),
          checkpoints: recordCheckpointRowsFor(projection, stringValue(run.run_id), outcomeIndex, recordByRun)
            ?? runCheckpointRows(projection, stringValue(run.run_id)),
        })),
      },
      interfacesRaw: { interfaces: interfaceRows(projection) },
      bindingsRaw: { bindings: bindings.filter((row) => stringValue(row.status) === "projected") },
      artifactsRaw: { artifacts: limitRows(artifacts, { limit: 300 }) },
      marksRaw: { marks: limitRows(marks, { limit: 300 }) },
      workerEnvironmentsRaw: { worker_environments: limitRows(workerEnvironmentRows(projection), { limit: 1_000 }) },
      movementsRaw: includeLedger
        ? { movements: (await recentMovementRows(projection, 1_000)).map(withoutRawProjectionEvidence), total: projection.ledgerRows.length }
        : { movements: [], total: 0 },
      runDetailsRaw: runDetails,
    };
  }

  function interfaceRows(projection) {
    const rows = baseInterfaceRows(projection);
    const targetsByInterface = groupRowsBy(
      store.interfaceArtifactTransferTargetRowsFor(rows),
      "interface_id",
    );
    return rows.map((row) => {
      const interfaceId = stringValue(row.rule_id ?? row.id);
      const targetSummary = summarizeInterfaceArtifactTransferTargets(
        targetsByInterface.get(interfaceId) ?? [],
        objectOrEmpty(row.artifact_transfer),
      );
      return {
        ...row,
        artifact_transfer_protocol_status: targetSummary.status,
        artifact_transfer_target_count: targetSummary.target_count,
        artifact_transfer_ready_targets: targetSummary.ready_targets,
        artifact_transfer_target_reason: targetSummary.reason,
      };
    });
  }

  function artifactRows(projection) {
    return projectionMemo(projection, "ledger", "artifactRows", () => {
      const registryArtifacts = descriptorRows(projection, "artifact").map((row) => ({
        ...row,
        artifact_id: stringValue(row.id) || stringValue(row.name),
        label: stringValue(row.label) || stringValue(row.id) || stringValue(row.name),
        status: "registered",
        route_label: stringValue(row.route_label),
        created_at: stringValue(row.created_at),
      }));
      return dedupeRows([...registryArtifacts, ...ledgerArtifactRows(projection)], "artifact_id");
    });
  }

  function ledgerArtifactRows(projection) {
    return projectionMemo(projection, "ledger", "ledgerArtifactRows", () => {
      const ledgerArtifacts = new Map();
      for (const row of envelopeRows(projection)) {
        const artifactId = ledgerArtifactId(row);
        if (!artifactId) continue;
        const coordinates = artifactMovementCoordinates(row);
        const previewRank = isAdmittedInterfaceArtifactMovement(row)
          ? (coordinates.runId && coordinates.interfaceId ? 2 : 1)
          : 0;
        const current = ledgerArtifacts.get(artifactId);
        if (
          !current
          || previewRank > current.previewRank
          || previewRank === current.previewRank && compareArtifactMovementNewest(row, current.row) < 0
        ) {
          ledgerArtifacts.set(artifactId, { coordinates, previewRank, row });
        }
      }
      return [...ledgerArtifacts.entries()].map(([artifactId, entry]) => {
        const runtime = objectOrEmpty(objectOrEmpty(entry.row.invocation_metadata).runtime);
        return {
          artifact_id: artifactId,
          label: ledgerArtifactLabel(entry.row),
          route_label: "",
          run_id: entry.coordinates.runId,
          interface_id: entry.coordinates.interfaceId,
          ee_id: stringValue(runtime.ee),
          worker: stringValue(runtime.worker),
          status: "recorded",
          created_at: timestampOf(entry.row),
          seq: numberValue(entry.row.seq, null),
          movement_kind: stringValue(entry.row.kind),
          previewable: entry.previewRank === 2,
          source: "ledger",
          raw: entry.row,
        };
      }).sort((left, right) => (
        compareArtifactMovementNewest(left.raw, right.raw)
        || stringValue(left.artifact_id).localeCompare(stringValue(right.artifact_id))
      ));
    });
  }

  function artifactListProjection(projection, params) {
    const requestedSource = stringValue(params?.source);
    if (requestedSource && !["ledger", "registry"].includes(requestedSource)) {
      throw new Error("artifact list source must be ledger or registry");
    }
    if (params?.previewable_only !== undefined && typeof params.previewable_only !== "boolean") {
      throw new Error("artifact list previewable_only must be a boolean");
    }
    const parsedLimit = params?.limit === undefined ? 1_000 : indexedUnsignedInteger(params.limit);
    if (parsedLimit === null || parsedLimit < 1 || parsedLimit > 5_000) {
      throw new Error("artifact list limit must be an integer between 1 and 5000");
    }
    const allRows = requestedSource === "ledger" ? ledgerArtifactRows(projection) : artifactRows(projection);
    const sourceRows = requestedSource
      ? allRows.filter((row) => stringValue(row.source) === requestedSource)
      : allRows;
    const matchingRows = filterByString(sourceRows, "label", params?.label);
    const previewableTotal = matchingRows.filter((row) => row.previewable === true).length;
    const unavailableTotal = matchingRows.length - previewableTotal;
    const selectedRows = params?.previewable_only === true
      ? matchingRows.filter((row) => row.previewable === true)
      : matchingRows;
    const artifacts = selectedRows.slice(0, parsedLimit).map(({ raw: _raw, ...row }) => row);
    return {
      artifacts,
      total: matchingRows.length,
      previewable_total: previewableTotal,
      unavailable_total: unavailableTotal,
      returned: artifacts.length,
      limit: parsedLimit,
      truncated: selectedRows.length > artifacts.length,
    };
  }

  function artifactLabelRows(projection) {
    const byLabel = new Map();
    for (const artifact of artifactRows(projection)) {
      const label = stringValue(artifact.label);
      if (!label || byLabel.has(label)) continue;
      byLabel.set(label, {
        label,
        lifecycle: stringValue(artifact.lifecycle) || stringValue(artifact.status) || "artifact",
        allowed_writers: arrayValue(artifact.allowed_writers),
        finalize_recommended: Boolean(artifact.finalize_recommended),
        source: stringValue(artifact.source) || "projection.artifacts",
      });
    }
    return Array.from(byLabel.values()).sort((a, b) => a.label.localeCompare(b.label));
  }

  return { artifactLabelRows, artifactListProjection, artifactRows, cachedTopologySnapshot, interfaceRows, ledgerArtifactRows, topologySnapshot };
}
