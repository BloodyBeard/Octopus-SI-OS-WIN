const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const fsSync = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const vm = require("node:vm");
const { createRequire } = require("node:module");
const test = require("node:test");
const projectionFile = path.join(__dirname, "../electron/projection.cjs");
const appRequire = createRequire(projectionFile);
const { blake3 } = appRequire("@noble/hashes/blake3");
const lifecycle = require("../../registry/interface/world_operator_chat/maintenance-lifecycle.cjs");
const { maintenanceFixture } = require("../../registry/interface/world_operator_chat/test_maintenance_client.cjs");
const projectionAuthoring = require("../electron/clientWorkspaceAuthoring.cjs");
const workspaceSetup = require("../electron/workspaceSetup.cjs");
const { withFixturePublisher, fixtureFilePublisher, fixtureDirectoryPublisher } = require("./fixturePublication.cjs");
const commitProjectionClientAuthoring = (home, registry, request, options = {}) =>
  projectionAuthoring.commitProjectionClientAuthoring(home, registry, request, withFixturePublisher({ octopusHomeRoot: home, ...options }));
const recoverWorkspaceTransactions = (options) => workspaceSetup.recoverWorkspaceTransactions(withFixturePublisher(options));
const loaded = { exports: require("../electron/projection/taskDeletion.cjs") };
const authoringFile = path.join(__dirname, "../electron/workspaceSetup.cjs"), authoring = { exports: {} };
vm.runInNewContext(`${fsSync.readFileSync(authoringFile, "utf8")}\nmodule.exports = {
  normalizeRoots, ensureStateRoots, parseClientAuthoringPlan, stageClientAuthoringTransaction, applyJournalWrite,
  readJournal, writeJournal, retirementContext, applyClientAuthoringRemoval, clientAuthoringReceipt, writeClientAuthoringReceipt };`, {
  Buffer, __dirname: path.dirname(authoringFile), __filename: authoringFile, module: authoring,
  process, require: createRequire(authoringFile), console, structuredClone, setTimeout, clearTimeout,
});
const adapterFile = path.join(__dirname, "../electron/clientWorkspaceAuthoring.cjs"), adapter = { exports: {} };
vm.runInNewContext(fsSync.readFileSync(adapterFile, "utf8"), { module: adapter, require: (id) => id === "./workspaceSetup.cjs"
  ? { commitClientAuthoringTransaction: (options) => options } : createRequire(adapterFile)(id) });
const canonical = (value) => JSON.stringify(value, (_key, entry) => entry && typeof entry === "object" && !Array.isArray(entry)
  ? Object.fromEntries(Object.keys(entry).sort().map((key) => [key, entry[key]])) : entry);
const pin = (value) => Buffer.from(blake3(Buffer.from(canonical(value)))).toString("hex");
const writeJson = async (file, value) => { await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, JSON.stringify(value)); };
const digest = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");

async function proofFor(ctx) {
  const ledger = await fs.readFile(path.join(ctx.roots.ledger_root, "movements.jsonl"));
  return { schema_version: ctx.directory_inventory ? 2 : 1, phase: "held", operation_id: ctx.operation_id, selection_digest: ctx.selection_digest,
    ...(ctx.directory_inventory ? { directory_inventory_digest: digest(Buffer.from(canonical(ctx.directory_inventory))) } : {}),
    interface_ids: [...ctx.selection.interface_ids], originals: ctx.originals.map(({ file, expected_digest }) => ({ file, expected_digest })),
    retired: ctx.retired.map((entry) => ({ ...entry })),
    ledger_boundary: { complete_offset: ledger.length, sequence: 1, sha256: digest(ledger) } };
}

function contractBarrier(overrides = {}) {
  return { verifyBefore: overrides.verifyBefore ?? (async (ctx) => {
    assert.equal(ctx.prior_proof, null);
    for (const original of ctx.originals) assert.equal(digest(await fs.readFile(original.file)), original.expected_digest);
  }), withHeldProof: async (ctx, operation) => {
    for (const retired of ctx.retired) await assert.rejects(fs.stat(retired.file), /ENOENT/);
    let held = true;
    const publication = fixtureDirectoryPublisher(path.dirname(ctx.roots.ledger_root));
    try {
      return await operation({ verify: async () => {
        assert.equal(held, true);
        for (const original of ctx.originals) assert.equal(digest(Buffer.from(original.content)), original.expected_digest);
        return overrides.verify ? overrides.verify(ctx) : proofFor(ctx);
      }, ...Object.fromEntries(["snapshotDirectory", "publishDirectoryMoves", "reclaimDirectory"].map((method) =>
        [method, async (...args) => { assert.equal(held, true); return publication[method](...args); }])) });
    } finally { held = false; }
  } };
}

