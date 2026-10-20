const assert = require("node:assert/strict");
const path = require("node:path");
const fs = require("node:fs");
const ts = require("typescript");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");
const React = require("react");
const dom = new JSDOM('<div id="header"></div><div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "MouseEvent", "KeyboardEvent", "AbortController", "navigator", "getComputedStyle"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : key === "getComputedStyle" ? w[key].bind(w) : w[key] });
}
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.ResizeObserver = class { observe() {} disconnect() {} };
globalThis.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = () => ({ matches: true, addEventListener() {}, removeEventListener() {} });
w.HTMLElement.prototype.scrollIntoView = function () { this.dataset.scrolled = "true"; };
w.HTMLElement.prototype.getBoundingClientRect = function () {
  if (this.classList.contains("chat-tooltip")) {
    const left = Number.parseFloat(this.style.left) - 50, top = Number.parseFloat(this.style.top);
    return { x: left, y: top, left, top, right: left + 100, bottom: top + 20, width: 100, height: 20 };
  }
  return { x: 0, y: 0, left: 0, top: 0, right: 1024, bottom: 768, width: 1024, height: 768 };
};
const capture = new WeakMap();
w.HTMLElement.prototype.setPointerCapture = function (id) { capture.set(this, id); };
w.HTMLElement.prototype.hasPointerCapture = function (id) { return capture.get(this) === id; };
w.HTMLElement.prototype.releasePointerCapture = function () { capture.delete(this); };
const { createRoot } = require("react-dom/client");
const { useMotionValue } = require("motion/react");
const compiled = build({
  stdin: { contents: "import './src/components/icon-glyphs/shell'; export * from './src/components/shell/WorkspaceSurfacePane'; export * from './src/components/shell/WorkspacePaneHeaderActions'; export * from './src/components/shell/paneTabs'; export * from './src/components/shell/WorkspacePaneManager'; export * from './src/components/shell/WindowPaneControls'; export * from './src/components/shell/PanelResizeHandle'; export * from './src/components/shell/useClientPaneWindowDrag';", resolveDir: path.resolve(__dirname, ".."), loader: "tsx" },
  bundle: true, platform: "node", format: "cjs", write: false, loader: { ".css": "empty" }, external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"], logLevel: "silent",
}).then(result => { const module = { exports: {} }; Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports); return module.exports; });
const errors = [];
w.addEventListener("error", event => errors.push(event.error));
const click = async element => React.act(() => element.click());
const flush = async () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 40)); });
after(() => { assert.deepEqual(errors, []); dom.window.close(); });

