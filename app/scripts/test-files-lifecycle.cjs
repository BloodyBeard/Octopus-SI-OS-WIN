const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const ts = require("typescript");
const { test } = require("node:test");
const root = path.resolve(__dirname, "..");
const cache = new Map();
const stubs = new Map();
function load(relative) {
  const filename = path.resolve(root, relative);
  if (stubs.has(filename)) return stubs.get(filename);
  if (cache.has(filename)) return cache.get(filename).exports;
  const module = { exports: {} }; cache.set(filename, module);
  const code = ts.transpileModule(fs.readFileSync(filename, "utf8"), { fileName: filename, compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, esModuleInterop: true } }).outputText;
  const localRequire = (specifier) => {
    if (!specifier.startsWith(".")) return stubs.has(specifier) ? stubs.get(specifier) : require(specifier);
    if (specifier.endsWith(".css")) return {};
    const target = path.resolve(path.dirname(filename), specifier);
    if (stubs.has(target)) return stubs.get(target);
    for (const suffix of ["", ".ts", ".tsx", ".js"]) if (fs.existsSync(target + suffix)) return load(target + suffix);
    throw new Error(`Unresolved test import: ${specifier}`);
  };
  new Function("require", "module", "exports", code)(localRequire, module, module.exports);
  return module.exports;
}
const lifecycle = load("src/components/files/filesPaneLifecycle.ts");
const snapshot = load("src/components/files/filesPaneSnapshot.ts");
const host = require("../electron/clientWindows.cjs");
const digest = (text) => crypto.createHash("sha256").update(text).digest("hex");
const file = (text = "base") => ({ rootId: "source", path: "edit.txt", name: "edit.txt", size: Buffer.byteLength(text), modifiedAt: "2026-09-08T00:00:00.000Z", kind: "text", text, truncated: false, digest: digest(text) });
function pane(text = "base") {
  const key = lifecycle.filesPaneKey("live", crypto.randomUUID());
  lifecycle.ensureFilesPane(key);
  lifecycle.updateFilesPane(key, (s) => ({ ...s, rootId: "source", rootPath: "/test/root", file: file(), draft: text }));
  return key;
}
const roots = async () => ({ roots: [{ id: "source", path: "/test/root", label: "Source" }] });
const transport = { osFileRoots: roots, writeOsFile: async (request) => ({ ...file(request.text), outsideOctopusEvidence: true }) };
const tick = () => new Promise((resolve) => setImmediate(resolve));
function discard(key) { const state = lifecycle.readFilesPane(key); lifecycle.forgetFilesPane(key, state.snapshot); }

test("host and renderer snapshot parsers agree and preserve exact data", () => {
  const key = pane("\n  trailing spaces  \n");
  const good = lifecycle.captureFilesPane(key);
  const boundary = "é".repeat(1024 * 1024);
  const cases = [good, { ...good, revision: Number.MAX_SAFE_INTEGER }, { ...good, file: null, draft: "" }, { ...good, file: file(boundary), draft: boundary }];
  const firstInvalid = cases.length;
  for (const [field, value] of [["version", 2], ["id", ""], ["id", "  "], ["revision", -1], ["revision", 1.5], ["revision", Number.MAX_SAFE_INTEGER + 1], ["rootPath", ""], ["rootPath", "/test/\nroot"], ["draft", boundary + "é"], ["extra", true]]) cases.push({ ...good, [field]: value });
  for (const [field, value] of [["digest", "bad"], ["rootId", "elsewhere"], ["modifiedAt", "yesterday"], ["size", -1], ["truncated", true], ["kind", "other"], ["text", boundary + "é"]]) cases.push({ ...good, file: { ...good.file, [field]: value } });
  for (const offset of [-1, Infinity, NaN, "0"]) cases.push({ ...good, view: { scrollTop: offset, scrollLeft: 0 } });
  cases.push({ ...good, target: { kind: "artifact", artifactId: "a", interfaceId: "i", label: "l", runId: "r" } });
  const afterInvalid = cases.length;
  for (let i = 0; i < 40; i++) cases.push({ ...good, revision: i, view: { scrollTop: i * 17.5, scrollLeft: i }, draft: `${i}\n  Ω  ` });
  for (const [index, candidate] of cases.entries()) {
    const decoded = snapshot.normalizeFilesPaneSnapshot(candidate);
    assert.equal(decoded !== null, index < firstInvalid || index >= afterInvalid);
    let native;
    try { native = host.validateTransferableWorkspacePaneTarget({ kind: "files", workspaceKey: "octopus-os", snapshot: candidate }).snapshot; } catch { native = null; }
    assert.deepEqual(native, decoded);
  }
  assert.equal(snapshot.normalizeFilesPaneSnapshot(good).draft, "\n  trailing spaces  \n");
  discard(key);
});

