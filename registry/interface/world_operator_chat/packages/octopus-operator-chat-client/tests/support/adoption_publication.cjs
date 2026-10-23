const fs = require("node:fs/promises");
const path = require("node:path");
const crypto = require("node:crypto");
const { adoptOperatorChatPackageUpdates } = require("../../../../client/surfacePlanning.cjs");
const { readConfig, resolvePackageExecutable } = require("../../../../client/roots.cjs");

(async () => {
  const chunks = []; for await (const chunk of process.stdin) chunks.push(chunk);
  const { home, plan, mode, runHash } = JSON.parse(Buffer.concat(chunks));
  const registry = path.join(home, "registry");
  const binary = resolvePackageExecutable(await readConfig(), home, registry, "topology_planner");
  await fs.mkdir(path.dirname(binary), { recursive: true });
  await fs.writeFile(path.join(home, "host.json"), JSON.stringify({ root_refs: { run_records_root: path.join(home, "runs"), ee_instances_root: path.join(home, "ee") } }));
  await fs.writeFile(binary, `#!${process.execPath}
const fs = require("node:fs"), path = require("node:path");
const plan = ${JSON.stringify(plan)}, mode = ${JSON.stringify(mode)};
require("node:readline").createInterface({input:process.stdin}).on("line", line => {
  const request = JSON.parse(line), response = {schema_version:1,request_id:request.request_id};
  if (request.operation === "finish") {
    process.stdout.write(JSON.stringify({...response,finished:true})+"\\n", () => process.exit(0)); return;
  }
  if (mode === "grant") {
    const pin = plan.preimages.find(pin => path.dirname(pin.path) === ${JSON.stringify(path.join(home, "ledger/metadata"))});
    if (!pin) throw new Error("missing checked grant preimage");
    fs.writeFileSync(pin.path, "{}");
  }
  process.stdout.write(JSON.stringify({...response,plan})+"\\n");
});
`, { mode: 0o700 });
  const controller = new AbortController(); let checks = 0, commits = 0;
  const hash = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
  try {
    const result = await adoptOperatorChatPackageUpdates({
      octopusHome: () => home, registryRoot: () => registry, installationRoot: () => home, signal: controller.signal,
      assertStopped: async () => { if (++checks === 2 && mode === "cancel") controller.abort(new Error("cancelled before publication")); },
      commitClientAuthoring: async request => {
        commits++;
        for (const pin of request.condition.preimages) {
          const actual = await fs.readFile(pin.path).then(hash, error => { if (error.code === "ENOENT") return null; throw error; });
          if (actual !== pin.sha256) throw new Error("commit dependency changed");
        }
        for (const write of request.writes) await fs.writeFile(write.file, write.content);
      },
      onResult: async () => {},
    }, { clientRuns: [{ run_id: plan.run_id, record_digest: runHash }] });
    process.stdout.write(JSON.stringify({ commits, changed: result.changed }));
  } catch (error) { process.stdout.write(JSON.stringify({ commits, error: error.message })); }
})().catch(error => { console.error(error.message); process.exitCode = 1; });
