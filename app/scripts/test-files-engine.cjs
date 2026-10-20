const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { test } = require("node:test");
const ts = require("typescript");
const app = path.resolve(__dirname, "..");
const sha = (value) => crypto.createHash("sha256").update(value).digest("hex");
const supportPath = path.join(app, "src/components/files/fileSupport.ts");
const support = evaluate(fs.readFileSync(supportPath, "utf8"), supportPath, { "@octopus/files-diffs": { EXTENSION_TO_FILE_FORMAT: { ts: "typescript", txt: "text" } } });
const adoptedFilesHosts = [];
const filesStyles = { adoptFilesStyles(host) { adoptedFilesHosts.push(host); } };
const themeNames = { octopusDiffThemeName: (kind, id) => `octopus-codex-${kind}-${id}` };

async function withGlobals(values, run) {
  const previous = Object.keys(values).map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)]);
  for (const [name, value] of Object.entries(values)) Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  try { return await run(); } finally {
    for (const [name, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor); else delete globalThis[name];
    }
  }
}

function evaluate(source, filename, dependencies = {}, globals = {}) {
  const code = ts.transpileModule(source, {
    fileName: filename,
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} }, localRequire = require("node:module").createRequire(filename);
  Function("module", "exports", "require", ...Object.keys(globals), code)(module, module.exports,
    (name) => name in dependencies ? dependencies[name] : localRequire(name), ...Object.values(globals));
  return module.exports;
}

test("Files adapters use the public renderer, editor and shared theme registry", async () => withGlobals({ navigator: { userAgent: "Files contract", platform: "MacIntel", maxTouchPoints: 0 } }, async () => {
  const renderer = await import("@octopus/files-diffs");
  const edit = await import("@octopus/files-diffs/edit");
  for (const name of ["CodeView", "Virtualizer", "VirtualizedFile", "registerCustomTheme"]) assert.equal(typeof renderer[name], "function", name);
  assert.equal(typeof edit.Editor, "function");
  for (const raw of ["", "α😀\r\n", "a\nb", "a\r\nb", "ab", "ba"]) assert.equal(support.fileContentHash(raw), sha(raw));
}));

test("upstream editable CodeView routes change events to the current document owner", async () => withGlobals({
  navigator: { userAgent: "Files contract", platform: "MacIntel", maxTouchPoints: 0 },
  requestAnimationFrame: () => 1, cancelAnimationFrame() {},
  document: { createElement: () => ({ style: {} }) },
}, async () => {
  const { CodeView } = await import("@octopus/files-diffs");
  const received = [];
  let notify, createdType;
  const editor = { edit: () => () => {}, cleanUp() {} };
  const view = new CodeView({
    createEditor(type, options) { createdType = type; notify = options.onChange; return editor; },
    onItemEditChange(event, item) { received.push({ event, item }); },
  });
  const original = { id: "a", type: "file", edit: true, file: { name: "a.ts", contents: "before" } };
  view.setItems([original]);
  const record = view.idToItem.get("a");
  record.element = {};
  view.attachItemEditor(record);
  assert.equal(createdType, "file");
  assert.equal(view.getEditor("a"), editor);
  const event = { editor, file: { name: "a.ts", contents: "after" }, changes: [] };
  notify(event);
  assert.equal(received[0].event, event);
  assert.equal(received[0].item, original);
  const newer = { ...original, version: 1 };
  view.idToItem.get("a").item = newer;
  notify(event);
  assert.equal(received[1].item, newer);
  view.idToItem.delete("a");
  notify(event);
  assert.equal(received.length, 2);
}));

test("public upstream TextDocument preserves CRLF/LF/tabs/Unicode and history without mount normalization", async () => {
  const { TextDocument: AuditTextDocument } = await import("@octopus/files-diffs/edit");
  for (const text of ["", "\r\n", "a\r\nb\r\n", "a\nb\n", "a\nb", "\tα😀e\u0301\r\n尾\t\r\n", "a\rb\r"]) {
    const document = new AuditTextDocument("audit.txt", text);
    assert.equal(document.getText(), text); assert.equal(sha(document.getText()), sha(text));
    assert.equal(document.version, 0); assert.equal(document.canUndo, false); assert.equal(document.canRedo, false);
    assert.equal(document.applyEdits([]), undefined); assert.equal(document.canUndo, false);
    document.applyEdits([{ range: { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } }, newText: "X" }]);
    assert.equal(document.getText(), `X${text}`); assert.equal(document.canUndo, true);
    document.undo(); assert.equal(document.getText(), text); assert.equal(document.canRedo, true);
    document.redo(); assert.equal(document.getText(), `X${text}`); document.undo();
    const newline = document.normalizeEol("\n");
    if (text.includes("\r\n")) assert.equal(newline, "\r\n");
    document.applyResolvedEdits([{ start: text.length, end: text.length, text: newline }]);
    assert.equal(document.getText(), text + newline); document.undo(); assert.equal(document.getText(), text);
  }
  const unicode = new AuditTextDocument("unicode.txt", "α😀尾");
  unicode.applyResolvedEdits([{ start: 2, end: 2, text: "X" }]);
  assert.equal(unicode.getText(), "αX😀尾"); unicode.undo(); assert.equal(unicode.getText(), "α😀尾");
});