test("save retains CAS and newer edits; failure never approves close", async () => {
  const key = pane("first"); let finish;
  const saving = lifecycle.saveFilesPane(key, { osFileRoots: roots, writeOsFile: (request) => {
    assert.equal(request.expectedDigest, digest("base")); assert.equal(request.expectedSize, 4);
    return new Promise((resolve) => { finish = resolve; });
  } });
  await tick();
  assert.throws(() => lifecycle.captureFilesPane(key), /Finish saving/);
  lifecycle.updateFilesPane(key, (s) => ({ ...s, draft: "newer" }));
  finish({ ...file("first"), outsideOctopusEvidence: true });
  assert.equal(await saving, false);
  assert.equal(lifecycle.readFilesPane(key).snapshot.draft, "newer");
  assert.equal(lifecycle.readFilesPane(key).snapshot.file.text, "first");
  const closing = lifecycle.requestCloseFilesPanes([key]);
  await lifecycle.resolveFilesPaneClose(key, "save", { osFileRoots: roots, writeOsFile: async () => { throw new Error("CAS conflict"); } });
  assert.equal(lifecycle.readFilesPane(key).closeRequested, true);
  assert.match(lifecycle.readFilesPane(key).error, /CAS conflict/);
  await lifecycle.resolveFilesPaneClose(key, "cancel");
  assert.equal(await closing, false); assert.equal(lifecycle.readFilesPane(key).snapshot.draft, "newer");
  const retry = lifecycle.requestCloseFilesPanes([key]);
  await lifecycle.resolveFilesPaneClose(key, "save", transport);
  assert.equal(await retry, true); lifecycle.forgetFilesPane(key);
});

