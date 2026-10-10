const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createRequire } = require("node:module");
const { test } = require("node:test");
const ts = require("typescript");
const appRoot = path.resolve(__dirname, "..");
const cache = new Map();
function load(relative) {
  const filename = path.resolve(appRoot, relative);
  if (cache.has(filename)) return cache.get(filename).exports;
  const module = { exports: {} }; cache.set(filename, module);
  const localRequire = createRequire(filename);
  const requireSource = (id) => {
    if (id.startsWith(".") && !id.endsWith(".json")) {
      const target = path.resolve(path.dirname(filename), id);
      const source = [target, `${target}.ts`, `${target}.tsx`].find((file) => fs.existsSync(file) && fs.statSync(file).isFile());
      if (source) return load(source);
    }
    return localRequire(id);
  };
  const code = ts.transpileModule(fs.readFileSync(filename, "utf8"), { fileName: filename.replace(/\.mjs$/, ".ts"), compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX, esModuleInterop: true } }).outputText;
  Function("module", "exports", "require", code)(module, module.exports, requireSource);
  return module.exports;
}
const { admitClientSurfaceDocument, admitClientSurfaceSource } = load("src/client-surfaces/validation.ts");
const { materializeRegistry, uniqueExtensionSurfaces } = load("src/client-surfaces/documentRegistry.ts");
const { selectHomeSurfaceDocuments, homeSurfaceIdentity } = load("src/client-surfaces/homePresentation.ts");
const authoring = require("../src/client-surfaces/home-widget-authoring.json");
const scope = { runId: "home-run", interfaceId: "home-client" };
const now = Date.parse("2026-10-04T10:00:00.000Z");
function widget(revision = 1) { return { ...structuredClone(authoring.example), revision }; }
function source(document, patch = {}) {
  const text = JSON.stringify(document);
  return { path: "artifact:widget", artifactId: "widget", digest: `sha256:${require("node:crypto").createHash("sha256").update(text).digest("hex")}`, text, bytes: Buffer.byteLength(text), modifiedAt: new Date(now).toISOString(), sequence: document.revision, ...scope, ...patch };
}
function registry(sources, errors = [], readError = "") {
  return materializeRegistry({ generation: 1, root: "artifact://ledger", documents: sources, errors }, readError, false, "live");
}
function admitted(document = widget(), patch) {
  const result = admitClientSurfaceSource(source(document, patch));
  assert.equal(result.verdict.passed, true, JSON.stringify(result.verdict));
  return result.admitted;
}

test("Home selects exact deliveries, both renderers, and bounds presentation without installing", () => {
  const native = admitted();
  const sandboxDoc = widget(); sandboxDoc.id = "home.sandbox";
  sandboxDoc.render = { kind: "sandbox", html: "<p>Result</p>", javascript: "document.body.dataset.ready='true'" };
  const sandbox = admitted(sandboxDoc);
  const foreign = admitted(widget(), { interfaceId: "other-client" });
  const otherRun = admitted(widget(), { runId: "other-run" });
  assert.deepEqual(selectHomeSurfaceDocuments([native, sandbox, foreign, otherRun], scope, now).documents, [native, sandbox]);
  assert.equal(native.sourceExtensionId, undefined);
  assert.equal(selectHomeSurfaceDocuments([native], scope, now + 4999).documents.length, 1);
  assert.equal(selectHomeSurfaceDocuments([native], scope, now + 5000).documents.length, 0);
  assert.equal(selectHomeSurfaceDocuments([native], { runId: scope.runId, interfaceId: "" }, now).documents.length, 0);
  const persistentDoc = widget(); delete persistentDoc.data.home_presentation;
  assert.equal(selectHomeSurfaceDocuments([admitted(persistentDoc)], scope, now + 1e10).documents.length, 1);
  persistentDoc.data.home_presentation = { expires_at: new Date(now + 1000).toISOString() };
  assert.equal(selectHomeSurfaceDocuments([admitted(persistentDoc)], scope, now + 1000).documents.length, 0);
  const many = Array.from({ length: 33 }, (_, i) => admitted({ ...persistentDoc, id: `home.widget-${i}` }));
  assert.match(selectHomeSurfaceDocuments(many, scope, now).errors.join(" "), /more than 32/);
});

test("malformed current revisions, conflicts and failed reads never revive previous documents", () => {
  const first = source(widget());
  const bad = widget(2); bad.render = { kind: "unknown" };
  const refused = registry([first, source(bad)]);
  assert.equal(refused.documents.length, 0);
  assert.match(refused.errors.join(" "), /invalid current revision/);
  assert.equal(registry([first, source(bad), source(widget(3))]).documents[0].document.revision, 3);
  assert.equal(registry([first, source({ ...widget(), title: "Conflict" })]).documents.length, 0);
  assert.equal(registry([first], [], "reader unavailable").documents.length, 0);
  assert.equal(registry([first], ["payload unavailable"]).documents.length, 0);
  assert.equal(registry([first, { ...source(widget(2)), text: "{" }]).documents.length, 0);
  assert.equal(registry([first, source({ ...widget(2), revision: "2" }, { sequence: 2 })]).documents.length, 0);
  assert.equal(registry([first, source(widget(2), { artifactId: undefined })]).documents.length, 0);
  const localDoc = widget(); delete localDoc.data.home_presentation;
  const local = source(localDoc, { path: "installed/widget.json", artifactId: undefined, runId: undefined, interfaceId: undefined, sequence: undefined });
  const brokenOtherHome = source(widget(2), { artifactId: undefined, runId: "other-home" });
  assert.equal(registry([first, local, brokenOtherHome]).documents.length, 2);
  assert.equal(registry([first, local], ["payload unavailable"]).documents.length, 1);
  const lost = (sequence) => ({ ...source(widget()), text: "", digest: `sha256:${require("node:crypto").createHash("sha256").update("").digest("hex")}`, bytes: 0, sequence });
  assert.equal(registry([lost(1), source(widget(2))]).documents.length, 1);
  assert.equal(registry([first, lost(2)]).documents.length, 0);
  assert.equal(registry([first, local, source(widget(2), { runId: undefined })]).documents.length, 1);
});

