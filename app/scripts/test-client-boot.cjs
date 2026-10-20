const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "MouseEvent", "localStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} disconnect() {} };
w.matchMedia = () => ({ matches: true, addEventListener() {}, removeEventListener() {} });

const React = require("react");
const { createRoot } = require("react-dom/client");
const appRoot = path.resolve(__dirname, "..");
const boot = globalThis.__boot = { launchMounts: 0, launchUnmounts: 0, launchProps: null, terminals: [], reduceMotion: true };
const settle = (ms = 20) => new Promise((resolve) => setTimeout(resolve, ms));

async function compile(entry, stubs, exportName) {
  const importer = path.join(appRoot, entry);
  const result = await build({
    entryPoints: [importer], bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
    loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
    define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
    external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
    plugins: [{ name: "stubs", setup(stage) {
      stage.onResolve({ filter: /.*/ }, (args) => (args.importer === importer && Object.hasOwn(stubs, args.path) ? { path: args.path, namespace: "stub" } : undefined));
      stage.onLoad({ filter: /.*/, namespace: "stub" }, (args) => ({ contents: stubs[args.path], loader: "jsx", resolveDir: appRoot }));
    } }],
  });
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return exportName ? module.exports[exportName] : module.exports;
}

const passthrough = (name) => `export function ${name}({ children }) { return children ?? null; }`;
const nothing = (...names) => names.map((name) => `export function ${name}() { return null; }`).join("\n");
const appStubs = {
  "./components/icon-glyphs/shell": "",
  "./components/shell/OperatorShell": `import { createElement } from "react"; export function OperatorShell() { return createElement("section", { className: "stub-operator-shell" }); }`,
  "./components/onboarding/WelcomeOverlay": nothing("WelcomeOverlay"),
  "./components/onboarding/PasswordGate": `import { createElement } from "react"; export function PasswordGate({ mode }) { return createElement("form", { className: "stub-password-gate", "data-mode": mode }); }`,
  "./components/onboarding/ChatGptSignInGate": `import { createElement } from "react"; export function ChatGptSignInGate() { return createElement("div", { className: "stub-chatgpt-gate" }); }`,
  "./onboarding/chatgptSignIn": "export const chatgptSignInAvailable = () => globalThis.__boot.chatgptAvailable === true;",
  "./account/useChatgptAccount": "export function useChatgptAccount() { return { account: { signedIn: globalThis.__boot.chatgptSignedIn !== false } }; }",
  "./components/onboarding/MorphVideoDotCloud": nothing("StateMorphDotCloud"),
  "./components/onboarding/LaunchTerminal": `import { createElement, useEffect } from "react";
export function LaunchTerminal(props) {
  useEffect(() => { globalThis.__boot.launchMounts += 1; return () => { globalThis.__boot.launchUnmounts += 1; }; }, []);
  globalThis.__boot.launchProps = props;
  return createElement("main", { className: "stub-launch", "data-leaving": String(props.leaving), "data-greeting": String(props.greeting) });
}`,
  "./components/onboarding/WorkspaceSetupGate": passthrough("WorkspaceSetupGate"),
  "./components/shell/TrafficLights": nothing("TrafficLights"),
  "./components/OctopusLoadingLogo": `import { createElement } from "react"; export function OctopusLoadingState({ label }) { return createElement("div", { className: "stub-loading" }, label); }`,
  "./onboarding/onboardingStorage": "const state = { completed: true, visitCount: 3 }; export const completeOnboarding = () => state; export const recordOnboardingVisit = () => state; export const resetOnboardingState = () => {};",
  "./clientAccess": "export const lockClientAccess = async () => ({}); export const readTouchIdStatus = async () => ({ available: false, enrolled: false, prompted: true }); export const setTouchIdEnabled = async () => ({}); export const unlockWithTouchId = async () => ({});",
  "./useClientAccess": `import { useState } from "react"; export function useClientAccess() { const [access, setAccess] = useState(globalThis.__boot.access); globalThis.__boot.setAccess = setAccess; return { access, accessError: null, setAccess }; }`,
  "./toasts": "export const appToast = { danger() {} }; export function AppToastViewport() { return null; }",
  "./clientPreferences": passthrough("ClientPreferencesProvider"),
  "./components/QuickTaskWindow": nothing("QuickTaskWindow", "QuickTaskAccessUnavailable"),
  "./components/worker-environment/WorkerEnvironmentWindow": nothing("WorkerEnvironmentWindow", "WorkerEnvironmentWindowAccessUnavailable"),
  "./components/chat/TaskMonitorWindow": nothing("TaskMonitorWindow", "TaskMonitorAccessUnavailable"),
};
const App = compile("src/App.tsx", appStubs, "App");
const LaunchTerminal = compile("src/components/onboarding/LaunchTerminal.tsx", {
  "../../appearance": "export const useShouldReduceMotion = () => globalThis.__boot.reduceMotion;",
  "../terminal/OctopusTerminal": "export function OctopusTerminal(props) { globalThis.__boot.terminals.push(props); return null; }",
  "../OctopusLoadingLogo": nothing("OctopusLoadingLogo"),
}, "LaunchTerminal");
const RuntimeTopologyProbe = compile("src/runtimeTopology.ts", {}, "useRuntimeTopology").then((useRuntimeTopology) => function Probe() {
  boot.topology = useRuntimeTopology(true, [], false);
  return null;
});

