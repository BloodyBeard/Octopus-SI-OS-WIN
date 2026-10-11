const { isDeepStrictEqual } = require("node:util");

const text = value => typeof value === "string" && value.length ? value : null;
const integer = value => Number.isSafeInteger(value) && value >= 0 ? value : null;
const list = value => Array.isArray(value) ? value : [];
const object = value => value && typeof value === "object" && !Array.isArray(value) ? value : {};

function contentHash(value) {
  if (typeof value === "string" && /^[a-f0-9]{64}$/.test(value)) return value;
  if (Array.isArray(value) && value.length === 32
    && value.every(byte => Number.isInteger(byte) && byte >= 0 && byte <= 255)) {
    return Buffer.from(value).toString("hex");
  }
  return null;
}

function artifactRef(value) {
  const id = text(value?.id), content = contentHash(value?.content);
  return id && content ? { id, content } : null;
}

function markName(row) {
  return typeof row.kind === "string" ? /^MarkOutcomeAdmitted\((.+)\)$/.exec(row.kind)?.[1] ?? null : null;
}

function generation(row) {
  const metadata = object(row.invocation_metadata);
  const mark = markName(row);
  return mark
    ? { runId: text(metadata.run), checkpointId: text(metadata.subject), epoch: integer(metadata.run_epoch) }
    : { runId: text(metadata.runtime?.run), checkpointId: text(metadata.runtime?.checkpoint),
      epoch: integer(metadata.runtime?.epoch) };
}

function sameGeneration(row, target) {
  const actual = generation(row);
  return actual.epoch !== null && target.epoch !== null
    && actual.runId === target.runId && actual.checkpointId === target.checkpointId
    && actual.epoch === target.epoch;
}

function receipt(row) {
  const metadata = object(row.invocation_metadata);
  return {
    seq: integer(row.seq), lamport: integer(row.lamport), kind: text(row.kind), trace: text(row.trace),
    ...generation(row), fromInterface: text(row.from?.Interface), toInterface: text(row.to?.Interface),
    artifact: artifactRef(row.artifact), sourceMovementSeq: integer(metadata.source_movement_seq),
    artifactLabel: text(metadata.checkpoint_admission?.artifact_label ?? metadata.verification_failure?.artifact_label),
    sourceFile: text(row.source_file), sourceLine: integer(row.source_line),
    ...(metadata.verification_failure ? { verificationFailure: structuredClone(metadata.verification_failure) } : {}),
    ...(metadata.delivery_verdict ? { deliveryVerdict: structuredClone(metadata.delivery_verdict) } : {}),
  };
}

function requirement(value) {
  return { mark: text(value?.mark), artifact_label: text(value?.artifact_label),
    verifier: text(value?.verifier), params: structuredClone(value?.params ?? null) };
}

function indexed(rows, keyOf) {
  const index = new Map();
  for (const row of rows) {
    const key = keyOf(row);
    const group = index.get(key) ?? [];
    group.push(row);
    index.set(key, group);
  }
  return index;
}

function edgeKey(row) {
  return JSON.stringify([generation(row), text(row.from?.Interface), text(row.to?.Interface),
    text(row.trace), artifactRef(row.artifact)]);
}

function recordedProofs(checkpoint, arrivals, marks, uncertainties) {
  const arrivalsByEdge = indexed(arrivals, edgeKey);
  const proofs = [];
  for (const mark of marks) {
    const metadata = object(mark.invocation_metadata), artifact = artifactRef(mark.artifact);
    const packagePin = metadata.verifier_package;
    const descriptorContent = contentHash(packagePin?.descriptor_content);
    const scriptContent = contentHash(packagePin?.script_content);
    if (metadata.name !== markName(mark) || metadata.class !== "success" || !artifact
      || !isDeepStrictEqual(artifact, artifactRef(metadata.evidence)) || !descriptorContent || !scriptContent) {
      uncertainties.push({ code: "incomplete_mark_evidence", checkpointId: checkpoint.checkpointId, seq: mark.seq });
      continue;
    }
    const matching = (arrivalsByEdge.get(edgeKey(mark)) ?? []).filter(arrival => {
      const input = object(arrival.invocation_metadata), runtime = object(input.runtime);
      const declared = list(input.required_verified_artifacts).map(requirement);
      const selected = declared.filter(item => item.artifact_label === input.checkpoint_admission?.artifact_label);
      return integer(arrival.seq) !== null && integer(mark.seq) !== null && arrival.seq < mark.seq
        && integer(arrival.lamport) !== null && integer(mark.lamport) !== null && arrival.lamport < mark.lamport
        && text(arrival.trace) && !input.verification_failure
        && arrival.from?.Interface === runtime.interface && arrival.to?.Interface === runtime.worker
        && selected.length === 1 && selected[0].mark === metadata.name && selected[0].verifier === metadata.verifier;
    });
    if (matching.length !== 1) {
      uncertainties.push({ code: matching.length ? "ambiguous_mark_arrival" : "unmatched_mark_arrival",
        checkpointId: checkpoint.checkpointId, seq: mark.seq, arrivalSeqs: matching.map(row => row.seq) });
      continue;
    }
    const arrival = matching[0], input = arrival.invocation_metadata;
    const recordedRequirements = list(input.required_verified_artifacts).map(requirement);
    const selected = recordedRequirements.find(item => item.artifact_label === input.checkpoint_admission.artifact_label);
    proofs.push({
      kind: "recorded_admitted_mark", ...generation(mark), mark: metadata.name,
      artifactLabel: selected.artifact_label, verifier: metadata.verifier, params: selected.params,
      artifact, arrivalSeq: arrival.seq, markSeq: mark.seq, trace: mark.trace,
      fromInterface: text(mark.from?.Interface), toInterface: text(mark.to?.Interface),
      verifierPackage: { descriptorContent, scriptContent },
      matchesCurrentRequirements: isDeepStrictEqual(recordedRequirements, checkpoint.required),
      matchesCurrentEndpoints: arrival.from?.Interface === checkpoint.interfaceId
        && arrival.to?.Interface === checkpoint.worker && input.runtime.ee === checkpoint.ee,
      snapshotCompatibility: "not_evaluated",
    });
  }
  const byArrival = indexed(proofs, proof => proof.arrivalSeq);
  return proofs.filter(proof => {
    if (byArrival.get(proof.arrivalSeq).length === 1) return true;
    uncertainties.push({ code: "multiple_marks_for_arrival", checkpointId: checkpoint.checkpointId,
      seq: proof.markSeq, arrivalSeq: proof.arrivalSeq });
    return false;
  });
}

