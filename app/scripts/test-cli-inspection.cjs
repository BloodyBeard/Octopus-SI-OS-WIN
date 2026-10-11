const assert = require("node:assert/strict");
const { test } = require("node:test");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFileSync } = require("node:child_process");
const Ajv = require("ajv");
const { inspectRun } = require("../cli/inspection.cjs");
const { projectionReadRequest } = require("../electron/projectionReadContract.cjs");

const ajv = new Ajv({ strict: false, logger: false });
const validateMovement = ajv.compile(require("../../schemas/movement.schema.json"));
const validateRun = ajv.compile(require("../../schemas/run_record.schema.json"));
const hash = byte => Array(32).fill(byte);
const required = ["first", "second"].map(label => ({ mark: `observed.${label}`, artifact_label: label,
  verifier: "artifact_exists", params: {} }));
const runtime = { run: "receiver", checkpoint: "wait", epoch: 3, ee: "ee_test", worker: "system_worker_receiver",
  interface: "system_worker_relay", run_record_hash: "a".repeat(64) };
const projection = () => ({ run: { run_id: "receiver", worker: runtime.worker, ee_id: runtime.ee, source: "run_record" },
  checkpoints: [{ checkpoint_id: "wait", epoch: 3, interface: runtime.interface, status: "projected",
    required_verified_artifacts: required.map(item => ({ ...item, outcome: "closed" })) }],
  bindings: [{ run_id: "receiver", checkpoint_id: "wait", epoch: 3, interface_id: runtime.interface,
    status: "projected", direction: "inbound" }] });
function arrival(seq, label = "first", coordinates = {}) {
  return { seq, lamport: seq + 1, kind: "InterfaceArrival", trace: "trace:wait", recorded_at_unix_ms: seq,
    from: { Interface: runtime.interface }, to: { Interface: runtime.worker }, artifact: { id: `artifact:${label}`, content: hash(1) },
    invocation_metadata: { runtime: { ...runtime, ...coordinates }, required_verified_artifacts: structuredClone(required),
      checkpoint_admission: { artifact_label: label, required_artifact_labels: ["first", "second"], complete: label === "second" } } };
}
function admitted(input, seq = input.seq + 1) {
  const provenance = input.invocation_metadata.runtime, label = input.invocation_metadata.checkpoint_admission.artifact_label;
  return { ...structuredClone(input), seq, lamport: seq + 1, kind: { MarkOutcomeAdmitted: `observed.${label}` },
    invocation_metadata: { name: `observed.${label}`, subject: provenance.checkpoint, run: provenance.run, run_epoch: provenance.epoch,
      class: "success", evidence: structuredClone(input.artifact), verifier: "artifact_exists",
      verifier_package: { descriptor_content: hash(2), script_content: hash(3) } } };
}
function client(rows, initial = projection(), final = initial) {
  let count = 0;
  return { async read(method, params) {
    projectionReadRequest(method, params);
    if (method === "projection.runs.get") {
      assert.deepEqual(params, { run_id: "receiver", include_ledger: false });
      return structuredClone(count++ ? final : initial);
    }
    assert.equal(method, "projection.envelopes.list");
    assert.deepEqual(params, {});
    return { envelopes: rows.map(row => {
      assert.equal(validateMovement(row), true, JSON.stringify(validateMovement.errors));
      return { ...structuredClone(row), kind: typeof row.kind === "string" ? row.kind : `MarkOutcomeAdmitted(${row.kind.MarkOutcomeAdmitted})` };
    }) };
  } };
}

test("AND waits use exact recorded mark evidence, ignoring optimistic aggregate outcome and unverified arrivals", async () => {
  const first = arrival(10), second = arrival(15, "second");
  const failure = { ...structuredClone(second), seq: 16, kind: "VerificationFailed" };
  failure.invocation_metadata.verification_failure = { error: "payload unavailable", mark: "observed.second", artifact_label: "second" };
  const rows = [first, admitted(first), second, failure];
  const result = await inspectRun(client(rows), "receiver");
  assert.deepEqual(result.outstanding.map(item => item.artifactLabel), ["second"]);
  assert.equal(result.checkpoints[0].arrivals.length, 2);
  assert.equal(result.checkpoints[0].failures[0].verificationFailure.error, "payload unavailable");
  assert.deepEqual(result.checkpoints[0].admittedProofs[0].verifierPackage,
    { descriptorContent: "02".repeat(32), scriptContent: "03".repeat(32) });
  const restored = await inspectRun(client([...rows, admitted(second, 17)]), "receiver");
  assert.equal(restored.outstanding.length, 0);
  assert.equal(restored.checkpoints[0].runtimeCompletion, "not_evaluated");
});

test("another run, checkpoint or epoch cannot satisfy the current requirement", async () => {
  for (const coordinates of [{ epoch: 2 }, { checkpoint: "old_wait" }, { run: "producer" }]) {
    const input = arrival(10, "first", coordinates);
    const result = await inspectRun(client([input, admitted(input)]), "receiver");
    assert.equal(result.outstanding.length, 2);
    assert.equal(result.checkpoints[0].admittedProofs.length, 0);
  }
});