const root = createRoot(document.getElementById("root"));
after(() => { root.unmount(); w.close(); });

function runtimeBridge(initial, extra = {}) {
  const listeners = new Set();
  const bridge = {
    state: initial,
    bootStatus: () => (bridge.state instanceof Error ? Promise.reject(bridge.state) : bridge.state instanceof Promise ? bridge.state : Promise.resolve(bridge.state)),
    onRuntimeHostState: (listener) => { listeners.add(listener); return () => listeners.delete(listener); },
    emit: (state) => { for (const listener of listeners) listener(state); },
    listenerCount: () => listeners.size,
    ...extra,
  };
  window.octopusTransport = bridge;
  return bridge;
}
const runtime = (phase, ready = phase === "ready") => ({ phase, ready, ownership: "none", pid: null, error: null });
const live = { configured: true, authenticated: true, profile: "live" };
const shown = (selector) => document.querySelector(selector) !== null;

async function showApp(access, state) {
  root.render(null);
  await settle(5);
  Object.assign(boot, { access, launchMounts: 0, launchUnmounts: 0, launchProps: null, chatgptAvailable: false, chatgptSignedIn: true });
  const bridge = runtimeBridge(state);
  root.render(React.createElement(await App));
  await settle();
  return bridge;
}

test("the launch terminal gates a stopped runtime before the password screen and leaves once the runtime is ready", async () => {
  const bridge = await showApp({ configured: false, authenticated: false, profile: null }, runtime("unmanaged"));
  assert.ok(shown(".stub-launch"));
  assert.equal(shown(".stub-password-gate"), false, "the launch gate precedes setting a password");
  assert.deepEqual([boot.launchProps.greeting, boot.launchProps.leaving], [true, false]);
  bridge.emit(runtime("ready"));
  await settle();
  assert.equal(document.querySelector(".stub-password-gate").dataset.mode, "set");
  assert.equal(document.querySelector(".stub-launch").dataset.leaving, "true", "the launch scene stays to play its exit");
  assert.deepEqual([boot.launchMounts, boot.launchUnmounts], [1, 0], "the same launch scene carries over into the next screen");
  boot.launchProps.onExited();
  await settle();
  assert.equal(shown(".stub-launch"), false);
  bridge.emit(runtime("unmanaged"));
  await settle();
  assert.equal(shown(".stub-launch"), false, "a runtime that was ready once never brings the launch gate back");
});

test("an authenticated live window waits on the first probe, then shows the shell, and the launch scene crosses over into it", async () => {
  let resolveProbe;
  const bridge = await showApp(live, new Promise((resolve) => { resolveProbe = resolve; }));
  assert.equal(document.querySelector(".stub-loading")?.textContent, "Opening Octopus");
  resolveProbe(runtime("unmanaged"));
  await settle();
  assert.ok(shown(".stub-launch"));
  assert.equal(shown(".stub-operator-shell"), false);
  bridge.emit(runtime("ready"));
  await settle();
  assert.ok(shown(".stub-operator-shell"));
  assert.equal(document.querySelector(".stub-launch").dataset.leaving, "true");
  assert.deepEqual([boot.launchMounts, boot.launchUnmounts], [1, 0]);
});

