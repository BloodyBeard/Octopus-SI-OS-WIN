const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { createRequire } = require("node:module");
const path = require("node:path");
const { test } = require("node:test");
const React = require("react");
const { createRoot } = require("react-dom/client");
const { JSDOM } = require("jsdom");
const ts = require("typescript");
const app = path.resolve(__dirname, "..");

function load(file, mocks = {}) {
  const filename = path.join(app, file), module = { exports: {} }, nativeRequire = createRequire(filename);
  const compiled = ts.transpileModule(readFileSync(filename, "utf8"), { fileName: filename, reportDiagnostics: true,
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } });
  assert.deepEqual(compiled.diagnostics, []);
  Function("require", "module", "exports", compiled.outputText)((id) => {
    if (Object.hasOwn(mocks, id)) return mocks[id];
    if (id.endsWith(".css")) return {};
    if (id.startsWith(".")) return load(path.relative(app, nativeRequire.resolve(id + ".tsx")), mocks);
    return nativeRequire(id);
  }, module, module.exports);
  return module.exports;
}
const movement = (seq, from = "a", to = "b") => ({ seq, kind: "InterfaceOutbound", movement_id: `movement-${seq}`,
  artifact_id: `artifact-${seq}`, from: { kind: "interface", id: from }, to: { kind: "interface", id: to },
  trace: "trace", raw_run_id: "run", raw: { invocation_metadata: { runtime: { checkpoint: "checkpoint" } } } });

test("live transfer pages retain exact ledger order and pace later batches without rewriting evidence", async () => {
  const pages = [{ next_seq: 10, movements: [movement(9)] },
    { next_seq: 16, movements: [movement(14, "b", "a"), movement(12)], next_before_seq: 12 },
    { next_seq: 16, movements: [movement(11), movement(10)] }];
  const rows = pages.flatMap((page) => page.movements), original = JSON.stringify(rows), requests = [];
  const animation = load("src/components/ArtifactTransferAnimation.tsx", {
    "../projectionClient": { readProjection: async (surface, request) => { assert.equal(surface, "projection.movements.list"); requests.push(request); return pages.shift(); } },
    "../projectionLive": {},
  });
  assert.deepEqual(await animation.readArtifactTransferBatch(null), { nextSeq: 10, transfers: [] });
  const batch = await animation.readArtifactTransferBatch(10);
  assert.deepEqual(batch.transfers.map((row) => row.sequence), [10, 11, 12, 14]);
  assert.equal(requests[2].before_seq, 12);
  assert.equal(requests[2].after_seq, 9);
  const scheduled = animation.scheduleArtifactTransfers([], batch.transfers, 1000);
  assert.deepEqual(scheduled.map((row) => row.sequence), [10, 14]);
  assert.deepEqual(scheduled.map((row) => [row.startedAt, row.movementCount, row.firstSequence, row.lastSequence]), [[1000, 3, 10, 12], [1800, 1, 14, 14]]);
  for (const row of scheduled) assert.deepEqual(Object.fromEntries(Object.keys(batch.transfers[0]).map((key) => [key, row[key]])), batch.transfers.find((item) => item.sequence === row.sequence));
  const next = { ...batch.transfers.at(-1), sequence: 16, id: "movement-16" };
  assert.equal(animation.scheduleArtifactTransfers(scheduled, [next], 2000).at(-1).startedAt, 2600);
  assert.equal(animation.scheduleArtifactTransfers(scheduled, [next], 5000)[0].startedAt, 5000);
  assert.equal(JSON.stringify(batch.transfers).includes("startedAt"), false);
  assert.equal(JSON.stringify(rows), original);
});

