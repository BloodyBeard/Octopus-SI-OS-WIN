const fs = require("node:fs/promises");
const path = require("node:path");
const { requestComposerPrediction } = require("../../../registry/interface/world_operator_chat/composer-predictions.cjs");

async function predictionFixture(home, runId, provider, role = "prediction") {
  const registry = path.join(home, "registry/interface"), source = path.resolve(__dirname, "../../../registry/interface");
  for (const id of ["system_client_maintenance", "system_worker_client_maintenance"]) {
    await fs.cp(path.join(source, id), path.join(registry, id), { recursive: true });
  }
  await fs.mkdir(path.join(registry, "world_operator_chat"), { recursive: true });
  await fs.copyFile(path.join(source, "world_operator_chat/composer-predictions.json"), path.join(registry, "world_operator_chat/composer-predictions.json"));
  await fs.copyFile(path.join(source, "world_operator_chat/task-titles.json"), path.join(registry, "world_operator_chat/task-titles.json"));
  const file = path.join(registry, provider, "interface.json"), descriptor = JSON.parse(await fs.readFile(file));
  descriptor.interface_driver_ref = "codex_app_server";
  await fs.writeFile(file, JSON.stringify(descriptor));
  let request, record;
  const requestMaintenance = role === "title" ? require("../../../registry/interface/world_operator_chat/task-titles.cjs").requestTaskTitle : requestComposerPrediction;
  await requestMaintenance({ octopusHome: () => home,
    withRunLock: (_id, fn) => fn(), withTopologyLock: (_home, fn) => fn(), assertCurrent() {}, waitForInterface: async () => {},
    readSource: async () => role === "title" && request ? null : ({ input: { artifact_id: "input", content_hash: "01".repeat(32), movement_seq: 1 }, chat: { provider_bound_interface_id: provider, os_root_ee_id: "ee_existence" },
      terminal: { artifact_id: "answer", content_hash: "01".repeat(32), movement_seq: 1 } }),
    commit: async ({ writes, publishBarrierPath }) => {
      for (const row of writes) {
        await fs.mkdir(path.dirname(row.file), { recursive: true });
        await fs.writeFile(row.file, row.content ?? JSON.stringify(row.next));
      }
      record = JSON.parse(await fs.readFile(publishBarrierPath));
    },
    handIn: async ({ manifest }) => { request = JSON.parse(await fs.readFile(manifest.payload_path)); return { admitted: { seq: 2 } }; },
    readArtifacts: async ({ interface_id }) => request ? [{ interface_id, movement_kind: "InterfaceOutbound",
      payload: { request, result: role === "title" ? { ok: true, value: { title: "Fixture" } } : { status: "completed", suggestion: "Continue" } } }] : [],
  }, { runId });
  return record;
}
module.exports = { predictionFixture };
if (require.main === module) predictionFixture(...process.argv.slice(2)).then(record => console.log(JSON.stringify(record))).catch(error => { console.error(error); process.exitCode = 1; });
