const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const appRoot = path.join(__dirname, "..");
const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "navigator", "localStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} disconnect() {} };
w.matchMedia = (query) => ({ matches: query.includes("reduce"), addEventListener() {}, removeEventListener() {} });
w.localStorage.setItem("octopus.onboarding.v1", JSON.stringify({ version: 1, completed: true, visitCount: 3 }));

const React = require("react");
const { createRoot } = require("react-dom/client");
const { renderToStaticMarkup } = require("react-dom/server");
const stubbed = {
  OperatorShell: 'export function OperatorShell({ accessProfile }) { return createElement("div", { "data-operator-shell": accessProfile }); }',
  WorkspaceSetupGate: "export function WorkspaceSetupGate({ children }) { return children; }",
  OctopusTerminal: 'export function OctopusTerminal() { return createElement("div", { "data-terminal": "" }); }',
  shellSession: "export function createLaunchShellSession() { return {}; }",
  WelcomeOverlay: "export function WelcomeOverlay() { return null; }",
  PasswordGate: 'export function PasswordGate() { return createElement("div", { "data-password-gate": "" }); }',
  ChatGptSignInGate: "export function ChatGptSignInGate() { return null; }",
  MorphVideoDotCloud: "export function StateMorphDotCloud() { return null; }",
  TrafficLights: "export function TrafficLights() { return null; }",
  QuickTaskWindow: "export function QuickTaskWindow() { return null; } export function QuickTaskAccessUnavailable() { return null; }",
  WorkerEnvironmentWindow: "export function WorkerEnvironmentWindow() { return null; } export function WorkerEnvironmentWindowAccessUnavailable() { return null; }",
  TaskMonitorWindow: "export function TaskMonitorWindow() { return null; } export function TaskMonitorAccessUnavailable() { return null; }",
};
const bundle = (entry, names) => build({
  stdin: { contents: entry, resolveDir: appRoot, loader: "ts" },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  plugins: [{
    name: "launch-stubs",
    setup(builder) {
      builder.onResolve({ filter: new RegExp(`/(${names.join("|")})$`) }, (args) => ({ path: path.basename(args.path), namespace: "stub" }));
      builder.onLoad({ filter: /.*/, namespace: "stub" }, (args) => ({ loader: "js", contents: `import { createElement } from "react";\n${stubbed[args.path]}` }));
    },
  }],
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl", ".webm": "dataurl", ".mp4": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return module.exports;
});
const appBundle = bundle([
  'export { App } from "./src/App";',
  'export { LaunchTerminal } from "./src/components/onboarding/LaunchTerminal";',
  'export { OctopusLoadingState } from "./src/components/OctopusLoadingLogo";',
].join("\n"), Object.keys(stubbed).filter((name) => name !== "WorkspaceSetupGate").concat("WorkspaceSetupGate"));
const gateBundle = bundle('export { WorkspaceSetupGate } from "./src/components/onboarding/WorkspaceSetupGate";', ["OctopusTerminal", "shellSession"]);

const container = document.getElementById("root");
const root = createRoot(container);
after(() => { root.unmount(); w.close(); });
const settle = (ms = 30) => new Promise((resolve) => setTimeout(resolve, ms));
const query = (selector) => container.querySelector(selector);
const runtimeListeners = new Set();
function host(access, runtime) {
  w.octopusTransport = {
    accessStatus: async () => access,
    bootStatus: async () => runtime,
    onRuntimeHostState: (listener) => { runtimeListeners.add(listener); return () => runtimeListeners.delete(listener); },
    onClientAccessLocked: () => () => {},
  };
}
const runtime = (phase, ready = false) => ({ phase, ready, ownership: "owned", pid: 1, home: "/home", sourceRegistry: "", runtimeBinary: "", repoRoot: "", launchCwd: "", launchCommand: "", launchEnv: {}, endpoint: "", error: null, updatedAtMs: 1 });
const access = (profile) => ({ configured: true, authenticated: true, profile });
async function showApp() {
  const { App } = await appBundle;
  root.render(null);
  await settle();
  root.render(React.createElement(App));
  await settle(60);
}

