const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "localStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.HTMLElement.prototype.getBoundingClientRect = function () { return { x: 600, y: 10, left: 600, top: 10, right: 632, bottom: 42, width: 32, height: 32 }; };

const React = require("react");
const { createRoot } = require("react-dom/client");
const compiled = build({
  stdin: {
    contents: [
      'export { ChatThreadHeader } from "./src/components/chat/ChatThreadHeader";',
      'export { WorkspaceCompactTabProvider } from "./src/components/shell/WorkspaceCompactTabContext";',
      'export { subscribeOpenClientTaskActionsMenu } from "./src/shell/taskActionsMenu";',
      'export { subscribeClientCommands } from "./src/clientActions";',
    ].join("\n"),
    resolveDir: path.join(__dirname, ".."), loader: "ts",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return module.exports;
});

const root = createRoot(document.getElementById("root"));
after(() => { root.unmount(); w.close(); });
const settle = () => new Promise((resolve) => setTimeout(resolve, 20));
const query = (selector) => document.querySelector(selector);
const run = (extra = {}) => ({ runId: "run-1", title: "Quarterly report", clientSurfaceRole: "task", ...extra });
const noop = () => {};
async function show(props, compactTab = null) {
  const { ChatThreadHeader, WorkspaceCompactTabProvider } = await compiled;
  root.render(React.createElement(WorkspaceCompactTabProvider, { value: compactTab },
    React.createElement(ChatThreadHeader, {
      accessProfile: "lab", activeRun: run(), edgeScrolled: false, pinnedContextCount: 0, pinnedContextOpen: false, summaryDisplayMode: "overlay",
      summaryOpen: false, summaryPopoverContent: React.createElement("p", null, "Summary body"), mode: "window",
      onPinnedContextOpenChange: noop, onSummaryOpenChange: noop, ...props,
    })));
  await settle();
}
const press = async (target, type, init = {}) => {
  target.dispatchEvent(new w[type === "keydown" ? "KeyboardEvent" : "MouseEvent"](type, { bubbles: true, cancelable: true, ...init }));
  await settle();
};

test("task actions open the task menu for the exact run from the button, the keyboard and the title", async () => {
  const { subscribeOpenClientTaskActionsMenu } = await compiled;
  const requests = [];
  const stop = subscribeOpenClientTaskActionsMenu((request) => requests.push(request));
  try {
    await show();
    const actions = query('button[aria-label="Task actions"]');
    assert.equal(actions.getAttribute("aria-haspopup"), "menu");
    await press(actions, "click");
    await press(actions, "keydown", { key: "ArrowDown" });
    await press(actions, "keydown", { key: "Enter" });
    await press(query("[data-thread-title]"), "contextmenu", { clientX: 40, clientY: 12 });
    assert.equal(requests.length, 3);
    assert.ok(requests.every((request) => request.runId === "run-1"));
    await show({ activeRun: run({ clientSurfaceRole: "home" }) });
    assert.ok(!query('button[aria-label="Task actions"]'), "Home has no task actions");
  } finally {
    stop();
  }
});

test("the title opens recent tasks and hands pane drags to the compact tab", async () => {
  const { subscribeClientCommands } = await compiled;
  const commands = [];
  const stop = subscribeClientCommands((command) => commands.push(command));
  const drags = [];
  try {
    await show({ mode: "pane" }, { onPointerDown: (event) => drags.push(event.type) });
    const title = query("[data-thread-title]");
    assert.equal(title.getAttribute("aria-label"), "Open recent tasks. Current task: Quarterly report");
    await press(title, "click");
    assert.ok(JSON.stringify(commands).includes("open_command_menu"));
    await press(title, "pointerdown");
    assert.deepEqual(drags, ["pointerdown"]);
    await show({ mode: "window" }, { onPointerDown: (event) => drags.push(event.type) });
    await press(query("[data-thread-title]"), "pointerdown");
    assert.deepEqual(drags, ["pointerdown"], "a window header is not a tab drag handle");
  } finally {
    stop();
  }
});

test("the summary toggle names its mode, reflects its state and opens an end-aligned popover in overlay mode", async () => {
  const changes = [];
  await show({ onSummaryOpenChange: (open) => changes.push(open) });
  let trigger = query("[data-task-summary-trigger]");
  assert.deepEqual([trigger.getAttribute("aria-label"), trigger.getAttribute("aria-pressed"), trigger.dataset.state], ["Toggle summary", "false", "closed"]);
  assert.ok(trigger.classList.contains("app-titlebar-action"));
  assert.ok(trigger.querySelector(".glyph-taskSummary"));
  await press(trigger, "click");
  assert.deepEqual(changes, [true]);
  await show({ summaryOpen: true, onSummaryOpenChange: (open) => changes.push(open) });
  const popover = query(".task-summary-popover");
  assert.deepEqual([popover.dataset.open, popover.dataset.placement, popover.dataset.align], ["true", "bottom", "end"]);
  assert.equal(query('[role="dialog"][aria-label="Task summary"] [data-task-summary-popover]').textContent, "Summary body");
  await press(query("[data-task-summary-popover]"), "keydown", { key: "Escape" });
  assert.deepEqual(changes, [true, false]);
  await show({ summaryDisplayMode: "shift", summaryOpen: true });
  trigger = query("[data-task-summary-trigger]");
  assert.deepEqual([trigger.getAttribute("aria-label"), trigger.getAttribute("aria-pressed"), trigger.dataset.state], ["Toggle pinned summary", "true", "open"]);
  assert.ok(!query(".task-summary-popover"), "a pinned summary is not a popover");
});

test("pinned context and the top fade follow their state", async () => {
  const changes = [];
  await show({ pinnedContextCount: 2, onPinnedContextOpenChange: (open) => changes.push(open) });
  const pinned = query('button[aria-label="Open pinned context"]');
  await press(pinned, "click");
  assert.deepEqual(changes, [true]);
  await show({ pinnedContextCount: 2, pinnedContextOpen: true });
  assert.equal(query('button[aria-label="Close pinned context"]').getAttribute("aria-pressed"), "true");
  await show({ pinnedContextCount: 0 });
  assert.ok(!query('[aria-label$="pinned context"]'));
  assert.equal(query(".chat-thread-top-fade").dataset.appShellMainContentTopFade, undefined);
  await show({ edgeScrolled: true });
  assert.equal(query(".chat-thread-top-fade").dataset.appShellMainContentTopFade, "visible");
});