test("the window header owns pane tabs and the Tools surface retains content, selection and focus", async () => {
  const { WorkspaceSurfacePane, WorkspacePaneManager, WindowPaneControlsProvider, WorkspacePaneHeaderActions, useClientPaneWindowDrag, usePaneTabs } = await compiled;
  const actions = [], closed = [], reorders = [], transfers = [], focusChanges = [];
  window.octopusTransport = { onClientPaneDragState: () => () => {}, beginClientPaneDrag: async () => {}, finishClientPaneDrag: async () => {}, cancelClientPaneDrag: async () => {} };
  const browserSource = ts.createSourceFile("browser.tsx", fs.readFileSync(path.join(__dirname, "../src/components/worker-environment/RunEnvironmentBrowserSurfaces.tsx"), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const geometrySource = browserSource.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "nativeBrowserGeometry");
  assert(geometrySource);
  const geometry = Function("window", ts.transpileModule(`${geometrySource.getText(browserSource)}\nreturn nativeBrowserGeometry;`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText)(window);
  let mounts = 0;
  function Content() {
    React.useEffect(() => { mounts++; }, []);
    return React.createElement(React.Fragment, null, React.createElement("input", { id: "retained", defaultValue: "unsaved text" }),
      React.createElement(WorkspacePaneHeaderActions, null, React.createElement("button", { id: "surface-action", type: "button" }, "Share")));
  }
  function Harness() {
    const width = useMotionValue(0);
    const model = usePaneTabs({ tabs: [{ id: "a", title: "Files", glyph: "folder", showWhenSingle: true }, { id: "b", title: "Terminal", glyph: "terminal", showWhenSingle: true }], activeTabId: "a" });
    const { tabs, activeTabId, selectTab: select } = model;
    const drag = useClientPaneWindowDrag({ begin: id => { transfers.push(id); return { transferId: id }; }, onError: error => errors.push(error), onReorder: (...ids) => reorders.push(ids) });
    return React.createElement(WindowPaneControlsProvider, { host: document.getElementById("header") }, React.createElement(WorkspacePaneManager, {
      primary: "Primary", secondaryOpen: true, secondaryFocused: false, secondaryWidthPercent: 40, resolvedWidthMotion: width,
      onOpenChange() {}, onSecondaryFocusedChange() {}, onSecondaryWidthPercentChange() {},
      secondary: React.createElement(WorkspaceSurfacePane, {
        controlsInWindowHeader: true, activeTabId, tabs, focused: false, onFocusedChange: (next) => focusChanges.push(next),
        addActions: [{ id: "files", title: "Files", glyph: "folder" }, { id: "terminal", title: "Terminal", glyph: "terminal", shortcut: "⌃`" }],
        onNewTab() { model.openTab({ id: crypto.randomUUID(), title: "New tab", glyph: "browser", launcher: true, showWhenSingle: true }); },
        onAddAction(id) { actions.push(id); model.openTab(tabs.find(tab => tab.id === (id === "files" ? "a" : "b")), activeTabId); },
        canNativeDrag: tab => !tab.launcher,
        onClose() { closed.push("pane"); }, onTabSelect: select,
        onTabClose(id) { closed.push(id); model.closeTab(id); },
        onCloseOtherTabs() {}, onCloseTabsToRight() {}, onTabReorder: (...ids) => reorders.push(ids), onTabWindowPointerDown: (tab, event) => drag(tab.id, event),
      }, React.createElement(Content)),
    }));
  }
  const root = createRoot(document.getElementById("root"));
  await React.act(() => root.render(React.createElement(Harness))); await flush();
  const header = document.getElementById("header"), content = document.getElementById("retained");
  assert.equal(header.querySelectorAll(".workspace-surface-pane-header").length, 1);
  assert.ok(document.querySelector(".workspace-surface-pane .workspace-surface-pane-header") === null);
  assert.equal(header.querySelector(".pane-tabs-scroll .workspace-pane-tab-add"), null);
  const controls = header.querySelector('[data-pane-manager-controls="true"]');
  assert.ok(controls.dataset.paneManagerId);
  assert.equal(header.querySelector("#surface-action").closest(".workspace-pane-header-actions") !== null, true, "a surface's header actions render in the window header");
  assert.ok(!document.querySelector(".workspace-surface-pane-content #surface-action"));
  const focus = header.querySelector(".workspace-surface-focus");
  assert.equal(focus.getAttribute("aria-label"), "Focus this workspace");
  await click(focus);
  assert.deepEqual(focusChanges, [true]);
  const add = () => header.querySelector('.workspace-pane-tab-add button[aria-label="New tab"]');
  await click(add()); await flush();
  assert(document.getElementById("retained") === content);
  assert(content.closest("[hidden][inert]"));
  assert.equal(geometry(content, { emulatedSize: null }).visible, false);
  assert.equal(header.querySelector('[role="tab"][aria-selected="true"]').textContent, "New tab");
  assert.equal(document.querySelectorAll(".workspace-pane-launcher-item").length, 2);
  assert.equal(document.activeElement.getAttribute("aria-label"), "Files");
  const firstLauncherId = header.querySelector('[role="tab"][aria-selected="true"]').closest('[data-pane-tab-id]').dataset.paneTabId;
  await click(add()); await flush();
  assert.equal(header.querySelectorAll('[role="tab"][aria-label="New tab"]').length, 2);
  await click(header.querySelector(`[data-pane-tab-id="${firstLauncherId}"] [role="tab"]`));
  await click(header.querySelector('[aria-label="Close New tab tab"]')); await flush();
  assert.equal(header.querySelectorAll('[role="tab"][aria-label="New tab"]').length, 1);
  assert.equal(header.querySelector('[role="tab"][aria-selected="true"]').textContent, 'New tab');
  const launcherTab = header.querySelector('[role="tab"][aria-selected="true"]');
  document.elementFromPoint = () => launcherTab;
  const pointer = async (target, type, x) => {
    const event = new w.MouseEvent(type, { button: 0, clientX: x, clientY: 10, bubbles: true, cancelable: true });
    Object.defineProperties(event, { pointerId: { value: 1 }, isPrimary: { value: true } });
    await React.act(() => target.dispatchEvent(event));
  };
  await pointer(launcherTab, "pointerdown", 5); await pointer(window, "pointermove", 40); await pointer(window, "pointerup", 40);
  assert.deepEqual(transfers, []);
  await pointer(header.querySelector('[role="tab"][aria-label="Files"]'), "pointerdown", 5);
  await pointer(window, "pointermove", 40); await flush(); await pointer(window, "pointerup", 40); await flush();
  assert.deepEqual(transfers, ["a"]); assert.deepEqual(reorders, []);
  await click(document.querySelector('.workspace-pane-launcher-item[aria-label="Terminal"]')); await flush();
  assert.deepEqual(actions, ["terminal"]);
  assert.equal(header.querySelector('[role="tab"][aria-selected="true"]').textContent, "Terminal");
  assert.ok(document.querySelector(".workspace-pane-launcher") === null);
  assert.equal(content.closest("[hidden]"), null);
  assert.equal(geometry(content, { emulatedSize: null }).visible, true);
  await click(add()); await click(header.querySelector('[aria-label="Close New tab tab"]')); await flush();
  assert.equal(closed.length, 2);
  assert.equal(document.activeElement.getAttribute("aria-label"), "Terminal");
  await click(add());
  await React.act(() => document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })));
  await flush();
  assert.ok(document.querySelector(".workspace-pane-launcher") === null);
  assert(document.activeElement === content.closest(".workspace-surface-pane-content"));
  assert.equal(content.value, "unsaved text"); assert.equal(mounts, 1);
  const selected = header.querySelector('[role="tab"][aria-selected="true"]');
  await React.act(() => selected.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true, cancelable: true }))); await flush();
  assert.equal(document.activeElement.getAttribute("aria-label"), "Files");
  await React.act(() => document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: "Delete", bubbles: true, cancelable: true }))); await flush();
  assert.equal(closed.length, 4);
  assert.equal(closed.at(-1), "a");
  assert.equal(document.activeElement.getAttribute("aria-label"), "Terminal");
  const closedBefore = closed.length;
  await React.act(() => header.querySelector('[role="tab"][aria-label="Terminal"]').dispatchEvent(new w.MouseEvent("auxclick", { button: 1, bubbles: true, cancelable: true }))); await flush();
  assert.deepEqual(closed.slice(closedBefore), ["b"], "a middle click closes the tab");
  await React.act(() => root.unmount());
  delete window.octopusTransport;
});

