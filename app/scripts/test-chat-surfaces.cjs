const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "HTMLInputElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "KeyboardEvent", "MouseEvent", "WheelEvent", "localStorage", "navigator", "getComputedStyle"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : key === "getComputedStyle" ? w[key].bind(w) : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.scrollTo = () => {};
const geometry = new WeakMap();
const setBox = (element, values) => geometry.set(element, { ...geometry.get(element), ...values });
Object.defineProperty(w.HTMLElement.prototype, "scrollTop", { configurable: true, get() { return geometry.get(this)?.scrollTop ?? 0; }, set(value) { setBox(this, { scrollTop: value }); } });
for (const name of ["scrollHeight", "clientHeight"]) Object.defineProperty(w.HTMLElement.prototype, name, { configurable: true, get() { return geometry.get(this)?.[name] ?? 0; } });
w.HTMLElement.prototype.scrollTo = function (options) { if (options && typeof options.top === "number") this.scrollTop = options.top; };
w.HTMLElement.prototype.scrollIntoView = function () {};
w.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 100, y: 100, left: 100, top: 100, right: 200, bottom: 120, width: 100, height: 20 });

const React = require("react");
const { createRoot } = require("react-dom/client");
const heavy = /^(@pierre\/diffs|shiki|@shikijs|mermaid|katex)/;
const inert = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : name === "preloadHighlighter" ? async () => {} : () => null });
const stubs = {};
globalThis.__surfaceStubs = stubs;
const stubPlugin = (modules) => ({
  name: "surface-stubs",
  setup(bundle) {
    for (const key of Object.keys(modules)) {
      bundle.onResolve({ filter: new RegExp(key.startsWith(".") ? `^${key.replace(/[./]/g, "\\$&")}$` : `(^|/)${key}$`) }, () => ({ path: key, namespace: "surface-stub" }));
    }
    bundle.onLoad({ filter: /.*/, namespace: "surface-stub" }, ({ path: name }) => ({ loader: "js", contents: typeof modules[name] === "string" ? modules[name]
      : modules[name].map((key) => `export function ${key}(...args) { const stub = globalThis.__surfaceStubs[${JSON.stringify(name)}]?.${key}; return stub ? stub(...args) : null; }`).join("\n") }));
  },
});
function bundle(contents, stubbed) {
  return build({
    stdin: { contents, resolveDir: path.join(__dirname, ".."), loader: "ts" },
    bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent", plugins: [stubPlugin(stubbed)],
    define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
    loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
    external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
  }).then((result) => {
    const module = { exports: {} };
    Function("require", "module", "exports", result.outputFiles[0].text)((id) => heavy.test(id) ? inert : require(id), module, module.exports);
    return module.exports;
  });
}
const surfaces = bundle([
  'export { ChatView } from "./src/components/ChatView";',
  'export { QuickTaskWindow } from "./src/components/QuickTaskWindow";',
  'export { VoiceView } from "./src/components/voice/VoiceView";',
  'export { OPEN_WORKER_ENVIRONMENT_PANE_EVENT } from "./src/components/worker-environment/workerEnvironmentPane";',
  'export { ClientPreferencesProvider } from "./src/clientPreferences";',
].join("\n"), {
  "./chat": ["ChatComposer", "ChatMessageList", "QueuedMessageList", "ChatThreadHeader", "ChatThreadHeaderActions", "ChatRenderBoundary", "TaskSummaryPanel", "useChatWorkspace"],
  ChatComposer: ["ChatComposer"], useChatWorkspace: ["useChatWorkspace"], MessageContent: ["MessageContent"], QueuedMessageList: ["QueuedMessageList"],
  OperatorRequestSurface: ["OperatorRequestSurface"], VoiceOrbScene: ["VoiceOrbScene"], OctopusOceanScene: ["OctopusOceanScene"],
  OctopusSwarm: ["OctopusSwarm"], ClientSurfaceRenderer: ["ClientSurfaceRenderer", "resetVoiceWidgetLayout", "restoreVoiceWidgetLayout", "useVoiceWidgetResize"],
  toasts: "export const appToast = { warning: (message) => globalThis.__surfaceStubs.toasts.warnings.push(message), info() {}, success() {}, danger() {} }; export function useAppErrorToast() {} export function AppToastViewport() { return null; }",
});
const presentations = bundle([
  'export { TaskSummaryPanel } from "./src/components/chat/TaskSummaryPanel";',
  'export { TurnActivity } from "./src/components/chat/TurnActivity";',
  'export { ChatMessageList } from "./src/components/chat/ChatMessageList";',
  'export { GeneralSection } from "./src/components/settings/GeneralSettingsSection";',
  'export { ShortcutsSection } from "./src/components/settings/ShortcutSettingsSection";',
  'export { CommandMenu } from "./src/components/CommandMenu";',
  'export { ClientPreferencesProvider, useSetClientPreferences } from "./src/clientPreferences";',
  'export { formatKeyboardShortcut } from "./src/keyboardShortcuts";',
  'export { renameClientTask, recordClientTaskOpened } from "./src/shell/clientWorkspaces";',
].join("\n"), { MessageContent: ["MessageContent"] });

