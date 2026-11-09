const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { createRequire } = require("node:module");
const { test } = require("node:test");
const { MINIMUM_WRITABLE_BYTES, blockedStorageHealth, STORAGE_BLOCKED_MESSAGE } = require("../electron/storageHealth.cjs");
const { operatorChatSubmitFailure, operatorChatSubmitSuccess } = require("../electron/operatorChatSubmitResult.cjs");

function load(file, mocks) {
  const filename = path.resolve(__dirname, "../electron", file), module = { exports: {} }, nativeRequire = createRequire(filename);
  vm.runInNewContext(fs.readFileSync(filename, "utf8"), { module, exports: module.exports,
    require: (id) => Object.hasOwn(mocks, id) ? mocks[id] : nativeRequire(id),
    process: { argv: [] }, Buffer, Error, Date, setTimeout, clearTimeout, setInterval, clearInterval,
  }, { filename });
  return module.exports;
}

test("Home storage uses available blocks and a durable writable probe on the selected Home", async () => {
  const calls = [];
  let available = 1n, failure = null;
  const { readHomeStorageHealth } = load("storageHealth.cjs", { "node:fs/promises": {
    statfs: async (home) => { calls.push(home); return { bavail: available, bsize: 4096n }; },
    open: async (file, mode) => { assert.equal(path.dirname(file), "/selected"); assert.equal(mode, "wx"); return {
      writeFile: async (bytes) => { assert.equal(bytes.length, 4096); if (failure) throw failure; },
      sync: async () => calls.push("sync"), close: async () => calls.push("close"),
    }; }, unlink: async () => calls.push("unlink"),
  } });
  assert.equal((await readHomeStorageHealth("/selected")).code, "insufficient_space");
  assert.deepEqual(calls, ["/selected"]);
  available = BigInt(MINIMUM_WRITABLE_BYTES / 4096);
  assert.equal((await readHomeStorageHealth("/selected")).status, "writable");
  assert.deepEqual(calls.slice(-3), ["sync", "close", "unlink"]);
  for (const code of ["ENOSPC", "EDQUOT", "EACCES"]) {
    failure = Object.assign(new Error("native write failed"), { code });
    const health = await readHomeStorageHealth("/selected");
    assert.equal(health.code, code);
    assert.equal(health.status, code === "EACCES" ? "unavailable" : "blocked");
    assert.deepEqual(calls.slice(-2), ["close", "unlink"]);
  }
  failure = new Error("provider text contains ENOSPC or disk full");
  assert.equal((await readHomeStorageHealth("/selected")).status, "unavailable");
});

test("native storage failure remains blocked until a fresh writable probe without changing runtime readiness", async () => {
  let release, immediate = false;
  const writable = { status: "writable", availableBytes: 1e9, minimumBytes: MINIMUM_WRITABLE_BYTES, code: null, message: null };
  const { createRuntimeHost } = load("runtimeHost.cjs", { "./storageHealth.cjs": {
    ...require("../electron/storageHealth.cjs"), readHomeStorageHealth: async (home) => {
      assert.equal(home, "/selected");
      return immediate ? writable : new Promise((resolve) => { release = resolve; });
    },
  } });
  const host = createRuntimeHost({ home: () => "/selected", sourceRegistry: () => "/registry", runtimeBinary: () => "/bin/runtime", endpoint: () => "/selected/control.sock" });
  host.transition("ready", { ready: true, pid: 101, ownership: "external" });
  const pending = host.refreshStorageHealth(true);
  host.recordStorageFailure(Object.assign(new Error("failed"), { code: "EDQUOT" }));
  release(writable);
  await pending;
  assert.equal(host.snapshot().storageHealth.code, "EDQUOT");
  host.recordStorageFailure(new Error("ENOSPC provider text"));
  assert.equal(host.snapshot().storageHealth.code, "EDQUOT");
  assert.equal(host.snapshot().phase, "ready");
  assert.equal(host.snapshot().ready, true);
  const copy = host.snapshot(); copy.storageHealth.code = "changed";
  assert.equal(host.snapshot().storageHealth.code, "EDQUOT");
  immediate = true;
  await host.assertStorageWritable();
  assert.equal(host.snapshot().storageHealth.status, "writable");
});