test("the window shows the bundled octopus logo before the client script runs", () => {
  const page = new JSDOM(fs.readFileSync(path.join(appRoot, "index.html"), "utf8")).window.document;
  const loader = page.querySelector("#root > .startup-loader");
  assert.ok(loader);
  assert.equal(loader.getAttribute("aria-hidden"), "true");
  const logo = loader.querySelector("img.startup-loader__base");
  assert.deepEqual([logo.getAttribute("src"), logo.getAttribute("alt")], ["./octopus-logo.png", ""]);
  assert.ok(fs.existsSync(path.join(appRoot, "public", "octopus-logo.png")));
});

test("the loading status draws the octopus logo and masks its shimmer with the same logo", async () => {
  const { OctopusLoadingState } = await appBundle;
  const markup = new JSDOM(renderToStaticMarkup(React.createElement(OctopusLoadingState, { label: "Opening Octopus" }))).window.document;
  const status = markup.querySelector('[role="status"][aria-label="Opening Octopus"]');
  assert.equal(status.getAttribute("aria-busy"), "true");
  assert.equal(status.querySelector("img").getAttribute("src"), "./octopus-logo.png");
  const overlay = status.querySelector(".octopus-loading-logo-overlay").getAttribute("style");
  assert.match(overlay, /-webkit-mask-image:url\(\.\/octopus-logo\.png\)/);
  assert.match(overlay, /(^|;)mask-image:url\(\.\/octopus-logo\.png\)/);
});

test("a live client opens behind the logo while the runtime is probed and behind the launch scene while it starts", async () => {
  host(access("live"), runtime("probing"));
  await showApp();
  assert.ok(query('[role="status"][aria-label="Opening Octopus"] .octopus-loading-logo'));
  assert.ok(!query('[aria-label="Octopus launch"]') && !query("[data-operator-shell]"));
  host(access("live"), runtime("starting"));
  await showApp();
  assert.ok(query('main[aria-label="Octopus launch"]'));
  assert.ok(!query("[data-operator-shell]") && !query('[aria-label="Opening Octopus"]'));
  for (const listener of runtimeListeners) listener(runtime("ready", true));
  await settle();
  assert.equal(query("[data-operator-shell]").dataset.operatorShell, "live");
});

test("a lab client opens its shell without waiting on the runtime", async () => {
  host(access("lab"), runtime("starting"));
  await showApp();
  assert.equal(query("[data-operator-shell]").dataset.operatorShell, "lab");
  assert.ok(!query('[aria-label="Octopus launch"]') && !query('[aria-label="Opening Octopus"]'));
});

test("the launch scene greets with the logo and leaves only after the greeting", async () => {
  const { LaunchTerminal } = await appBundle;
  const exits = [];
  root.render(React.createElement(LaunchTerminal, { greeting: true, leaving: true, onExited: () => exits.push(Date.now()) }));
  await settle();
  const scene = query('main[aria-label="Octopus launch"]');
  assert.equal(scene.querySelector(".launch-scene-greeting img").getAttribute("src"), "./octopus-logo.png");
  assert.deepEqual([scene.dataset.phase, scene.dataset.leaving, exits.length], ["greeting", undefined, 0]);
  await settle(400);
  assert.equal(exits.length, 0, "leaving during the greeting waits for it");
  await settle(450);
  assert.deepEqual([query('main[aria-label="Octopus launch"]').dataset.phase, exits.length], ["terminal", 1]);
  root.render(null);
  await settle();
  root.render(React.createElement(LaunchTerminal, { greeting: false, leaving: true, onExited: () => exits.push(Date.now()) }));
  await settle();
  assert.equal(exits.length, 2, "without a greeting the scene leaves at once");
  assert.ok(!query(".launch-scene-greeting"));
});

test("a live workspace gate opens behind the logo until its workspace is read; a lab gate opens at once", async () => {
  const { WorkspaceSetupGate } = await gateBundle;
  w.octopusTransport = new Proxy({}, { get: (_, name) => typeof name === "string" && name.startsWith("on") ? () => () => {} : () => new Promise(() => {}) });
  const shell = React.createElement("div", { "data-shell": "" });
  root.render(React.createElement(WorkspaceSetupGate, { accessProfile: "live" }, shell));
  await settle();
  assert.ok(query('[role="status"][aria-label="Opening Octopus"] .octopus-loading-logo'));
  assert.ok(!query("[data-shell]"));
  root.render(null);
  await settle();
  root.render(React.createElement(WorkspaceSetupGate, { accessProfile: "lab" }, shell));
  await settle();
  assert.ok(query("[data-shell]"));
  assert.ok(!query('[aria-label="Opening Octopus"]'));
  root.render(null);
  await settle();
});
