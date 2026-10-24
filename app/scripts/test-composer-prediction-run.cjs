const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const path = require("node:path");
const os = require("node:os");
const { test } = require("node:test");
const Ajv = require("ajv");
const { requestComposerPrediction } = require("../../registry/interface/world_operator_chat/composer-predictions.cjs");
const lifecycle = require("../../registry/interface/world_operator_chat/maintenance-lifecycle.cjs");

test("client maintenance uses isolated ports, preserves results and rearms only settled unchanged records", async () => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-prediction-run-"));
  const write = async (file, value) => { await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, JSON.stringify(value)); };
  const registry = path.join(home, "registry", "interface");
  const sourcePath = path.join(home, "state/runs/chat.json");
  const source = { id: "chat", interface_state: { client: { provider_runtime_interface: "provider" } } };
  const topology = () => lifecycle.readMaintenanceTaskTopology({ registryRoot: path.dirname(registry),
    runRecordsRoot: path.dirname(sourcePath), sourceRecord: source, sourceClientInterface: "client" });
  const valid = new Ajv({ strict: false, logger: false }).compile(require("../../schemas/run_record.schema.json"));
  let sequence = 1, deliveries = 0, commits = 0, recordPath, failed = false;
  const rows = [];
  const options = {
    octopusHome: () => home, withRunLock: (_id, work) => work(), withTopologyLock: (_home, work) => work(),
    assertCurrent() {}, waitForInterface: async () => {},
    readSource: async () => ({ chat: { provider_bound_interface_id: "provider", os_root_ee_id: "root" },
      terminal: { artifact_id: `answer-${sequence}`, content_hash: "01".repeat(32), movement_seq: sequence } }),
    readArtifacts: async ({ interface_id }) => rows.filter((row) => row.interface_id === interface_id),
    commit: async ({ writes, publishBarrierPath }) => {
      commits++; recordPath = publishBarrierPath;
      assert.ok(writes.every(row => row.file !== sourcePath));
      await write(sourcePath, { ...source, status: "running", active_checkpoints: ["next"] });
      const record = writes.find((row) => row.file === recordPath).next;
      assert.equal(valid(record), true, JSON.stringify(valid.errors));
      assert.equal(record.checkpoints.length, 2);
      assert.notEqual(record.checkpoints[0].interface_binding.interface, record.checkpoints[1].interface_binding.interface);
      assert.deepEqual(record.checkpoints[1].outbound_interface_bindings, []);
      for (const row of writes) {
        if (row.content !== undefined) assert.equal(await fs.readFile(row.file, "utf8"), row.content);
        else await write(row.file, row.next);
      }
    },
    handIn: async ({ manifest, interfaceId }) => {
      deliveries++;
      const request = JSON.parse(await fs.readFile(manifest.payload_path));
      assert.notEqual(request.reply_to, interfaceId);
      rows.push({ interface_id: request.reply_to, movement_kind: "InterfaceOutbound", payload: { request,
        result: failed ? { status: "failed", suggestion: null, error: "provider unavailable" } : { status: "completed", suggestion: "Покажи пример" } } });
      return { admitted: { seq: sequence + 1 } };
    },
  };
  try {
    for (const id of ["system_client_maintenance", "system_worker_client_maintenance"]) {
      await fs.cp(path.join(__dirname, "../../registry/interface", id), path.join(registry, id), { recursive: true });
    }
    const lifePath = path.join(registry, "system_client_maintenance", "life.json");
    const life = JSON.parse(await fs.readFile(lifePath));
    life.processes[0].bin = "owner://packages/bin/octopus-app-server-activation-life";
    await write(lifePath, life);
    await write(path.join(registry, "provider", "interface.json"), { id: "provider", interface_driver_ref: "codex_app_server" });
    await write(sourcePath, source);
    assert.equal((await requestComposerPrediction(options, { runId: "chat" })).text, "Покажи пример");
    const initial = JSON.parse(await fs.readFile(recordPath));
    const eeFile = path.join(home, "state/ee_instances", `${initial.ee}.json`);
    const ee = JSON.parse(await fs.readFile(eeFile));
    assert.equal(ee.parent_ee_id, null);
    await write(eeFile, { ...ee, parent_ee_id: null, config: { custom: true }, policy_narrowing: { allowed_tools: [] } });
    const owned = await topology();
    assert.equal(owned.records.get(initial.id).role, "prediction");
    assert.equal(owned.interfaces.size, 3);
    assert.equal(owned.interfaces.has("provider"), false);
    const clone = JSON.parse(await fs.readFile(path.join(registry, initial.checkpoints[0].interface_binding.interface, "life.json")));
    assert.equal(clone.processes[0].bin, "interface://system_client_maintenance/packages/bin/octopus-app-server-activation-life");
    await requestComposerPrediction(options, { runId: "chat" });
    assert.equal(deliveries, 1); assert.equal(commits, 1);
    sequence = 10; failed = true;
    await assert.rejects(requestComposerPrediction(options, { runId: "chat" }), /provider unavailable/);
    sequence = 20; failed = false;
    await requestComposerPrediction(options, { runId: "chat" });
    const retained = JSON.parse(await fs.readFile(eeFile));
    assert.equal(retained.parent_ee_id, null);
    assert.deepEqual(retained.config, { custom: true });
    assert.deepEqual(retained.policy_narrowing, { allowed_tools: [] });
    assert.equal(commits, 3);
    const record = JSON.parse(await fs.readFile(recordPath));
    assert.equal(record.checkpoints[0].epoch, 2);
    rows.length = 0; sequence = 30;
    assert.equal(await requestComposerPrediction(options, { runId: "chat" }), null);
    assert.equal(commits, 3);
    record.checkpoints[0].artifact_bindings.push({ id: "local-note" });
    await write(recordPath, record);
    await assert.rejects(topology(), /another owner/);
    await assert.rejects(requestComposerPrediction(options, { runId: "chat" }), /another owner/);
    assert.equal(commits, 3);
    await fs.unlink(sourcePath);
    assert.equal(await requestComposerPrediction(options, { runId: "chat" }), null);
    assert.equal(commits, 3);
  } finally { await fs.rm(home, { recursive: true, force: true }); }
});
