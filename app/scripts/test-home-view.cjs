const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "PointerEvent", "StorageEvent", "navigator", "getComputedStyle", "localStorage"]) {
  if (w[key] || key === "window") Object.defineProperty(globalThis, key, { configurable: true, value: key === "getComputedStyle" ? w[key].bind(w) : key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };
globalThis.MutationObserver = w.MutationObserver;
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.HTMLElement.prototype.scrollTo = function () {};
w.HTMLElement.prototype.getBoundingClientRect = function () {
  if (this.classList.contains("voice-widget-canvas")) return { x: 0, y: 0, left: 0, top: 0, right: 1200, bottom: 800, width: 1200, height: 800 };
  return { x: 600, y: 10, left: 600, top: 10, right: 632, bottom: 42, width: 32, height: 32 };
};
for (const name of ["clientWidth", "clientHeight"]) {
  Object.defineProperty(w.HTMLElement.prototype, name, { configurable: true, get() { return this.classList.contains("voice-widget-canvas") ? (name === "clientWidth" ? 1200 : 800) : 0; } });
}

const React = require("react");
const { createRoot } = require("react-dom/client");
const appRoot = path.join(__dirname, "..");
const stubbed = {
  ClientSurfaceRenderer: 'export function ClientSurfaceRenderer() { return null; } export function resetVoiceWidgetLayout() { return null; } export function restoreVoiceWidgetLayout() {}',
  ChatComposer: 'export function ChatComposer() { return createElement("div", { "data-composer": "" }); }',
  MessageContent: 'export function MessageContent({ text }) { return createElement("span", { "data-content": "" }, text); }',
  QueuedMessageList: "export function QueuedMessageList() { return null; }",
  OperatorRequestSurface: "export function OperatorRequestSurface() { return null; }",
  AirHandVoiceController: "export function AirHandVoiceController() { return null; }",
  HomeCallControls: "export function HomeCallControls() { return null; }",
  VoiceOrbScene: "export function VoiceOrbScene() { return null; }",
  OctopusOceanScene: "export function OctopusOceanScene() { return null; }",
};
const bundle = (entry, names) => build({
  stdin: { contents: entry, resolveDir: appRoot, loader: "ts" },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  plugins: [{
    name: "home-stubs",
    setup(builder) {
      if (!names.length) return;
      builder.onResolve({ filter: new RegExp(`/(${names.join("|")})$`) }, (args) => ({ path: path.basename(args.path), namespace: "stub" }));
      builder.onLoad({ filter: /.*/, namespace: "stub" }, (args) => ({ loader: "js", contents: `import { createElement } from "react";\n${stubbed[args.path]}` }));
    },
  }],
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl", ".webm": "dataurl", ".mp4": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
}).then((result) => {
  const module = { exports: {} };
  const heavy = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : name === "preloadHighlighter" ? async () => {} : () => null });
  Function("require", "module", "exports", result.outputFiles[0].text)((id) => /^(@pierre\/diffs|shiki|@shikijs|mermaid|katex)/.test(id) ? heavy : require(id), module, module.exports);
  return module.exports;
});
const homeBundle = bundle('export { VoiceView } from "./src/components/voice/VoiceView";', Object.keys(stubbed));
const widgetBundle = bundle('export { ClientSurfaceRenderer } from "./src/client-surfaces/ClientSurfaceRenderer"; export { buildVoiceWidgetDocuments } from "./src/components/voice/voiceWidgetDocuments";', []);

const container = document.getElementById("root");
const root = createRoot(container);
after(() => { root.unmount(); w.close(); });
const settle = (ms = 30) => new Promise((resolve) => setTimeout(resolve, ms));
const query = (selector) => container.querySelector(selector);
const noop = () => {};
const meter = { level: 0, waveform: [] };
const session = { phase: "idle", transcript: [], context: null, startedAtMs: 0, error: "", deviceError: "", inputDevices: [], outputDevices: [], selectedInputId: "", selectedOutputId: "",
  subscribeOutputMeter: () => noop, getOutputMeterSnapshot: () => meter, subscribeMeter: () => noop, getMeterSnapshot: () => meter, providerActivity: false,
  setSpeakerEnabled: noop, selectInputDevice: noop, selectOutputDevice: noop, refreshDevices: noop, toggleMute: noop, end: async () => {}, start: async () => {}, updateContext: async () => {} };