const root = createRoot(document.getElementById("root"));
after(() => { root.unmount(); w.close(); });
const settle = async (rounds = 4) => { for (let round = 0; round < rounds; round += 1) await new Promise((resolve) => setTimeout(resolve, 10)); };
const show = async (element) => { root.render(null); await settle(1); root.render(element); await settle(); };
const text = (element) => element.textContent.replace(/\s+/g, " ").trim();
const noop = () => {};

const composerProps = [];
stubs.ChatComposer = { ChatComposer: (props) => { composerProps.push(props); return React.createElement("div", { "data-composer": true }); } };
stubs["./chat"] = {
  ChatComposer: stubs.ChatComposer.ChatComposer,
  ChatMessageList: ({ footer }) => React.createElement("div", { "data-thread": true }, footer),
  ChatRenderBoundary: ({ children }) => children,
  ChatThreadHeaderActions: ({ summaryPopoverContent }) => summaryPopoverContent ?? null,
  TaskSummaryPanel: (props) => { stubs.summaryProps = props; return null; },
};
const toasts = [];
stubs.toasts = { warnings: toasts };

function workspace(overrides = {}) {
  const fields = {
    accessProfile: "lab", accessMode: "project_access", activeMessageCount: 0, activeProvider: undefined, activeRun: null, activeRunId: "", agentPresetId: "", applicationIds: [],
    applicationSelectionLockReason: "", applicationSelectionLocked: false, artifactWriterAvailable: true, attachments: [], busy: false, canQueue: false, canSendQueued: false, canSteer: false,
    canSubmit: false, composerAgentPresets: [], continuation: null, declaredProviderInterfaces: [], dragOver: false, driverNames: [], editingQueuedTurn: false, eeId: "", error: "",
    executionContextError: "", executionContexts: [], fileInputRef: { current: null }, fullAccessUnavailableReason: "", loadingMessages: false, messageQuery: "", newTaskAuthoringError: "",
    pausedQueueSubmissionCount: 0, pinnedContextItems: [], pinnedMessages: [], pinningArtifactIds: [], providerInterfaceError: "", providerInterfaceId: "", providerInterfaces: [],
    providerInterfacesLoaded: true, providerInterfacesRefreshing: false, providerModel: "", queueNotice: "", queuePaused: false, queuedTurns: [], reasoningEffort: "", renderedMessages: [],
    runs: [], runsError: "", runsLoaded: true, serviceTier: "", steerableMessageId: "", submitBlockReason: "", submitting: false, resumeAvailable: false, text: "", then: undefined,
    toolPresentations: {}, turnControlPending: false, turnOpen: false, workerEnvironmentRooms: [], workerInterfaceId: "", ...overrides,
  };
  return new Proxy(fields, { get: (target, key) => key in target ? target[key] : typeof key === "symbol" ? undefined : noop });
}
const run = (runId, extra = {}) => ({ runId, title: `Task ${runId}`, eeId: "ee.root", chatInterfaceId: `chat.${runId}`, clientSurfaceRole: "task", writable: true, archived: false, updatedAtMs: 1,
  messageCount: 0, turnOpen: false, turnState: "idle", pinnedArtifactIds: [], ...extra });
const meter = { level: 0, waveform: [] };
const session = { phase: "idle", transcript: [], context: null, startedAtMs: 0, error: "", deviceError: "", inputDevices: [], outputDevices: [], selectedInputId: "", selectedOutputId: "",
  subscribeOutputMeter: () => noop, getOutputMeterSnapshot: () => meter, subscribeMeter: () => noop, getMeterSnapshot: () => meter, setSpeakerEnabled: noop, selectInputDevice: noop,
  selectOutputDevice: noop, refreshDevices: noop, toggleMute: noop, end: async () => {}, start: async () => {}, updateContext: async () => {} };
const topology = (runs = []) => ({ projection: { mode: "live" }, environments: [{ id: "ee.root", name: "Root", runs }] });

