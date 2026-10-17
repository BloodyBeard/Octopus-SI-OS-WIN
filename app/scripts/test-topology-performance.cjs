const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createRequire } = require("node:module");
const { performance } = require("node:perf_hooks");
const { test } = require("node:test");
const ts = require("typescript");
const app = path.resolve(__dirname, "..");

function load(root = path.join(app, "src")) {
  const filename = path.join(root, "runtimeTopology.ts"), module = { exports: {} };
  const source = fs.readFileSync(filename, "utf8") + "\nexport { projectRuntime };";
  const code = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
  const data = { emptySnapshot: projection => ({ environments: [], summaryStats: ["0 EEs", "0 runs", "0 artifacts"], artifactStores: [], eventLog: [], interfaceIndex: {}, projection }) };
  Function("require", "module", "exports", code)(id => id === "./data/topology" ? data : id === "./projectionClient" ? { projectionTargetLabel: () => "fixture" } : {}, module, module.exports);
  return module.exports.projectRuntime;
}

function fixture(eeCount = 2, runCount = 86, interfaceCount = 351) {
  const runs = Array.from({ length: runCount }, (_, i) => ({ run_id: `run-${i}`, ee_id: `ee-${i % eeCount}`, worker: `worker-${i % 4}`, status: "idle", created_at: "2026-10-17T00:00:00Z" }));
  const bindings = runs.flatMap(run => [0, 1, 2].map(i => ({ run_id: run.run_id, ee_id: run.ee_id, interface_id: `interface-${i}`, checkpoint_id: `cp-${i}`, status: "projected", direction: i ? "outbound" : "inbound" })));
  const rows = count => Array.from({ length: count }, (_, i) => ({ run_id: runs[i % runs.length].run_id, ee_id: `ee-${(i + 1) % eeCount}`, artifact_id: `artifact-${i}`, mark_name: `mark-${i}`, status: i % 3 ? "stored" : "pending", created_at_unix_ms: 1792195200000 + i }));
  return {
    generation: 1, snapshotRaw: { projection: { mode: "live", ledger_rows: 1000, warnings: ["retained warning"] } },
    runtimeObservation: { socket_connected: true, phase: "ready", host_error: "", ownership: "fixture" },
    eeRaw: { ee_instances: Array.from({ length: eeCount }, (_, i) => ({ ee_id: `ee-${i}`, template_name: `template-${i}`, parent_ee_id: i % 4 ? `ee-${i - i % 4}` : undefined, policy: { allowed_interfaces: ["worker-0", "interface-0"] } })) },
    runsRaw: { runs }, runCheckpointsRaw: { run_checkpoints: runs.map(run => ({ run_id: run.run_id, checkpoints: [{ id: "cp-0", interface: "interface-0", out_interface: "interface-1", status: "made" }] })) },
    interfacesRaw: { interfaces: Array.from({ length: interfaceCount }, (_, i) => ({ rule_id: i < 4 ? `worker-${i}` : `interface-${i - 4}`, interface_type: i % 2 ? "world" : "system", role: i < 4 ? "worker" : "provider", ee_template_name: `template-${i % eeCount}` })) },
    bindingsRaw: { bindings }, artifactsRaw: { artifacts: rows(300) }, marksRaw: { marks: rows(300) },
    workerEnvironmentsRaw: { worker_environments: runs.map(run => ({ worker_environment_id: `room-${run.run_id}`, run_id: run.run_id, ee_id: run.ee_id, worker_environment_kind: "fixture" })) },
    movementsRaw: { movements: Array.from({ length: 1000 }, (_, i) => ({ seq: 999 - i, raw_run_id: runs[i % runs.length].run_id, raw_ee_id: `ee-${(i + 1) % eeCount}`, kind: i % 7 ? "InterfaceOutbound" : "DeliveryRefused", recorded_at_unix_ms: 1792195200000 + i, from: { kind: "interface", id: "interface-0" }, to: { kind: "interface", id: "interface-1" } })) },
    runDetailsRaw: { "run-0": { detail: { run: runs[0], artifacts: [{ artifact_id: "focused-detail" }], checkpoints: [{ id: "focused", interface: "interface-2", required_verified_artifacts: [{ mark: "failed", artifact_label: "candidate", verifier: "check", outcome: "failed" }] }] }, error: "Detail read refused" } },
  };
}

