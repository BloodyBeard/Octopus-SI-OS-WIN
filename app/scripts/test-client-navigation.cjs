const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createRequire } = require("node:module");
const { test, after } = require("node:test");
const ts = require("typescript");

const appRoot = path.resolve(__dirname, "..");
test("app document navigation is blocked while the shell still loads programmatically", () => {
  const tree = ts.createSourceFile("main.cjs", fs.readFileSync(path.join(appRoot, "electron/main.cjs"), "utf8"), ts.ScriptTarget.Latest, true);
  const source = tree.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "loadRendererWindow").getText(tree);
  const loadRenderer = Function("path", "__dirname", "isDev", `${source}; return loadRendererWindow;`)(path, path.join(appRoot, "electron"), false);
  const handlers = new Map(), loads = [];
  loadRenderer({ webContents: { on: (name, handler) => handlers.set(name, handler) }, loadFile: (...args) => loads.push(args) });
  assert.equal(loads[0][0], path.join(appRoot, "dist/index.html"));
  let prevented = false;
  handlers.get("will-navigate")({ preventDefault() { prevented = true; } }, "file:///workspace/report.md");
  assert.equal(prevented, true);
});
function load(relative, mocks = {}) {
  const filename = path.join(appRoot, relative);
  const code = ts.transpileModule(fs.readFileSync(filename, "utf8"), {
    fileName: filename,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} }, localRequire = createRequire(filename);
  Function("module", "exports", "require", code)(module, module.exports, (id) => id in mocks ? mocks[id] : localRequire(id));
  return module.exports;
}
const workspaces = load("src/shell/clientWorkspaces.ts");
const navigation = load("src/shell/navigation.ts", { "./clientWorkspaces": workspaces });
const windows = load("src/shell/clientWindowModel.ts", { "../components/files/filesPaneSnapshot": {} });
const state = load("src/components/shell/operatorShellState.ts", {
  "../../clientActions": { clientCommands: [] },
  "../../shell/clientWorkspaces": workspaces,
  "../../shell/navigation": navigation,
  "../../shell/clientWindowModel": windows,
  "../files/filesPaneSnapshot": {},
  "./paneTabs": load("src/components/shell/paneTabs.ts"),
});
const surfaces = navigation.clientNavigationDestinations.map(({ surfaceId }) => ({
  id: surfaceId, placements: surfaceId === "dashboard" ? ["fullscreen"] : ["home"],
})).concat([
  { id: "scheduled", placements: ["home"] },
  { id: "registry", placements: ["fullscreen"] },
  { id: "custom-home", placements: ["home"] },
  { id: "custom-dashboard", placements: ["fullscreen"] },
]);