test("every task surface hands the composer the workspace's submit state and exact Resume action", async () => {
  const { ChatView, QuickTaskWindow, VoiceView, ClientPreferencesProvider } = await surfaces;
  w.octopusTransport = {};
  const surfacesUnderTest = {
    chat: (chat) => React.createElement(ChatView, { workspace: chat, searchFocusTick: 0, topology: topology(), surfaces: [], onOpenFiles: noop }),
    quick: (chat) => { stubs.useChatWorkspace = { useChatWorkspace: () => chat }; return React.createElement(QuickTaskWindow, { accessProfile: "lab", runtimeReady: true, runtimeHome: "/home/quick" }); },
    home: (chat) => React.createElement(VoiceView, { accessProfile: "lab", activity: { entries: [], total: 0 }, builtinWidgetSources: {}, homeRunId: "home", installedWidgetIds: [],
      installedWidgetsReady: true, session, surfaces: [], topology: topology(), workerEnvironmentSurfaces: [], widgetsOpen: false, workspace: chat, viewMode: "voice" }),
  };
  for (const [name, render] of Object.entries(surfacesUnderTest)) {
    for (const state of [{ submitting: true, resumeAvailable: true, submitBlockReason: "Task history is still loading." }, { submitting: false, resumeAvailable: false, submitBlockReason: "" }]) {
      const resumeActiveTurn = () => {};
      const homeRun = run("home", { clientSurfaceRole: "home" });
      composerProps.length = 0;
      await show(React.createElement(ClientPreferencesProvider, { profile: "lab" },
        render(workspace({ ...state, resumeActiveTurn, activeRunId: name === "home" ? "home" : "", activeRun: name === "home" ? homeRun : null, runs: name === "home" ? [homeRun] : [] }))));
      const props = composerProps.at(-1);
      assert.ok(props, `${name} rendered no composer`);
      assert.deepEqual([props.isSubmitting, props.submitBlockReason, props.resumeAvailable, props.onResume === resumeActiveTurn],
        [state.submitting, state.submitBlockReason, state.resumeAvailable, true], name);
    }
  }
});

test("the task summary opens a projected environment at the exact surface it names, or at its default", async () => {
  const { ChatView, OPEN_WORKER_ENVIRONMENT_PANE_EVENT, ClientPreferencesProvider } = await surfaces;
  const opened = [];
  w.addEventListener(OPEN_WORKER_ENVIRONMENT_PANE_EVENT, (event) => opened.push(event.detail));
  const surface = { id: "terminal:a", kind: "terminal", root: "", data: {}, label: "Terminal", mode: "direct", status: "ready", viewerUrl: "", protocol: "", readOnly: false };
  const environment = { id: "room-a", runId: "run-1", revision: "", eeId: "ee.root", kind: "shell", lifecycle: "active", status: "ready", live: true, label: "Room", detail: "", substrateFamily: "",
    door: "", root: "/", session: "", transport: "", cells: [], capabilities: [], tools: [], surfaces: [surface] };
  const projected = { id: "run-1", eventLog: [], workerEnvironment: environment, workerEnvironments: [environment] };
  const activeRun = run("run-1");
  w.innerWidth = 1600;
  await show(React.createElement(ClientPreferencesProvider, { profile: "lab" },
    React.createElement(ChatView, { workspace: workspace({ activeRunId: "run-1", activeRun, runs: [activeRun] }), searchFocusTick: 0, topology: topology([projected]), surfaces: [], onOpenFiles: noop })));
  assert.equal(stubs.summaryProps.runProjection, projected);
  stubs.summaryProps.onOpenEnvironment("room-a", "files", "files:a");
  stubs.summaryProps.onOpenEnvironment("room-a");
  stubs.summaryProps.onOpenEnvironment("room-gone", "terminal", "terminal:a");
  assert.deepEqual(opened, [
    { schemaVersion: 1, workerEnvironmentId: "room-a", panelKind: "files", surfaceId: "files:a", clientSurface: undefined },
    { schemaVersion: 1, workerEnvironmentId: "room-a", panelKind: "terminal", surfaceId: "terminal:a" },
  ]);
  assert.deepEqual(toasts, ["This task no longer projects that Worker Environment."]);
});

const message = (id, eventKind, textValue, eventData = {}, extra = {}) => ({ id, role: "assistant", text: textValue, createdAtMs: 1, eventKind, eventData, ...extra });