const home = { runId: "home", title: "Home", clientSurfaceRole: "home", chatInterfaceId: "home-client", pinnedArtifactIds: [], updatedAtMs: 1, writable: true };
const homeMessage = (id, role, eventKind, text, createdAtMs, extra = {}) => ({ id, runId: "home", interfaceId: "home-client", role, direction: role === "user" ? "in" : "out", eventKind, text, createdAtMs, attachments: [], ...extra });
const prompt = homeMessage("prompt", "user", "message", "Read the quarterly report", 1_000);
const reading = homeMessage("read", "assistant", "dynamic_tool", "", 1_001, { eventData: { tool: "workspace.read_file", call_id: "read", status: "in_progress", arguments: { path: "report.md" }, _activity_origin: { interface_role: "provider" } } });
const longReply = homeMessage("reply", "assistant", "message", Array.from({ length: 8 }, (_, index) => `Finding ${index + 1} of the quarterly report.`).join("\n"), 1_002, { terminal: true });
const workspace = (overrides = {}) => ({
  runsLoaded: true, runsError: "", activeRunId: "home", activeRun: home, runs: [home], loadingMessages: false, renderedMessages: [], declaredProviderInterfaces: [],
  busy: false, submitting: false, toolPresentations: {}, text: "", queuedTurns: [], providerInterfaces: [], attachments: [], dragOver: false,
  setText: noop, submit: noop, interruptActiveTurn: noop, respondToOperatorRequest: async () => {}, onDragLeave: noop, onDragOver: noop, dropFiles: noop,
  ...overrides,
});
async function showHome(chat) {
  const { VoiceView } = await homeBundle;
  root.render(React.createElement(VoiceView, { accessProfile: "live", activity: { entries: [], total: 0 }, builtinWidgetSources: {}, homeRunId: "home", installedWidgetIds: [],
    installedWidgetsReady: true, session, surfaces: [], topology: { projection: { mode: "live" }, environments: [] }, workerEnvironmentSurfaces: [], widgetsOpen: false, workspace: chat, viewMode: "voice" }));
  await settle();
}
const stage = () => query('[aria-label="Home latest response viewport"]');

test("Home opens behind its loading status until its own task is selected and loaded", async () => {
  await showHome(workspace({ activeRunId: "", activeRun: null }));
  assert.ok(query('.voice-view.is-loading [role="status"][aria-label="Opening Home"]'));
  assert.ok(!stage());
  await showHome(workspace({ loadingMessages: true }));
  assert.ok(query('[aria-label="Opening Home"]'));
  await showHome(workspace());
  assert.ok(!query('[aria-label="Opening Home"]'));
  assert.ok(stage());
});

test("a working Home turn shows its live activity in a compact stage, and a long reply expands it", async () => {
  await showHome(workspace({ busy: true, renderedMessages: [prompt, reading] }));
  assert.ok(stage().classList.contains("is-compact") && stage().classList.contains("is-generating"));
  const live = query(".home-response-output--live .octopus-turn-activity");
  assert.equal(live.dataset.state, "working");
  assert.match(live.textContent, /report\.md/);
  await showHome(workspace({ renderedMessages: [prompt, { ...reading, eventData: { ...reading.eventData, status: "completed" } }, longReply] }));
  assert.ok(stage().classList.contains("is-expanded") && !stage().classList.contains("is-compact"));
  assert.ok(!query(".home-response-output--live"));
  assert.match(stage().textContent, /Finding 8 of the quarterly report\./);
});

const widgetInput = { context: [], environmentAvailable: false, goals: [], memory: [], scheduled: [], tools: [],
  insights: { change: "+1%", changeDetail: "vs last week", label: "Leads", series: [1, 2], value: "12" },
  taskProgress: { completion: 50, running: 1, scheduled: 0, unseenDone: 0, waiting: 0 } };

test("every built-in Home widget docks on the right rail", async () => {
  const { buildVoiceWidgetDocuments } = await widgetBundle;
  const slots = Object.fromEntries(buildVoiceWidgetDocuments(widgetInput).map(({ document }) => [document.id, document.target.slot]));
  assert.equal(slots["builtin.voice.live-context"], "right");
  assert.equal(slots["builtin.voice.insights"], "right");
  assert.ok(Object.values(slots).every((slot) => slot === "right"), JSON.stringify(slots));
});

test("live context and insights saved before they moved to the right rail are placed on it again; other saved widgets keep their place", async () => {
  const { ClientSurfaceRenderer, buildVoiceWidgetDocuments } = await widgetBundle;
  const documents = Object.fromEntries(buildVoiceWidgetDocuments(widgetInput).map((admitted) => [admitted.document.id, admitted]));
  const legacy = { x: 20, y: 20, width: 300, height: 240, stageWidth: 1200, stageHeight: 800, z: 1 };
  for (const [id, left] of [["builtin.voice.live-context", "892px"], ["builtin.voice.insights", "892px"], ["builtin.voice.task-progress", "20px"]]) {
    w.localStorage.setItem("octopus.voice.widget-layout.v3", JSON.stringify({ [id]: legacy }));
    root.render(null);
    await settle();
    root.render(React.createElement("div", { className: "voice-widget-canvas" },
      React.createElement(ClientSurfaceRenderer, { accessProfile: "live", admitted: documents[id], voicePlacement: { slot: "right", index: 0, count: 1 } })));
    await settle(60);
    const widget = query(`[data-surface-document="${id}"]`);
    assert.equal(widget.style.left, left, id);
    assert.equal(JSON.parse(w.localStorage.getItem("octopus.voice.widget-layout.v3"))[id].slot, "right", id);
  }
  root.render(null);
  await settle();
});