test("upstream editor keeps find/replace and platform keyboard commands", async () => withGlobals({
  navigator: { userAgent: "Files contract", platform: "MacIntel", maxTouchPoints: 0 },
}, async () => {
  const { TextDocument, resolveEditorCommandFromKeyboardEvent: resolve } = await import("@octopus/files-diffs/edit");
  const document = new TextDocument("search.txt", "foo Foo food\r\nfoo");
  const query = { text: "foo", caseSensitive: false, wholeWord: true, regex: false };
  assert.deepEqual(document.search(query), [[0, 3], [4, 7], [14, 17]]);
  assert.deepEqual(document.search({ ...query, caseSensitive: true }), [[0, 3], [14, 17]]);
  assert.deepEqual(document.search({ ...query, text: "f.o", regex: true }), [[0, 3], [4, 7], [14, 17]]);
  for (const [key, flags, expected] of [
    ["z", { metaKey: true }, "undo"], ["z", { metaKey: true, shiftKey: true }, "redo"],
    ["f", { metaKey: true }, "openSearchPanel"], ["f", { metaKey: true, altKey: true }, "openSearchReplacePanel"],
    ["d", { metaKey: true }, "findNextMatch"], ["Tab", {}, "indent"], ["Tab", { shiftKey: true }, "outdent"],
    ["ArrowUp", { altKey: true }, "moveLineUp"], ["ArrowDown", { altKey: true, shiftKey: true }, "copyLineDown"],
  ]) assert.equal(resolve({ key, ...flags }, undefined, true), expected);
}));

test("Files metrics measurement and equality preserve CSS parsing and equality", async () => {
  const { readFileMetrics: measure, equalFileMetrics: equal } = support;
  const element = {}, defaults = { hunkLineCount: 32, lineHeight: 21.6, diffHeaderHeight: 0, hunkSeparatorHeight: 32, spacing: 0 };
  for (const [height, gap, expected] of [["27px", "4px", { lineHeight: 27, spacing: 4 }], ["0", "0", { lineHeight: 0 }], ["normal", "2rem", {}], ["NaNpx", "Infinitypx", {}], [" 18.5px ", " -2px ", { lineHeight: 18.5, spacing: -2 }]]) {
    await withGlobals({ window: { getComputedStyle(target) { assert.equal(target, element); return { lineHeight: height, getPropertyValue(name) { assert.equal(name, "--diffs-gap-block"); return gap; } }; } } }, () => {
      const result = measure(element); assert.deepEqual(result, { ...defaults, ...expected }); assert.equal(equal(result, { ...result }), true);
      for (const field of [...Object.keys(defaults), "paddingTop", "paddingBottom"]) assert.equal(equal(result, { ...result, [field]: (result[field] ?? 0) + 1 }), false, field);
    });
  }
});

function hookHost(createRef = () => ({})) {
  const cells = []; let index = 0, effects = [], render, dirty = true, value;
  const changed = (a, b) => !a || a.length !== b.length || a.some((v, i) => !Object.is(v, b[i]));
  const react = {
    createElement: require("react").createElement,
    useRef(initial) { return cells[index++] ??= { current: initial }; },
    useState(initial) {
      const cell = cells[index++] ??= { value: typeof initial === "function" ? initial() : initial };
      cell.set ??= (next) => { const resolved = typeof next === "function" ? next(cell.value) : next; if (!Object.is(resolved, cell.value)) { cell.value = resolved; dirty = true; } };
      return [cell.value, cell.set];
    },
    useMemo(compute, deps) { const i = index++; if (changed(cells[i]?.deps, deps)) cells[i] = { deps, value: compute() }; return cells[i].value; },
    useLayoutEffect(effect, deps) { const i = index++; if (changed(cells[i]?.deps, deps)) effects.push(() => { cells[i]?.cleanup?.(); cells[i] = { deps, effect, cleanup: effect() }; }); },
  };
  function bindRefs(node) {
    if (!node || typeof node !== "object") return;
    if (Array.isArray(node)) { node.forEach(bindRefs); return; }
    if (node.props?.ref) node.props.ref.current ??= createRef(node);
    bindRefs(node.props?.children);
  }
  return {
    react, mount(fn) { render = fn; this.flush(); },
    flush() { for (let count = 0; dirty; count++) { assert.ok(count < 40); dirty = false; index = 0; value = render(); bindRefs(value); const next = effects; effects = []; next.forEach((run) => run()); } return value; },
    update() { dirty = true; return this.flush(); },
    reconnectLayout() { cells.forEach((cell) => { if (cell.effect) cell.cleanup = cell.effect(); }); },
    close() { cells.forEach((cell) => cell.cleanup?.()); },
  };
}