function measure(project, payload, repetitions = 3) {
  project(payload);
  const times = [];
  let formats = 0;
  const original = Date.prototype.toLocaleTimeString;
  Date.prototype.toLocaleTimeString = function (...args) { formats++; return original.apply(this, args); };
  try {
    for (let i = 0; i < repetitions; i++) { const start = performance.now(); project(payload); times.push(performance.now() - start); }
  } finally { Date.prototype.toLocaleTimeString = original; }
  return { medianMs: +times.sort((a, b) => a - b)[Math.floor(times.length / 2)].toFixed(2), dateLocaleCallsPerProjection: formats / repetitions };
}

test("topology retains ordered joins, focused details, refusal rows and input data", () => {
  const project = load(), payload = fixture(), original = JSON.stringify(payload), result = project(payload);
  assert.equal(JSON.stringify(payload), original);
  assert.equal(result.environments.flatMap(ee => ee.runs).length, 86);
  assert.equal(result.environments[0].runs[0].eventLog[0].detail, "Detail read refused");
  assert.equal(result.environments[0].runs[0].checkpoints[0].requiredVerifiedArtifacts[0].outcome, "failed");
  assert.ok(result.eventLog.some(row => row.kind === "DeliveryRefused" && row.tone === "amber"));
  assert.ok(result.projection.detail.includes("retained warning"));
  assert.equal(project({ ...payload, eeRaw: {} }).environments.length, 0);
  const { topologyArtifactRow } = require("../electron/projection.cjs");
  const heavy = Object.fromEntries(["routes", "module_source", "source_content", "text", "operations", "authoring"].map(key => [key, "payload".repeat(256)]));
  payload.artifactsRaw.artifacts.push(...["recorded_at_unix_ms", "created_at_unix_ms", "ts_unix_ms", "created_at", "ts", "time"].map(field => ({ name: field, route_label: "metadata", meta: { run_id: "run-0", ee_id: "ee-1", ...heavy }, [field]: "2026-10-17T00:00:00Z" })));
  for (const rows of [payload.artifactsRaw, payload.runDetailsRaw["run-0"].detail]) rows.artifacts = rows.artifacts.map((row, index) => ({ ...row, ...heavy, label: `label-${index}` }));
  const full = JSON.stringify(payload), compact = structuredClone(payload);
  for (const rows of [compact.artifactsRaw, compact.runDetailsRaw["run-0"].detail]) rows.artifacts = rows.artifacts.map(topologyArtifactRow);
  assert.deepEqual(project(compact), project(payload));
  assert.equal(JSON.stringify(payload), full);
  const beforeBytes = Buffer.byteLength(JSON.stringify(payload.artifactsRaw)), afterBytes = Buffer.byteLength(JSON.stringify(compact.artifactsRaw));
  assert.ok(afterBytes < beforeBytes / 10);
  console.log(JSON.stringify({ name: "topology-artifact-summary-payload", equivalentOutput: true, beforeBytes, afterBytes }));
});

