const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
const { test, after, afterEach } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");
const React = require("react");
const { act } = React;
const dom = new JSDOM("<!doctype html><html><body></body></html>", { pretendToBeVisual: true, url: "http://localhost" });
for (const name of ["window", "document", "navigator", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "Element", "Node", "Event", "KeyboardEvent", "MutationObserver"]) global[name] = name === "window" ? dom.window : dom.window[name];
global.IS_REACT_ACT_ENVIRONMENT = true;
global.getComputedStyle = dom.window.getComputedStyle;
global.requestAnimationFrame = dom.window.requestAnimationFrame.bind(dom.window);
global.cancelAnimationFrame = dom.window.cancelAnimationFrame.bind(dom.window);
global.ResizeObserver = class { observe() {} disconnect() {} };
window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
HTMLElement.prototype.scrollIntoView = () => {};
HTMLElement.prototype.getBoundingClientRect = () => ({ x: 100, y: 100, top: 100, left: 100, right: 400, bottom: 132, width: 300, height: 32 });
const { createRoot } = require("react-dom/client");
const roots = [];
const runtimeErrors = [];
window.addEventListener("error", (event) => runtimeErrors.push(event.error ?? event.message));
afterEach(async () => { await act(async () => { for (const root of roots.splice(0)) root.unmount(); }); document.body.replaceChildren(); assert.deepEqual(runtimeErrors.splice(0), []); });
after(() => dom.window.close());

const appRoot = path.resolve(__dirname, "..");
const compiled = build({
  stdin: { contents: 'import "./src/components/icon-glyphs/shell"; export * from "./src/components/task/TasksViewSwitcher"; export * from "./src/components/scheduled/ScheduledWorkspace"; export * from "./src/components/scheduled/ScheduledSidebar"; export * from "./src/components/scheduled/LiveScheduledView";', resolveDir: appRoot, loader: "tsx" },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent", loader: { ".css": "empty" },
  external: ["react", "react-dom", "react/jsx-runtime"],
  plugins: [{ name: "schedule-fixture", setup(builder) {
    builder.onResolve({ filter: /projectionClient$|projectionLive$/ }, ({ path: imported }) => ({ path: imported.endsWith("projectionClient") ? "projection" : "live", namespace: "fixture" }));
    builder.onLoad({ filter: /.*/, namespace: "fixture" }, ({ path: name }) => ({ contents: name === "live"
      ? "export function useProjectionGeneration() { return 0; }"
      : `export function canMutateScheduledInterfaces() { return globalThis.scheduleFixture.writable; }
         export async function readProjection(name) { const f = globalThis.scheduleFixture; f.reads.push(name); if (f.error) throw Error(f.error); return { scheduled_interfaces: f.rows }; }
         export async function mutateScheduledInterface(request) { return globalThis.scheduleFixture.mutate(request); }`, loader: "js" }));
  } }],
}).then(({ outputFiles }) => { const module = { exports: {} }; Function("module", "exports", "require", outputFiles[0].text)(module, module.exports, require); return module.exports; });

