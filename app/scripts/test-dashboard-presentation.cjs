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
  const scheduled = animation.scheduleArtifactTransfers([], batch.transfers, 1000, new Map());
  assert.deepEqual(scheduled.map((row) => row.sequence), [10, 14]);
  assert.deepEqual(scheduled.map((row) => [row.startedAt, row.movementCount, row.firstSequence, row.lastSequence]), [[1000, 3, 10, 12], [1800, 1, 14, 14]]);
  for (const row of scheduled) assert.deepEqual(Object.fromEntries(Object.keys(batch.transfers[0]).map((key) => [key, row[key]])), batch.transfers.find((item) => item.sequence === row.sequence));
  const next = { ...batch.transfers.at(-1), sequence: 16, id: "movement-16" };
  const presented = new Map(scheduled.map(row => [row.sequence, { startedAt: row.startedAt, updatedAt: row.startedAt + 500, completedAt: row.startedAt + 500 }]));
  assert.equal(animation.scheduleArtifactTransfers(scheduled, [next], 2000, presented).at(-1).startedAt, 2600);
  assert.equal(animation.scheduleArtifactTransfers(scheduled, [next], 5000, presented)[0].startedAt, 5000);
  presented.set(scheduled[0].sequence, { startedAt: scheduled[0].startedAt, updatedAt: scheduled[0].startedAt });
  assert.ok(animation.scheduleArtifactTransfers(scheduled, [next], 5000, presented).some(row => row.sequence === scheduled[0].sequence));
  assert.equal(JSON.stringify(batch.transfers).includes("startedAt"), false);
  assert.equal(JSON.stringify(rows), original);
  pages.push({ next_seq: 15, movements: [] });
  await assert.rejects(animation.readArtifactTransferBatch(16), /invalid or regressed movement cursor/);
});