test("60 voice parent updates preserve topology output and skip unrelated stage renders", async () => {
  const React = require("react"), { createRoot } = require("react-dom/client"), { JSDOM } = require("jsdom");
  const baseline = process.argv.includes("--baseline") ? process.argv[process.argv.indexOf("--baseline") + 1] : null;
  const snapshot = load()(fixture(24, 96, 80)), metrics = [], outputs = [];
  const filters = { statuses: { projected: true, idle: true, active: true } }, onSelect = () => {};
  for (const sourceRoot of [...(baseline ? [baseline] : []), path.join(app, "src")]) {
    const dom = new JSDOM('<div id="root"></div>', { url: "https://fixture.invalid" });
    Object.assign(globalThis, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
    let renders = 0;
    const filename = path.join(sourceRoot, "components/TopologyStage.tsx"), module = { exports: {} };
    const code = ts.transpileModule(fs.readFileSync(filename, "utf8"), { fileName: filename, compilerOptions: {
      target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX,
    } }).outputText;
    const mocks = {
      "./IconGlyphs": { Glyph: () => null }, "./WorldProjection": {}, "../toasts": { useAppErrorToast() {} },
      "./DashboardLabel": { DashboardLabel: ({ text }) => text, DashboardTooltip: ({ children }) => children },
      "./ArtifactTransferAnimation": { SystemArtifactTransfers: () => null, useArtifactTransferFeed: () => { renders++; return { feed: {}, error: "" }; } },
    };
    const native = createRequire(path.join(app, "src/components/TopologyStage.tsx"));
    Function("require", "module", "exports", code)(id => Object.hasOwn(mocks, id) ? mocks[id] : native(id), module, module.exports);
    const Stage = module.exports.TopologyStage, root = createRoot(document.getElementById("root"));
    let query = "";
    const Parent = ({ voice }) => React.createElement("div", { "data-voice-level": voice }, React.createElement(Stage, {
      snapshot, query, scope: "System", viewMode: "zones", filters, selected: null, onSelect,
    }));
    try {
      await React.act(async () => root.render(React.createElement(Parent, { voice: 0 })));
      const before = document.querySelector(".topology-shell").outerHTML, initialRenders = renders;
      const start = performance.now();
      for (let voice = 1; voice <= 60; voice++) await React.act(async () => root.render(React.createElement(Parent, { voice })));
      metrics.push({ elapsedMs: +(performance.now() - start).toFixed(2), stageRenders: renders - initialRenders });
      assert.equal(document.querySelector(".topology-shell").outerHTML, before);
      outputs.push(before.replace(/[_a-zA-Z0-9-]+-zone-mask-/g, "zone-mask-"));
      query = "no-matching-topology";
      await React.act(async () => root.render(React.createElement(Parent, { voice: 61 })));
      assert.ok(document.querySelector(".topology-shell").textContent.includes("No EEs in current view"));
    } finally { await React.act(async () => root.unmount()); dom.window.close(); }
  }
  assert.equal(metrics.at(-1).stageRenders, 0);
  if (baseline) assert.equal(outputs[0], outputs[1]);
  console.log(JSON.stringify({ name: "60-voice-updates-24-EEs-96-runs", equivalentOutput: true, ...(baseline ? { before: metrics[0] } : {}), after: metrics.at(-1) }));
});

test("120 transfer frames retain every visible segment without React frame commits", async () => {
  const React = require("react"), { createRoot } = require("react-dom/client"), { JSDOM } = require("jsdom");
  const baseline = process.argv.includes("--baseline") ? process.argv[process.argv.indexOf("--baseline") + 1] : null;
  const metrics = [], outputs = [];
  for (const sourceRoot of [...(baseline ? [baseline] : []), path.join(app, "src")]) {
    const dom = new JSDOM('<div id="root"></div>');
    Object.assign(globalThis, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
    let now = 1000, next = 0, commits = 0;
    const frames = new Map(), filename = path.join(sourceRoot, "components/ArtifactTransferAnimation.tsx"), module = { exports: {} };
    Object.defineProperty(window.performance, "now", { value: () => now });
    window.requestAnimationFrame = fn => { frames.set(++next, fn); return next; }; window.cancelAnimationFrame = id => frames.delete(id);
    const code = ts.transpileModule(fs.readFileSync(filename, "utf8"), { fileName: filename, compilerOptions: {
      target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX,
    } }).outputText;
    Function("require", "module", "exports", code)(id => id.startsWith(".") ? {} : require(id), module, module.exports);
    const transfers = Array.from({ length: 96 }, (_, sequence) => ({ id: `movement-${sequence}`, sequence,
      from: sequence % 2 ? "b" : "a", to: sequence % 2 ? "a" : "b", runId: `run-${Math.floor(sequence / 2)}`, checkpointId: "cp", startedAt: 1000 + sequence % 2 * 800 }));
    const resolve = () => transfers.map(transfer => ({ transfer, points: [{ x: transfer.sequence % 2 ? 100 : 0, y: transfer.sequence }, { x: transfer.sequence % 2 ? 0 : 100, y: transfer.sequence }] }));
    const root = createRoot(document.getElementById("root")), rendered = [];
    try {
      await React.act(async () => root.render(React.createElement(React.Profiler, { id: "frames", onRender: () => commits++ },
        React.createElement("svg", null, React.createElement(module.exports.ArtifactTransferSegments, { transfers, resolve, presentation() {}, onPresent() {} })))));
      const initialCommits = commits, start = performance.now();
      for (let frame = 1; frame <= 120; frame++) {
        now = 1000 + frame * 1000 / 60;
        await React.act(async () => { const pending = [...frames.values()]; frames.clear(); pending.forEach(fn => fn(now)); });
        rendered.push([...document.querySelectorAll('.artifact-transfer-segment[d]:not([d=""])')].map(node => [node.dataset.movementId, node.getAttribute("d"), node.dataset.movementCount]));
      }
      metrics.push({ elapsedMs: +(performance.now() - start).toFixed(2), reactFrameCommits: commits - initialCommits });
      assert.equal(frames.size, 0); outputs.push(rendered);
    } finally { await React.act(async () => root.unmount()); dom.window.close(); }
  }
  assert.equal(metrics.at(-1).reactFrameCommits, 0);
  if (baseline) assert.deepEqual(outputs[0], outputs[1]);
  console.log(JSON.stringify({ name: "120-frames-48-request-return-paths", equivalentOutput: true, ...(baseline ? { before: metrics[0] } : {}), after: metrics.at(-1) }));
});

if (process.argv.includes("--benchmark")) {
  const baselinePath = process.argv[process.argv.indexOf("--baseline") + 1];
  assert.ok(process.argv.includes("--baseline") && baselinePath, "Supply baseline source directory");
  const before = load(baselinePath), after = load();
  const now = Date.now; Date.now = () => 1792198800000;
  try {
    for (const [name, payload] of [["fresh-shaped", fixture()], ["large", fixture(128, 768, 1200)]]) {
      const original = JSON.stringify(payload);
      assert.deepEqual(after(payload), before(payload), `${name} complete output differs`);
      const previous = measure(before, payload), current = measure(after, payload);
      assert.equal(JSON.stringify(payload), original);
      console.log(JSON.stringify({ name, equivalentOutput: true, before: previous, after: current, speedup: +(previous.medianMs / current.medianMs).toFixed(2) }));
    }
    const edge = fixture(4, 12, 20);
    edge.runsRaw.runs[1] = { ...edge.runsRaw.runs[1], ee_id: undefined, meta: { source_meta: { ee_id: "ee-2" } } };
    edge.runsRaw.runs.push({ ...edge.runsRaw.runs[0], ee_id: "ee-3" });
    edge.bindingsRaw.bindings.push(edge.bindingsRaw.bindings[0]);
    edge.artifactsRaw.artifacts.push({ meta: { run_id: "run-0", ee_id: "ee-1" }, name: "metadata" });
    edge.interfacesRaw.interfaces.push({ template_name: "interface-0", ee_template_name: "template-0" });
    assert.deepEqual(after(edge), before(edge));
    console.log(JSON.stringify({ name: "duplicate-and-cross-EE-joins", equivalentOutput: true }));
  } finally { Date.now = now; }
}

module.exports = { fixture, load };