test("Client Lab and an access-required probe never show the launch gate, and a local unlock comes before the account gate", async () => {
  await showApp({ configured: true, authenticated: true, profile: "lab" }, runtime("unmanaged"));
  assert.ok(shown(".stub-operator-shell"));
  assert.equal(shown(".stub-launch"), false);
  await showApp(live, new Error("Client access session is locked or expired"));
  assert.ok(shown(".stub-operator-shell"));
  assert.equal(shown(".stub-launch"), false);
  root.render(null);
  await settle(5);
  Object.assign(boot, { access: { configured: true, authenticated: false, profile: null }, chatgptAvailable: true, chatgptSignedIn: false });
  runtimeBridge(runtime("ready"));
  root.render(React.createElement(await App));
  await settle();
  assert.equal(document.querySelector(".stub-password-gate")?.dataset.mode, "unlock");
  assert.equal(shown(".stub-chatgpt-gate"), false);
  boot.setAccess(live);
  await settle();
  assert.ok(shown(".stub-chatgpt-gate"));
});

test("the launch scene shows the launch shell, greets first, and exits only after the greeting", async () => {
  const writes = [];
  const data = new Set();
  runtimeBridge(runtime("unmanaged"), {
    openLaunchTerminal: async (dimensions) => { writes.push(["open", dimensions]); return { terminalId: "launch", sessionId: "s-1", running: true, transcript: "$ octopus up" }; },
    writeLaunchTerminal: async (...args) => { writes.push(["write", ...args]); return {}; },
    resizeLaunchTerminal: async (...args) => { writes.push(["resize", ...args]); return {}; },
    onTerminalData: (listener) => { data.add(listener); return () => data.delete(listener); },
    onTerminalExit: () => () => {},
  });
  root.render(null);
  await settle(5);
  boot.terminals.length = 0;
  let exited = 0;
  const show = async (leaving) => {
    root.render(React.createElement(await LaunchTerminal, { greeting: true, leaving, onExited: () => { exited += 1; } }));
    await settle();
  };
  await show(true);
  const scene = document.querySelector(".launch-scene");
  assert.equal(scene.dataset.phase, "greeting");
  assert.equal(scene.textContent.trim(), "", "the launch scene carries no status copy of its own");
  assert.equal(exited, 0, "leaving during the greeting waits for it");
  const [terminal] = boot.terminals;
  assert.equal(terminal.id, "terminal-panel-launch");
  const output = [];
  terminal.session.subscribe({ onData: (text) => output.push(text), onInitLog: (text) => output.push(text) });
  await terminal.session.open({ cols: 120, rows: 40 });
  terminal.session.write("\r");
  terminal.session.resize({ cols: 100, rows: 30 });
  for (const listener of data) listener({ terminalId: "launch", sessionId: "s-1", data: "ready" });
  for (const listener of data) listener({ terminalId: "shell", sessionId: "s-9", data: "foreign" });
  assert.deepEqual(writes, [["open", { cols: 120, rows: 40 }], ["write", "s-1", "\r"], ["resize", "s-1", { cols: 100, rows: 30 }]]);
  assert.deepEqual(output, ["$ octopus up", "ready"]);
  await settle(750);
  assert.equal(document.querySelector(".launch-scene").dataset.phase, "terminal");
  await settle();
  assert.equal(exited, 1);
  root.render(null);
  await settle(5);
  exited = 0;
  await show(false);
  await settle(750);
  assert.equal(exited, 0);
});

test("runtime host state updates the topology status in place without another projection read", async () => {
  root.render(null);
  await settle(5);
  const reads = [];
  const bridge = runtimeBridge(runtime("unmanaged"), {
    projectionRead: async (request) => {
      reads.push(request.method);
      return { generation: 4, snapshotRaw: { projection: { octopus_home: "/fixture-home", ledger_rows: 3 } }, eeRaw: { ee_instances: [] }, runDetailsRaw: {} };
    },
    runtimeStatus: async () => ({ phase: "unmanaged", up: false }),
    onProjectionChanged: () => () => {},
  });
  root.render(React.createElement(await RuntimeTopologyProbe));
  await settle(40);
  assert.deepEqual(reads, ["projection.runtime.topology"]);
  assert.equal(boot.topology.projection.mode, "offline");
  bridge.emit(runtime("ready"));
  await settle();
  assert.deepEqual([boot.topology.projection.mode, boot.topology.projection.label], ["live", "Client observes runtime socket"]);
  bridge.emit({ ...runtime("degraded"), error: "socket refused" });
  await settle();
  assert.deepEqual([boot.topology.projection.mode, boot.topology.projection.detail], ["error", "socket refused"]);
  assert.deepEqual(reads, ["projection.runtime.topology"], "a runtime state change is not a projection change");
  root.render(null);
  await settle(5);
  assert.equal(bridge.listenerCount(), 0);
});