test("readonly adapter preserves raw file identity, upstream file/virtualizer ownership, theme/metrics updates and cleanup without an edit callback", async () => {
  const metrics = support;
  const filename = path.join(app, "src/components/files/FilesReadonlyViewer.tsx"), source = fs.readFileSync(filename, "utf8");
  const nodes = {}, views = [], virtualizers = [], observers = [], events = [], registrations = [];
  const rootHost = hookHost((node) => { const name = node.props["data-review-file-source-metrics-probe"] !== undefined ? "probe" : node.props["data-review-path"] ? "content" : "root"; return nodes[name] ??= { name }; });
  class Virtualizer {
    constructor() { virtualizers.push(this); }
    setup(root, content) { this.root = root; this.content = content; events.push("parent setup"); }
    cleanUp() { events.push("parent cleanup"); }
  }
  class File {
    constructor(options, virtualizer, metrics, worker, managed) { Object.assign(this, { options, virtualizer, metrics, worker, managed }); views.push(this); }
    hydrate(input) { this.initial = input; events.push("file hydrate"); }
    setOptions(value) { this.options = value; } setMetrics(value) { this.metrics = value; }
    render(input) { this.rendered = input; } cleanUp() { events.push("file cleanup"); this.cleaned = (this.cleaned ?? 0) + 1; }
  }
  class Resize { constructor(callback) { this.callback = callback; observers.push(this); } observe(node) { this.node = node; } disconnect() { this.disconnected = true; } }
  const appearance = { darkCodeTheme: "codex", lightCodeTheme: "codex", themeMode: "dark", codeFontSize: 12 };
  let height = "22px";
  const themeRegistry = themeNames;
  const dependencies = { "../../appearance": { useAppearance: () => appearance }, "@octopus/files-diffs": { DIFFS_TAG_NAME: "diffs-container", VirtualizedFile: File, Virtualizer }, "./filesPierreSetup": filesStyles, "./fileSupport": metrics, "./filesCodeStyles": { filesCodeSurfaceCss: "source styles" }, "../settings/octopusDiffThemes": themeRegistry };
  const Wrapper = evaluate(source, filename, { ...dependencies, react: rootHost.react }).default;
  function findFile(node) {
    if (Array.isArray(node)) return node.map(findFile).find(Boolean);
    if (!node || typeof node !== "object") return undefined;
    return typeof node.type === "function" && node.type.name === "ReadonlyFile" ? node : findFile(node.props?.children);
  }
  const props = { documentId: '["root-a","path.txt"]', filename: "path.txt", label: "Readonly", value: "\tα😀e\u0301\r\n尾\t\r\n", wordWrap: false };
  await withGlobals({ ResizeObserver: Resize, window: { getComputedStyle(node) { assert.equal(node, nodes.probe); return { lineHeight: height, getPropertyValue: () => "0px" }; } } }, async () => {
    rootHost.mount(() => Wrapper(props));
    assert.equal(virtualizers.length, 1); assert.equal(virtualizers[0].root, nodes.root); assert.equal(virtualizers[0].content, nodes.content); assert.equal(observers[0].node, nodes.probe);
    let element = findFile(rootHost.flush()); assert.ok(element); assert.equal(element.key, `${props.documentId}:22`);
    let childHost, Child;
    function mountChild() {
      childHost = hookHost(() => ({ name: "diffs-container" }));
      Child = evaluate(`${source}\nexport { ReadonlyFile as AuditReadonlyFile };`, filename, { ...dependencies, react: childHost.react }).AuditReadonlyFile;
      childHost.mount(() => Child(element.props));
    }
    mountChild();
    const first = views[0]; assert.ok(adoptedFilesHosts.includes(first.initial.fileContainer));
    assert.ok(!["createEditor", "onChange", "onItemEditChange"].some((key) => key in first.options), "a readonly view wires no editing");
    const nativeHost = {}; first.options.onPostRender(nativeHost, first, "mount"); assert.equal(adoptedFilesHosts.at(-1), nativeHost);
    const adoptedCount = adoptedFilesHosts.length; first.options.onPostRender(nativeHost, first, "unmount"); assert.equal(adoptedFilesHosts.length, adoptedCount);
    assert.equal(first.initial.file.contents, props.value); assert.equal(sha(first.initial.file.contents), sha(props.value));
    assert.ok(first.initial.file.cacheKey.includes(props.documentId)); assert.equal(first.virtualizer, virtualizers[0]); assert.equal(first.worker, undefined); assert.equal(first.managed, true);
    assert.equal(first.options.enableLineSelection, true); assert.equal(first.options.overflow, "scroll");
    assert.equal(first.initial.fileContainer.name, "diffs-container");
    const key = first.initial.file.cacheKey;
    props.value += "X"; props.wordWrap = true; appearance.themeMode = "light";
    element = findFile(rootHost.update()); childHost.update();
    assert.equal(views.length, 1); assert.equal(first.rendered.file.contents, props.value); assert.notEqual(first.rendered.file.cacheKey, key);
    assert.equal(first.options.overflow, "wrap"); assert.equal(first.options.themeType, "light");
    appearance.darkCodeTheme = "github";
    element = findFile(rootHost.update()); childHost.update();
    assert.equal(first.options.theme.dark, `octopus-codex-dark-${appearance.darkCodeTheme}`);
    height = "27px"; appearance.codeFontSize = 15; element = findFile(rootHost.update());
    assert.ok(observers[0].disconnected); assert.equal(element.key, `${props.documentId}:27`);
    childHost.close(); mountChild(); assert.equal(first.cleaned, 1); assert.equal(views[1].metrics.lineHeight, 27);
    assert.equal(views[1].initial.file.contents, props.value);
    const before = element.props.metrics; observers.at(-1).callback(); element = findFile(rootHost.flush()); assert.equal(element.props.metrics, before);
    props.documentId = '["root-b","path.txt"]'; element = findFile(rootHost.update()); childHost.update();
    assert.equal(views[1].cleaned, 1); assert.equal(views[2].initial.file.contents, props.value);
    assert.ok(views[2].initial.file.cacheKey.includes(props.documentId)); assert.equal(views[2].virtualizer, virtualizers[0]);
    const startReplay = events.length;
    rootHost.close(); childHost.close();
    childHost.reconnectLayout(); rootHost.reconnectLayout();
    assert.deepEqual(events.slice(startReplay), ["parent cleanup", "file cleanup", "file hydrate", "parent setup"]);
    assert.equal(virtualizers.length, 1); assert.equal(views[3].initial.file.contents, props.value); assert.equal(views[3].virtualizer, virtualizers[0]);
    rootHost.close(); childHost.close();
    assert.equal(events.filter((event) => event === "parent cleanup").length, 2); assert.equal(events.filter((event) => event === "file cleanup").length, 4); assert.ok(observers.every((observer) => observer.disconnected));
  });
});