function projectCheckpoint(runId, run, row, rows, uncertainties) {
  const checkpoint = {
    runId, checkpointId: text(row.checkpoint_id), epoch: integer(row.epoch),
    active: row.status === "projected", projectedStatus: text(row.status),
    interfaceId: text(row.interface), worker: text(run.worker), ee: text(run.ee_id),
    required: list(row.required_verified_artifacts).map(requirement),
  };
  const scoped = rows.filter(movement => sameGeneration(movement, checkpoint));
  const arrivals = scoped.filter(movement => movement.kind === "InterfaceArrival");
  const marks = scoped.filter(movement => markName(movement));
  const failures = scoped.filter(movement => ["VerificationFailed", "DeliveryRefused"].includes(movement.kind));
  const admittedProofs = recordedProofs(checkpoint, arrivals, marks, uncertainties);
  const requirements = checkpoint.required.map(required => {
    const evidence = admittedProofs.filter(proof => proof.matchesCurrentRequirements && proof.matchesCurrentEndpoints
      && proof.mark === required.mark && proof.verifier === required.verifier
      && proof.artifactLabel === required.artifact_label && isDeepStrictEqual(proof.params, required.params));
    const completeIdentity = checkpoint.epoch !== null && checkpoint.checkpointId !== null
      && required.mark !== null && required.artifact_label !== null && required.verifier !== null;
    return {
      mark: required.mark, artifactLabel: required.artifact_label, verifier: required.verifier, params: required.params,
      state: !completeIdentity ? "unknown" : evidence.length === 1 ? "recorded_admitted"
        : evidence.length > 1 ? "ambiguous" : "outstanding",
      admittedProofs: evidence,
    };
  });
  if (checkpoint.epoch === null) uncertainties.push({ code: "checkpoint_epoch_not_projected", checkpointId: checkpoint.checkpointId });
  if (admittedProofs.some(proof => !proof.matchesCurrentRequirements || !proof.matchesCurrentEndpoints)) {
    uncertainties.push({ code: "checkpoint_contract_changed_within_generation", checkpointId: checkpoint.checkpointId });
  }
  const { required, worker, ee, ...identity } = checkpoint;
  return { ...identity, requirements, admittedProofs,
    recordedMarks: marks.map(row => ({ ...receipt(row), mark: markName(row), verifier: text(row.invocation_metadata?.verifier) })),
    arrivals: arrivals.map(receipt), failures: failures.map(receipt),
    awaits: structuredClone(object(row.awaits)), runtimeCompletion: "not_evaluated" };
}