test("incomplete, mismatched, noncausal and ambiguous proof records stay outstanding", async () => {
  const input = arrival(10);
  const variants = [mark => { delete mark.invocation_metadata.verifier_package; },
    mark => { mark.invocation_metadata.evidence.content = hash(4); }, mark => { mark.trace = "other"; },
    mark => { mark.invocation_metadata.verifier = "other_verifier"; }, mark => { mark.lamport = input.lamport; },
    mark => { mark.from.Interface = "wrong_pin"; }];
  for (const change of variants) {
    const mark = admitted(input); change(mark);
    const result = await inspectRun(client([input, mark]), "receiver");
    assert.equal(result.outstanding.length, 2);
    assert.ok(result.uncertainties.some(item => /mark/.test(item.code)));
  }
  for (const rows of [[input, { ...input, seq: 11 }, admitted(input, 12)],
    [input, admitted(input, 11), admitted(input, 12)]]) {
    const result = await inspectRun(client(rows), "receiver");
    assert.equal(result.outstanding.length, 2);
  }
});

test("the same mark name on separate labels does not let one proof satisfy both", async () => {
  const current = projection(), input = arrival(10);
  current.checkpoints[0].required_verified_artifacts[1].mark = "observed.first";
  input.invocation_metadata.required_verified_artifacts[1].mark = "observed.first";
  const result = await inspectRun(client([input, admitted(input)], current), "receiver");
  assert.deepEqual(result.outstanding.map(item => item.artifactLabel), ["second"]);
});

test("catch-up remains a candidate and uses exact current pins, source sequence and artifact references", async () => {
  const outbound = { ...arrival(4), kind: "InterfaceOutbound", from: { Interface: "world_sensor" },
    to: { Interface: runtime.interface }, invocation_metadata: { runtime: { ...runtime, run: "producer", checkpoint: "emit" } } };
  const input = arrival(10); input.invocation_metadata.source_movement_seq = 4;
  const refusal = { ...structuredClone(input), seq: 9, kind: "DeliveryRefused",
    invocation_metadata: { runtime, source_movement_seq: 4, delivery_verdict: { refused: { reason: "policy", detail: "denied" } } } };
  let result = await inspectRun(client([outbound, refusal, input]), "receiver");
  assert.equal(result.relay.candidates.length, 1);
  assert.equal(result.relay.candidates[0].observation, "arrival_without_recorded_mark");
  assert.equal(result.relay.candidates[0].failures.length, 1);
  assert.equal(result.relay.candidates[0].queued, null);
  assert.equal(result.relay.candidates[0].replayAdmission, "not_evaluated");
  result = await inspectRun(client([outbound, input, admitted(input)]), "receiver");
  assert.equal(result.relay.candidates[0].admittedProofs.length, 1);
  input.invocation_metadata.source_movement_seq = 3;
  result = await inspectRun(client([outbound, input, admitted(input)]), "receiver");
  assert.equal(result.relay.candidates[0].arrivals.length, 0);
  const changed = projection(); changed.bindings[0].interface_id = "other_pin";
  assert.equal((await inspectRun(client([outbound], changed), "receiver")).relay.candidates.length, 0);
});

test("contract changes and missing generations are explicit uncertainty, never an epoch-zero assumption", async () => {
  const before = projection(), after = projection(), input = arrival(10);
  after.checkpoints[0].required_verified_artifacts[0].params = { extra_check: true };
  const result = await inspectRun(client([input, admitted(input)], before, after), "receiver");
  assert.equal(result.outstanding.length, 2);
  assert.equal(result.coverage.runProjectionStable, false);
  assert.ok(result.uncertainties.some(item => item.code === "checkpoint_contract_changed_within_generation"));
  delete after.checkpoints[0].epoch;
  assert.equal((await inspectRun(client([], after), "receiver")).outstanding[0].state, "unknown");
  await assert.rejects(inspectRun(client([input, input]), "receiver"), /duplicate sequence/);
  await assert.rejects(inspectRun({ read: async () => ({}) }, "receiver"), /Incomplete run projection/);
});

test("package example emits schema-valid initial and repair records without creating selected files", () => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "observer-repair-contract-")));
  try {
    const source = path.resolve(__dirname, "../../registry/interface/world_file_observer/examples/binding_repair.py");
    const example = JSON.parse(execFileSync("python3", ["-B", source, "--read-root", root], { encoding: "utf8" }));
    for (const phase of [example.initial, example.repair]) {
      for (const [name, record] of Object.entries(phase)) if (name.startsWith("runs/")) {
        assert.equal(validateRun(record), true, JSON.stringify(validateRun.errors));
      }
    }
    const receiver = "runs/run_file_repair_receiver.json";
    const repaired = structuredClone(example.repair[receiver]);
    assert.equal(repaired.checkpoints[0].interface_binding.interface, example.demonstration.relay_interface);
    repaired.checkpoints[0].interface_binding = example.initial[receiver].checkpoints[0].interface_binding;
    assert.deepEqual(repaired, example.initial[receiver]);
    assert.deepEqual(fs.readdirSync(root), []);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