test("editable adapter separates document identity, fresh callbacks, shared theme names and cleanup", async () => {

  const nodes = [], scrolls = [];
  const host = hookHost(() => { const node = Object.assign(new EventTarget(), { scrollTop: 0, scrollLeft: 0 }); nodes.push(node); return node; }), views = [], registrations = [], changes = [];
  class Editor { canUndo = false; canRedo = false; setOptions(options) { this.options = options; } undo() {} redo() {} }
  class View {
    constructor(options) { this.options = options; this.items = []; this.cleanups = 0; views.push(this); }
    setup(container) { this.container = container; } setItems(items) { this.items = items; this.initialItems = structuredClone(items); }
    setOptions(options) { this.options = options; } render() { this.editor ??= this.options.createEditor("file", { onChange() {} }); } cleanUp() { this.cleanups++; this.container.dispatchEvent(new Event("scroll")); }
    scrollTo({ type, position }) { assert.equal(type, "position"); this.container.scrollTop = position; }
    getScrollTop() { return this.container.scrollTop; }
    getItem(id) { return this.items.find((item) => item.id === id); }
    updateItem(item) { this.items = [item]; return true; }
  }
  const appearance = { darkCodeTheme: "codex", lightCodeTheme: "codex", themeMode: "dark", codeFontSize: 12 };
  const filename = path.join(app, "src/components/files/FilesPierreEditor.tsx");
  const Wrapper = evaluate(fs.readFileSync(filename, "utf8"), filename, {
    react: host.react, "../../appearance": { useAppearance: () => appearance }, "@octopus/files-diffs/edit": { Editor },
    "@octopus/files-diffs": { CodeView: View }, "./filesPierreSetup": filesStyles, "./fileSupport": support,
    "./filesCodeStyles": { filesCodeSurfaceCss: "" },
    "../settings/octopusDiffThemes": themeNames, "../IconGlyphs": { Glyph: () => null },
  }).default;
  const original = "\tα😀\r\nlast\r\n";
  const props = { documentId: '["root-a","path.txt"]', filename: "path.txt", label: "Edit", onChange: (value) => changes.push(value), value: original, wordWrap: false,
    initialViewState: { scrollTop: 37, scrollLeft: 11 }, onViewStateChange: (view) => scrolls.push(view) };
  host.mount(() => Wrapper(props));
  try {
    assert.deepEqual(changes, [], "mount must not manufacture a normalized write");
    assert.equal(views[0].initialItems[0].file.contents, original);
    assert.ok(views[0].initialItems[0].file.cacheKey.includes(props.documentId));
    assert.equal(nodes[0].scrollTop, 37); assert.equal(nodes[0].scrollLeft, 11);
    nodes[0].scrollTop = 91; nodes[0].dispatchEvent(new Event("scroll"));
    assert.deepEqual(scrolls, [{ scrollTop: 91, scrollLeft: 11 }]);
    props.onViewStateChange = (view) => scrolls.push({ ...view, fresh: true }); host.update();
    nodes[0].dispatchEvent(new Event("scroll")); assert.equal(scrolls.at(-1).fresh, true);
    const first = views[0], item = first.items[0], edited = { ...item.file, contents: original + "X" };
    assert.deepEqual(first.options.theme, { dark: "octopus-codex-dark-codex", light: "octopus-codex-light-codex" }, "the editor names themes through the shared registry");
    const nativeHost = { shadowRoot: { querySelector: () => ({ setAttribute(name, value) { assert.equal(name, "aria-label"); assert.equal(value, props.label); } }) } };
    first.options.onPostRender(nativeHost, first, "mount"); assert.equal(adoptedFilesHosts.at(-1), nativeHost);
    const adoptedCount = adoptedFilesHosts.length; first.options.onPostRender(nativeHost, first, "unmount"); assert.equal(adoptedFilesHosts.length, adoptedCount);
    first.options.onItemEditChange({ editor: first.editor, file: edited }, item); host.flush();
    assert.deepEqual(changes, [edited.contents]);
    assert.equal(item.file.contents, original, "notifications must not feed edits back into item state");
    props.value = edited.contents; props.onChange = (value) => changes.push(`fresh:${value}`); host.update();
    first.options.onItemEditChange({ editor: first.editor, file: { ...edited, contents: "fresh edit" } }, item); host.flush();
    assert.equal(changes.at(-1), "fresh:fresh edit");
    appearance.codeFontSize = 15; props.wordWrap = true; host.update();
    assert.equal(views.length, 1); assert.equal(first.options.overflow, "wrap"); assert.equal(first.options.itemMetrics.lineHeight, 27);
    const stale = first.options.onItemEditChange;
    props.documentId = '["root-b","path.txt"]'; props.value = "other root\r\n"; host.update();
    assert.equal(first.cleanups, 1); assert.equal(views.length, 2);
    assert.equal(scrolls.length, 2, "cleanup must remove its scroll listener before the engine emits teardown scroll");
    assert.equal(views[1].initialItems[0].file.contents, props.value);
    const count = changes.length; stale({ editor: first.editor, file: { ...edited, contents: "late old edit" } }, item); host.flush(); assert.equal(changes.length, count);
    const current = views[1].items[0]; props.value = "external replacement\n"; host.update();
    assert.equal(views[1].items[0].file.contents, props.value);
    const notified = changes.length;
    views[1].options.onItemEditChange({ editor: views[1].editor, file: views[1].items[0].file }, views[1].items[0]);
    views[1].options.onItemEditChange({ editor: new Editor(), file: { ...views[1].items[0].file, contents: "wrong editor" } }, views[1].items[0]);
    assert.equal(changes.length, notified, "external updates and foreign editor events must not emit writes"); assert.notEqual(views[1].items[0].file.cacheKey, current.file.cacheKey);
    host.close(); assert.equal(views[1].cleanups, 1);
    assert.equal(require("node:events").getEventListeners(nodes[0], "scroll").length, 0);
    views[1].options.onItemEditChange({ editor: views[1].editor, file: { ...edited, contents: "after cleanup" } }, current); assert.equal(changes.length, count);
  } catch (error) { host.close(); throw error; }
});