test("runtime control requires interface_failures and reports exact life failures while healthy siblings remain ready", async () => {
  const nativeFs = require("node:fs/promises"), net = require("node:net");
  const { createRuntimeHost } = require("../electron/runtimeHost.cjs");
  const home = await nativeFs.mkdtemp("/tmp/octopus-health-");
  const root = path.join(home, "state", "runtime-instance"), now = Date.now();
  await nativeFs.mkdir(root, { recursive: true });
  await nativeFs.writeFile(path.join(root, "state.json"), JSON.stringify({ schema_version: 3, pid: process.pid, phase: "ready", started_at_unix_ms: now, updated_at_unix_ms: now, launch_owner: null }));
  let frame = { outcome: "status", pid: process.pid, phase: "ready", started_at_unix_ms: now, stopping: null, interface_failures: [{ interface: "provider.bound", error: "life stopped" }] };
  const server = net.createServer((socket) => socket.once("data", (bytes) => {
    assert.deepEqual(JSON.parse(bytes.toString()), { request: "status" }); socket.end(`${JSON.stringify(frame)}\n`);
  }));
  await new Promise((resolve) => server.listen(path.join(root, "control.sock"), resolve));
  const host = createRuntimeHost({ home: () => home, sourceRegistry: () => "/registry", runtimeBinary: () => "/bin/runtime", endpoint: () => path.join(home, "arrival.sock"), launchCwd: () => home, launchCommand: () => "runtime up" });
  try {
    const ready = await host.probe();
    assert.equal(ready.ready, true);
    assert.deepEqual(ready.interfaceFailures, [{ interfaceId: "provider.bound", message: "life stopped" }]);
    delete frame.interface_failures;
    assert.match((await host.probe()).error, /interface_failures is required/);
    frame.interface_failures = [{ interface: "provider.bound", error: "stopped", extra: true }];
    assert.equal((await host.probe()).ready, false);
    frame.interface_failures = [];
    assert.deepEqual((await host.probe()).interfaceFailures, []);
    frame.stopping = "stop";
    assert.equal((await host.probe()).ready, false);
  } finally {
    host.close(); await new Promise((resolve) => server.close(resolve)); await nativeFs.rm(home, { recursive: true, force: true });
  }
});

test("turn and steering IPC preserve storage refusal and uncertain delivery without guessing from provider text", async () => {
  let api, reply;
  load("preload.cjs", { electron: { contextBridge: { exposeInMainWorld: (_name, value) => { api = value; } },
    ipcRenderer: { invoke: async () => reply }, webFrame: {}, webUtils: {},
  } });
  for (const code of ["ENOSPC", "EDQUOT", "home_storage_blocked"]) for (const submissionStarted of [false, true]) {
    reply = operatorChatSubmitFailure(Object.assign(new Error(STORAGE_BLOCKED_MESSAGE), { code }), { submissionStarted });
    for (const method of ["submitChatTurn", "submitChatSteer"]) await assert.rejects(api[method]({}), (error) => {
      assert.equal(error.code, "home_storage_blocked");
      assert.equal(error.message, STORAGE_BLOCKED_MESSAGE);
      assert.equal(error.retryable, false);
      assert.equal(error.deliveryState, submissionStarted ? "unknown" : "not_sent");
      return true;
    });
  }
  assert.equal(operatorChatSubmitFailure(new Error("ENOSPC disk full")).error.code, "message_send_failed");
  reply = operatorChatSubmitSuccess({ runId: "run-1" });
  assert.equal((await api.submitChatSteer({})).runId, "run-1");
});

test("disk-full refusal retains the attachment's original owned stage and bytes", async (t) => {
  const nativeFs = require("node:fs/promises");
  const root = await nativeFs.mkdtemp("/tmp/octopus-draft-");
  const store = require("../electron/clientAttachments.cjs").createClientAttachmentStagingStore({ root: () => root });
  t.after(async () => { await store.abortAll(); await nativeFs.rm(root, { recursive: true, force: true }); });
  const handle = await store.stageBytes("owner", { name: "draft.txt", label: "operator.file", type: "text/plain", lastModified: 0 }, Buffer.from("original draft bytes"));
  const attachment = await store.consume("owner", handle), attachments = [attachment];
  assert.throws(() => store.restore("another-owner", attachment), /cannot be restored/);
  const source = fs.readFileSync(path.resolve(__dirname, "../electron/main.cjs"), "utf8");
  const start = source.indexOf("function retainChatAttachmentsAfterStorageFailure(");
  const end = source.indexOf('\nipcMain.handle("octopus:submit-chat-turn"', start);
  const retain = Function("getClientAttachmentStagingStore", `${source.slice(start, end)}; return retainChatAttachmentsAfterStorageFailure;`)(() => store);
  retain("owner", attachments, Object.assign(new Error("write failed"), { code: "ENOSPC" }));
  assert.equal(attachments.length, 0);
  const restored = await store.consume("owner", handle);
  assert.equal(await nativeFs.readFile(restored.payloadPath, "utf8"), "original draft bytes");
  await store.discard(restored);
});

test("storage blocked activity replaces Starting and Thinking without a completed or stopped turn", async () => {
  const { build } = require("esbuild");
  const React = require("react"), { renderToStaticMarkup } = require("react-dom/server");
  const built = await build({ entryPoints: [path.resolve(__dirname, "../src/components/chat/TurnActivity.tsx")], bundle: true,
    platform: "node", format: "cjs", write: false, logLevel: "silent", loader: { ".css": "empty" },
    external: ["react", "react-dom", "react/jsx-runtime", "motion/react"], define: { "import.meta.env.DEV": "false" },
  });
  const module = { exports: {} }; Function("require", "module", "exports", built.outputFiles[0].text)(require, module, module.exports);
  const html = renderToStaticMarkup(React.createElement(module.exports.TurnActivity, {
    activities: [{ id: "pending", role: "assistant", text: "", createdAtMs: 1, eventData: { work_status: "submitting" } }],
    blockingReason: blockedStorageHealth("ENOSPC").message, presentation: "conversation", startedAtMs: 1,
    toolPresentations: {}, turnInProgress: true, renderActivity: () => null,
  }));
  assert.match(html, /data-state="blocked"/);
  assert.match(html, /Disk full/);
  assert.doesNotMatch(html, /Starting your task|Thinking|data-state="complete"|data-state="stopped"/);
});