async function stagedRetiredGroup(f, withProof = false) {
  let request;
  await f.remove({}, async (value) => { request = value; return { cleanupWarnings: [] }; });
  const options = adapter.exports.commitProjectionClientAuthoring(f.root, f.registry, request);
  const api = authoring.exports, roots = await api.normalizeRoots({ ...options,
    publishFiles: fixtureFilePublisher(options.octopusHomeRoot), publication: fixtureDirectoryPublisher(options.octopusHomeRoot) });
  await api.ensureStateRoots(roots);
  const transactionRoot = await api.stageClientAuthoringTransaction(roots, api.parseClientAuthoringPlan(options.plan));
  const journal = await api.readJournal(roots, transactionRoot);
  const before = new Map(await Promise.all(journal.writes.map(async (write) => [write.destination, await fs.readFile(write.destination)])));
  journal.phase = "committing";
  for (const write of [...journal.writes].sort((a, b) => Number(a.publish_barrier) - Number(b.publish_barrier))) {
    if (write.state !== "unchanged") { await api.applyJournalWrite(roots, transactionRoot, journal, journal.writes.indexOf(write)); write.state = "applied"; }
  }
  if (withProof) {
    journal.retirement.proof = await proofFor(await api.retirementContext(roots, transactionRoot, journal));
    journal.phase = "published";
  }
  await api.writeJournal(transactionRoot, journal);
  return { roots, transactionRoot, journal, before };
}

function catalogue(worker, ee) {
  const scope = { ee_id: ee, worker_interface: worker };
  const document = { label: "context.reference", description: "catalogue", text: "accepted working context", collection_catalogue: {
    schema_version: 1, scope, limits: { max_sources: 32, max_entries: 64, max_snapshot_bytes: 65536, max_scan_bytes: 65536 }, sources: [], uninspected_bindings: [],
  } };
  document.id = `artifact-collection-catalogue-${pin(document)}`;
  return { document, adoption: { schema_version: 1, scope, snapshot: { id: document.id, content: Array.from(Buffer.from(pin(document), "hex")) } } };
}

async function adoptCatalogue(f) {
  const captured = catalogue(f.source.worker, "original_ee"), provider = f.source.interface_state.source_provider;
  provider.collection_catalogue_adoption = captured.adoption;
  const input = { id: "catalogue_input", run: f.source.id, epoch: 0, trace: "trace:catalogue_input",
    interface_binding: { interface: "source_client" }, outbound_interface_bindings: [{ interface: "source_provider" }],
    artifact_bindings: [{ id: captured.document.id }], required_verified_artifacts: [] };
  f.source.checkpoints.push(input); f.source.active_checkpoints.push(input.id);
  provider.run_authoring_binding.checkpoints.push({ name: "catalogue_input", role: "stable", include_context: true,
    interface_binding: input.interface_binding, outbound_interface_bindings: input.outbound_interface_bindings,
    static_artifact_bindings: [captured.document.id], required_verified_artifacts: [] });
  const file = path.join(f.registry, "artifact", captured.document.id, "artifact.json");
  await writeJson(file, captured.document); await writeJson(f.sourceFile, f.source);
  return { ...captured, file, before: await fs.readFile(file) };
}