function fileSaveHarness(options = {}) {
  const filename = path.join(app, "src/components/files/FilesView.tsx"), text = fs.readFileSync(filename, "utf8");
  const source = ts.createSourceFile(filename, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const owner = source.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "FilesView");
  const names = ["saveFile", "openFile", "refreshFile", "refreshFileForced", "setRootId", "paneLocked", "changeDraft", "rememberView"];
  const functions = owner.body.statements.filter((node) => ts.isFunctionDeclaration(node) && names.includes(node.name?.text));
  assert.equal(functions.length, names.length);
  const setters = ["setFile", "setDraft", "setFileLoading"].map((name) => {
    const statement = owner.body.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => item.name.getText(source) === name));
    const declaration = statement.declarationList.declarations.find((item) => item.name.getText(source) === name);
    const initial = declaration.initializer;
    return `const ${name} = ${(ts.isCallExpression(initial) ? initial.arguments[0] : initial).getText(source)};`;
  });
  const effects = owner.body.statements.filter((node) => ts.isExpressionStatement(node) && ts.isCallExpression(node.expression) && node.expression.expression.getText(source) === "useLayoutEffect").map((node) => node.expression.arguments[0]);
  const mounted = effects.find((node) => node.getText(source).includes("mountedRef.current = true"));
  const rootChanged = effects.find((node) => node.getText(source).includes("fileRequestRef.current += 1") && !node.getText(source).includes("mountedRef"));
  const flush = effects.find((node) => node.getText(source).includes("registerFilesPaneFlush"));
  assert.ok(mounted && rootChanged && flush, "read cleanup, root generation and flush must come from the actual FilesView effects");
  const snapshotFile = path.join(app, "src/components/files/filesPaneSnapshot.ts");
  const snapshot = evaluate(fs.readFileSync(snapshotFile, "utf8"), snapshotFile);
  const lifecycleFile = path.join(app, "src/components/files/filesPaneLifecycle.ts");
  const lifecycle = evaluate(fs.readFileSync(lifecycleFile, "utf8"), lifecycleFile, { "./filesPaneSnapshot": snapshot });
  const paneKey = lifecycle.filesPaneKey("live", crypto.randomUUID());
  const file = { rootId: "root-a", path: "path.txt", name: "path.txt", kind: "text", text: "before", digest: sha("before"), modifiedAt: "2026-09-08T00:00:00.000Z", size: 6, truncated: false };
  lifecycle.ensureFilesPane(paneKey);
  lifecycle.updateFilesPane(paneKey, (state) => ({ ...state, rootId: file.rootId, rootPath: "/audit/root-a", file, draft: "sent edit" }));
  const ui = { loading: options.fileLoading ?? false, error: "", toasts: [] }, calls = [], reads = [];
  const roots = [{ id: "root-a", path: "/audit/root-a" }, { id: "root-b", path: "/audit/root-b" }];
  const refs = { fileLoadingRef: { current: ui.loading }, fileRequestRef: { current: 0 }, mountedRef: { current: true }, activeRootRef: { current: file.rootId }, discardContinuationRef: { current: null } };
  let resolve, reject, signalStarted;
  const started = new Promise((done) => { signalStarted = done; });
  const transport = {
    osFileRoots: async () => ({ roots }),
    writeOsFile(request) { calls.push(request); signalStarted(); return new Promise((done, fail) => { resolve = done; reject = fail; }); },
    readOsFile(request) { return new Promise((done, fail) => reads.push({ request, resolve: done, reject: fail })); },
  };
  const base = {
    ...lifecycle, ...refs, paneKey, roots, window: { octopusTransport: transport },
    setFileLoadingState: (value) => { ui.loading = value; }, setFileError: (value) => { ui.error = value; }, failureMessage: (error) => error.message,
    artifactTarget: null, setArtifactRefreshGeneration: () => assert.fail("unexpected artifact refresh"),
    requestDiscard: () => assert.fail("this scenario must settle a dirty draft before replacing its document"),
    appToast: { success: (message) => ui.toasts.push(message), danger: (message) => ui.toasts.push(message) },
  };
  const dynamic = { file, draft: "sent edit", rootId: file.rootId, dirty: true, saving: false, locked: false, fileLoading: ui.loading };
  const create = evaluate(`export function create(deps: any) { const { ${Object.keys({ ...base, ...dynamic }).join(",")} } = deps;
    ${setters.join("\n")} ${functions.map((node) => node.getText(source)).join("\n")}
    return { ${names.join(",")}, mounted: ${mounted.getText(source)}, rootChanged: ${rootChanged.getText(source)}, flush: ${flush.getText(source)} }; }`, filename).create;
  function callbacks() {
    const state = lifecycle.readFilesPane(paneKey), { snapshot: document } = state;
    refs.activeRootRef.current = document.rootId;
    return create({ ...base, file: document.file, draft: document.draft, rootId: document.rootId, dirty: snapshot.filesSnapshotDirty(document), saving: state.saving,
      locked: state.transferPending || state.nativeClosePending, fileLoading: ui.loading });
  }
  let detachRead = callbacks().mounted(), detachFlush = callbacks().flush();
  const update = (change) => lifecycle.updateFilesPane(paneKey, (state) => ({ ...state, ...change }));
  return {
    lifecycle, paneKey, file, calls, reads, roots, refs, started,
    get current() { const { snapshot, error, saving } = lifecycle.readFilesPane(paneKey); return { file: snapshot.file, draft: snapshot.draft, error, saving, toasts: ui.toasts, readError: ui.error }; },
    update, run: () => callbacks().saveFile(), open: (...args) => callbacks().openFile(...args), refresh: () => callbacks().refreshFile(),
    root: (id) => { callbacks().setRootId(id); callbacks().rootChanged(); },
    unmount: () => { detachRead(); detachFlush(); },
    remount: () => { detachRead = callbacks().mounted(); detachFlush = callbacks().flush(); },
    finish: (patch = {}) => resolve({ ...file, text: "sent edit", size: Buffer.byteLength("sent edit"), digest: sha("sent edit"), modifiedAt: "2026-09-08T00:00:01.000Z", outsideOctopusEvidence: true, ...patch }),
    fail: (error) => reject(error),
  };
}