test("actual OS file reads and CAS writes retain a failed draft and recover with fresh evidence", async () => {
  const temporary = fs.realpathSync(fs.mkdtempSync(path.join(require("node:os").tmpdir(), "octopus-files-contract-")));
  const filename = path.join(temporary, "edit.txt"), original = "base\r\nα😀\t", draft = "unsaved\r\nα😀\t";
  const store = require("../electron/osFiles.cjs").createOsFileStore({ resolveRoots: async () => [{ id: "source", label: "Fixture", root: temporary }] });
  const key = lifecycle.filesPaneKey("live", crypto.randomUUID());
  const injected = { osFileRoots: store.roots, writeOsFile: store.write };
  try {
    fs.writeFileSync(filename, original);
    const opened = await store.read({ rootId: "source", path: "edit.txt" });
    assert.equal(opened.text, original); assert.equal(opened.digest, digest(original));
    await assert.rejects(store.read({ rootId: "source", path: "missing.txt" }), { code: "ENOENT" });
    lifecycle.ensureFilesPane(key);
    lifecycle.updateFilesPane(key, (s) => ({ ...s, rootId: "source", rootPath: temporary, file: opened, draft }));
    fs.writeFileSync(filename, "external edit");
    assert.equal(await lifecycle.saveFilesPane(key, injected), false);
    const failed = lifecycle.readFilesPane(key);
    assert.equal(failed.snapshot.file, opened); assert.equal(failed.snapshot.draft, draft); assert.equal(failed.saving, false);
    assert.match(failed.error, /File changed after it was opened/);
    assert.equal(fs.readFileSync(filename, "utf8"), "external edit");
    const closing = lifecycle.requestCloseFilesPanes([key]);
    await lifecycle.resolveFilesPaneClose(key, "save", injected);
    assert.equal(lifecycle.readFilesPane(key).closeRequested, true);
    await lifecycle.resolveFilesPaneClose(key, "cancel");
    assert.equal(await closing, false); assert.equal(lifecycle.readFilesPane(key).snapshot.draft, draft);
    const current = await store.read({ rootId: "source", path: "edit.txt" });
    lifecycle.updateFilesPane(key, (s) => ({ ...s, file: current }));
    const retry = lifecycle.requestCloseFilesPanes([key]);
    await lifecycle.resolveFilesPaneClose(key, "save", injected);
    assert.equal(await retry, true);
    const saved = lifecycle.readFilesPane(key);
    assert.equal(saved.snapshot.file.text, draft); assert.equal(saved.snapshot.file.digest, digest(draft));
    assert.equal(saved.snapshot.draft, draft); assert.equal(saved.error, ""); assert.equal(saved.saving, false);
    assert.deepEqual(fs.readFileSync(filename), Buffer.from(draft));
    assert.deepEqual(fs.readdirSync(temporary), ["edit.txt"]);
  } finally {
    if (lifecycle.hasFilesPane(key)) discard(key);
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});

test("batch and window close recheck earlier approvals and new drafts", async () => {
  const first = pane("one"), second = pane("two");
  const closing = lifecycle.requestCloseFilesPanes([first, second]);
  await lifecycle.resolveFilesPaneClose(first, "discard"); await tick();
  lifecycle.updateFilesPane(first, (s) => ({ ...s, draft: "edited while second prompt open" }));
  await lifecycle.resolveFilesPaneClose(second, "discard");
  assert.equal(await closing, false);
  assert.throws(() => lifecycle.forgetFilesPane(first), /still belongs/);
  const windowClose = lifecycle.requestCloseAllFilesPanes("live");
  const newcomer = pane("late draft");
  await lifecycle.resolveFilesPaneClose(first, "discard");
  assert.equal(await windowClose, false);
  for (const key of [first, second, newcomer]) discard(key);
});

test("transfer locks the exact snapshot, rollback retains source, root remap rejects", async () => {
  const source = pane("unsaved"), original = lifecycle.captureFilesPane(source);
  const destination = lifecycle.filesPaneKey("live", `files:${original.id}`);
  lifecycle.ensureFilesPane(destination, undefined, original);
  lifecycle.setFilesPaneTransferPending(destination, true);
  await assert.rejects(lifecycle.validateFilesPaneRoot(destination, { osFileRoots: async () => ({ roots: [{ id: "source", path: "/other/root" }] }) }), /different admitted root/);
  assert.throws(() => lifecycle.updateFilesPane(destination, (s) => ({ ...s, draft: "wrong" })), /being transferred/);
  lifecycle.forgetFilesPane(destination, original);
  assert.equal(lifecycle.readFilesPane(source).snapshot.draft, "unsaved");
  lifecycle.updateFilesPane(source, (s) => ({ ...s, draft: "later" }));
  assert.equal(lifecycle.filesPaneMatchesSnapshot(source, original), false);
  const latest = lifecycle.captureFilesPane(source);
  lifecycle.setFilesPaneTransferPending(source, true);
  assert.equal(lifecycle.filesPaneMatchesSnapshot(source, latest), true);
  assert.equal(await lifecycle.requestCloseFilesPanes([source]), false);
  lifecycle.setFilesPaneTransferPending(source, false);
  assert.equal(lifecycle.readFilesPane(source).snapshot.draft, "later");
  lifecycle.updateFilesPane(source, (s) => ({ ...s, draft: s.file.text }));
  assert.throws(() => lifecycle.forgetFilesPane(source, latest), /changed after/);
  lifecycle.updateFilesPane(source, (s) => ({ ...s, draft: "later" }));
  lifecycle.forgetFilesPane(source, lifecycle.captureFilesPane(source));
});

test("native close locks existing and future panes for only the exact request", async () => {
  const key = pane(), other = lifecycle.filesPaneKey("other", "files");
  lifecycle.ensureFilesPane(other);
  const before = lifecycle.captureFilesPane(key);
  assert.equal(await lifecycle.requestCloseAllFilesPanes("live"), true);
  assert.equal(lifecycle.acquireFilesWindowClose("native-1", "live"), true);
  assert.equal(lifecycle.acquireFilesWindowClose("native-1", "live"), true);
  assert.equal(lifecycle.acquireFilesWindowClose("native-2", "live"), false);
  assert.equal(lifecycle.acquireFilesWindowClose("native-1", "other"), false);
  assert.throws(() => lifecycle.captureFilesPane(key), /window is closing/);
  assert.throws(() => lifecycle.updateFilesPane(key, (s) => ({ ...s, draft: "late" })), /window is closing/);
  assert.throws(() => lifecycle.setFilesPaneTransferPending(key, true), /window is closing/);
  assert.equal(await lifecycle.saveFilesPane(key, transport), false);
  assert.equal(await lifecycle.requestCloseAllFilesPanes("live"), false);
  const future = lifecycle.filesPaneKey("live", "future");
  lifecycle.ensureFilesPane(future);
  assert.equal(lifecycle.readFilesPane(future).nativeClosePending, true);
  assert.throws(() => lifecycle.updateFilesPane(future, (s) => ({ ...s, rootId: "source" })), /window is closing/);
  assert.equal(lifecycle.readFilesPane(other).nativeClosePending, false);
  lifecycle.releaseFilesWindowClose("native-2");
  assert.equal(lifecycle.readFilesPane(key).nativeClosePending, true);
  assert.deepEqual(lifecycle.readFilesPane(key).snapshot, before);
  lifecycle.releaseFilesWindowClose("native-1");
  assert.equal(lifecycle.readFilesPane(future).nativeClosePending, false);
  for (const k of [key, other, future]) lifecycle.forgetFilesPane(k);
});

test("native close flush and final check refuse unfinished ownership", async () => {
  const key = pane("dirty");
  assert.throws(() => lifecycle.acquireFilesWindowClose("  "), /exact request/);
  assert.equal(lifecycle.acquireFilesWindowClose("native-check"), false);
  const closing = lifecycle.requestCloseAllFilesPanes();
  assert.equal(lifecycle.acquireFilesWindowClose("native-check"), false);
  await lifecycle.resolveFilesPaneClose(key, "discard");
  assert.equal(await closing, true);
  lifecycle.setFilesPaneTransferPending(key, true);
  assert.equal(lifecycle.acquireFilesWindowClose("native-check"), false);
  lifecycle.setFilesPaneTransferPending(key, false);
  let added;
  const unregister = lifecycle.registerFilesPaneFlush(key, () => { added ??= pane("draft created during flush"); });
  assert.equal(lifecycle.acquireFilesWindowClose("native-check"), false);
  assert.equal(lifecycle.readFilesPane(key).nativeClosePending, false);
  unregister(); discard(added);
  lifecycle.updateFilesPane(key, (s) => ({ ...s, draft: "save" }));
  let finish;
  const saving = lifecycle.saveFilesPane(key, { osFileRoots: roots, writeOsFile: () => new Promise((resolve) => { finish = resolve; }) });
  await tick();
  assert.equal(lifecycle.acquireFilesWindowClose("native-check"), false);
  finish({ ...file("save"), outsideOctopusEvidence: true });
  assert.equal(await saving, true);
  assert.equal(lifecycle.acquireFilesWindowClose("native-check"), true);
  lifecycle.releaseFilesWindowClose("native-check");
  lifecycle.forgetFilesPane(key);
});

test("native close abort owns only its dialogs and never discards a draft", async () => {
  const key = pane("unsaved"), controller = new AbortController();
  const foreign = lifecycle.requestCloseFilesPanes([key]);
  assert.equal(await lifecycle.requestCloseAllFilesPanes("live", controller.signal), false);
  controller.abort();
  assert.equal(lifecycle.readFilesPane(key).closeRequested, true);
  await lifecycle.resolveFilesPaneClose(key, "cancel"); assert.equal(await foreign, false);
  const owned = new AbortController();
  const closing = lifecycle.requestCloseAllFilesPanes("live", owned.signal);
  assert.equal(require("node:events").getEventListeners(owned.signal, "abort").length, 1);
  owned.abort();
  assert.equal(await closing, false);
  assert.equal(require("node:events").getEventListeners(owned.signal, "abort").length, 0);
  assert.equal(lifecycle.readFilesPane(key).closeRequested, false);
  assert.equal(lifecycle.readFilesPane(key).snapshot.draft, "unsaved");
  const savingClose = new AbortController(); let finish;
  const pending = lifecycle.requestCloseAllFilesPanes("live", savingClose.signal);
  const save = lifecycle.resolveFilesPaneClose(key, "save", { osFileRoots: roots, writeOsFile: () => new Promise((resolve) => { finish = resolve; }) });
  await tick(); savingClose.abort();
  assert.equal(await pending, false);
  assert.equal(lifecycle.readFilesPane(key).saving, true);
  assert.equal(lifecycle.acquireFilesWindowClose("abort-save"), false);
  finish({ ...file("unsaved"), outsideOctopusEvidence: true }); await save;
  assert.equal(lifecycle.readFilesPane(key).closeRequested, false);
  assert.equal(lifecycle.readFilesPane(key).snapshot.file.text, "unsaved");
  lifecycle.forgetFilesPane(key);
});

test("actual Files leaf retains drafts and rejects stale file-read completions", async () => {
  const { JSDOM } = require("jsdom"), React = require("react"), { createRoot } = require("react-dom/client");
  const dom = new JSDOM('<div id="root"></div>', { url: "https://files.test" });
  const globals = ["window", "document", "HTMLElement", "Node", "ResizeObserver", "requestAnimationFrame", "cancelAnimationFrame", "IS_REACT_ACT_ENVIRONMENT"];
  const previous = globals.map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]);
  for (const key of ["window", "document", "HTMLElement", "Node"]) globalThis[key] = dom.window[key];
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver = class { observe() {} disconnect() {} };
  globalThis.requestAnimationFrame = (fn) => setTimeout(fn, 0); globalThis.cancelAnimationFrame = clearTimeout;
  window.octopusTransport = { ...transport, listOsFiles: async ({ rootId, path: p }) => ({ rootId, path: p, entries: [] }) };
  const stub = (p, value) => stubs.set(path.resolve(root, `src/${p}`), value);
  const noop = () => null, node = ({ children }) => React.createElement("div", null, children);
  stub("appearance", { useShouldReduceMotion: () => false, useAppearance: () => ({ darkCodeTheme: "default", lightCodeTheme: "default", themeMode: "light", codeFontSize: 12 }) });
  stub("design-system/components", { OcButton: noop, OcIconButton: noop, OcMenuItem: noop });
  stub("design-system/OcDropdown", { OcDropdown: noop }); stub("runtimeFormat", { copyText: async () => true }); stub("toasts", { appToast: { danger() {}, success() {} } });
  stub("components/artifacts/ArtifactPreview", { ArtifactPreview: noop }); stub("components/artifacts/artifactFileOperations", { safeArtifactFilename: (s) => s });
  stub("components/files/filesPierreSetup", { adoptFilesStyles() {} });
  stub("components/IconGlyphs", { Glyph: noop }); stub("components/navigation/OctopusSpinner", { OctopusSpinner: noop });
  let openPath, treeMounted = false;
  stub("components/OctopusSurface", { OctopusFileTree: (props) => { openPath = props.onOpenPath; return null; }, OctopusFilterField: noop, OctopusPanelNote: node, octopusTreeAncestors: () => [] });
  stub("components/SetupDialogShell", { SetupDialogShell: noop });
  stub("components/shell/PanelResizeHandle", { PanelResizeHandle: noop, useAnimatedPanelSize: () => ({ mounted: treeMounted }) });
  stub("components/files/FilesBreadcrumb", { FilesBreadcrumb: noop });
  stub("components/settings/octopusDiffThemes", { octopusDiffThemeName: () => "test" });
  stubs.set("@octopus/files-diffs/edit", { Editor: class { setOptions() {} canUndo = false; canRedo = false; } });
  let edit;
  stubs.set("@octopus/files-diffs", { EXTENSION_TO_FILE_FORMAT: { ts: "typescript", txt: "text" }, CodeView: class {
    constructor(options) { this.options = options; } setup(container) { this.container = container; }
    setItems(items) { this.item = items[0]; } setOptions(options) { this.options = options; }
    render() { this.editor ??= this.options.createEditor("file", {}); edit = (text) => this.options.onItemEditChange({ editor: this.editor, file: { ...this.item.file, contents: text } }, this.item); }
    getItem() { return this.item; } updateItem(item) { this.item = item; } getScrollTop() { return this.container.scrollTop; }
    scrollTo({ position }) { this.container.scrollTop = position; }
    cleanUp() { this.container.dispatchEvent(new window.Event("scroll")); }
  } });
  const { FilesView } = load("src/components/files/FilesView.tsx");
  const key = pane("already unsaved");
  lifecycle.updateFilesPane(key, (s) => ({ ...s, view: { scrollTop: 37, scrollLeft: 11 } }));
  const initial = lifecycle.captureFilesPane(key), reactRoot = createRoot(document.getElementById("root"));
  const render = (active) => React.createElement(FilesView, { accessProfile: "live", active, paneKey: key, snapshot: initial });
  let raceKey;
  try {
    await React.act(async () => { reactRoot.render(render(true)); await tick(); });
    assert.equal(document.querySelector(".os-files-pierre-viewport").scrollTop, 37);
    await React.act(async () => { edit("keystroke draft"); });
    await React.act(async () => { reactRoot.render(render(false)); });
    assert.equal(lifecycle.readFilesPane(key).snapshot.draft, "keystroke draft");
    await React.act(async () => { reactRoot.render(null); });
    assert.equal(lifecycle.readFilesPane(key).snapshot.draft, "keystroke draft");
    lifecycle.updateFilesPane(key, (s) => ({ ...s, draft: s.file.text }));
    assert.equal(lifecycle.acquireFilesWindowClose("leaf-close", "live"), true);
    await React.act(async () => { reactRoot.render(render(true)); await tick(); edit("locked keystroke"); });
    assert.equal(lifecycle.readFilesPane(key).snapshot.draft, "base");
    assert.equal(document.querySelector(".os-files-pierre-viewport").scrollLeft, 11);
    assert.equal(document.querySelector(".os-files-view").hasAttribute("inert"), true);
    await React.act(async () => { lifecycle.releaseFilesWindowClose("leaf-close"); await tick(); edit("keystroke draft"); });
    await React.act(async () => { reactRoot.render(null); });
    lifecycle.setFilesPaneTransferPending(key, true);
    await React.act(async () => { reactRoot.render(render(true)); await tick(); });
    assert.equal(lifecycle.readFilesPane(key).snapshot.draft, "keystroke draft");
    const expected = lifecycle.captureFilesPane(key);
    await React.act(async () => { reactRoot.render(null); lifecycle.forgetFilesPane(key, expected); });
    assert.equal(lifecycle.hasFilesPane(key), false);

    const reads = [];
    treeMounted = true;
    window.octopusTransport = { ...transport,
      osFileRoots: async () => ({ roots: [...(await roots()).roots, { id: "second", path: "/test/second", label: "Second" }] }),
      listOsFiles: async ({ rootId, path: p }) => ({ rootId, path: p, entries: [{ name: "edit.txt", path: "edit.txt", kind: "file", size: 4 }] }),
      readOsFile: (request) => new Promise((resolve, reject) => { reads.push({ ...request, resolve, reject }); }),
    };
    raceKey = pane();
    await React.act(async () => { reactRoot.render(React.createElement(FilesView, { accessProfile: "live", active: true, paneKey: raceKey })); await tick(); });
    const open = async (path) => { assert.equal(typeof openPath, "function"); await React.act(async () => { openPath(path); }); };
    const settle = async (read, text, failure) => React.act(async () => {
      if (failure) read.reject(failure);
      else read.resolve({ ...file(text), rootId: read.rootId, path: read.path, name: read.path });
      await tick();
    });
    const current = () => lifecycle.readFilesPane(raceKey).snapshot;
    const opening = () => document.body.textContent.includes("Opening file");
    await open("older.txt"); await open("newer.txt");
    assert.equal(reads.length, 2);
    await settle(reads[1], "newer bytes");
    await React.act(async () => { edit("newer draft"); });
    await settle(reads[0], "obsolete bytes");
    assert.equal(current().file.path, "newer.txt"); assert.equal(current().draft, "newer draft");

    await React.act(async () => { edit("newer bytes"); });
    await open("failed-old.txt"); await open("latest.txt");
    await settle(reads[2], null, new Error("obsolete read failure"));
    assert.equal(opening(), true); assert.equal(document.body.textContent.includes("obsolete read failure"), false);
    assert.equal(current().file.path, "newer.txt"); assert.equal(current().draft, "newer bytes");
    await settle(reads[3], "latest bytes");
    assert.equal(opening(), false); assert.equal(current().file.path, "latest.txt"); assert.equal(current().draft, "latest bytes");

    await open("withdrawn.txt"); await open("latest.txt");
    assert.equal(reads.length, 5); assert.equal(opening(), false);
    await React.act(async () => { edit("retained draft"); });
    await settle(reads[4], "withdrawn bytes");
    assert.equal(current().file.path, "latest.txt"); assert.equal(current().draft, "retained draft");

    await React.act(async () => { edit("latest bytes"); });
    await open("old-root.txt");
    await React.act(async () => { lifecycle.updateFilesPane(raceKey, (s) => ({ ...s, rootId: "second", rootPath: "/test/second", file: null, draft: "" })); await tick(); });
    await open("new-root.txt");
    assert.deepEqual(reads.slice(5).map(({ rootId, path }) => [rootId, path]), [["source", "old-root.txt"], ["second", "new-root.txt"]]);
    await settle(reads[5], "old-root bytes");
    assert.equal(opening(), true); assert.equal(current().file, null); assert.equal(current().draft, "");
    await settle(reads[6], "new-root bytes");
    assert.equal(opening(), false); assert.equal(current().file.rootId, "second"); assert.equal(current().draft, "new-root bytes");
  } finally {
    await React.act(async () => { reactRoot.unmount(); }); dom.window.close();
    if (raceKey && lifecycle.hasFilesPane(raceKey)) lifecycle.forgetFilesPane(raceKey);
    for (const [key, descriptor] of previous) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key]; }
  }
});