test("sustained bursts bound memory and lag while unrelated paths run together and the read watermark advances", async () => {
  const dom = new JSDOM('<div id="root"></div>', { pretendToBeVisual: true });
  Object.assign(globalThis, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  let now = 0, page = { next_seq: 0, movements: [] }, changed, displayed;
  const requests = [];
  Object.defineProperty(window.performance, "now", { value: () => now });
  const animation = load("src/components/ArtifactTransferAnimation.tsx", {
    "motion/react": { useReducedMotion: () => false },
    "../projectionClient": { hasProjectionBridge: () => true, readProjection: async (_surface, request) => { requests.push(request); return page; } },
    "../projectionLive": { subscribeProjectionChanges: (listener) => { changed = listener; return () => { changed = null; }; } },
  });
  function Probe() { displayed = animation.useArtifactTransfers({ mode: "live", endpoint: "fixture" }); return null; }
  const root = createRoot(document.getElementById("root"));
  try {
    await React.act(async () => root.render(React.createElement(Probe)));
    for (let tick = 0; tick < 100; tick++) {
      now = tick * 100;
      const rows = Array.from({ length: 100 }, (_, index) => movement(tick * 100 + index, index % 2 ? "b" : "a", index % 2 ? "a" : "b"));
      page = { next_seq: (tick + 1) * 100, movements: rows };
      await React.act(async () => changed());
      assert.equal(displayed.error, "");
      assert.ok(displayed.transfers.length <= 3);
      assert.ok(displayed.transfers.every((row) => row.startedAt <= now + 1600));
      assert.equal(Math.max(...displayed.transfers.map((row) => row.lastSequence ?? row.sequence)), page.next_seq - 1);
      if (tick === 8 || tick === 16) {
        const active = displayed.transfers.find((row) => row.startedAt <= now && now < row.startedAt + 500);
        assert.equal(active.from, tick === 8 ? "b" : "a");
      }
      assert.equal(requests.at(-1).after_seq, tick ? tick * 100 - 1 : undefined);
    }
    const independent = Array.from({ length: 200 }, (_, sequence) => ({ id: `m-${sequence}`, sequence,
      from: `a-${sequence}`, to: `b-${sequence}`, runId: "run", checkpointId: "cp", traceId: "trace" }));
    const fresh = animation.scheduleArtifactTransfers([], independent, now);
    assert.equal(fresh.length, animation.ARTIFACT_TRANSFER_PATH_LIMIT);
    assert.ok(fresh.every((row) => row.startedAt === now));
    assert.equal(fresh.at(-1).sequence, 199);
    assert.equal(requests.length, 101);
  } finally { await React.act(async () => root.unmount()); assert.equal(changed, null); dom.window.close(); }
});

test("request and return with distinct checkpoint traces have a visible gap and retire all animation frames", async () => {
  const dom = new JSDOM('<div id="root"></div>', { pretendToBeVisual: true });
  Object.assign(globalThis, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  let now = 1000, nextFrame = 0;
  const frames = new Map();
  Object.defineProperty(window.performance, "now", { value: () => now });
  window.requestAnimationFrame = (callback) => { const id = ++nextFrame; frames.set(id, callback); return id; };
  window.cancelAnimationFrame = (id) => frames.delete(id);
  const animation = load("src/components/ArtifactTransferAnimation.tsx", { "../projectionClient": {}, "../projectionLive": {} });
  const runId = "run_operator_chat_36a5e9adea50641026079331752ee97d";
  const provider = "world_provider_chatgpt_36a5e9adea50641026079331752ee97d", context = "system_context_36a5e9adea50641026079331752ee97d";
  const raw = [
    { id: "movement-2260", sequence: 2260, from: provider, to: context, runId,
      checkpointId: "cp_35762d3d7a04a220566a_derived_aba543f4e9694241c925" },
    { id: "movement-2275", sequence: 2275, from: context, to: provider, runId,
      checkpointId: "cp_63ccadcdca3da9932ab6_context_derived_d2b16965cc3d6177a2ea" },
  ].map((row) => ({ ...row, traceId: `trace:${row.runId}:${row.checkpointId}` }));
  const transfers = animation.scheduleArtifactTransfers([], raw, now);
  assert.notEqual(raw[0].traceId, raw[1].traceId);
  assert.deepEqual(transfers.map((row) => row.traceId), raw.map((row) => row.traceId));
  const root = createRoot(document.getElementById("root"));
  const resolve = () => transfers.map((transfer) => ({ transfer, points: transfer.from === provider
    ? [{ x: 0, y: 0 }, { x: 100, y: 0 }] : [{ x: 100, y: 0 }, { x: 0, y: 0 }] }));
  const ids = () => [...document.querySelectorAll(".artifact-transfer-segment")].map((node) => node.dataset.movementId);
  const frame = async (at) => {
    now = at;
    await React.act(async () => { const callbacks = [...frames.values()]; frames.clear(); callbacks.forEach((callback) => callback(at)); });
  };
  try {
    await React.act(async () => root.render(React.createElement("svg", null, React.createElement(animation.ArtifactTransferSegments, { transfers, resolve }))));
    await frame(1250); assert.deepEqual(ids(), ["movement-2260"]);
    await frame(1650); assert.deepEqual(ids(), []); assert.equal(frames.size, 1);
    await frame(2050); assert.deepEqual(ids(), ["movement-2275"]);
    await frame(2301); assert.deepEqual(ids(), []); assert.equal(frames.size, 0);
  } finally { await React.act(async () => root.unmount()); dom.window.close(); }
});

test("bounded System and World labels preserve full grapheme-safe identities and styled tooltip text", async () => {
  const dom = new JSDOM('<div id="root"></div>', { pretendToBeVisual: true });
  Object.assign(globalThis, { window: dom.window, document: dom.window.document, Element: dom.window.Element, IS_REACT_ACT_ENVIRONMENT: true });
  window.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 200, y: 200, left: 200, top: 200, right: 460, bottom: 220, width: 260, height: 20 });
  const labels = load("src/components/DashboardLabel.tsx");
  const full = "system_worker_operator_chat_" + "1234567890abcdef".repeat(2);
  const segmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });
  const family = "👩‍👩‍👧‍👦", unicode = family.repeat(30) + "_ending";
  assert.equal(labels.compactDashboardLabel("Home"), "Home");
  assert.equal([...segmenter.segment(labels.compactDashboardLabel(unicode))].length, 24);
  assert.ok(labels.compactDashboardLabel(full).endsWith(`…${full.slice(-7)}`));
  const root = createRoot(document.getElementById("root"));
  try {
    await React.act(async () => root.render(React.createElement(React.Fragment, null,
      React.createElement(labels.DashboardLabel, { text: full }),
      React.createElement("svg", null, React.createElement(labels.DashboardSvgLabel, { text: full, x: 200, y: 100, centered: true, width: 92, limit: 18 })))));
    const visible = [...document.querySelectorAll(".dashboard-label-text")];
    assert.deepEqual(visible.map((node) => [...segmenter.segment(node.textContent)].length), [24, 18]);
    assert.ok([...document.querySelectorAll(".oc-visually-hidden")].every((node) => node.textContent === full));
    assert.equal(document.querySelector("foreignObject").getAttribute("x"), "154");
    await React.act(async () => {
      visible[1].dispatchEvent(new window.MouseEvent("pointermove", { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 220));
    });
    assert.equal(document.querySelector('[role="tooltip"]').textContent, full);
    assert.ok(document.querySelector('[role="tooltip"]').classList.contains("is-shell"));
    assert.ok(document.querySelector('[role="tooltip"] .dashboard-tooltip-label'));
    assert.equal(document.querySelector('[role="tooltip"]').closest("svg"), null);
  } finally { await React.act(async () => root.unmount()); dom.window.close(); }
});