test("save receipt uses original CAS but cannot overwrite later edits", async () => {
  const h = fileSaveHarness(), pending = h.run(); await h.started;
  assert.deepEqual(h.calls[0], { rootId: "root-a", path: "path.txt", text: "sent edit", expectedDigest: sha("before"), expectedModifiedAt: h.file.modifiedAt, expectedSize: 6 });
  h.update({ draft: "typed while save pending" }); h.finish(); assert.equal(await pending, false);
  assert.equal(h.current.draft, "typed while save pending", "late save must preserve newer unsaved draft");
  assert.equal(h.current.file.digest, sha("sent edit"));
});

test("save receipt cannot replace a different document owned by the pane", async () => {
  const h = fileSaveHarness(), pending = h.run(); await h.started;
  h.update({ rootId: "root-b", rootPath: "/audit/root-b", file: { ...h.file, rootId: "root-b", path: "other.txt", digest: sha("other") }, draft: "other document" });
  h.finish(); assert.equal(await pending, false);
  assert.equal(h.current.file.rootId, "root-b"); assert.equal(h.current.draft, "other document");
});

test("save receipt cannot own a refreshed snapshot or same-text document in another root", async () => {
  for (const patch of [{ digest: sha("reloaded") }, { rootId: "root-b" }]) {
    const h = fileSaveHarness(), pending = h.run(); await h.started;
    h.update({ file: { ...h.file, ...patch }, ...(patch.rootId ? { rootId: patch.rootId, rootPath: "/audit/root-b" } : {}) });
    const snapshot = h.current.file; h.finish(); assert.equal(await pending, false);
    assert.equal(h.current.file, snapshot); assert.equal(h.current.draft, "sent edit");
  }
});

test("save receipt updates its unchanged owner and rejects an invalid manual-edit receipt", async () => {
  const h = fileSaveHarness(), pending = h.run(); await h.started; h.finish(); assert.equal(await pending, true);
  assert.equal(h.current.file.digest, sha("sent edit")); assert.equal(h.current.draft, "sent edit");
  const invalid = fileSaveHarness(), failed = invalid.run(); await invalid.started; invalid.finish({ outsideOctopusEvidence: false }); assert.equal(await failed, false);
  assert.equal(invalid.current.file.digest, sha("before")); assert.match(invalid.current.error, /invalid manual-edit receipt/);
});

test("reload waits for save; newer reads, root changes and actual leaf cleanup reject stale read callbacks", async () => {
  const h = fileSaveHarness(), pending = h.run(); await h.started;
  await h.refresh(); await h.open("other.txt", true); assert.equal(h.reads.length, 0);
  h.finish(); assert.equal(await pending, true);
  const reload = h.refresh();
  assert.deepEqual(h.reads[0].request, { rootId: "root-a", path: "path.txt" });
  h.reads[0].resolve({ ...h.current.file, ...h.file, text: "fresh", digest: sha("fresh") }); await reload;
  assert.equal(h.current.draft, "fresh");
  const old = h.open("old.txt", true), newer = h.open("newer.txt", true);
  h.reads[2].resolve({ ...h.file, path: "newer.txt", text: "newer", digest: sha("newer") }); await newer;
  h.reads[1].reject(Error("stale read failure")); await old;
  assert.equal(h.current.file.path, "newer.txt"); assert.equal(h.current.readError, "");
  const previousRoot = h.open("old-root.txt", true);
  h.root("root-b");
  h.reads[3].resolve({ ...h.file, path: "old-root.txt" }); await previousRoot;
  assert.equal(h.current.file, null); assert.equal(h.current.draft, "");
  const retired = h.open("retired.txt", true), oldId = h.lifecycle.readFilesPane(h.paneKey).snapshot.id;
  h.unmount(); assert.equal(await h.lifecycle.requestCloseFilesPanes([h.paneKey]), true); h.lifecycle.forgetFilesPane(h.paneKey);
  h.lifecycle.ensureFilesPane(h.paneKey);
  assert.notEqual(h.lifecycle.readFilesPane(h.paneKey).snapshot.id, oldId);
  h.lifecycle.setFilesPaneError(h.paneKey, "new pane error");
  h.reads[4].resolve({ ...h.file, rootId: "root-b", path: "retired.txt" }); await retired;
  assert.equal(h.current.file, null); assert.equal(h.current.error, "new pane error");
});