async function fixture(t) {
  const root = await fs.mkdtemp(path.join(await fs.realpath(os.tmpdir()), "octopus-lifecycle-contract-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const registry = path.join(root, "registry"), runs = path.join(root, "state", "runs");
  for (const directory of [registry, runs, path.join(root, "state", "ee_instances"), path.join(registry, "tool"), path.join(registry, "ee_template")]) await fs.mkdir(directory, { recursive: true });
  const plan = maintenanceFixture(registry, runs), source = JSON.parse(plan.run_record_content);
  const d = source.interface_state.source_provider.maintenance_observer;
  const targets = [d, d.projection_run];
  const privateIds = new Set([d.source_relay_interface, d.experience_interface, d.outcome_interface, d.capture_interface]);
  const privateTools = new Set([d.publication_tool]);
  const stage = (kind, id, value, name = `${kind}.json`) => {
    const file = path.join(registry, kind, id, name);
    plan.interface_writes = plan.interface_writes.filter((entry) => entry.path !== file);
    plan.interface_writes.push({ path: file, content: JSON.stringify(value) });
  };
  const writerRecords = [];
  for (const target of targets) {
    const record = target.initial_record, owner = target.provider_interface;
    const topology = record.interface_state[owner].run_authoring_binding;
    const call = topology.checkpoints.find((cp) => cp.role === "authoring_call");
    const result = topology.checkpoints.find((cp) => cp.role === "authoring_result");
    const tool = `file.write.${record.id}`;
    privateTools.add(tool);
    call.required_verified_artifacts = [{ artifact_label: "tool.call", mark: "tool.call.allowed", verifier: "tool_call_allowed", params: { tools: [tool] } }];
    topology.authoring_source_interface = call.interface_binding.interface;
    const binding = topology.writer_run, writer = binding.initial_record;
    binding.role_artifact_bindings = [];
    writer.status = "idle";
    writer.interface_state[owner] = { file_writer_target: { run: record.id, provider: owner,
      source: call.interface_binding.interface, door: result.interface_binding.interface, generation: 0 } };
    for (const [index, cp] of writer.checkpoints.entries()) {
      cp.trace = `trace:${writer.id}:${cp.id}`;
      cp.required_verified_artifacts = structuredClone(index === 0 ? call.required_verified_artifacts : result.required_verified_artifacts);
    }
    record.checkpoints = record.checkpoints.filter((cp) => ![call.interface_binding.interface, result.interface_binding.interface].includes(cp.interface_binding.interface));
    record.active_checkpoints = record.checkpoints.map((cp) => cp.id);
    record.status = "idle";
    record.interface_state[owner].context_generation = 0;
    record.interface_state[owner].authoring_generation = 0;
    target.initial_record_content = `${JSON.stringify(record, null, 2)}\n`;
    for (const cp of topology.checkpoints) {
      privateIds.add(cp.interface_binding.interface);
      for (const to of cp.outbound_interface_bindings) privateIds.add(to.interface);
    }
    privateIds.add(record.worker); privateIds.add(owner);
    stage("interface", record.worker, { id: record.worker, ceiling: {
      interfaces: [...new Set([record.worker, owner, ...topology.checkpoints.flatMap((cp) =>
        [cp.interface_binding.interface, ...cp.outbound_interface_bindings.map((to) => to.interface)])])],
      tools: [tool, ...(target === d ? [d.publication_tool] : [])], wek: [],
    } }, "policy.json");
    stage("tool", tool, { id: tool, interface: result.interface_binding.interface });
    writerRecords.push(writer);
  }
  d.lifecycle_ownership = { schema_version: 1, private_interface_ids: [...privateIds].sort(), private_tool_ids: [...privateTools].sort() };
  source.status = "idle";
  source.interface_state.source_client = { client_surface_role: "task", provider_runtime_interface: "source_provider" };
  for (const write of plan.interface_writes) { await fs.mkdir(path.dirname(write.path), { recursive: true }); await fs.writeFile(write.path, write.content); }
  for (const target of targets) await fs.writeFile(path.join(runs, `${target.run}.json`), target.initial_record_content);
  for (const writer of writerRecords) await writeJson(path.join(runs, `${writer.id}.json`), writer);
  for (const [id, component] of [["source_client", "client"], ["source_worker", "worker"], ["source_provider", "provider"]]) {
    await writeJson(path.join(registry, "interface", id, "octopus_os.json"), { schema_version: 1, workspace: "octopus_os",
      interface: id, owner_interface: "source_client", component, surface_role: "task",
      ...(component === "client" ? { provider_choice_interface: "provider_template", worker_choice_interface: "worker_template" } : {}) });
  }
  const sourceFile = path.join(runs, `${source.id}.json`);
  await writeJson(sourceFile, source);
  const entries = path.join(registry, "artifact", d.collection_source, "entries");
  await fs.mkdir(entries, { recursive: true }); await fs.writeFile(path.join(entries, "published.json"), "published knowledge");
  await fs.mkdir(path.join(root, "ledger", "blobs"), { recursive: true });
  await fs.writeFile(path.join(root, "ledger", "movements.jsonl"), "retained movements\n");
  await fs.writeFile(path.join(root, "ledger", "blobs", "snapshot"), "retained snapshot");
  const readTopology = (record = source) => lifecycle.readMaintenanceTaskTopology({ registryRoot: registry, runRecordsRoot: runs,
    sourceRecord: record, sourceClientInterface: "source_client", readWriterRuns: (target, ids) => loaded.exports.octopusOsOwnedWriterRuns(root, target, ids) });
  const connectedClients = [];
  const remove = async (hooks = {}, commit) => {
    const digest = crypto.createHash("sha256").update(await fs.readFile(sourceFile)).digest("hex");
    const scope = { clientRuns: [{ run_id: source.id, surface_role: "task", record_path: sourceFile,
      record_digest: digest, interface_id: "source_client", ee_id: source.ee }, ...connectedClients], readOnlyRuns: [] };
    return loaded.exports.deleteOctopusOsTask(registry, root, scope, { runId: source.id, expectedDigest: digest }, hooks.releaseTaskWorkerEnvironments ?? null,
      commit ?? ((request) => commitProjectionClientAuthoring(root, registry, request, { retirementBarrier: hooks.retirementBarrier })),
      { taskLifecycle: lifecycle, ...hooks });
  };
  return { root, registry, runs, source, d, sourceFile, entries, readTopology, remove, connectedClients };
}

test("task deletion disconnects idle client outputs atomically and retains authoring context", async (t) => {
  const f = await fixture(t), file = path.join(f.runs, "connected_run.json");
  const edge = { interface_binding: { interface: "connected_provider" }, outbound_interface_bindings: [{ interface: "source_client" }], required_verified_artifacts: [] };
  const peer = { id: "connected_run", ee: f.source.ee, worker: "connected_worker", status: "idle", trace: "trace:connected_run",
    checkpoints: [{ ...edge, id: "send", run: "connected_run", epoch: 0, trace: "trace:send", artifact_bindings: [{ id: "role", access: { read: true } }] }], active_checkpoints: ["send"],
    interface_state: { connected_client: { provider_runtime_interface: "connected_provider" }, connected_provider: {
      frontier_checkpoint_ids: ["send"], context_generation: 12, run_authoring_binding: { checkpoints: [{ ...edge, name: "send", role: "provider_interface_output", include_context: false }] } } } };
  await writeJson(file, peer); f.connectedClients.push({ run_id: peer.id, interface_id: "connected_client" });
  const policyFile = path.join(f.registry, "interface", "connected_worker", "policy.json");
  await writeJson(policyFile, { id: "connected_worker", ceiling: { interfaces: ["source_client", "connected_provider"], tools: [] } });
  const original = await fs.readFile(file);
  const coordinates = { id: "task_coordinates", label: "context.reference", text: "Retained task coordinates",
    authoring: { run: f.source.id, worker: f.source.worker, provider_interface: "source_provider", registry_root: f.registry, run_file: f.sourceFile } };
  const artifact = path.join(f.registry, "artifact", coordinates.id, "artifact.json"); await writeJson(artifact, coordinates);
  f.source.checkpoints[0].artifact_bindings.push({ id: coordinates.id }); await writeJson(f.sourceFile, f.source);
  await fs.mkdir(path.join(f.registry, "tool", ".octopus-publication"), { mode: 0o700 });
  await assert.rejects(f.remove({ readTaskStatus: async () => ({ turn_open: true }) }), /connected task to finish/);
  const hooks = { readTaskStatus: async () => ({ turn_open: false, turn_state: "idle" }), retirementBarrier: contractBarrier() };
  await assert.rejects(f.remove(hooks, request => {
    request.writes.find(write => write.file === file).next.status = "running";
    return commitProjectionClientAuthoring(f.root, f.registry, request, { retirementBarrier: hooks.retirementBarrier });
  }), /more than its exact outgoing connections/);
  await assert.rejects(f.remove({ ...hooks, retirementBarrier: contractBarrier({ verify: async () => { throw new Error("held proof failed"); } }) }), /held proof failed/);
  assert.deepEqual(await fs.readFile(file), original); assert.ok(await fs.stat(f.sourceFile));
  await f.remove(hooks);
  const remaining = JSON.parse(await fs.readFile(file));
  assert.deepEqual(remaining.checkpoints, []); assert.deepEqual(remaining.active_checkpoints, []);
  assert.equal(remaining.interface_state.connected_provider.context_generation, 12);
  assert.deepEqual(JSON.parse(await fs.readFile(policyFile)).ceiling.interfaces, ["connected_provider"]);
  assert.deepEqual(JSON.parse(await fs.readFile(artifact)), coordinates);
});

test("exact companion inventory includes writers; rejected inventory and writer drift preserve records", async (t) => {
  const f = await fixture(t), topology = await f.readTopology();
  assert.equal(topology.records.size, 4);
  for (const change of [(d) => d.lifecycle_ownership.private_interface_ids.push("source_worker"),
    (d) => d.lifecycle_ownership.private_tool_ids.pop(), (d) => d.collection_entries_root = f.registry,
    (d) => d.source_client_interface = "foreign_client"]) {
    const source = structuredClone(f.source); change(source.interface_state.source_provider.maintenance_observer);
    await assert.rejects(f.readTopology(source), /inventory|collection root|exact source/);
  }
  const privateDoor = path.join(f.registry, "interface", "publication_door"), savedDoor = `${privateDoor}_saved`;
  await fs.rename(privateDoor, savedDoor); await fs.symlink(savedDoor, privateDoor);
  await assert.rejects(f.readTopology(), /symbolic link/);
  await fs.unlink(privateDoor); await fs.rename(savedDoor, privateDoor);
  const writerFile = path.join(f.runs, "observer_run_writer.json"), writer = JSON.parse(await fs.readFile(writerFile));
  writer.ee = "another_ee"; await writeJson(writerFile, writer);
  await assert.rejects(f.readTopology(), /declared generation/);
  assert.equal(JSON.parse(await fs.readFile(f.sourceFile)).id, f.source.id);
});

test("catalogue capture scope is immutable provenance; unknown or changed content remains refused", async () => {
  const { document: value, adoption } = catalogue("source_worker", "old_ee");
  const before = canonical(value), dependency = lifecycle.catalogueProvenanceProjection(value, adoption, pin);
  assert.equal(dependency.collection_catalogue.scope.worker_interface, undefined);
  assert.equal(dependency.collection_catalogue.scope.ee_id, "old_ee"); assert.equal(canonical(value), before);
  for (const changed of [{ ...value, unknown: "source_worker" }, { ...value, text: "changed" }]) {
    assert.throws(() => lifecycle.catalogueProvenanceProjection(changed, adoption, pin), /unknown fields|content pin/);
  }
});

test("busy or unavailable retirement refuses before cleanup; idle alone authorizes nothing", async (t) => {
  const f = await fixture(t), observerFile = path.join(f.runs, "observer_run.json");
  await assert.rejects(f.remove(), { code: "task_retirement_unavailable" });
  const observer = JSON.parse(await fs.readFile(observerFile));
  let called = false;
  for (const pending of [{ terminal: false }, { terminal: true, context_publication: { writes: [] } }]) {
    observer.interface_state.observer_provider.pending_handoff = pending; await writeJson(observerFile, observer);
    await assert.rejects(f.remove({ retirementBarrier: contractBarrier({ verifyBefore: async () => { called = true; } }) }), { code: "maintenance_effect_unresolved" });
  }
  assert.equal(called, false); assert.ok(await fs.stat(f.sourceFile)); assert.ok(await fs.stat(observerFile));
});

test("settled group deletion uses held owner hook and existing transaction; shared evidence survives", async (t) => {
  const f = await fixture(t), captured = await adoptCatalogue(f);
  const prediction = await require("./support/composer-prediction-fixture.cjs").predictionFixture(f.root, f.source.id, "source_provider");
  const consumer = path.join(f.runs, "snapshot_consumer.json");
  await writeJson(consumer, { id: "snapshot_consumer", worker: "other_worker", checkpoints: [{
    interface_binding: { interface: "other_provider" }, artifact_bindings: [{ id: captured.document.id }] }] });
  let checks = 0;
  const outcome = await f.remove({ retirementBarrier: contractBarrier({ verify: async (ctx) => {
    assert.equal(ctx.selection.records.length, 6);
    assert.equal(ctx.selection.records.find(row => row.run === prediction.id).role, "prediction");
    assert.equal(Boolean(ctx.prior_proof), checks++ > 0);
    return proofFor(ctx);
  } }) });
  assert.ok(checks > 3);
  assert.deepEqual(Array.from(outcome.removedRuns).sort(), ["source_run", "observer_run", "observer_run_writer", "projection_run", "projection_run_writer", prediction.id].sort());
  for (const id of outcome.removedRuns) await assert.rejects(fs.stat(path.join(f.runs, `${id}.json`)), /ENOENT/);
  assert.equal(await fs.readFile(path.join(f.entries, "published.json"), "utf8"), "published knowledge");
  assert.equal(await fs.readFile(path.join(f.root, "ledger", "movements.jsonl"), "utf8"), "retained movements\n");
  assert.equal(await fs.readFile(path.join(f.root, "ledger", "blobs", "snapshot"), "utf8"), "retained snapshot");
  assert.deepEqual(await fs.readFile(captured.file), captured.before); assert.ok(await fs.stat(consumer));
});

test("current record EE may differ from immutable catalogue capture; changed worker, payload or read binding refuses", async (t) => {
  const f = await fixture(t), captured = await adoptCatalogue(f);
  const read = (record) => lifecycle.readTaskCatalogueEvidence({ registryRoot: f.registry, records: [record], canonicalContentHash: pin });
  assert.equal((await read(f.source)).get(captured.file).document.collection_catalogue.scope.ee_id, "original_ee");
  for (const mutate of [(record) => record.worker = "different_worker", (record) =>
    record.interface_state.source_provider.collection_catalogue_adoption.scope.ee_id = "different_capture", (record) =>
    record.checkpoints.at(-1).artifact_bindings[0].access = { read: false }]) {
    const changed = structuredClone(f.source); mutate(changed);
    await assert.rejects(read(changed), /task worker pin|accepted capture|readable provider binding/);
  }
  assert.deepEqual(await fs.readFile(captured.file), captured.before);
});

test("failed held settlement rolls back exact retired Run bytes and preserves all private directories", async (t) => {
  const f = await fixture(t);
  const originals = new Map(await Promise.all((await fs.readdir(f.runs)).map(async (name) => [name, await fs.readFile(path.join(f.runs, name))])));
  await assert.rejects(f.remove({ retirementBarrier: contractBarrier({ verify: async () => {
    throw new Error("accepted tool effect is still InFlight");
  } }) }), /effect is still InFlight/);
  for (const [name, bytes] of originals) assert.deepEqual(await fs.readFile(path.join(f.runs, name)), bytes);
  for (const id of (await f.readTopology()).interfaces) assert.ok(await fs.stat(path.join(f.registry, "interface", id)));
  assert.equal(await fs.readFile(path.join(f.entries, "published.json"), "utf8"), "published knowledge");
});

test("companion worker environments require their own formation and release obligation", async (t) => {
  const f = await fixture(t), we = `we_${"1".repeat(32)}`;
  const file = path.join(f.root, "interfaces", "publication_door", "worker-environments", we, "worker_environment.json");
  const record = { schema_version: 5, lifecycle: "active", formation: { worker_environment_id: we, door_interface: "publication_door",
    run: "observer_run", worker: "observer_worker", ee: f.source.ee, requires_release: true } };
  const retirementBarrier = contractBarrier();
  await writeJson(file, record);
  await assert.rejects(f.remove({ retirementBarrier }), /before worker environment .* is released/);
  record.lifecycle = "released"; record.formation.worker = "source_worker"; await writeJson(file, record);
  await assert.rejects(f.remove({ retirementBarrier }), /does not belong/);
  record.formation.worker = "observer_worker"; await writeJson(file, record);
  const outcome = await f.remove({ retirementBarrier });
  assert.ok(outcome.removedRuns.includes("observer_run")); await assert.rejects(fs.stat(file), /ENOENT/);
});

test("foreign live references and uncertain owner proof cannot disappear into companion ownership", async (t) => {
  const f = await fixture(t), foreign = path.join(f.runs, "unrelated.json");
  await writeJson(foreign, { id: "unrelated", worker: "other_worker", checkpoints: [{ interface_binding: { interface: "projector" } }] });
  const retirementBarrier = contractBarrier();
  await assert.rejects(f.remove({ retirementBarrier }), /also bound by run/);
  await fs.unlink(foreign);
  const rogue = path.join(f.registry, "tool", "undeclared.tool", "tool.json");
  await writeJson(rogue, { id: "undeclared.tool", interface: "publication_door" });
  await assert.rejects(f.remove({ retirementBarrier }), /without declared ownership/);
  await fs.rm(path.dirname(rogue), { recursive: true });
  await assert.rejects(f.remove({ retirementBarrier: contractBarrier({ verifyBefore: async () => {
    throw new Error("ordered writer outcome remains uncertain");
  } }) }), /outcome remains uncertain/);
  assert.ok(await fs.stat(f.sourceFile)); assert.equal(await fs.readFile(path.join(f.entries, "published.json"), "utf8"), "published knowledge");
});

test("empty or preflight-only final proofs cannot authorize cleanup", async (t) => {
  for (const value of [undefined, { schema_version: 1, phase: "preflight" }]) {
    const f = await fixture(t), before = await fs.readFile(f.sourceFile);
    await assert.rejects(f.remove({ retirementBarrier: contractBarrier({ verify: async () => value }) }), /held proof/);
    assert.deepEqual(await fs.readFile(f.sourceFile), before); assert.equal((await f.readTopology()).records.size, 4);
    assert.deepEqual(await fs.readdir(path.join(f.root, "client-state/workspace/transactions")), []);
  }
});

test("crash after source publication barrier without held proof rolls back through the existing journal", async (t) => {
  const f = await fixture(t), staged = await stagedRetiredGroup(f);
  await assert.rejects(fs.stat(f.sourceFile), /ENOENT/);
  assert.equal(staged.journal.writes.find((write) => write.publish_barrier).state, "applied");
  assert.equal(staged.journal.retirement.proof, null);
  assert.equal((await recoverWorkspaceTransactions({ octopusHomeRoot: f.root, registryRoot: f.registry })).recovered, 1);
  for (const [file, bytes] of staged.before) assert.deepEqual(await fs.readFile(file), bytes);
  assert.equal((await f.readTopology()).records.size, 4);
  assert.deepEqual(await fs.readdir(staged.roots.transactionsRoot), []);
});

test("changed live Run bytes or corrupt saved originals stop rollback without overwriting state", async (t) => {
  for (const corruptOriginal of [false, true]) {
    const f = await fixture(t), staged = await stagedRetiredGroup(f);
    const write = staged.journal.writes.find((entry) => entry.destination === f.sourceFile);
    const changed = Buffer.from(JSON.stringify({ ...f.source, status: "running" }));
    const file = corruptOriginal ? path.join(staged.transactionRoot, write.before_file) : f.sourceFile;
    await fs.writeFile(file, changed);
    await assert.rejects(recoverWorkspaceTransactions({ octopusHomeRoot: f.root, registryRoot: f.registry }), /changed bytes|snapshot .* corrupt/);
    assert.deepEqual(await fs.readFile(file), changed);
    assert.ok(await fs.stat(path.join(staged.transactionRoot, "journal.json")));
    for (const id of f.d.lifecycle_ownership.private_interface_ids) assert.ok(await fs.stat(path.join(f.registry, "interface", id)));
    await assert.rejects(fs.stat(path.join(f.runs, "observer_run.json")), /ENOENT/);
  }
});

test("dead helper after durable proof refuses the next removal; recovery must reacquire ownership", async (t) => {
  const f = await fixture(t);
  let checks = 0;
  await assert.rejects(f.remove({ retirementBarrier: contractBarrier({ verify: async (ctx) => {
    if (++checks > 1) throw new Error("held native helper has died");
    return proofFor(ctx);
  } }) }), /helper has died/);
  await assert.rejects(fs.stat(f.sourceFile), /ENOENT/);
  for (const id of f.d.lifecycle_ownership.private_interface_ids) assert.ok(await fs.stat(path.join(f.registry, "interface", id)));
  const options = { octopusHomeRoot: f.root, registryRoot: f.registry };
  await assert.rejects(recoverWorkspaceTransactions(options), /live preflight and held ownership barrier/);
  let heldChecks = 0;
  await recoverWorkspaceTransactions({ ...options, retirementBarrier: contractBarrier({ verifyBefore: async () => {
    throw new Error("recovery must use durable prior proof");
  }, verify: async (ctx) => {
    assert.ok(ctx.prior_proof); heldChecks += 1; return proofFor(ctx);
  } }) });
  assert.ok(heldChecks > 1); assert.equal(await fs.readFile(path.join(f.entries, "published.json"), "utf8"), "published knowledge");
});

test("recovery after descriptor tombstoning uses pinned originals and prior proof; failed tail proof preserves the journal", async (t) => {
  const f = await fixture(t), staged = await stagedRetiredGroup(f, true);
  const removal = staged.journal.removals.find((entry) => entry.destination === path.join(f.registry, "interface", f.d.provider_interface));
  await authoring.exports.applyClientAuthoringRemoval(staged.roots, staged.transactionRoot, removal, staged.journal);
  removal.state = "moved"; await authoring.exports.writeJournal(staged.transactionRoot, staged.journal);
  const options = { octopusHomeRoot: f.root, registryRoot: f.registry };
  await assert.rejects(recoverWorkspaceTransactions({ ...options, retirementBarrier: contractBarrier({ verify: async () => {
    throw new Error("selected Run has a new affected ledger tail");
  } }) }), /affected ledger tail/);
  assert.ok(await fs.stat(path.join(staged.transactionRoot, removal.tombstone)));
  assert.ok(await fs.stat(path.join(f.registry, "interface", f.d.projection_run.provider_interface)));
  let checks = 0;
  await recoverWorkspaceTransactions({ ...options, retirementBarrier: contractBarrier({ verify: async (ctx) => {
    assert.ok(ctx.prior_proof.ledger_boundary.sha256); assert.equal(ctx.originals.length, 5);
    await assert.rejects(fs.stat(removal.destination), /ENOENT/); checks += 1; return proofFor(ctx);
  } }) });
  assert.ok(checks > 1); assert.equal(await fs.readFile(path.join(f.root, "ledger/movements.jsonl"), "utf8"), "retained movements\n");
});

test("a cleaned journal renamed for cleanup without its native plan fails closed and keeps private state", async (t) => {
  const f = await fixture(t), staged = await stagedRetiredGroup(f, true);
  for (const removal of staged.journal.removals) {
    await authoring.exports.applyClientAuthoringRemoval(staged.roots, staged.transactionRoot, removal, staged.journal);
    removal.state = "moved";
    await fs.rm(path.join(staged.transactionRoot, removal.tombstone), { recursive: true });
  }
  staged.journal.phase = "cleaned";
  await authoring.exports.writeJournal(staged.transactionRoot, staged.journal);
  await authoring.exports.writeClientAuthoringReceipt(staged.roots, authoring.exports.clientAuthoringReceipt(staged.journal));
  const cleanupRoot = path.join(staged.roots.transactionsRoot, `.cleanup-${staged.journal.operation_id}`);
  await fs.rename(staged.transactionRoot, cleanupRoot);
  await fs.unlink(path.join(cleanupRoot, staged.journal.writes.find((write) => write.before_file).before_file));
  await assert.rejects(recoverWorkspaceTransactions({ octopusHomeRoot: f.root, registryRoot: f.registry }),
    (error) => error.code === "workspace_directory_publication_uncertain");
  assert.deepEqual(await fs.readdir(staged.roots.transactionsRoot), [path.basename(cleanupRoot)]);
  assert.equal(await fs.readFile(path.join(f.entries, "published.json"), "utf8"), "published knowledge");
});

test("a rollback written outside native publication is refused while ordinary Run activity advances", async (t) => {
  const f = await fixture(t), staged = await stagedRetiredGroup(f);
  for (const write of staged.journal.writes) {
    await fs.writeFile(write.destination, staged.before.get(write.destination));
    if (write.state !== "unchanged") write.state = "rolled_back";
  }
  staged.journal.phase = "rolled_back";
  await authoring.exports.writeJournal(staged.transactionRoot, staged.journal);
  const cleanupRoot = path.join(staged.roots.transactionsRoot, `.rollback-${staged.journal.operation_id}`);
  await fs.rename(staged.transactionRoot, cleanupRoot);
  await fs.unlink(path.join(cleanupRoot, staged.journal.writes.find((write) => write.before_file).before_file));
  const advanced = Buffer.from(JSON.stringify({ ...f.source, status: "running" }));
  await fs.writeFile(f.sourceFile, advanced);
  await assert.rejects(recoverWorkspaceTransactions({ octopusHomeRoot: f.root, registryRoot: f.registry }), /exact publication evidence/);
  assert.deepEqual(await fs.readFile(f.sourceFile), advanced);
  assert.deepEqual(await fs.readdir(staged.roots.transactionsRoot), [path.basename(cleanupRoot)]);
  assert.equal((await f.readTopology()).records.size, 4);
});

test("ordinary source release completes before retirement preflight and final source preimage is pinned", async (t) => {
  const f = await fixture(t);
  let released = false;
  await f.remove({ releaseTaskWorkerEnvironments: async (request) => {
    assert.equal(released, false); assert.equal(request.runId, f.source.id);
    const source = JSON.parse(await fs.readFile(f.sourceFile)); source.status = "running";
    await writeJson(f.sourceFile, source); released = true;
    return { runId: source.id, expectedDigest: digest(await fs.readFile(f.sourceFile)), releasedWorkerEnvironmentIds: [] };
  }, retirementBarrier: contractBarrier({ verifyBefore: async (ctx) => {
    assert.equal(released, true);
    const source = ctx.originals.find((entry) => entry.file === f.sourceFile);
    assert.equal(JSON.parse(source.content).status, "running"); assert.equal(digest(await fs.readFile(f.sourceFile)), source.expected_digest);
  } }) });
  assert.equal(released, true);
});

test("holder lifetime failure after callback completion propagates through the transaction caller", async (t) => {
  const f = await fixture(t), barrier = contractBarrier();
  await assert.rejects(f.remove({ retirementBarrier: { ...barrier, withHeldProof: async (ctx, operation) => {
    await barrier.withHeldProof(ctx, operation);
    throw new Error("native holder exited unexpectedly");
  } } }), /holder exited unexpectedly/);
  await assert.rejects(fs.stat(f.sourceFile), /ENOENT/);
  assert.equal(await fs.readFile(path.join(f.entries, "published.json"), "utf8"), "published knowledge");
  assert.equal(await fs.readFile(path.join(f.root, "ledger/movements.jsonl"), "utf8"), "retained movements\n");
});