function sidebarChrome(ui) {
  const source = ts.createSourceFile("Sidebar.tsx", fs.readFileSync(path.join(appRoot, "src/components/Sidebar.tsx"), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const declarations = source.statements.filter((node) => ts.isFunctionDeclaration(node) && ["Sidebar", "moveSidebarFocus"].includes(node.name?.text)).map((node) => node.getText(source).replace(/^export /, "")).join("\n");
  const code = ts.transpileModule(declarations, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
  const Chrome = Function("require", "exports", "useState", "useEffect", "useAppErrorToast", "TasksViewSwitcher", "ScheduledSidebar", "TaskNavigation", `${code}; return Sidebar;`)(require, {}, React.useState, React.useEffect, () => {}, ui.TasksViewSwitcher, ui.ScheduledSidebar, ({ archivedView }) => React.createElement("div", { "data-task-navigation": archivedView ? "archived" : "tasks" }));
  return function Navigation() {
    const [surface, setSurface] = React.useState("scheduled");
    return React.createElement(Chrome, { accessProfile: "live", navigationSection: "tasks", activeSurfaceId: surface, activeChatRunId: "", chatRuns: [], runtimeHost: { home: "fixture" }, surfaceErrors: [], onSurfaceChange: setSurface, onAgentProjectFilterEeIdChange() {} });
  };
}

function schedule(index, status = "enabled") {
  return {
    interface_id: `world_task_${index}`, display_name: `Task ${index}`, status, valid: status !== "invalid", activation_digest: String(index % 10).repeat(64),
    schedule: { type: "cron", cron: "0 13 * * *" }, timezone: "UTC", fire_on_start: false, instructions: `Review task ${index}.`, clone_eligible: status !== "invalid",
    run_template: { run_id: `run_task_${index}`, ee_id: "ee_project", worker_interface_id: "system_worker_flash", checkpoint_count: 2, outbound_interface_ids: ["world_delivery"], artifact_binding_count: 1 },
    next_run_at_ms: Date.now() + (index + 1) * 60_000, next_run_source: "driver_state", error: status === "invalid" ? "Invalid fixture configuration" : "",
  };
}
function fixture(rows) {
  const f = { rows, reads: [], writes: [], error: "", writable: true, async mutate(request) {
    f.writes.push(request);
    if (request.action === "create") f.rows = [...f.rows, { ...schedule(20), interface_id: request.interfaceId, display_name: "New brief", instructions: request.draft.instructions, schedule: { type: "cron", cron: request.draft.cron } }];
    else if (request.action === "set_enabled") f.rows = f.rows.map((row) => row.interface_id === request.interfaceId ? { ...row, status: request.enabled ? "enabled" : "disabled" } : row);
  } };
  global.scheduleFixture = f;
  return f;
}
async function mount({ active = true, scope = "Experiment", secondary = false } = {}) {
  const ui = await compiled;
  const Navigation = sidebarChrome(ui);
  const host = document.createElement("div"); document.body.append(host);
  const root = createRoot(host); roots.push(root);
  const render = async (next = scope) => act(async () => root.render(React.createElement(ui.ScheduledWorkspaceProvider, { active, key: next },
    !secondary && React.createElement(Navigation), React.createElement(ui.LiveScheduledView, { navigationInPane: secondary, searchFocusTick: 0 }))));
  await render();
  return { host, render };
}
function button(host, label) { return [...host.querySelectorAll("button")].find((element) => element.getAttribute("aria-label") === label || element.textContent.trim() === label); }
async function click(element) { assert.ok(element, "button exists"); await act(async () => element.click()); }
async function input(element, value) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
  await act(async () => { setter.call(element, value); element.dispatchEvent(new Event("input", { bubbles: true })); });
}

test("Scheduled keeps navigation, selection, search and existing authoring in one workspace", async () => {
  const f = fixture([...Array.from({ length: 8 }, (_, index) => schedule(index)), schedule(8, "disabled"), schedule(9, "invalid")]);
  const { host } = await mount();
  assert.deepEqual(f.reads, ["projection.scheduled_interfaces.list"]);
  const switcher = host.querySelector('[aria-label="Tasks views"]');
  assert.equal(host.querySelector("aside h1"), null);
  assert.equal(button(switcher, "Scheduled tasks").getAttribute("aria-pressed"), "true");
  await click(button(switcher, "All tasks")); assert.ok(host.querySelector('[data-task-navigation="tasks"]'));
  await click(button(switcher, "Scheduled tasks")); assert.ok(host.querySelector(".scheduled-sidebar"));
  await click(button(switcher, "Archived tasks")); assert.ok(host.querySelector('[data-task-navigation="archived"]'));
  await click(button(switcher, "Scheduled tasks")); assert.equal(host.querySelector('[aria-label="Tasks views"]'), switcher);
  assert.equal(host.querySelectorAll(".scheduled-suggestion").length, 6);
  assert.equal(host.querySelectorAll(".scheduled-sidebar-row").length, 7);
  assert.match(host.querySelector(".scheduled-sidebar-row-detail").textContent, /Today .* · Daily at /);
  await click(button(host, "Show more")); assert.equal(host.querySelectorAll(".scheduled-sidebar-row").length, 10);
  await click(button(host, "Search scheduled tasks"));
  const search = host.querySelector(".scheduled-sidebar-search input"); assert.equal(document.activeElement, search);
  await input(search, "Task 6"); assert.equal(host.querySelectorAll(".scheduled-sidebar-row").length, 1);
  await click(host.querySelector(".scheduled-sidebar-row"));
  assert.equal(host.querySelector(".scheduled-detail h1").textContent, "Task 6");
  assert.equal(host.querySelector("[data-selected-scheduled-task]").getAttribute("aria-current"), "true");
  assert.equal(host.querySelector(".scheduled-interface-details").open, false);
  await act(async () => search.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  assert.equal(host.querySelector(".scheduled-sidebar-search input"), null);
  assert.equal(document.activeElement, button(host, "Search scheduled tasks"));
  assert.equal(host.querySelector(".scheduled-detail h1").textContent, "Task 6");
  await click(button(host, "Back to Scheduled"));
  await click(host.querySelector(".scheduled-suggestion"));
  assert.equal(f.writes.length, 0, "suggestions only open a draft");
  assert.equal(host.querySelector('input[placeholder="0 9 * * 1-5"]').value, "0 9 * * 1-5");
  await click(button(host, "Create"));
  assert.equal(f.writes.length, 1);
  assert.equal(f.writes[0].action, "create");
  assert.equal(f.writes[0].sourceInterfaceId, "world_task_0");
  assert.equal(f.writes[0].sourceActivationDigest, "0".repeat(64));
  assert.equal(f.writes[0].draft.fireOnStart, false);
  assert.equal(f.writes[0].draft.timezone, "UTC");
  assert.equal(host.querySelector(".scheduled-detail h1").textContent, "New brief");
});

test("filters, pause, empty and read-only states retain exact declared interface identity", async () => {
  const f = fixture([schedule(0), schedule(1, "disabled")]);
  const { host, render } = await mount();
  await click(button(host, "Filter tasks")); await click(button(document, "Paused"));
  assert.equal(host.querySelectorAll(".scheduled-sidebar-row").length, 1);
  await click(host.querySelector(".scheduled-sidebar-row"));
  await click(button(host, "Scheduled task actions")); await click(button(document, "Resume"));
  assert.deepEqual(f.writes[0], { action: "set_enabled", interfaceId: "world_task_1", expectedDigest: "1".repeat(64), enabled: true });
  assert.equal(host.querySelector(".scheduled-detail-state").textContent, "Active");
  f.rows = []; await render("empty");
  assert.equal(button(host, "New task").disabled, true);
  assert.ok([...host.querySelectorAll(".scheduled-suggestion")].every((element) => element.disabled));
  assert.match(host.textContent, /complete scheduled interface template/);
  f.rows = [schedule(0)]; f.writable = false; await render("read-only");
  assert.equal(button(host, "New task").disabled, true);
  await click(host.querySelector(".scheduled-sidebar-row")); assert.equal(button(host, "Edit").disabled, true);
  f.error = "Projection disconnected"; await render("error");
  assert.match(host.textContent, /Projection disconnected/);
  assert.equal(host.querySelectorAll(".scheduled-sidebar-row").length, 0);
});

test("inactive Scheduled reads nothing and calendar labels respect timezone boundaries", async () => {
  const f = fixture([schedule(0)]);
  await mount({ active: false }); assert.equal(f.reads.length, 0);
  const { scheduledCalendarTime } = await compiled;
  const now = Date.parse("2026-10-03T21:30:00Z"), next = Date.parse("2026-10-04T01:00:00Z");
  assert.match(scheduledCalendarTime(next, now, "Europe/Kyiv"), /^Today /);
  assert.match(scheduledCalendarTime(next, now, "UTC"), /^Tomorrow /);
  assert.equal(scheduledCalendarTime(next, now, "invalid/timezone"), "Time unavailable");
});

test("a secondary Scheduled pane retains its own list without replacing the primary sidebar", async () => {
  fixture([schedule(0)]);
  const { host } = await mount({ secondary: true });
  assert.equal(host.querySelector("aside"), null);
  await click(button(host, "Scheduled tasks"));
  assert.ok(document.querySelector('[role="dialog"] .scheduled-sidebar-row'));
  await click(document.querySelector('[role="dialog"] .scheduled-sidebar-row'));
  assert.equal(host.querySelector(".scheduled-detail h1").textContent, "Task 0");
  assert.equal(document.querySelector('[role="dialog"]'), null);
});

test("one-shot deadlines retain their kind and exact timestamp through Scheduled editing", async () => {
  const dueAt = "2026-10-12T14:30:00.123456789+03:00";
  const one = { ...schedule(0), schedule: { type: "once", due_at: dueAt }, next_run_at_ms: Date.parse(dueAt), next_run_source: "schedule" };
  const f = fixture([one]);
  const { host } = await mount();
  assert.match(host.querySelector('.scheduled-sidebar-row-detail').textContent, /Once/);
  assert.doesNotMatch(host.querySelector('.scheduled-sidebar-row-detail').textContent, /Estimated/);
  await click(host.querySelector('.scheduled-sidebar-row'));
  await click(button(host, 'Edit'));
  assert.ok(host.querySelector('input[type="datetime-local"]'));
  assert.equal(host.querySelector('input[placeholder="0 9 * * 1-5"]'), null);
  await click(button(host, 'Save'));
  assert.equal(f.writes.length, 1);
  assert.equal(f.writes[0].draft.type, 'once');
  assert.equal(f.writes[0].draft.dueAt, dueAt);
  assert.equal(f.writes[0].draft.fireOnStart, false);
  assert.equal(f.writes[0].draft.cron, undefined);
});

test("admitted one-shot displays Triggered without inventing a next run or Task success", async () => {
  fixture([{ ...schedule(0), schedule: { type: 'once', due_at: '2026-10-04T00:00:00Z' }, last_run_at_ms: Date.parse('2026-10-04T00:00:01Z'), next_run_at_ms: null, next_run_source: '' }]);
  const { host } = await mount();
  assert.match(host.querySelector('.scheduled-sidebar-row-detail').textContent, /Triggered/);
  await click(host.querySelector('.scheduled-sidebar-row'));
  assert.equal(host.querySelector('.scheduled-detail-state').textContent, 'Triggered');
  assert.match(host.querySelector('.scheduled-detail').textContent, /Does not repeat/);
});

test("one-shot timestamps validate calendar dates and preserve nanosecond deadline identity", () => {
  const { parseScheduledDueAt } = require('../electron/scheduledTime.cjs');
  assert.equal(parseScheduledDueAt('2026-10-12T14:30:00.123456789+03:00'), parseScheduledDueAt('2026-10-12T11:30:00.123456789Z'));
  assert.equal(parseScheduledDueAt('2026-10-12T11:30:00.123456790Z') - parseScheduledDueAt('2026-10-12T11:30:00.123456789Z'), 1n);
  for (const invalid of ['2026-02-30T00:00:00Z', '2026-10-04T00:00:00-00:00', '2026-10-04T00:00:00', '2263-01-01T00:00:00Z']) assert.throws(() => parseScheduledDueAt(invalid));
});