test("hidden or unmounted leaf retains real save ownership; close and competing save cannot orphan it", async () => {
  for (const failure of [false, true]) {
    const h = fileSaveHarness(), pending = h.run(); await h.started;
    const other = h.lifecycle.filesPaneKey("live", "other"); h.lifecycle.ensureFilesPane(other); h.lifecycle.setFilesPaneError(other, "other pane error");
    assert.equal(h.current.saving, true);
    h.unmount(); assert.equal(h.current.saving, true);
    assert.equal(await h.lifecycle.requestCloseFilesPanes([h.paneKey]), false);
    assert.throws(() => h.lifecycle.forgetFilesPane(h.paneKey), /save is still in progress/);
    assert.equal(await h.run(), false); assert.equal(h.calls.length, 1);
    if (failure) h.fail(Error("retained write failure")); else h.finish();
    assert.equal(await pending, !failure); h.remount();
    assert.equal(h.current.saving, false); assert.equal(h.current.draft, "sent edit");
    assert.equal(h.current.file.digest, failure ? sha("before") : sha("sent edit"));
    assert.equal(h.current.error, failure ? "retained write failure" : "");
    assert.equal(h.lifecycle.readFilesPane(other).error, "other pane error");
    h.unmount();
  }
});

test("pending file load or remapped admitted root prevents stale save submission", async () => {
  const loading = fileSaveHarness({ fileLoading: true }); assert.equal(await loading.run(), false);
  assert.equal(loading.calls.length, 0); assert.equal(loading.current.saving, false);
  const switched = fileSaveHarness(); switched.roots[0].path = "/audit/remapped-root";
  assert.equal(await switched.run(), false);
  assert.equal(switched.calls.length, 0); assert.equal(switched.current.saving, false);
  assert.equal(switched.current.draft, "sent edit"); assert.match(switched.current.error, /admitted Files root changed/);
});

test("Files language policy keeps original full-name and compound-extension matching", async () => {
  const renderer = await import("@octopus/files-diffs");
  const { fileLanguage } = evaluate(fs.readFileSync(supportPath, "utf8"), supportPath, { "@octopus/files-diffs": renderer });
  for (const [name, expected] of [["file.ts", "typescript"], ["a.component.ts", "angular-ts"], ["template.blade.php", "blade"], ["Dockerfile", "dockerfile"], ["nested/Dockerfile", "text"], ["nested\\Dockerfile", "text"], ["file.odin", "text"], ["build.bazel", "text"], ["BUILD", "text"], ["foo.unknown", "text"]]) assert.equal(fileLanguage(name), expected, name);
});

test("upstream history restores forward and backward selection snapshots", async () => {
  const { TextDocument } = await import("@octopus/files-diffs/edit");
  for (const direction of ["forward", "backward"]) {
    const document = new TextDocument("selection.txt", "α😀\r\nsecond");
    const before = [{ start: { line: 0, character: 0 }, end: { line: 0, character: 3 }, direction }];
    const after = [{ start: { line: 0, character: 1 }, end: { line: 0, character: 1 }, direction: "none" }];
    document.applyEdits([{ range: before[0], newText: "X" }], true, before, after, true);
    assert.equal(document.getText(), "X\r\nsecond");
    assert.deepEqual(document.undo()[1], before);
    assert.equal(document.getText(), "α😀\r\nsecond");
    assert.deepEqual(document.redo()[1], after);
    assert.equal(document.getText(), "X\r\nsecond");
  }
});

test("upstream virtual windows preserve original clipping and overscroll contracts", async () => {
  const { createWindowFromScrollPosition } = await import("@octopus/files-diffs");
  for (const [input, expected] of [
    [{ scrollTop: 0, height: 600, scrollHeight: 10000, overscrollSize: 1000 }, { top: 0, bottom: 1600 }],
    [{ scrollTop: 5000, height: 600, scrollHeight: 10000, overscrollSize: 1000 }, { top: 4000, bottom: 6600 }],
    [{ scrollTop: 9400, height: 600, scrollHeight: 10000, overscrollSize: 200 }, { top: 9200, bottom: 10000 }],
    [{ scrollTop: 50, height: 600, scrollHeight: 300, overscrollSize: 200 }, { top: 50, bottom: 1000 }],
    [{ scrollTop: 500, height: 600, scrollHeight: 10000, overscrollSize: 200, fitPerfectly: true, fitPerfectlyOverscroll: 100 }, { top: 400, bottom: 1300 }],
  ]) assert.deepEqual(createWindowFromScrollPosition(input), expected);
});