test("a widget delivered to several recipients stays visible to Home's own recipient", () => {
  const doc = widget(); delete doc.data.home_presentation;
  const fanout = ["presentation", "worker", "provider", scope.interfaceId].map((interfaceId, index) => source(doc, { interfaceId, sequence: 10 + index }));
  const voice = uniqueExtensionSurfaces(registry(fanout).voiceSurfaces, "voice", [], true);
  assert.equal(selectHomeSurfaceDocuments(voice, scope, now).documents.length, 1);
  assert.equal(uniqueExtensionSurfaces(registry(fanout).voiceSurfaces, "workspace", []).length, 1);
});

test("revision selection keeps interfaces isolated and replay does not extend a lifetime", () => {
  const initial = source(widget());
  const replay = { ...initial, sequence: 20, modifiedAt: new Date(now + 10000).toISOString() };
  const selected = registry([replay, source(widget(2), { interfaceId: "other-client" }), initial]);
  assert.equal(selected.documents.length, 2);
  assert.equal(selectHomeSurfaceDocuments(selected.documents, scope, now + 5000).documents.length, 0);
  assert.equal(admitted(widget(), { modifiedAt: "2026-10-04T10:00:00.123456Z" }).sourceDeliveredAt, "2026-10-04T10:00:00.123456Z");
});

test("presentation and provenance errors refuse admission while extension documents retain their path", () => {
  for (const value of [null, { lifetime_ms: 0 }, { lifetime_ms: 86400001 }, { lifetime_ms: 1.5 }, { expires_at: "2026-02-30T10:00:00Z" }, { expires_at: "tomorrow" }, { lifetime_ms: 10, expires_at: new Date(now).toISOString() }, { installed: true }]) {
    const document = widget(); document.data.home_presentation = value;
    assert.equal(admitClientSurfaceSource(source(document)).verdict.passed, false);
  }
  for (const patch of [{ modifiedAt: "" }, { sequence: -1 }, { artifactId: undefined }, { interfaceId: "" }, { runId: "" }]) {
    assert.equal(admitClientSurfaceSource(source(widget(), patch)).verdict.passed, false);
  }
  const extensionDoc = widget(); delete extensionDoc.data.home_presentation;
  const extension = admitClientSurfaceDocument(extensionDoc, "extension-digest", "extension:approved").admitted;
  extension.sourceExtensionId = "reviewed-extension";
  assert.deepEqual(selectHomeSurfaceDocuments([extension], { runId: "", interfaceId: "" }, now).documents, [extension]);
});

test("dismissal survives remount, is revision scoped, fails visibly on corrupt storage, and expiry wakes without projection events", async () => {
  const { JSDOM } = require("jsdom");
  const dom = new JSDOM("<!doctype html><body></body>", { url: "http://localhost" });
  const saved = new Map(["window", "document", "navigator", "IS_REACT_ACT_ENVIRONMENT"].map((key) => [key, Object.getOwnPropertyDescriptor(global, key)]));
  for (const [key, value] of Object.entries({ window: dom.window, document: dom.window.document, navigator: dom.window.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(global, key, { configurable: true, writable: true, value });
  const React = require("react"); const { createRoot } = require("react-dom/client");
  const { useHomeSurfaceDocuments } = load("src/client-surfaces/useHomeSurfaceDocuments.ts");
  let state, root;
  function Harness({ documents }) { state = useHomeSurfaceDocuments(documents, { ...scope, accessProfile: "live" }); return null; }
  async function mount(documents) { const host = document.createElement("div"); document.body.append(host); root = createRoot(host); await React.act(async () => root.render(React.createElement(Harness, { documents }))); }
  async function close() { await React.act(async () => root.unmount()); root = null; }
  try {
    const doc = widget(); delete doc.data.home_presentation;
    const first = admitted(doc); await mount([first]);
    await React.act(async () => state.dismiss(first)); assert.equal(state.visibleDocuments.length, 0);
    await close(); await mount([first]); assert.equal(state.visibleDocuments.length, 0);
    await React.act(async () => state.restoreAll()); assert.equal(state.visibleDocuments.length, 1);
    assert.notEqual(homeSurfaceIdentity(first), homeSurfaceIdentity(admitted({ ...doc, revision: 2 })));
    await close();
    const storageKey = `octopus.home.surface-dismissals.v1:${JSON.stringify(["live", scope.runId, scope.interfaceId])}`;
    window.localStorage.setItem(storageKey, "{"); await mount([first]);
    assert.ok(state.errors.length); assert.equal(state.visibleDocuments.length, 0);
    await React.act(async () => state.restoreAll()); assert.equal(state.visibleDocuments.length, 1);
    await close(); doc.data.home_presentation = { lifetime_ms: 100 };
    await mount([admitted(doc, { modifiedAt: new Date().toISOString() })]); assert.equal(state.visibleDocuments.length, 1);
    await React.act(async () => new Promise((resolve) => setTimeout(resolve, 130))); assert.equal(state.visibleDocuments.length, 0);
  } finally {
    if (root) await close(); dom.window.close();
    for (const [key, descriptor] of saved) { if (descriptor) Object.defineProperty(global, key, descriptor); else delete global[key]; }
  }
});