test("the task summary lists every environment, background process and related agent, opens surfaces from the projection alone, and offers no binding facts or git actions", async () => {
  const { TaskSummaryPanel } = await presentations;
  const transportReads = [];
  w.octopusTransport = new Proxy({}, { get: (_, key) => { transportReads.push(key); return undefined; } });
  const processes = Array.from({ length: 13 }, (_, index) => message(`process-${index}`, "tool", "", { process_id: `pid-${index}`, command: `job ${index}`, status: "running" }));
  const agents = Array.from({ length: 13 }, (_, index) => message(`agent-${index}`, "subagent_activity", "", { agent_thread_id: `thread-${String(index).padStart(2, "0")}`, agent_path: `root/agent_${index}`, kind: "started" }));
  const room = (id, root) => ({ id, runId: "run-1", revision: "", eeId: "ee.root", kind: "shell", lifecycle: "active", status: "ready", live: true, label: id, detail: "", substrateFamily: "",
    door: "", root, session: "", transport: "", cells: [], capabilities: [], tools: [], surfaces: [{ id: `review:${id}`, kind: "changes", root, data: {}, label: "Changes", mode: "direct",
      status: "ready", viewerUrl: "", protocol: "git.worktree", readOnly: false }] });
  const opened = [];
  await show(React.createElement(TaskSummaryPanel, { messages: [...processes, ...agents], runProjection: { id: "run-1", workerEnvironment: room("room-a", "/work"), workerEnvironments: [room("room-a", "/work"), room("room-b", "/other")] },
    onOpenEnvironment: (...args) => opened.push(args), onRevealMessage: noop, onShowAllSources: noop }));
  assert.equal(document.querySelectorAll(".task-environment-room").length, 2);
  [...document.querySelectorAll(".task-environment-room button")].find((button) => text(button).startsWith("Changes")).click();
  assert.deepEqual(opened, [["room-a", "changes", "review:room-a"]]);
  const section = (title) => [...document.querySelectorAll(".task-summary-section")].find((element) => text(element.querySelector(".task-summary-section-heading") ?? element).startsWith(title));
  const background = [...section("Background processes").querySelectorAll(".task-summary-row")];
  assert.equal(background.length, 13);
  assert.deepEqual([...new Set(background.map((row) => text(row.querySelector(".task-summary-row-meta"))))], ["Running"]);
  assert.equal(section("Related agents").querySelectorAll(".task-summary-related-agents .task-summary-row").length, 13);
  assert.doesNotMatch(text(document.body), /Execution environment|Worker binding|Provider|Checkpoints|Commit or push|Create pull request/);
  assert.deepEqual(transportReads, [], "the summary reads nothing from the host beyond its projection");
});

test("an activity entry is presented by its latest report", async () => {
  const { TurnActivity } = await presentations;
  const spawn = (id, status) => message(id, "collaboration", "", { tool: "spawnAgent", status, call_id: "spawn-1", receiver_thread_ids: ["thread-a"] });
  const subagent = (id, kind) => message(id, "subagent_activity", "", { agent_thread_id: "thread-b", agent_path: "root/data_checker", kind, item_id: "watch-1" });
  const answer = message("answer", "message", "Done.", { phase: "final_answer" }, { terminal: true, createdAtMs: 5 });
  const activity = (activities) => show(React.createElement(TurnActivity, { activities, presentation: "conversation", renderActivity: () => null, response: answer, startedAtMs: 1,
    statusBoundaryVisible: false, toolPresentations: {}, turnInProgress: false }));
  await activity([spawn("spawn-started", "inProgress"), spawn("spawn-done", "completed")]);
  assert.match(text(document.body), /Created an agent/);
  assert.doesNotMatch(text(document.body), /Creating/);
  await activity([subagent("watch-started", "started"), subagent("watch-stopped", "interrupted")]);
  assert.match(text(document.querySelector('[data-testid="subagent-activity-inline-group"]')), /Data checker.*interrupted/);
});