test("Files alias isolates package resolution, highlighters and unchanged theme registration", async () => withGlobals({
  navigator: { userAgent: "Files isolation", platform: "MacIntel", maxTouchPoints: 0 },
}, async () => {
  const { createRequire } = require("node:module");
  for (const [name, pierreVersion, shikiVersion] of [["@pierre/diffs", "1.2.12", "4.3.1"], ["@octopus/files-diffs", "1.5.2", "3.20.0"]]) {
    const packagePath = path.join(app, "node_modules", name, "package.json");
    const consumer = createRequire(packagePath);
    assert.equal(JSON.parse(fs.readFileSync(packagePath)).version, pierreVersion);
    assert.equal(JSON.parse(fs.readFileSync(consumer.resolve("shiki/package.json"))).version, shikiVersion);
  }
  assert.equal(JSON.parse(fs.readFileSync(path.join(app, "node_modules/@shikijs/themes/package.json"))).version, "4.3.1");
  const regular = await import("@pierre/diffs"), files = await import("@octopus/files-diffs");
  const filename = path.join(app, "src/components/settings/octopusDiffThemes.ts");
  const dependencies = { "@pierre/diffs": regular };
  for (const kind of ["dark", "light"]) {
    for (const family of ["codex", "raycast"]) {
      const loader = path.join(app, `src/components/settings/code-themes/${kind}-${family}.ts`);
      dependencies[`./code-themes/${kind}-${family}`] = await import(require("node:url").pathToFileURL(loader).href);
    }
  }
  const themes = evaluate(fs.readFileSync(filename, "utf8"), filename, dependencies);
  themes.registerOctopusDiffThemes();
  themes.registerOctopusDiffThemes(files.registerCustomTheme);
  const requested = { langs: ["typescript"], themes: ["codex", "raycast"].flatMap((family) => ["dark", "light"].map((kind) => themes.octopusDiffThemeName(kind, family))) };
  const a = await regular.getSharedHighlighter(requested), b = await files.getSharedHighlighter(requested);
  assert.notEqual(a, b);
  for (const name of requested.themes) assert.deepEqual(b.getTheme(name), a.getTheme(name));
  files.registerCustomTheme("files-isolation-contract", async () => ({ name: "files-isolation-contract", type: "dark", colors: {}, tokenColors: [] }));
  await files.getSharedHighlighter({ ...requested, themes: ["files-isolation-contract"] });
  assert.ok(b.getLoadedThemes().includes("files-isolation-contract"));
  assert.ok(!a.getLoadedThemes().includes("files-isolation-contract"));
}));

test("Files stylesheet ownership changes only its shadow root and reuses the official sheet", async () => {
  const renderer = await import("@octopus/files-diffs");
  const filename = path.join(app, "src/components/files/filesPierreSetup.ts");
  const registrations = [], sheets = [];
  class Sheet { constructor() { sheets.push(this); } replaceSync(css) { this.css = css; } }
  const { adoptFilesStyles } = evaluate(fs.readFileSync(filename, "utf8"), filename, {
    "@pierre/diffs": {}, "@octopus/files-diffs": renderer,
    "../settings/octopusDiffThemes": { registerOctopusDiffThemes: registrar => registrations.push(registrar) },
  }, { CSSStyleSheet: Sheet });
  assert.deepEqual(registrations, [renderer.registerCustomTheme]);
  const document = {}, originalSheet = {}, regular = { ownerDocument: document, shadowRoot: { adoptedStyleSheets: [originalSheet] } };
  const first = { ownerDocument: document, shadowRoot: { adoptedStyleSheets: [originalSheet] } };
  const second = { ownerDocument: document, shadowRoot: { adoptedStyleSheets: [originalSheet] } };
  adoptFilesStyles(first); adoptFilesStyles(second);
  assert.equal(sheets.length, 1); assert.equal(sheets[0].css, renderer.wrapCoreCSS(""));
  assert.deepEqual(regular.shadowRoot.adoptedStyleSheets, [originalSheet]);
  assert.deepEqual(first.shadowRoot.adoptedStyleSheets, [sheets[0]]);
  assert.deepEqual(second.shadowRoot.adoptedStyleSheets, [sheets[0]]);
  const adopted = first.shadowRoot.adoptedStyleSheets; adoptFilesStyles(first);
  assert.equal(first.shadowRoot.adoptedStyleSheets, adopted);
  first.shadowRoot.adoptedStyleSheets = [originalSheet]; adoptFilesStyles(first);
  assert.deepEqual(first.shadowRoot.adoptedStyleSheets, [sheets[0]]);
  adoptFilesStyles({ ownerDocument: {}, shadowRoot: { adoptedStyleSheets: [] } }); assert.equal(sheets.length, 2);
  assert.throws(() => adoptFilesStyles({ ownerDocument: document, shadowRoot: null }), /open shadow root/);
});

test("official dual imports retain the baseline custom-element stylesheet", () => {
  const setup = fs.readFileSync(path.join(app, "src/components/files/filesPierreSetup.ts"), "utf8");
  const parsed = ts.createSourceFile("setup.ts", setup, ts.ScriptTarget.Latest, true);
  const imports = parsed.statements.filter(ts.isImportDeclaration).map(node => node.moduleSpecifier.text);
  assert.deepEqual(imports.slice(0, 2), ["@pierre/diffs", "@octopus/files-diffs"]);
  require("node:child_process").execFileSync(process.execPath, ["--input-type=module", "-e", `
    import assert from "node:assert/strict";
    const registry = new Map();
    Object.defineProperty(globalThis, "navigator", { value: { platform: "MacIntel", userAgent: "stylesheet contract", maxTouchPoints: 0 } });
    globalThis.CSSStyleSheet = class { replaceSync(css) { this.css = css; } };
    globalThis.HTMLElement = class { attachShadow() { return this.shadowRoot = { adoptedStyleSheets: [] }; } };
    globalThis.customElements = { get: name => registry.get(name), define(name, value) { assert.ok(!registry.has(name)); registry.set(name, value); } };
    const regular = await import("@pierre/diffs");
    const baselineClass = registry.get("diffs-container");
    assert.equal(typeof baselineClass, "function");
    const before = new baselineClass(), originalSheet = before.shadowRoot.adoptedStyleSheets[0];
    const files = await import("@octopus/files-diffs");
    assert.equal(registry.get("diffs-container"), baselineClass);
    const after = new baselineClass();
    assert.equal(after.shadowRoot.adoptedStyleSheets[0], originalSheet);
    assert.ok(regular.wrapCoreCSS("").includes(originalSheet.css));
    assert.notEqual(files.wrapCoreCSS(""), regular.wrapCoreCSS(""));
  `], { cwd: app, stdio: "pipe" });
});