function relayEvidence(runId, run, bindings, rows, checkpoints) {
  const outbounds = rows.filter(row => row.kind === "InterfaceOutbound");
  const outboundsByInterface = indexed(outbounds, row => text(row.to?.Interface));
  const linkedBySource = indexed(rows.filter(row => generation(row).runId === runId),
    row => integer(row.invocation_metadata?.source_movement_seq));
  const candidates = [];
  for (const binding of bindings) {
    if (binding.run_id !== runId || binding.direction !== "inbound" || binding.status !== "projected") continue;
    const checkpoint = checkpoints.find(cp => cp.checkpointId === binding.checkpoint_id
      && cp.epoch !== null && cp.epoch === integer(binding.epoch) && cp.active
      && cp.interfaceId === binding.interface_id);
    if (!checkpoint) continue;
    for (const outbound of outboundsByInterface.get(binding.interface_id) ?? []) {
      const source = generation(outbound), metadata = object(outbound.invocation_metadata);
      if (source.runId === runId && source.checkpointId === checkpoint.checkpointId) continue;
      if (!source.runId || !source.checkpointId || source.epoch === null || integer(outbound.seq) === null
        || !text(outbound.from?.Interface) || !artifactRef(outbound.artifact)
        || metadata.runtime?.interface !== binding.interface_id) continue;
      const linked = (linkedBySource.get(outbound.seq) ?? []).filter(row => sameGeneration(row, checkpoint)
        && row.seq > outbound.seq && row.from?.Interface === binding.interface_id && row.to?.Interface === run.worker
        && isDeepStrictEqual(artifactRef(row.artifact), artifactRef(outbound.artifact)));
      const arrivals = linked.filter(row => row.kind === "InterfaceArrival");
      const proofs = checkpoint.admittedProofs.filter(proof => arrivals.some(row => row.seq === proof.arrivalSeq));
      candidates.push({
        state: "candidate", checkpointId: checkpoint.checkpointId, epoch: checkpoint.epoch,
        interfaceId: binding.interface_id, outbound: receipt(outbound),
        arrivals: arrivals.map(receipt), admittedProofs: proofs,
        failures: linked.filter(row => ["VerificationFailed", "DeliveryRefused"].includes(row.kind)).map(receipt),
        observation: proofs.length ? "recorded_mark_evidence" : arrivals.length ? "arrival_without_recorded_mark" : "no_linked_arrival_observed",
        runtimeDisposition: "not_projected", replayAdmission: "not_evaluated",
        bindingPolicy: "not_evaluated", queued: null,
      });
    }
  }
  return { candidates, recordedOutbounds: outbounds.filter(row => generation(row).runId === runId).map(receipt) };
}

function runProjection(value, runId) {
  if (!value || value.run?.run_id !== runId || !Array.isArray(value.checkpoints) || !Array.isArray(value.bindings)) {
    throw new Error(`Incomplete run projection for ${runId}`);
  }
  return structuredClone({ run: value.run, checkpoints: value.checkpoints, bindings: value.bindings });
}

async function inspectRun(client, runId) {
  if (!client || typeof client.read !== "function") throw new TypeError("inspectRun requires a projection client.read method");
  if (!text(runId) || runId.trim() !== runId) throw new TypeError("inspectRun requires an exact nonempty runId");
  const params = { run_id: runId, include_ledger: false };
  const before = runProjection(await client.read("projection.runs.get", params), runId);
  const ledger = await client.read("projection.envelopes.list", {});
  if (!Array.isArray(ledger?.envelopes)) throw new Error("Incomplete movement envelope projection");
  const after = runProjection(await client.read("projection.runs.get", params), runId);
  const runProjectionStable = isDeepStrictEqual(before, after);
  const uncertainties = [
    { code: "recorded_evidence_only", detail: "No verifier, current policy formation, queue state or runtime replay admission is evaluated." },
    { code: "projection_coverage", detail: "All returned envelopes are inspected; this method exposes neither invalid-row diagnostics nor an atomic registry/ledger revision." },
    { code: "binding_snapshot_not_projected", detail: "Run projection omits full binding policies and formed grants; a recorded mark does not establish current snapshot compatibility." },
  ];
  if (!runProjectionStable) uncertainties.push({ code: "run_projection_changed_during_read" });
  if (after.run.source !== "run_record") uncertainties.push({ code: "current_run_record_not_projected" });
  const sequences = new Set();
  for (const row of ledger.envelopes) {
    if (integer(row?.seq) === null || sequences.has(row.seq)) throw new Error("Movement projection has missing or duplicate sequence evidence");
    sequences.add(row.seq);
  }
  const rows = [...ledger.envelopes].sort((a, b) => a.seq - b.seq);
  const byGeneration = indexed(rows, row => JSON.stringify(generation(row)));
  const checkpoints = after.checkpoints.map(checkpoint => projectCheckpoint(runId, after.run, checkpoint,
    byGeneration.get(JSON.stringify({ runId, checkpointId: text(checkpoint.checkpoint_id), epoch: integer(checkpoint.epoch) })) ?? [], uncertainties));
  return {
    runId, run: after.run, checkpoints,
    outstanding: checkpoints.filter(checkpoint => checkpoint.active).flatMap(checkpoint => checkpoint.requirements
      .filter(required => required.state !== "recorded_admitted")
      .map(required => ({ runId, checkpointId: checkpoint.checkpointId, epoch: checkpoint.epoch, ...required }))),
    relay: relayEvidence(runId, after.run, after.bindings, rows, checkpoints),
    coverage: { source: "projection.envelopes.list", rowsRead: rows.length, firstSeq: rows[0]?.seq ?? null,
      lastSeq: rows.at(-1)?.seq ?? null, runProjectionStable, atomic: false, invalidRows: "not_exposed" },
    uncertainties,
  };
}

module.exports = { inspectRun };