test("resize cancellation uses the last captured position and ignores unrelated pointers", async () => {
  const { PanelResizeHandle } = await compiled;
  const sizes = [], commits = [], opened = [], resizing = [];
  const root = createRoot(document.getElementById("root"));
  await React.act(() => root.render(React.createElement(PanelResizeHandle, {
    ariaLabel: "Resize", defaultSize: 360, edge: "right", maximum: 700, minimum: 240, value: 400,
    onChange: value => sizes.push(value), onCommit: value => commits.push(value), onOpenChange: value => opened.push(value), onResizingChange: value => resizing.push(value),
  })));
  const handle = document.querySelector('[role="separator"]');
  async function pointer(type, clientX, pointerId = 1) {
    const event = new w.MouseEvent(type, { clientX, button: 0, bubbles: true, cancelable: true });
    Object.defineProperties(event, { pointerId: { value: pointerId }, isPrimary: { value: pointerId === 1 } });
    await React.act(() => (type === "pointerdown" ? handle : window).dispatchEvent(event));
  }
  await pointer("pointerdown", 500); await pointer("pointermove", 900, 2); await pointer("pointerup", 900, 2);
  assert.deepEqual(sizes, []); assert.deepEqual(commits, []);
  await pointer("pointermove", 600); await pointer("pointercancel", 0);
  assert.equal(commits.at(-1), 500); assert.equal(opened.at(-1), true); assert.equal(resizing.at(-1), false);
  assert.equal(handle.hasPointerCapture(1), false);
  await pointer("pointerdown", 500); await pointer("pointermove", 550); await pointer("lostpointercapture", 0);
  assert.equal(commits.at(-1), 450); assert.equal(resizing.at(-1), false);
  await React.act(() => handle.dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true, cancelable: true })));
  assert.equal(commits.at(-1), 700);
  await React.act(() => handle.dispatchEvent(new w.MouseEvent("dblclick", { bubbles: true, cancelable: true })));
  assert.equal(commits.at(-1), 360);
  await React.act(() => root.unmount());
});

test("the Tools launcher exposes overflow actions and an explicit empty projection", async () => {
  const { WorkspacePaneLauncher } = await compiled;
  const selected = [], actions = Array.from({ length: 8 }, (_, index) => ({ id: `surface-${index}`, title: `Surface ${index}`, glyph: "folder", section: index < 6 ? "workspace" : "surfaces", moreOnly: index === 0 }));
  const root = createRoot(document.getElementById("root"));
  await React.act(() => root.render(React.createElement(WorkspacePaneLauncher, { actions, onAction: id => selected.push(id) })));
  assert.equal(document.querySelectorAll(".workspace-pane-launcher-item").length, 6);
  assert.ok(document.querySelector('.workspace-pane-launcher-item[aria-label="Surface 0"]') === null);
  await click(document.querySelector('.workspace-pane-launcher-item[aria-haspopup="menu"]')); await flush();
  const menu = document.querySelector('[role="menu"][aria-label="More tools"]');
  assert(menu);
  assert.equal(menu.querySelectorAll('[role="menuitem"]').length, 3);
  await click(menu.querySelectorAll('[role="menuitem"]')[2]); await flush();
  assert.deepEqual(selected, ["surface-0"]);
  await React.act(() => root.render(React.createElement(WorkspacePaneLauncher, { actions: [], onAction: id => selected.push(id) })));
  assert.equal(document.querySelectorAll(".workspace-pane-launcher-item").length, 0);
  assert.match(document.querySelector(".workspace-pane-launcher-empty").textContent, /No tabs are available/);
  await React.act(() => root.unmount());
});