test("overscrolling past the latest reply counts as being at the latest reply", async () => {
  const { ChatMessageList } = await presentations;
  stubs.MessageContent = { MessageContent: ({ text: content }) => React.createElement("span", null, content) };
  const messages = Array.from({ length: 4 }, (_, index) => [{ id: `u${index}`, role: "user", text: `Question ${index}`, createdAtMs: index * 10 + 1 },
    { id: `a${index}`, role: "assistant", text: `Answer ${index}`, createdAtMs: index * 10 + 2, eventKind: "message", terminal: true }]).flat();
  await show(React.createElement(ChatMessageList, { activeMessageCount: messages.length, conversationId: "task-scroll", findQuery: "", findActiveIndex: 0, onFindMatchCountChange: noop,
    onHeaderEdgeScrollChange: noop, dragOver: false, onDragOver: noop, onDragLeave: noop, onDrop: noop, footer: React.createElement("div", null, "Composer"), onOperatorRequestResponse: async () => {}, onOpenAssistantReview: noop,
    onOpenAssistantVisualization: noop, onOpenFile: noop, resolveAssistantVisualization: () => null, pinnedArtifactIds: [], pinningArtifactIds: [], toolPresentations: {},
    onTogglePinnedMessage: async () => {}, onPinMessageExcerpt: async () => {}, onContinueInNewTask: noop, onEditUserMessage: () => true, onSteerUserMessage: async () => true,
    steerableMessageId: "", revealPinnedContext: null, responseStreaming: false, onPinnedContextRevealed: noop, messages }));
  const list = document.querySelector(".thread-scroll-container");
  setBox(list, { clientHeight: 500, scrollHeight: 2000 });
  const latest = () => document.querySelector('button[aria-label^="Scroll to bottom"]');
  const scroll = async (scrollTop) => {
    list.dispatchEvent(new w.WheelEvent("wheel", { deltaY: -120, bubbles: true }));
    list.scrollTop = scrollTop;
    list.dispatchEvent(new w.Event("scroll"));
    await settle();
  };
  await scroll(-400);
  assert.equal(latest().getAttribute("aria-hidden"), "false");
  await scroll(400);
  assert.equal(latest().getAttribute("aria-hidden"), "true");
});

test("settings describe Send as steering and document the queue shortcut the composer uses", async () => {
  const { GeneralSection, ShortcutsSection, ClientPreferencesProvider, useSetClientPreferences, formatKeyboardShortcut } = await presentations;
  w.octopusTransport = { providerInterfaces: async () => ({ repoRoot: "/repo", providers: [] }), onProjectionChanged: () => noop };
  let setPreferences;
  const Preferences = ({ children }) => { setPreferences = useSetClientPreferences(); return children; };
  const settings = (section) => show(React.createElement(ClientPreferencesProvider, { profile: "lab" }, React.createElement(Preferences, null, section)));
  await settings(React.createElement(GeneralSection, { accessProfile: "lab", surfaces: [] }));
  const activeReply = document.querySelector(`[data-settings-target="general.active-reply"]`);
  assert.match(text(activeReply), /^While a run is active.*Send steers the current reply immediately.*queue/);
  assert.equal(activeReply.querySelectorAll("button, select, input").length, 0, "steering while a run is active is not a choice");
  await settings(React.createElement(ShortcutsSection, { accessProfile: "lab" }));
  for (const [sendShortcut, queue] of [["enter", "Mod+Enter"], ["cmd-if-multiline", "Mod+Shift+Enter"], ["cmd-always", "Mod+Shift+Enter"]]) {
    setPreferences({ sendShortcut });
    await settle();
    assert.equal(text(document.querySelector('[role="group"][aria-label="Queue message while replying"] kbd')), formatKeyboardShortcut(queue), sendShortcut);
  }
  root.render(null);
  await settle();
});

test("the command menu ranks recent task titles first, caps the list, and searches local titles without archived tasks", async () => {
  const { CommandMenu, renameClientTask, recordClientTaskOpened } = await presentations;
  w.octopusTransport = { projectionRead: async () => ({ matches: [], has_older: false, next_before_seq: -1 }) };
  const chatRuns = Array.from({ length: 30 }, (_, index) => run(`task-${index}`, { title: `Task ${index}`, updatedAtMs: 1000 - index }));
  chatRuns.push(run("archived", { title: "Quasar Zebra Unique", updatedAtMs: 2000, archived: true }));
  renameClientTask("lab", "task-29", "Quasar Zebra Unique");
  recordClientTaskOpened("lab", "task-3");
  recordClientTaskOpened("lab", "task-7");
  await show(React.createElement(CommandMenu, { accessProfile: "lab", activeSurfaceId: "chat", chatRuns, onClose: noop, onOpenRun: noop, onSelectSurface: noop, open: true, surfaces: [], voiceActive: false }));
  const input = document.querySelector('[role="combobox"]');
  const tasks = () => [...document.querySelectorAll('section[aria-label="Recent tasks"] [role="option"]')].map((option) => option.id.replace("command-menu-run-", ""));
  const search = async (query) => {
    Object.getOwnPropertyDescriptor(w.HTMLInputElement.prototype, "value").set.call(input, query);
    input.dispatchEvent(new w.Event("input", { bubbles: true }));
    await settle();
    return tasks();
  };
  const recent = [7, 3, 0, 1, 2, 4, 5, 6, 8, 9, 10, 11].map((index) => `task-${index}`);
  assert.deepEqual(tasks(), recent);
  assert.deepEqual(await search("   "), recent);
  assert.deepEqual(await search("  QUASAR   ZEBRA "), ["task-29"]);
  assert.equal((await search("Task")).length, 12);
  root.render(null);
  await settle();
});