const operator = ts.createSourceFile("OperatorShell.tsx", fs.readFileSync(path.join(appRoot, "src/components/shell/OperatorShell.tsx"), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
function callback(name, environment) {
  let expression;
  const visit = (node) => {
    if (ts.isVariableDeclaration(node) && node.name.getText(operator) === name && ts.isCallExpression(node.initializer)) expression = node.initializer.arguments[0];
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) expression = node;
    ts.forEachChild(node, visit);
  };
  visit(operator);
  assert.ok(expression, `Missing callback ${name}`);
  const code = ts.transpileModule(`return (${expression.getText(operator)});`, {
    compilerOptions: { target: ts.ScriptTarget.ES2022 },
  }).outputText;
  return Function(...Object.keys(environment), code)(...Object.values(environment));
}

test("one history crosses destinations and keeps the selected duplicate after invalid entries disappear", () => {
  const current = {
    entries: [state.clientDestination("voice"), state.clientDestination("chat", "task-a"), state.clientDestination("missing"), state.clientDestination("voice"), state.clientDestination("chat", "deleted"), state.clientDestination("inbox")],
    index: 3,
  };
  const normalized = state.normalizeSurfaceNavigation(current, surfaces, new Set(["task-a"]));
  assert.deepEqual(normalized, {
    entries: [{ surfaceId: "voice" }, { surfaceId: "chat", runId: "task-a" }, { surfaceId: "voice" }, { surfaceId: "inbox" }], index: 2,
  });
  assert.equal(state.normalizeSurfaceNavigation(normalized, surfaces, new Set(["task-a"])), normalized);
  assert.deepEqual(state.normalizeSurfaceNavigation({ entries: [{ surfaceId: "missing" }], index: 0 }, surfaces), { entries: [{ surfaceId: "voice" }], index: 0 });
});

test("choosing a destination truncates forward history and preserves exact task identity", () => {
  let current = { entries: [{ surfaceId: "voice" }, { surfaceId: "inbox" }, { surfaceId: "drivers" }], index: 1 };
  let focusRequest = 10;
  const select = callback("selectDestination", {
    sameClientDestination: state.sameClientDestination,
    setSurfaceNavigation: (update) => { current = update(current); },
    setQuery: () => {},
    setSearchFocusTick: (next) => { focusRequest = next; },
  });
  select(state.clientDestination("chat", "task-a"));
  assert.equal(focusRequest, 0, "A previous surface's search request must not steal focus after navigation");
  assert.deepEqual(current, { entries: [{ surfaceId: "voice" }, { surfaceId: "inbox" }, { surfaceId: "chat", runId: "task-a" }], index: 2 });
  const before = current;
  select(state.clientDestination("chat", "task-a"));
  assert.equal(current, before);
  select(state.clientDestination("chat", "task-b"));
  assert.deepEqual(current.entries.at(-1), { surfaceId: "chat", runId: "task-b" });
});

const previousWindow = global.window;
after(() => { global.window = previousWindow; });
test("launch destination is scoped to the profile and window without the old workspace split", () => {
  const local = new Map(), session = new Map();
  global.window = { localStorage: { getItem: (key) => local.get(key) ?? null }, sessionStorage: { getItem: (key) => session.get(key) ?? null } };
  local.set(navigation.activeDestinationStorageKey("live"), "inbox");
  local.set(navigation.activeDestinationStorageKey("lab"), "applications");
  session.set(windows.clientWindowStateKey("peer-a", navigation.activeDestinationStorageKey("live")), "registry");
  assert.equal(state.initializeSurfaceNavigation("voice", "live").entries[0].surfaceId, "inbox");
  assert.equal(state.initializeSurfaceNavigation("voice", "lab").entries[0].surfaceId, "applications");
  assert.equal(state.initializeSurfaceNavigation("voice", "live", "", "peer-a").entries[0].surfaceId, "registry");
  assert.equal(state.initializeSurfaceNavigation("voice", "live", "", "peer-b").entries[0].surfaceId, "voice");
});

test("Home reopens its existing run and preserves a draft while its run is not available", () => {
  for (const [homeRunId, voiceSession, expectedRunId] of [
    ["", { phase: "idle" }, null],
    ["home-a", { phase: "idle" }, "home-a"],
    ["home-a", { phase: "listening", context: { homeRunId: "home-live", runId: "voice-call" } }, "home-live"],
  ]) {
    const calls = [];
    let draft = "Unsent Home message";
    const open = callback("openVoice", {
      setSecondaryPaneOpen: (value) => calls.push(["side-pane", value]),
      setSecondaryPaneFocused: (value) => calls.push(["focused", value]),
      homeRunId,
      voiceSession,
      isVoiceCallActive: load("src/components/voice/voicePresentation.ts").isVoiceCallActive,
      homeChatWorkspace: {
        activeRunId: "",
        selectRun: (runId) => calls.push(["home-run", runId]),
        createChat: () => { draft = ""; },
      },
      selectSurface: (id) => calls.push(["surface", id]),
    });
    open();
    assert.deepEqual(calls.at(-1), ["surface", "voice"]);
    assert.deepEqual(calls.filter(([name]) => name === "home-run"), expectedRunId ? [["home-run", expectedRunId]] : []);
    assert.equal(draft, "Unsent Home message");
  }
});

function railFixture({ remembered = { surfaceId: "chat", runId: "task-b" }, runsLoaded = true } = {}) {
  const calls = [];
  let draft = "Unsent task draft";
  const open = callback("openNavigationSection", {
    setHomeWidgetsOpen: () => {},
    setSecondaryPaneFocused: (value) => calls.push(["focused", value]),
    openVoice: () => calls.push(["home"]),
    setHomeViewMode: (value) => calls.push(["home-mode", value]),
    lastTasksDestinationRef: { current: remembered },
    visibleSurfaces: surfaces,
    surfaceNavigation: { entries: [{ surfaceId: "voice" }, { surfaceId: "registry" }], index: 1 },
    navigationSectionForSurface: navigation.navigationSectionForSurface,
    clientNavigationDestinations: navigation.clientNavigationDestinations,
    chatWorkspace: {
      activeRunId: "task-a",
      runsLoaded,
      selectRun: (id) => calls.push(["task", id]),
      createChat: () => { draft = ""; calls.push(["new"]); },
    },
    taskChatRuns: [{ runId: "task-a" }, { runId: "task-b" }],
    appToast: { warning: () => calls.push(["unavailable"]) },
    setSecondaryPaneOpen: (value) => calls.push(["side-pane", value]),
    selectSurface: (id, runId) => calls.push(runId === undefined ? ["surface", id] : ["surface", id, runId]),
  });
  return { calls, open, draft: () => draft };
}

test("Tasks restores its own task or Scheduled in the primary surface", () => {
  for (const [options, expected] of [
    [{}, ["surface", "chat", "task-b"]],
    [{ remembered: null }, ["surface", "chat", "task-a"]],
    [{ remembered: { surfaceId: "scheduled" } }, ["surface", "scheduled"]],
  ]) {
    const fixture = railFixture(options);
    fixture.open("tasks");
    assert.deepEqual(fixture.calls.at(-1), expected);
    assert.ok(fixture.calls.some(([name, value]) => name === "side-pane" && value === false));
    assert.ok(!fixture.calls.some(([name]) => name === "home"));
  }
});

test("a missing remembered task reports its absence and a pending list retains the exact task", () => {
  const remembered = { surfaceId: "chat", runId: "deleted" };
  const missing = railFixture({ remembered });
  missing.open("tasks");
  assert.deepEqual(missing.calls.at(-1), ["unavailable"]);
  assert.ok(!missing.calls.some(([name]) => name === "new" || name === "surface"));
  const pending = railFixture({ remembered, runsLoaded: false });
  pending.open("tasks");
  assert.deepEqual(pending.calls.at(-1), ["surface", "chat", "deleted"]);
  assert.ok(!pending.calls.some(([name]) => name === "unavailable" || name === "new"));
});

test("returning to a new task through the rail preserves its unsent draft", () => {
  const fixture = railFixture({ remembered: { surfaceId: "chat" } });
  fixture.open("tasks");
  fixture.open("tasks");
  assert.equal(fixture.draft(), "Unsent task draft");
  assert.ok(!fixture.calls.some(([name]) => name === "new"));
  assert.deepEqual(fixture.calls.at(-1), ["surface", "chat", ""]);
});

test("opening an existing task or pressing plus uses Tasks without opening Home", () => {
  const calls = [];
  const open = callback("openChat", {
    chatWorkspace: { selectRun: (id) => calls.push(["task", id]), createChat: (projectId) => calls.push(projectId ? ["new", projectId] : ["new"]) },
    setSecondaryPaneOpen: (value) => calls.push(["side-pane", value]),
    setSecondaryPaneFocused: (value) => calls.push(["focused", value]),
    selectSurface: (id, runId) => calls.push(["surface", id, runId]),
  });
  open("task-a");
  assert.deepEqual(calls.splice(0), [["task", "task-a"], ["side-pane", false], ["focused", false], ["surface", "chat", "task-a"]]);
  open("");
  assert.deepEqual(calls.splice(0), [["new"], ["side-pane", false], ["focused", false], ["surface", "chat", ""]]);
  open("", "project-a");
  assert.deepEqual(calls[0], ["new", "project-a"]);
});

test("Home opens its surface, Dashboard restores its view, and Inbox, Applications and Drivers open directly", () => {
  const home = railFixture(); home.open("home");
  assert.deepEqual(home.calls, [["focused", false], ["home"], ["home-mode", "voice"]]);
  for (const [section, surfaceId] of [["dashboard", "registry"], ["inbox", "inbox"], ["applications", "applications"], ["drivers", "drivers"]]) {
    const fixture = railFixture(); fixture.open(section);
    assert.deepEqual(fixture.calls, [["focused", false], ["side-pane", false], ["surface", surfaceId]]);
  }
  assert.equal(navigation.navigationSectionForSurface(surfaces.find(({ id }) => id === "custom-home")), "tasks");
  assert.equal(navigation.navigationSectionForSurface(surfaces.find(({ id }) => id === "custom-dashboard")), "dashboard");
});

test("global surface selection follows the rail and exposes additional Dashboard views", () => {
  const calls = [];
  const open = callback("openPrimarySurface", {
    clientNavigationDestinations: navigation.clientNavigationDestinations,
    openNavigationSection: (section) => calls.push(["rail", section]),
    setSecondaryPaneFocused: (value) => calls.push(["focused", value]),
    setSecondaryPaneOpen: (value) => calls.push(["side-pane", value]),
    selectSurface: (id) => calls.push(["surface", id]),
  });
  open(surfaces.find(({ id }) => id === "inbox"));
  assert.deepEqual(calls.splice(0), [["rail", "inbox"]]);
  open(surfaces.find(({ id }) => id === "voice"));
  assert.deepEqual(calls.splice(0), [["rail", "home"]]);
  open(surfaces.find(({ id }) => id === "registry"));
  assert.deepEqual(calls, [["focused", false], ["side-pane", false], ["surface", "registry"]]);
});

test("Back and Forward reveal their primary destination and restore its exact task", () => {
  let current = { entries: [{ surfaceId: "voice" }, { surfaceId: "chat", runId: "task-a" }, { surfaceId: "inbox" }], index: 2 };
  const calls = [];
  const move = (delta) => callback("navigateSurfaceHistory", {
    surfaceNavigation: current,
    setSecondaryPaneOpen: (value) => calls.push(["side-pane", value]),
    setSecondaryPaneFocused: (value) => calls.push(["focused", value]),
    setActivePane: (value) => calls.push(["pane", value]),
    chatWorkspace: { selectRun: (id) => calls.push(["task", id]), createChat: () => calls.push(["new"]), },
    setSurfaceNavigation: (update) => { current = update(current); },
    setQuery: () => {},
    setSearchFocusTick: () => {},
  })(delta);
  move(-1);
  assert.equal(current.index, 1);
  assert.deepEqual(calls.splice(0), [["side-pane", false], ["focused", false], ["pane", "primary"], ["task", "task-a"]]);
  move(1);
  assert.equal(current.index, 2);
  assert.deepEqual(calls.splice(0), [["side-pane", false], ["focused", false], ["pane", "primary"]]);
  move(1);
  assert.deepEqual(calls, []);
});

test("Settings is one history entry: sections replace it and closing returns to the calling destination", () => {
  let current = { entries: [{ surfaceId: "chat", runId: "task-a" }], index: 0 };
  const calls = [], preferences = { settingsSection: "general" };
  const environment = {
    clientPreferences: preferences, normalizeSettingsSection: (section) => section,
    sameClientDestination: state.sameClientDestination, clientDestination: state.clientDestination,
    setClientPreferences: (update) => Object.assign(preferences, update),
    setSurfaceNavigation: (update) => { current = update(current); },
    setSecondaryPaneOpen: () => calls.push("closed-pane"), setSecondaryPaneFocused: () => {}, setActivePane: () => {},
    chatWorkspace: { selectRun: () => calls.push("changed-task") }, setQuery: () => {}, setSearchFocusTick: () => {},
  };
  const open = callback("setSettingsOpen", environment), select = callback("selectSettingsSection", environment);
  open(true); select("appearance");
  assert.deepEqual(current.entries.map((entry) => entry.settingsSection), [undefined, "appearance"]);
  assert.equal(current.index, 1);
  const move = (delta) => callback("navigateSurfaceHistory", { ...environment, surfaceNavigation: current })(delta);
  move(-1); assert.deepEqual(current.entries[current.index], { surfaceId: "chat", runId: "task-a" });
  move(1); assert.equal(current.entries[current.index].settingsSection, "appearance");
  open(false);
  assert.deepEqual({ length: current.entries.length, index: current.index }, { length: 2, index: 0 });
  assert.deepEqual(current.entries[current.index], { surfaceId: "chat", runId: "task-a" });
  current = { entries: [{ surfaceId: "voice", settingsSection: "general" }], index: 0 };
  open(false);
  assert.deepEqual(current, { entries: [{ surfaceId: "voice" }], index: 0 });
  assert.deepEqual(calls, []);
});

test("Files opens in its calling workspace without changing the primary destination", () => {
  const calls = [];
  const open = callback("openFilesTab", {
    activeClientSurfaceIdentity: { workspaceKey: "octopus-os" },
    workspacePaneTabForTarget: windows.workspacePaneTabForTarget,
    openSecondaryTab: (tab) => calls.push(tab),
    selectSurface: () => assert.fail("Files must not navigate the primary surface"),
  });
  const target = { rootId: "artifacts", path: "artifact.payload" };
  open(target, "dashboard");
  assert.deepEqual(calls[0].target, { kind: "files", workspaceKey: "dashboard", openTarget: target });
  open();
  assert.equal(calls[1].target.workspaceKey, "octopus-os");
});

test("each New tab retains its task context across selection and restore without becoming a transferable surface", () => {
  const tabs = [], data = new Map(), previousWindow = globalThis.window;
  const source = { kind: "worker_environment", target: { schemaVersion: 1, workerEnvironmentId: "room-a", surfaceId: "files-b", panelKind: "files" } };
  const open = callback("openLauncherTab", {
    activeClientSurfaceIdentity: { ready: true, pane: "secondary", workspaceKey: "octopus-os" },
    activeSecondaryTab: { target: source },
    workerEnvironmentActionRunIds: ["task-a"],
    openSecondaryTab: tab => tabs.push(tab),
  });
  open(); open();
  assert.notEqual(tabs[0].id, tabs[1].id);
  const target = tabs[1].target;
  assert.deepEqual(target, { kind: "launcher", runId: "task-a", workspaceKey: "octopus-os", source });
  const identity = windows.deriveActiveClientSurfaceIdentity({ activePane: "secondary", primary: { target: { surfaceId: "chat", runId: "task-b", workspaceKey: "octopus-os" }, workspaceActiveRunId: "task-b" }, secondary: { open: true, target, workspaceActiveRunId: "task-c" } });
  assert.equal(identity.runId, "task-a");
  assert.equal(windows.transferableWorkspacePaneTarget(target), null);
  const { terminalLaunchCwd } = load("src/shell/terminalLaunchContext.ts");
  const room = { id: "room-a", surfaces: ["a", "b"].map(id => ({ id: `files-${id}`, root: `/workspace/${id}`, mode: "direct", protocol: "filesystem.readwrite", readOnly: false })) };
  const topology = { projection: { mode: "live" }, environments: [{ runs: [{ id: "task-a", workerEnvironments: [room] }], workerEnvironments: [] }] };
  assert.throws(() => terminalLaunchCwd(topology, "task-a"), /multiple filesystem roots/);
  const opened = [], terminalEnvironment = { activeClientSurfaceIdentity: identity, activeSecondaryTab: { target }, topology, terminalLaunchCwd, openSecondaryTab: tab => opened.push(tab), appToast: { warning: message => assert.fail(message) } };
  callback("openTerminalTab", terminalEnvironment)();
  assert.equal(opened.at(-1).target.cwd, "/workspace/b");
  terminalEnvironment.activeSecondaryTab.target = { ...target, source: { kind: "terminal", cwd: "/explicit/terminal" } };
  callback("openTerminalTab", terminalEnvironment)();
  assert.equal(opened.at(-1).target.cwd, "/explicit/terminal");
  try {
    globalThis.window = { localStorage: { getItem: key => data.get(key) ?? null, setItem: (key, value) => data.set(key, value) } };
    state.writeSecondaryPaneState("live", "home-a", "main", { activeTabId: tabs[1].id, tabs, open: true });
    assert.deepEqual(state.readSecondaryPaneState("live", "home-a"), { version: 3, activeTabId: tabs[1].id, tabs, open: true });
    assert.deepEqual(state.readSecondaryPaneState("live", "home-b").tabs, []);
  } finally { globalThis.window = previousWindow; }
});

test("a detached pane opens with its declared context instead of always restoring Home", () => {
  assert.deepEqual(state.clientWindowInitialDestination({ kind: "files", workspaceKey: "dashboard" }), { surfaceId: "dashboard" });
  assert.deepEqual(state.clientWindowInitialDestination({ kind: "surface", surfaceId: "registry", runId: "", workspaceKey: "dashboard" }), { surfaceId: "registry" });
  assert.deepEqual(state.clientWindowInitialDestination({ kind: "surface", surfaceId: "chat", runId: "task-a", workspaceKey: "octopus-os" }), { surfaceId: "chat", runId: "task-a" });
});

test("Dashboard expansion restores an inspector that was already closed", () => {
  let inspectorOpen = false;
  const toggle = callback("toggleDashboardFullscreen", { setInspectorOpen: (update) => { inspectorOpen = update(inspectorOpen); } });
  toggle();
  assert.equal(inspectorOpen, true);
  toggle();
  assert.equal(inspectorOpen, false);
});

test("leaving Tasks retains its sidebar through closing and releases it after motion completes", async () => {
  const React = require("react"), { createRoot } = require("react-dom/client");
  const { JSDOM } = require("jsdom");
  const dom = new JSDOM("<div id='root'></div>");
  const before = { window: global.window, document: global.document, act: global.IS_REACT_ACT_ENVIRONMENT };
  global.window = dom.window; global.document = dom.window.document; global.IS_REACT_ACT_ENVIRONMENT = true;
  const { PanelContent } = load("src/components/shell/PanelResizeHandle.tsx", { "motion/react": {}, "./panelResizeHandle.css": {} });
  let unmounted = 0;
  function Sidebar() { React.useEffect(() => () => { unmounted++; }, []); return React.createElement("nav", null, "Tasks"); }
  const node = document.querySelector("#root"), root = createRoot(node);
  const render = async (active, mounted) => React.act(async () => root.render(React.createElement(PanelContent, { active, mounted }, active ? React.createElement(Sidebar) : null)));
  try {
    await render(true, true); const sidebar = node.firstChild;
    await render(false, true); assert.equal(node.firstChild, sidebar); assert.equal(unmounted, 0);
    await render(true, true); assert.equal(node.firstChild, sidebar);
    await render(false, false); assert.equal(node.childElementCount, 0); assert.equal(unmounted, 1);
  } finally {
    await React.act(async () => root.unmount()); dom.window.close();
    global.window = before.window; global.document = before.document; global.IS_REACT_ACT_ENVIRONMENT = before.act;
  }
});

test("task labels preserve operator names and do not expose raw run ids as titles", () => {
  const organization = { aliases: { "task-a": "Review changes" } };
  assert.equal(workspaces.clientTaskTitle("task-a", "task-a", organization), "Review changes");
  assert.equal(workspaces.clientTaskTitle("task-b", "Projected title", organization), "Projected title");
  assert.equal(workspaces.clientTaskTitle("task-b", "task-b", organization), "Untitled task");
});

test("native window controls stay centered and reserve physical space across zoom levels", () => {
  const chrome = require("../electron/windowChrome.cjs");
  for (const [zoom, y, inset] of [[0.8, 11, 95], [1, 15, 76], [1.4, 24, 76 / 1.4]]) {
    assert.deepEqual(chrome.nativeTrafficLightPosition(zoom), { x: 16, y });
    assert.equal(chrome.nativeControlInset(zoom), inset);
  }
  for (const zoom of [0, -1, NaN, Infinity]) {
    assert.throws(() => chrome.nativeTrafficLightPosition(zoom), /positive/);
    assert.throws(() => chrome.nativeControlInset(zoom), /positive/);
  }
});
