const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const { openNativeAdoptionBatch } = require("./client/nativeAdoptionBatch.cjs");

async function fixture(t, mode) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-adoption-transport-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const binary = path.join(root, "planner");
  await fs.writeFile(binary, `#!/usr/bin/env node
const mode = ${JSON.stringify(mode)};
if (mode === "cancel") process.on("SIGTERM", () => {});
require("node:readline").createInterface({input:process.stdin}).on("line", line => {
  const value = JSON.parse(line);
  const result = {schema_version:1, request_id:value.request_id};
  if (value.operation === "finish") {
    process.stdout.write(JSON.stringify({...result,finished:true})+"\\n", () => process.exit(0)); return;
  }
  if (mode === "oversized") { process.stdout.write("x".repeat(4*1024*1024+1)); return; }
  if (mode === "native_error") { process.stderr.write("checked grant failed integrity\\n", () => process.exit(1)); return; }
  if (mode === "truncated") { process.stdout.write("{", () => process.exit(0)); return; }
  if (mode === "timeout" || mode === "cancel" && value.request_id > 1) return;
  process.stdout.write(JSON.stringify({...result,plan:{run_id:mode === "identity" ? "other" : value.request.run_id,pid:process.pid}})+"\\n");
});
`, { mode: 0o700 });
  return binary;
}

test("native adoption transport reuses one process and completes exact sequential requests", async t => {
  const batch = openNativeAdoptionBatch(await fixture(t, "ok"), { timeoutMs: 2000 });
  let pid;
  try {
    pid = (await batch.plan({ run_id: "one" })).pid;
    assert.equal((await batch.plan({ run_id: "two" })).pid, pid);
    await batch.finish();
  } finally { await batch.close(); }
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("native adoption transport refuses malformed frames and reaps cancelled processes", { timeout: 15000 }, async t => {
  for (const mode of ["identity", "oversized", "truncated", "native_error", "timeout", "request_bound"]) {
    const batch = openNativeAdoptionBatch(await fixture(t, mode), { timeoutMs: mode === "timeout" ? 300 : 2000 });
    await assert.rejects(batch.plan({ run_id: "one", ...(mode === "request_bound" ? { data: "x".repeat(4*1024*1024) } : {}) }), mode === "native_error" ? /checked grant failed integrity/ : undefined);
    await assert.rejects(batch.close());
  }
  const controller = new AbortController();
  const batch = openNativeAdoptionBatch(await fixture(t, "cancel"), { timeoutMs: 2000, signal: controller.signal });
  const { pid } = await batch.plan({ run_id: "one" });
  const pending = batch.plan({ run_id: "two" });
  controller.abort(new Error("test cancellation"));
  await assert.rejects(pending, /test cancellation/);
  await assert.rejects(batch.close(), /test cancellation/);
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
});

test("native adoption refuses publication after cancellation and bounds unconfirmed closure", { timeout: 10000 }, async t => {
  const controller = new AbortController();
  const batch = openNativeAdoptionBatch(await fixture(t, "ok"), { timeoutMs: 2000, signal: controller.signal });
  await batch.plan({ run_id: "one" });
  controller.abort(new Error("cancelled while checking preimages"));
  let published = false;
  assert.throws(() => { batch.assertActive(); published = true; }, /cancelled while checking preimages/);
  assert.equal(published, false);
  await assert.rejects(batch.close());
  const { EventEmitter } = require("node:events");
  const child = new EventEmitter(), signals = [];
  child.kill = signal => { signals.push(signal); return false; };
  child.unref = () => {};
  for (const stream of ["stdin", "stdout", "stderr"]) {
    child[stream] = new EventEmitter(); child[stream].destroy = () => {};
  }
  const module = { exports: {} };
  require("node:vm").runInNewContext(await fs.readFile(path.join(__dirname, "client/nativeAdoptionBatch.cjs"), "utf8"), {
    require: name => name === "node:child_process" ? { spawn: () => child } : require(name),
    module, Buffer, TextDecoder, setTimeout, clearTimeout,
  });
  const unconfirmed = module.exports.openNativeAdoptionBatch("fixture", { timeoutMs: 2000 });
  await assert.rejects(unconfirmed.close(), /closure unconfirmed.*SIGKILL was not delivered/);
  assert.ok(signals.includes("SIGKILL"));
});