test("System transfer geometry follows layout changes and refuses ambiguous anchors", async () => {
  const dom = new JSDOM('<div id="surface"><div data-run-id="run"><b data-projection-interface="a" data-projection-anchor="worker"></b><b data-projection-interface="b" data-projection-checkpoint="checkpoint"></b></div><div id="root"></div></div>');
  Object.assign(globalThis, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  let now = 1000, right = 100, next = 0;
  const frames = new Map(), surface = document.getElementById("surface"), root = createRoot(document.getElementById("root"));
  Object.defineProperty(window.performance, "now", { value: () => now });
  window.requestAnimationFrame = fn => { frames.set(++next, fn); return next; };
  window.cancelAnimationFrame = id => frames.delete(id);
  window.HTMLElement.prototype.getBoundingClientRect = function () { return { x: this.dataset.projectionInterface === "b" ? right : 0, y: 0, width: 10, height: 10 }; };
  const animation = load("src/components/ArtifactTransferAnimation.tsx", { "../projectionClient": {}, "../projectionLive": {} });
  const transfers = [{ id: "geometry", sequence: 1, from: "a", to: "b", runId: "run", checkpointId: "checkpoint", startedAt: now }];
  const feed = { snapshot: () => transfers, subscribe: () => () => {}, presentation() {}, present() {} }, interfaces = { a: { type: "system", descriptor: { role: "worker" } }, b: { type: "system", descriptor: {} } };
  const frame = async time => { now = time; await React.act(async () => { const pending = [...frames.values()]; frames.clear(); pending.forEach(fn => fn(time)); }); };
  try {
    await React.act(async () => root.render(React.createElement(animation.SystemArtifactTransfers, { container: surface, feed, interfaces })));
    await frame(1100);
    const before = document.querySelector(".artifact-transfer-segment").getAttribute("d");
    await React.act(async () => { right = 200; document.querySelector('[data-run-id="run"]').style.transform = "translateX(100px)"; });
    await frame(1100);
    assert.notEqual(document.querySelector(".artifact-transfer-segment").getAttribute("d"), before);
    await React.act(async () => { const duplicate = document.querySelector('[data-projection-interface="b"]').cloneNode(); document.querySelector('[data-run-id="run"]').append(duplicate); });
    await frame(1150);
    assert.equal(document.querySelectorAll('.artifact-transfer-segment[d]:not([d=""])').length, 0);
    assert.equal(frames.size, 0);
    assert.equal(feed.snapshot(), transfers);
  } finally { await React.act(async () => root.unmount()); assert.equal(frames.size, 0); dom.window.close(); }
});

test("sustained bursts bound memory and lag while unrelated paths run together and the read watermark advances", async () => {
  const dom = new JSDOM('<div id="root"></div>', { pretendToBeVisual: true });
  Object.assign(globalThis, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  let now = 0, page = { next_seq: 0, movements: [] }, changed, displayed, feed;
  const requests = [];
  Object.defineProperty(window.performance, "now", { value: () => now });
  const animation = load("src/components/ArtifactTransferAnimation.tsx", {
    "motion/react": { useReducedMotion: () => false },
    "../projectionClient": { hasProjectionBridge: () => true, readProjection: async (_surface, request) => { requests.push(request); return page; } },
    "../projectionLive": { subscribeProjectionChanges: (listener) => { changed = listener; return () => { changed = null; }; } },
  });
  function Probe() { const current = animation.useArtifactTransferFeed({ mode: "live", endpoint: "fixture" }); feed = current.feed; displayed = { transfers: animation.useArtifactTransferList(feed), error: current.error }; return null; }
  const root = createRoot(document.getElementById("root"));
  try {
    await React.act(async () => root.render(React.createElement(Probe)));
    for (let tick = 0; tick < 100; tick++) {
      now = tick * 100;
      const rows = Array.from({ length: 100 }, (_, index) => movement(tick * 100 + index, index % 2 ? "b" : "a", index % 2 ? "a" : "b"));
      page = { next_seq: (tick + 1) * 100, movements: rows };
      await React.act(async () => changed());
      const presented = displayed.transfers.find(row => row.startedAt <= now && now < row.startedAt + 500);
      if (presented) feed.present(presented.sequence, { startedAt: presented.startedAt, updatedAt: presented.startedAt + 500, completedAt: presented.startedAt + 500 });
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
    const fresh = animation.scheduleArtifactTransfers([], independent, now, new Map());
    assert.equal(fresh.length, animation.ARTIFACT_TRANSFER_PATH_LIMIT);
    assert.ok(fresh.every((row) => row.startedAt === now));
    assert.equal(fresh.at(-1).sequence, 199);
    assert.equal(requests.length, 101);
  } finally { await React.act(async () => root.unmount()); assert.equal(changed, null); dom.window.close(); }
});

test("real feeds retain unseen anchor transfers independently per pane, expire seen bursts and dispose", async () => {
  const dom = new JSDOM('<div id="seen"><div data-run-id="run"><b data-projection-interface="a" data-projection-anchor="worker"></b><b data-projection-interface="b" data-projection-checkpoint="checkpoint"></b></div><div id="root-seen"></div></div><div id="unseen"><div id="root-unseen"></div></div>');
  Object.assign(globalThis, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  let now = 1000, next = 0, renderer = 0, page = { next_seq: 0, movements: [] };
  const frames = new Map(), listeners = new Set(), feeds = {}, errors = {};
  Object.defineProperty(window.performance, "now", { value: () => now });
  window.requestAnimationFrame = fn => { frames.set(++next, fn); return next; }; window.cancelAnimationFrame = id => frames.delete(id);
  window.HTMLElement.prototype.getBoundingClientRect = function () { return { x: this.dataset.projectionInterface === "b" ? 100 : 0, y: 0, width: 10, height: 10 }; };
  const animation = load("src/components/ArtifactTransferAnimation.tsx", {
    "motion/react": { useReducedMotion: () => false },
    "../projectionClient": { hasProjectionBridge: () => true, readProjection: async () => page },
    "../projectionLive": { subscribeProjectionChanges: fn => { listeners.add(fn); return () => listeners.delete(fn); } },
  });
  const interfaces = { a: { type: "system", descriptor: { role: "worker" } }, b: { type: "system", descriptor: {} } };
  function Pane({ id }) { const current = animation.useArtifactTransferFeed({ mode: "live", endpoint: "fixture" }); feeds[id] = current.feed; errors[id] = current.error; return React.createElement(animation.SystemArtifactTransfers, { container: document.getElementById(id), feed: current.feed, interfaces, key: renderer }); }
  const roots = ["seen", "unseen"].map(id => [id, createRoot(document.getElementById(`root-${id}`))]);
  const notify = async () => React.act(async () => { for (const fn of [...listeners]) fn(); });
  const frame = async time => { now = time; await React.act(async () => { const pending = [...frames.values()]; frames.clear(); pending.forEach(fn => fn(now)); }); };
  try {
    await React.act(async () => { for (const [id, root] of roots) root.render(React.createElement(Pane, { id })); });
    page = { next_seq: 1, movements: [movement(0)] }; await notify(); await frame(1250);
    assert.ok(document.querySelector('#seen [data-movement-id="movement-0"]').getAttribute("d"));
    await frame(1500);
    const completed = feeds.seen.presentation(0);
    now = 1550; renderer++;
    await React.act(async () => roots[0][1].render(React.createElement(Pane, { id: "seen" })));
    await frame(1600); assert.equal(document.querySelector('#seen [data-movement-id="movement-0"]').getAttribute("d"), "");
    feeds.seen.present(0, { startedAt: 1550, updatedAt: 1600 }); assert.deepEqual(feeds.seen.presentation(0), completed);
    now = 2000; page = { next_seq: 2, movements: [movement(1, "c", "d")] }; await notify();
    assert.ok(!feeds.seen.snapshot().some(row => row.sequence === 0));
    assert.ok(feeds.unseen.snapshot().some(row => row.sequence === 0));
    await React.act(async () => document.getElementById("unseen").prepend(document.querySelector('#seen [data-run-id="run"]').cloneNode(true)));
    await frame(2250); await frame(2300);
    const rendered = document.querySelector('#unseen [data-movement-id="movement-0"]');
    assert.ok(rendered.getAttribute("d")); assert.equal(rendered.dataset.firstSequence, "0");
    await frame(2500); await frame(2750);
    now = 3100; page = { next_seq: 3, movements: [movement(2, "c", "d")] }; await notify();
    assert.ok(!feeds.unseen.snapshot().some(row => row.sequence === 0));
    assert.deepEqual(errors, { seen: "", unseen: "" });
  } finally {
    await React.act(async () => { for (const [, root] of roots) root.unmount(); });
    assert.equal(listeners.size, 0); assert.equal(frames.size, 0);
    assert.ok(Object.values(feeds).every(feed => feed.snapshot().length === 0)); dom.window.close();
  }
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
  const transfers = animation.scheduleArtifactTransfers([], raw, now, new Map());
  assert.notEqual(raw[0].traceId, raw[1].traceId);
  assert.deepEqual(transfers.map((row) => row.traceId), raw.map((row) => row.traceId));
  const root = createRoot(document.getElementById("root"));
  const resolve = () => transfers.map((transfer) => ({ transfer, points: transfer.from === provider
    ? [{ x: 0, y: 0 }, { x: 100, y: 0 }] : [{ x: 100, y: 0 }, { x: 0, y: 0 }] }));
  const ids = () => [...document.querySelectorAll('.artifact-transfer-segment[d]:not([d=""])')].map((node) => node.dataset.movementId);
  const frame = async (at, step = 16) => {
    while (now < at) {
      now = Math.min(at, now + step);
      await React.act(async () => { const callbacks = [...frames.values()]; frames.clear(); callbacks.forEach((callback) => callback(now)); });
    }
  };
  try {
    let commits = 0;
    const render = (transfers, resolve) => React.createElement(React.Profiler, { id: "transfers", onRender: () => commits++ },
      React.createElement("svg", null, React.createElement(animation.ArtifactTransferSegments, { transfers, resolve, presentation() {}, onPresent() {} })));
    await React.act(async () => root.render(render(transfers, resolve)));
    now = 1100; await React.act(async () => root.render(render(transfers, () => resolve())));
    now = 1200; await React.act(async () => root.render(render(transfers, () => resolve())));
    assert.deepEqual(ids(), [], "layout redraws cannot advance playback before RAF");
    const initialCommits = commits;
    await frame(1250); assert.deepEqual(ids(), ["movement-2260"]);
    await frame(1650); assert.deepEqual(ids(), []); assert.equal(frames.size, 1);
    await frame(2050); assert.deepEqual(ids(), ["movement-2275"]);
    await frame(2317); assert.deepEqual(ids(), []); assert.equal(frames.size, 0);
    assert.equal(commits, initialCommits, "animation frames must not commit React renders");
    const delayed = [{ ...transfers[0], sequence: 3000, id: "delayed", startedAt: 1000 }];
    now = 4000;
    await React.act(async () => root.render(render(delayed, () => [{ transfer: delayed[0], points: [{ x: 0, y: 0 }, { x: 100, y: 0 }] }])));
    await frame(4600, 600); assert.deepEqual(ids(), ["delayed"], "a stalled frame must paint instead of restarting at zero");
    assert.equal(document.querySelector('[data-movement-id="delayed"]').dataset.firstSequence, String(transfers[0].firstSequence));
    await frame(4901); assert.deepEqual(ids(), []); assert.equal(frames.size, 0);
    now = 6000;
    const stalled = transfers.map((transfer, i) => ({ ...transfer, sequence: 4000 + i, id: `stalled-${i}`, startedAt: 6000 + i * 800 }));
    await React.act(async () => root.render(render(stalled, () => stalled.map(transfer => ({ transfer, points: [{ x: 0, y: 0 }, { x: 100, y: 0 }] })))));
    await frame(6100, 100); await frame(6700, 600); await frame(7300, 600);
    await frame(7516); assert.deepEqual(ids(), [], "the gap starts at actual completion even after an overshooting frame");
    await frame(7616); assert.deepEqual(ids(), ["stalled-1"]);
    await React.act(async () => root.render(render([], () => [])));
    assert.equal(frames.size, 0);
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
    assert.ok(document.querySelector('[role="tooltip"]').closest("svg") === null);
  } finally { await React.act(async () => root.unmount()); dom.window.close(); }
});
