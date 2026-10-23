module.exports = { disconnectedTaskRecord, taskConnectionWrites, taskCoordinateEvidence };

const path = require("node:path");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, stableJson } = require("./jsonFiles.cjs");
const { listDirectFiles } = require("./providerDescriptors.cjs");
const { arrayValue, interfaceBindingId, isRunRecordJsonPath } = require("./values.cjs");

function disconnectedTaskRecord(run, removedInterfaces) {
  const rows = arrayValue(run.checkpoints).filter(cp => arrayValue(cp.outbound_interface_bindings)
    .some(binding => removedInterfaces.has(interfaceBindingId(binding))));
  if (!rows.length) throw new Error("Run has no declared task connection to remove");
  const clients = Object.values(run.interface_state ?? {}).filter(state => state?.provider_runtime_interface);
  const provider = clients.length === 1 ? clients[0].provider_runtime_interface : null;
  const state = run.interface_state?.[provider], topology = state?.run_authoring_binding;
  if (!provider || removedInterfaces.has(provider) || removedInterfaces.has(run.worker) || !Array.isArray(topology?.checkpoints)) {
    throw new Error(`run '${run.id}' has no exact client-owned output route`);
  }
  const names = new Set(), ids = new Set();
  for (const row of rows) {
    if (interfaceBindingId(row.interface_binding) !== provider || row.outbound_interface_bindings.length !== 1
      || arrayValue(row.required_verified_artifacts).length
      || arrayValue(row.artifact_bindings).some(binding => ["change", "create", "delete"].some(verb => binding.access?.[verb] === true))) {
      throw new Error(`run '${run.id}' still requires the task's output`);
    }
    const templates = topology.checkpoints.filter(cp => cp.role === "provider_interface_output" && cp.include_context === false
      && !arrayValue(cp.required_verified_artifacts).length && !arrayValue(cp.static_artifact_bindings).length
      && stableJson(cp.interface_binding) === stableJson(row.interface_binding)
      && stableJson(cp.outbound_interface_bindings) === stableJson(row.outbound_interface_bindings));
    if (templates.length !== 1 || ids.has(row.id) || names.has(templates[0].name)) {
      throw new Error(`run '${run.id}' has ambiguous task connection ownership`);
    }
    ids.add(row.id); names.add(templates[0].name);
  }
  const next = structuredClone(run);
  next.checkpoints = next.checkpoints.filter(cp => !ids.has(cp.id));
  next.active_checkpoints = next.active_checkpoints.filter(id => !ids.has(id));
  const nextState = next.interface_state[provider];
  nextState.run_authoring_binding.checkpoints = topology.checkpoints.filter(cp => !names.has(cp.name));
  for (const [owner, privateState] of Object.entries(next.interface_state)) {
    for (const field of ["owned_checkpoint_ids", "frontier_checkpoint_ids", "cycle_checkpoint_ids"]) {
      if (!arrayValue(privateState[field]).some(id => ids.has(id))) continue;
      if (owner !== provider) throw new Error(`run '${run.id}' has another owner for its task connection`);
      privateState[field] = privateState[field].filter(id => !ids.has(id));
    }
  }
  return { next, provider };
}

async function taskConnectionWrites(home, scope, removedRuns, removedInterfaces, hooks) {
  const writes = [];
  for (const file of await listDirectFiles(path.join(home, "state/runs"), isRunRecordJsonPath)) {
    const current = await readRegularJsonDocument(file, "connected task");
    const run = current.document;
    if (removedRuns.has(run.id)) continue;
    if (!arrayValue(run.checkpoints).some(cp => arrayValue(cp.outbound_interface_bindings)
      .some(binding => removedInterfaces.has(interfaceBindingId(binding))))) continue;
    const { next, provider } = disconnectedTaskRecord(run, removedInterfaces);
    const projected = scope.clientRuns.find(row => row.run_id === run.id);
    if (!projected || run.interface_state?.[projected.interface_id]?.provider_runtime_interface !== provider) {
      throw new Error(`task is still connected to run '${run.id}' outside the client scope`);
    }
    if (typeof hooks.readTaskStatus !== "function") throw new Error("Connected task status is unavailable");
    const status = await hooks.readTaskStatus(run.id), state = run.interface_state[provider];
    if (!status || status.turn_open || ["working", "waiting"].includes(status.turn_state)
      || state.pending_handoff && state.pending_handoff.terminal !== true) {
      throw new Error("Wait for the connected task to finish before deleting this task.");
    }
    writes.push({ file, beforeDigest: current.digest, next });
  }
  return writes;
}

async function taskCoordinateEvidence(registryRoot, targets) {
  const evidence = new Map();
  for (const { document: run, interfaces } of targets) {
    const bindings = new Set(arrayValue(run.checkpoints).flatMap(cp => arrayValue(cp.artifact_bindings).map(binding => binding.id)));
    for (const id of bindings) {
      if (typeof id !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(id)) continue;
      const file = path.join(registryRoot, "artifact", id, "artifact.json");
      const current = await readOptionalRegularJsonDocument(file, "task context artifact");
      const d = current?.document, coordinates = d?.authoring;
      if (d?.label !== "context.reference" || coordinates?.run !== run.id) continue;
      if (d.id !== id || coordinates.worker !== run.worker || !interfaces.has(coordinates.provider_interface)
        || coordinates.registry_root !== registryRoot || coordinates.run_file !== path.join(path.dirname(registryRoot), "state/runs", `${run.id}.json`)) {
        throw new Error(`task context artifact '${id}' has changed authoring coordinates`);
      }
      evidence.set(file, { ...current, projection: { ...d, authoring: null } });
    }
  }
  return evidence;
}
