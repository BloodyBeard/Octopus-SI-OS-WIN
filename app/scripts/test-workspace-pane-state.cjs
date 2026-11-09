const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const ts = require("typescript");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const app = path.resolve(__dirname, "..");
const source = ts.createSourceFile("OperatorShell.tsx", fs.readFileSync(path.join(app, "src/components/shell/OperatorShell.tsx"), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
function callback(name, environment) {
  let expression;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(source) === name) expression = ts.isCallExpression(node.initializer) ? node.initializer.arguments[0] : node.initializer;
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) expression = node;
    ts.forEachChild(node, visit);
  }
  visit(source);
  assert(expression, name);
  return Function(...Object.keys(environment), ts.transpileModule(`return (${expression.getText(source)});`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText)(...Object.values(environment));
}

test("exact chats restore pane tabs, contents and layout through A to B to A", async () => {
  const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
  const globals = new Map();
  function install(key, value) { globals.set(key, Object.getOwnPropertyDescriptor(globalThis, key)); Object.defineProperty(globalThis, key, { configurable: true, value, writable: true }); }
  const w = dom.window;
  for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "MouseEvent", "KeyboardEvent", "navigator", "getComputedStyle"]) install(key, key === "window" ? w : key === "getComputedStyle" ? w[key].bind(w) : w[key]);
  install("IS_REACT_ACT_ENVIRONMENT", true);
  install("requestAnimationFrame", w.requestAnimationFrame.bind(w));
  install("cancelAnimationFrame", w.cancelAnimationFrame.bind(w));
  install("ResizeObserver", class { observe() {} disconnect() {} });
  w.matchMedia = () => ({ matches: true, addEventListener() {}, removeEventListener() {} });
  w.HTMLElement.prototype.scrollIntoView = function () {};
  w.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 0, y: 0, left: 0, top: 0, right: 1200, bottom: 800, width: 1200, height: 800 });
  const errors = [];
  w.addEventListener("error", event => errors.push(event.error));
  const React = require("react"), { createRoot } = require("react-dom/client"), { useMotionValue } = require("motion/react");
  let root;
  try {
    const bundle = await build({
      stdin: { contents: `import './src/components/icon-glyphs/shell'; export * from './src/components/shell/useWorkspacePaneState'; export * from './src/components/shell/WorkspacePaneManager'; export * from './src/components/shell/WorkspaceSurfacePane'; export * from './src/components/shell/operatorShellState'; export * from './src/components/files/RetainedFilesTabs'; export * from './src/components/files/filesPaneLifecycle'; export * from './src/shell/paneTransferState';`, resolveDir: app, loader: "tsx" },
      bundle: true, platform: "node", format: "cjs", write: false, loader: { ".css": "empty" },
      external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"], logLevel: "silent",
      plugins: [{ name: "file-content-fixture", setup(b) {
        b.onLoad({ filter: /\/FilesView\.tsx$/ }, () => ({ loader: "tsx", contents: `
          import * as React from 'react';
          import { ensureFilesPane, readFilesPane, subscribeFilesPane } from ${JSON.stringify(path.join(app, "src/components/files/filesPaneLifecycle.ts"))};
          export function FilesView({ paneKey, target, snapshot }) {
            ensureFilesPane(paneKey, target, snapshot);
            const state = React.useSyncExternalStore(listener => subscribeFilesPane(paneKey, listener), () => readFilesPane(paneKey));
            return <output data-file-content={state.snapshot.target?.path}>{state.snapshot.draft}</output>;
          }` }));
      } }],
    });
    const module = { exports: {} };
    Function("require", "module", "exports", bundle.outputFiles[0].text)(require, module, module.exports);
    const api = module.exports;
    const defaults = { activePane: "primary", focused: false, sidebarOpen: true, sidebarWidth: 288, widthPercent: 34 };
    let controls, width;
    const observed = [];
    const activeFileContent = () => [...document.querySelectorAll("[data-file-content]")].find(element => !element.closest('[hidden], [inert], [aria-hidden="true"]'));
    function Fixture({ home = "home-a", profile = "live", windowScope = "main" }) {
      const [navigation, setNavigation] = React.useState({ entries: [api.clientDestination("chat", "A")], index: 0 });
      const destination = navigation.entries[navigation.index];
      const scope = JSON.stringify([home, destination.surfaceId, destination.runId ?? ""]);
      const key = api.secondaryPaneStorageKey(profile, "home-run", windowScope, scope);
      const initial = React.useMemo(() => { const stored = api.readSecondaryPaneState(profile, "home-run", windowScope, scope); return { ...stored, layout: stored.layout ?? defaults }; }, [key]);
      const pane = api.useWorkspacePaneState(key, initial);
      const model = pane.tabs;
      width = useMotionValue(0);
      React.useEffect(() => api.writeSecondaryPaneState(profile, "home-run", windowScope, pane.state, scope), [pane.state, key]);
      const workspace = { activeRunId: destination.runId ?? "", runs: [], selectRun() {}, createChat() {} };
      const selectSurface = callback("selectSurface", { visibleSurfaces: [{ id: "chat" }, { id: "voice" }], chatWorkspace: workspace, selectDestination: next => setNavigation(current => ({ entries: [...current.entries.slice(0, current.index + 1), next], index: current.index + 1 })), setActivePane: pane.setActivePane, clientDestination: api.clientDestination, appToast: { warning: assert.fail } });
      const openChat = callback("openChat", { chatWorkspace: workspace, projectFilterEeId: "", selectSurface, setSecondaryPaneOpen: pane.setOpen, setSecondaryPaneFocused: pane.setFocused });
      const history = delta => callback("navigateSurfaceHistory", { surfaceNavigation: navigation, chatWorkspace: workspace, setSurfaceNavigation: setNavigation, setSecondaryPaneOpen: pane.setOpen, setSecondaryPaneFocused: pane.setFocused, setActivePane: pane.setActivePane, setQuery() {}, setSearchFocusTick() {} })(delta);
      const selected = model.tabs.find(tab => tab.id === model.activeTabId);
      controls = { pane, model, key, openChat, selectSurface, history, run: destination.runId,
        openFile(id, file) { model.openTab({ id, target: { kind: "files", workspaceKey: "octopus-os", openTarget: { kind: "file", rootId: "docs", path: file } } }); pane.setOpen(true); },
        draft(text) { api.updateFilesPane(api.filesPaneKey(profile, model.activeTabId, key), snapshot => ({ ...snapshot, draft: text })); },
      };
      React.useLayoutEffect(() => { observed.push([destination.runId, activeFileContent()?.getAttribute("data-file-content")]); });
      return React.createElement("div", { "data-chat": destination.runId },
        React.createElement("nav", { "data-sidebar": true, hidden: !pane.state.layout.sidebarOpen, style: { width: pane.state.layout.sidebarWidth } }, "Tasks"),
        React.createElement(api.WorkspacePaneManager, {
          key, primary: React.createElement("span", null, destination.runId), secondaryOpen: pane.state.open,
          secondaryFocused: pane.state.layout.focused, secondaryWidthPercent: pane.state.layout.widthPercent, resolvedWidthMotion: width,
          onOpenChange: pane.setOpen, onSecondaryFocusedChange: pane.setFocused, onSecondaryWidthPercentChange: pane.setWidthPercent,
          secondary: React.createElement(api.WorkspaceSurfacePane, {
            activeTabId: model.activeTabId, tabs: model.tabs.map(tab => ({ id: tab.id, title: tab.id, glyph: "folder", showWhenSingle: true })),
            focused: pane.state.layout.focused, onFocusedChange: pane.setFocused, addActions: [], onAddAction() {}, onClose: () => pane.setOpen(false),
            onTabSelect: model.selectTab, onTabClose: model.closeTab, onCloseOtherTabs: model.closeOtherTabs, onCloseTabsToRight: model.closeTabsToRight, onTabReorder: model.reorderTabs,
          }, selected?.target.kind === "files" ? React.createElement(api.RetainedFilesTabs, { accessProfile: profile, active: pane.state.open, activeTabId: model.activeTabId, paneStateKey: key, tabs: model.tabs }) : React.createElement("output", null, selected?.id)),
        }));
    }
    root = createRoot(document.getElementById("root"));
    const act = fn => React.act(async () => { await fn(); await new Promise(resolve => setTimeout(resolve, 20)); });
    const render = props => act(() => root.render(React.createElement(React.StrictMode, null, React.createElement(Fixture, props))));
    const content = () => activeFileContent()?.textContent;
    await render();
    await act(() => controls.openFile("doc", "A.md"));
    await act(() => controls.draft("unsaved A"));
    await act(() => controls.openFile("notes", "A-notes.md"));
    await act(() => { controls.model.reorderTabs("notes", "doc"); controls.model.selectTab("doc"); controls.pane.setWidthPercent(42); controls.pane.setSidebarOpen(false); controls.pane.setSidebarWidth(356); });
    const keyA = controls.key;
    assert.equal(content(), "unsaved A");
    assert.equal(document.querySelector('[aria-label="Resize secondary pane"]').getAttribute("aria-valuenow"), "504");
    await act(() => controls.openChat("B"));
    assert.equal(document.querySelector(".workspace-pane-secondary"), null);
    assert.equal(controls.pane.state.layout.sidebarOpen, true);
    await act(() => controls.openFile("doc", "B.md"));
    await act(() => { controls.draft("unsaved B"); controls.pane.setWidthPercent(55); });
    const keyB = controls.key;
    assert.notEqual(keyA, keyB);
    await act(() => controls.openChat("A"));
    assert.equal(content(), "unsaved A");
    assert.deepEqual(controls.model.tabs.map(tab => tab.id), ["notes", "doc"]);
    assert.equal(controls.model.activeTabId, "doc");
    assert.equal(document.querySelectorAll('[role="tab"]').length, 2);
    assert.equal(document.querySelector('[data-sidebar]').hidden, true);
    assert.equal(document.querySelector('[data-sidebar]').style.width, "356px");
    assert.equal(document.querySelector('[aria-label="Resize secondary pane"]').getAttribute("aria-valuenow"), "504");
    const sourcePane = controls.pane;
    const owner = { key: keyA, read: sourcePane.getState, write: state => sourcePane.setStateAt(keyA, state) };
    const staged = { id: "shared-chat", target: { kind: "surface", surfaceId: "chat", runId: "X", workspaceKey: "octopus-os" } };
    const pending = { owner, previousActiveTabId: "doc", previousActivePane: "primary", previousFocused: false, previousOpen: true, previousTabs: controls.model.tabs, tabId: staged.id, stagedTab: staged };
    await act(() => controls.model.openTab(staged));
    await act(() => controls.openChat("B"));
    const rollback = callback("rollbackAcceptedPaneTransfer", { pendingAcceptedPaneTransfersRef: { current: new Map([["rollback-a", pending]]) },
      rollbackDestinationPaneTransfer: api.rollbackDestinationPaneTransfer, writeSecondaryPaneStateAtKey: api.writeSecondaryPaneStateAtKey,
      clientWindow: { scope: "main" }, accessProfile: "live", appToast: { danger: assert.fail } });
    await act(() => rollback("rollback-a"));
    assert.equal(content(), "unsaved B");
    await act(() => controls.openChat("A"));
    assert.deepEqual(controls.model.tabs.map(tab => tab.id), ["notes", "doc"]);
    await act(() => controls.model.openTab(staged));
    const clientWindow = { windowId: "main", scope: "main", primary: true };
    const completion = { sourceWindowId: "main", transferId: "move-from-a", sourceTabId: staged.id, sourcePane: "secondary", target: staged.target };
    let acknowledgements = 0;
    const bridge = { beginClientPaneTransfer: () => completion, acknowledgeClientPaneSourceFinalization: async () => { acknowledgements++; }, windowControl: assert.fail };
    w.octopusTransport = bridge;
    callback("beginPaneTransfer", { nativeWindowCloseRequestRef: { current: null }, clientWindow, paneStateKey: keyA })(staged.id, staged.target, "secondary");
    await act(() => controls.openChat("B"));
    await act(() => controls.model.openTab(staged));
    const transferPaneOwner = callback("transferPaneOwner", { workspacePane: controls.pane, clientWindow, readSecondaryPaneStateAtKey: api.readSecondaryPaneStateAtKey });
    const sourceTransferMatches = callback("sourceTransferMatches", { clientWindow, transferPaneOwner,
      transferSourceStateRef: { current: { paneStateKey: keyB, secondaryTabs: controls.model.tabs, activeSecondaryTabId: staged.id, primaryRunIdentityReady: true, secondaryRunIdentityReady: true } },
      transferableWorkspacePaneTarget: target => target, samePaneTransferTarget: callback("samePaneTransferTarget", {}), paneTransferBlockReasonRef: { current: () => "" } });
    assert.equal(sourceTransferMatches(completion), false);
    const finishSource = callback("finishSource", { bridge, clientWindow, transferPaneOwner, sourceTransferMatches,
      finalizingSourceIdsRef: { current: new Set() }, preparedSourceTransfersRef: { current: new Map() },
      setSourceFinalizationPendingCount() {}, setSourceHandoffPending() {}, writeSecondaryPaneStateAtKey: api.writeSecondaryPaneStateAtKey,
      getWorkspacePaneStates: controls.pane.getStates, appToast: { warning: assert.fail, danger: assert.fail } });
    await act(() => finishSource(completion));
    assert.equal(acknowledgements, 1);
    assert.equal(controls.model.tabs.some(tab => tab.id === staged.id), true);
    await act(() => controls.model.selectTab("doc"));
    await act(() => controls.openChat("A"));
    assert.deepEqual(controls.model.tabs.map(tab => tab.id), ["notes", "doc"]);
    assert.equal(content(), "unsaved A");
    await act(() => controls.pane.setFocused(true));
    await act(() => controls.openChat("B"));
    assert.equal(content(), "unsaved B");
    assert.equal(controls.pane.state.layout.focused, false);
    await act(() => controls.history(-1));
    assert.equal(content(), "unsaved A");
    assert.equal(document.querySelector(".workspace-pane-manager").classList.contains("is-secondary-focused"), true);
    await act(() => controls.pane.setOpen(false));
    await act(() => controls.openChat("B"));
    await act(() => controls.openChat("A"));
    assert.equal(document.querySelector(".workspace-pane-secondary"), null);
    assert.equal(controls.model.tabs.length, 2);
    await act(() => controls.pane.setOpen(true));
    assert.equal(content(), "unsaved A");
    await act(() => controls.model.openTab({ id: "terminal-live", target: { kind: "terminal", terminalId: "live-process", title: "Terminal", workspaceKey: "octopus-os" } }));
    await act(() => controls.model.openTab({ id: "sources-live", target: { kind: "task_sources", target: { runId: "A", sources: [] }, workspaceKey: "octopus-os" } }));
    await act(() => controls.openChat("B"));
    await act(() => controls.openChat("A"));
    assert.equal(controls.model.activeTabId, "sources-live");
    assert.deepEqual(controls.model.tabs.map(tab => tab.id), ["notes", "doc", "terminal-live", "sources-live"]);
    assert.equal(document.querySelectorAll('[role="tab"]').length, 4);
    await act(() => root.unmount());
    root = createRoot(document.getElementById("root"));
    await render();
    assert.deepEqual(controls.model.tabs.map(tab => tab.id), ["notes", "doc"]);
    assert.equal(controls.model.activeTabId, "notes");
    assert.equal(controls.pane.state.layout.widthPercent, 42);
    assert.equal(controls.pane.state.layout.sidebarOpen, false);
    await render({ windowScope: "peer-a" });
    assert.equal(document.querySelector(".workspace-pane-secondary"), null);
    await render({ home: "home-b" });
    assert.equal(controls.model.tabs.length, 0);
    await render({ profile: "lab" });
    assert.equal(controls.model.tabs.length, 0);
    assert.equal(observed.some(([chat, file]) => chat === "A" && file === "B.md" || chat === "B" && file?.startsWith("A")), false);
    assert.deepEqual(errors, []);
  } finally {
    if (root) await React.act(() => root.unmount());
    dom.window.close();
    for (const [key, descriptor] of globals) if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key];
  }
});
