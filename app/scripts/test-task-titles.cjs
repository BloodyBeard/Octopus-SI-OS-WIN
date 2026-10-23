const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const path = require("node:path");
const os = require("node:os");
const { test } = require("node:test");
const Ajv = require("ajv");
const { requestTaskTitle, readTaskTitleTopology } = require("../../registry/interface/world_operator_chat/task-titles.cjs");

test("a task title uses an isolated tool-free run, preserves its model and a concurrent rename, and stops its provider", async () => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-task-title-"));
  const registry = path.join(home, "registry"), source = path.resolve(__dirname, "../../registry/interface");
  const write = async (file, value) => { await fs.mkdir(path.dirname(file), { recursive: true }); await fs.writeFile(file, JSON.stringify(value)); };
  const sourceRecord = { id: "task", interface_state: { client: { provider_runtime_interface: "selected-provider" } } };
  const sourceFile = path.join(home, "state/runs/task.json"), sidecar = path.join(registry, "interface/client/octopus_os.json");
  let request, recordFile, deliveries = 0, concurrentRename = false;
  const rows = [];
  const schema = new Ajv({ strict: false, logger: false }).compile(require("../../schemas/run_record.schema.json"));
  const options = {
    octopusHome: () => home, withRunLock: (_key, fn) => fn(), withTopologyLock: (_home, fn) => fn(),
    assertCurrent() {}, waitForInterface: async () => {},
    readSource: async () => ({ chat: { chat_interface_id: "client", provider_bound_interface_id: "selected-provider", os_root_ee_id: "root" },
      input: { artifact_id: "operator.message:original", content_hash: "01".repeat(32), movement_seq: 4 } }),
    commit: async ({ writes, publishBarrierPath }) => {
      assert.ok(writes.some(row => row.file === publishBarrierPath), "every transaction has its publication barrier");
      for (const row of writes) {
        if (row.next?.id?.startsWith("run_client_title_")) { assert.ok(schema(row.next), JSON.stringify(schema.errors)); recordFile = row.file; }
        if (row.content !== undefined) assert.equal(await fs.readFile(row.file, "utf8"), row.content);
        else await write(row.file, row.next);
      }
    },
    handIn: async ({ interfaceId, manifest }) => {
      deliveries++;
      request = JSON.parse(await fs.readFile(manifest.payload_path));
      assert.notEqual(interfaceId, request.reply_to);
      assert.deepEqual(request.source.content, Array(32).fill(1));
      const record = JSON.parse(await fs.readFile(recordFile));
      assert.equal(JSON.parse(await fs.readFile(path.join(home, "state/ee_instances", `${record.ee}.json`))).parent_ee_id, null);
      const provider = record.checkpoints[0].outbound_interface_bindings[0].interface;
      assert.notEqual(provider, "selected-provider");
      const selected = await fs.readFile(path.join(registry, "interface", provider, "interface.json"), "utf8");
      assert.deepEqual(JSON.parse(selected).interface_driver_config.private_config, { model: "chosen-model", reasoning_effort: "medium", service_tier: "ultrafast" });
      const policy = JSON.parse(await fs.readFile(path.join(registry, "interface", record.worker, "policy.json")));
      assert.deepEqual(policy.ceiling.tools, []); assert.deepEqual(policy.ceiling.wek, []);
      assert.equal(policy.ceiling.interfaces.includes("selected-provider"), false);
      if (concurrentRename) await write(sidecar, { presentation: { name: "My own title", projects: [] } });
      rows.push({ movement_kind: "InterfaceOutbound", payload: { request, result: { ok: true, value: { title: "Починить прокрутку чата" } } } });
      return { admitted: { seq: 5 } };
    },
    readArtifacts: async () => rows,
  };
  try {
    for (const id of ["system_client_maintenance", "system_worker_client_maintenance"]) await fs.cp(path.join(source, id), path.join(registry, "interface", id), { recursive: true });
    await write(path.join(registry, "interface/selected-provider/interface.json"), { id: "selected-provider", copy_of: "world_provider_codex", interface_driver_ref: "codex_app_server",
      interface_driver_config: { private_config: { model: "chosen-model", reasoning_effort: "medium", service_tier: "ultrafast" } } });
    await write(path.join(registry, "interface/selected-provider/life.json"), { processes: [{ bin: "interface://world_provider_codex/packages/bin/provider" }] });
    await write(sourceFile, sourceRecord); await write(sidecar, {});
    const first = await requestTaskTitle(options, { runId: "task" });
    assert.equal(first.title, "Починить прокрутку чата");
    assert.equal(JSON.parse(await fs.readFile(sidecar)).presentation.name, first.title);
    assert.deepEqual(JSON.parse(await fs.readFile(recordFile)).active_checkpoints, []);
    await requestTaskTitle(options, { runId: "task" }); assert.equal(deliveries, 1);
    const topology = await readTaskTitleTopology({ sourceRecord, sourceClientInterface: "client", registryRoot: registry,
      runRecordsRoot: path.join(home, "state/runs"), readDocument: async (file) => ({ document: JSON.parse(await fs.readFile(file)) }) });
    assert.equal(topology.interfaces.size, 4); assert.equal(topology.interfaces.has("selected-provider"), false);
    assert.equal(topology.sourceProvider, "selected-provider");
    await write(sidecar, { presentation: { name: "Manual title", projects: [] } });
    await requestTaskTitle(options, { runId: "task" });
    assert.equal(JSON.parse(await fs.readFile(sidecar)).presentation.name, "Manual title");
    await fs.rm(recordFile); rows.length = 0; concurrentRename = true;
    await write(sidecar, {});
    assert.equal(await requestTaskTitle(options, { runId: "task" }), null);
    assert.equal(JSON.parse(await fs.readFile(sidecar)).presentation.name, "My own title");
    rows[0].payload.result = { ok: true, value: { title: "bad\nname" } };
    await assert.rejects(requestTaskTitle(options, { runId: "task" }), /malformed/);
    await fs.rm(recordFile); rows.length = 0;
    options.handIn = async () => { throw new Error("delivery failed"); };
    await assert.rejects(requestTaskTitle(options, { runId: "task" }), /delivery failed/);
    assert.deepEqual(JSON.parse(await fs.readFile(recordFile)).active_checkpoints, []);
  } finally { await fs.rm(home, { recursive: true, force: true }); }
});
