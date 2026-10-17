const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const path = require("node:path");
const os = require("node:os");
const { createRequire } = require("node:module");
const { setTimeout: delay } = require("node:timers/promises");
const { test } = require("node:test");

test("run progress refreshes exact records without invalidating configuration or model catalogues", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "oct-projection-cycle-"));
  const filename = path.join(__dirname, "../electron/projection.cjs");
  const sourceRequire = createRequire(filename);
  let catalogReads = 0, catalogFailure = false;
  const module = { exports: {} };
  new Function("require", "module", "exports", "__dirname", await fs.readFile(filename, "utf8"))(
    name => name === "./localPackages.cjs" ? { ...sourceRequire(name), readLocalPackageCatalog: async () => {
      catalogReads++;
      if (catalogFailure) throw new Error("Package configuration unavailable");
      return { packages: [], packs: [], errors: [] };
    } } : sourceRequire(name), module, module.exports, path.dirname(filename),
  );
  const changes = [];
  let home = path.join(root, "first");
  const provider = { id: "provider", interface_type: "world", role: "provider", source_names: ["model"],
    interface_driver_config: { private_config: { model: "first" } } };
  for (const name of ["first", "second"]) {
    for (const directory of ["registry/interface/provider", "registry/tool", "state/ee_instances", "state/runs", "client/package-state", "ledger"]) {
      await fs.mkdir(path.join(root, name, directory), { recursive: true });
    }
    await fs.writeFile(path.join(root, name, "registry/interface/provider/interface.json"), JSON.stringify(provider));
    await fs.writeFile(path.join(root, name, "ledger/movements.jsonl"), "");
  }
  const store = module.exports.createProjectionStore({ resolveRepoRoot: () => root, octopusHome: () => home,
    sourceRegistry: () => path.join(home, "registry"), onChange: (_generation, kind) => changes.push(kind) });
  const read = (method, params = {}) => store.read({ contract_version: 1, method, params });
  const changed = async (kind) => {
    const until = Date.now() + 3000;
    while (!changes.includes(kind) && Date.now() < until) await delay(20);
    assert.ok(changes.includes(kind), `Missing ${kind} change: ${changes}`);
    changes.length = 0;
  };
  try {
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "first");
    await changed("structure");
    const initialReads = catalogReads;
    const run = { id: "run", ee: "ee", worker: "worker", trace: "trace", status: "idle", active_checkpoints: [], checkpoints: [] };
    const runPath = path.join(home, "state/runs/run.json");
    await fs.writeFile(runPath, JSON.stringify(run));
    await changed("runs");
    assert.equal((await read("projection.runs.list", { include_ledger: false })).runs[0].record_status, "idle");
    assert.equal(catalogReads, initialReads);
    assert.equal((await read("projection.operator.os.workspace_setup")).status, "not_configured");
    const receipt = path.join(home, "client-state/workspace/receipts/broken.json");
    await fs.writeFile(receipt, "invalid");
    await changed("runs");
    assert.equal((await read("projection.operator.os.workspace_setup")).status, "degraded");
    assert.equal(catalogReads, initialReads, "workspace receipts do not change model or package declarations");
    await fs.unlink(receipt); await changed("runs");
    assert.equal((await read("projection.operator.os.workspace_setup")).status, "not_configured");
    await fs.writeFile(runPath, "invalid");
    await changed("runs");
    const invalid = await read("projection.runs.list", { include_ledger: false });
    assert.equal(invalid.runs.length, 0); assert.equal(invalid.invalid_run_records.length, 1);
    await fs.writeFile(runPath, JSON.stringify({ ...run, status: "running" }));
    await changed("runs");
    assert.equal((await read("projection.runs.list", { include_ledger: false })).runs[0].record_status, "running");
    assert.equal(catalogReads, initialReads);
    await fs.writeFile(path.join(home, "client/package-state/maintenance-lock.sqlite"), "lock");
    await delay(200);
    assert.deepEqual(changes, []);
    provider.interface_driver_config.private_config.model = "second";
    await fs.writeFile(path.join(home, "registry/interface/provider/interface.json"), JSON.stringify(provider));
    await fs.writeFile(runPath, JSON.stringify(run));
    await changed("structure");
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "second");
    assert.equal(catalogReads, initialReads + 1);
    await fs.mkdir(path.join(home, "registry/interface/provider/packages"));
    await fs.writeFile(path.join(home, "registry/interface/provider/packages/tool.py"), "pass\n");
    await changed("files");
    await read("projection.operator.providers.list");
    assert.equal(catalogReads, initialReads + 2, "owner files must refresh package diagnostics");
    catalogFailure = true; store.invalidate();
    await assert.rejects(read("projection.operator.providers.list"), /configuration unavailable/);
    assert.equal((await read("projection.movements.list", { limit: 1 })).next_seq, 0);
    catalogFailure = false;
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "second");
    await changed("structure");
    const beforeStateReads = catalogReads;
    await fs.mkdir(path.join(home, "interfaces/provider"), { recursive: true });
    await fs.writeFile(path.join(home, "interfaces/provider/stream-state.json"), "{}");
    await changed("runs");
    assert.equal((await read("projection.runs.list", { include_ledger: false })).runs.length, 1);
    assert.equal(catalogReads, beforeStateReads);
    await fs.writeFile(path.join(home, "interfaces/provider/environment.json"), JSON.stringify({ schema_version: 1, interface: "provider", values: {} }));
    await changed("structure");
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "second");
    assert.equal(catalogReads, beforeStateReads + 1);
    home = path.join(root, "second");
    assert.equal((await read("projection.operator.providers.list")).providers[0].model, "first");
    assert.deepEqual((await read("projection.runs.list", { include_ledger: false })).runs, []);
  } finally { store.close(); await fs.rm(root, { recursive: true, force: true }); }
});
