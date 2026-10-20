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
const compiled = build({
  entryPoints: [path.join(__dirname, "../src/components/chat/TaskSummaryPanel.tsx")],
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
}).then((result) => {
  const module = { exports: {} };
  const diffs = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : name === "preloadHighlighter" ? async () => {} : () => null });
  Function("require", "module", "exports", result.outputFiles[0].text)((id) => /^(@pierre\/diffs|shiki|@shikijs|mermaid|katex)/.test(id) ? diffs : require(id), module, module.exports);
  return module.exports.TaskSummaryPanel;
});

const root = createRoot(document.getElementById("root"));
after(() => { root.unmount(); w.close(); });
const settle = () => new Promise((resolve) => setTimeout(resolve, 20));
const message = (id, eventKind, text, eventData = {}) => ({ id, role: "assistant", text, createdAtMs: 1, eventKind, eventData });
async function show(messages, props = {}) {
  const TaskSummaryPanel = await compiled;
  const events = { revealed: [], shown: [], opened: [] };
  root.render(React.createElement(TaskSummaryPanel, {
    messages, runProjection: null, onOpenEnvironment: (...args) => events.opened.push(args),
    onRevealMessage: (id) => events.revealed.push(id), onShowAllSources: (sources) => events.shown.push(sources), ...props,
  }));
  await settle();
  return events;
}
const section = (title) => [...document.querySelectorAll(".task-summary-section")].find((element) => element.querySelector(".task-summary-section-heading")?.textContent.trim() === title);

test("related agents merge every report about one agent into its latest working or finished state", async () => {
  const events = await show([
    message("spawn", "collaboration", "", { tool: "spawnAgent", status: "completed", receiver_thread_ids: ["thread-a", "thread-b"], agents_states: { "thread-a": { status: "running" }, "thread-b": { status: "pendingInit" } } }),
    message("named", "subagent_activity", "", { agent_thread_id: "thread-a", agent_path: "root/data_checker", kind: "started" }),
    message("wait", "collaboration", "", { tool: "wait", agents_states: { "thread-b": { status: "completed" } } }),
    message("stopped", "subagent_activity", "", { agent_thread_id: "thread-c", agent_path: "root/chart_maker", kind: "interrupted" }),
    message("noise", "collaboration", "", { tool: "shell", agents_states: { "thread-z": { status: "running" } } }),
  ]);
  const agents = section("Related agents");
  assert.ok(agents);
  assert.equal(agents.querySelector(".task-summary-subagent-counts").textContent, "1 working2 finished");
  const rows = [...agents.querySelectorAll(".task-summary-related-agents .task-summary-row")];
  assert.deepEqual(rows.map((row) => [row.title, row.querySelector(".task-summary-row-title").textContent]),
    [["thread-a", "Data checker"], ["thread-b", "Agent"], ["thread-c", "Chart maker"]]);
  assert.equal(agents.querySelectorAll(".task-summary-agent-avatar.is-active").length, 1);
  rows[1].click();
  assert.deepEqual(events.revealed, ["wait"], "an agent row reveals the latest report about that agent");
  assert.ok(!agents.querySelector(".task-summary-subagents").onclick);
});

test("the plan row reveals the latest plan and sources offer every row behind View all", async () => {
  const sources = Array.from({ length: 5 }, (_, index) => ({ ...message(`upload-${index}`, "message", `Attached ${index}`), role: "user",
    attachments: [{ id: `file-${index}`, name: `notes-${index}.md`, size: 10, type: "text/markdown", label: "operator.file", lastModified: 1 }] }));
  const events = await show([message("plan-old", "plan", "Old plan"), message("plan-new", "plan", "Gather numbers\nWrite report"), ...sources]);
  const plan = document.querySelector(".task-summary-plan");
  assert.ok(plan.querySelector(".glyph-taskPlan"));
  plan.click();
  assert.deepEqual(events.revealed, ["plan-new"]);
  const sourcesSection = section("Sources");
  assert.ok(sourcesSection, document.body.textContent.slice(0, 300));
  const viewAll = sourcesSection.querySelector(".task-summary-show-all");
  assert.ok(viewAll.querySelector(".glyph-viewAll"));
  viewAll.click();
  assert.equal(events.shown.length, 1);
  assert.equal(sourcesSection.querySelectorAll(".task-summary-list > .task-summary-row:not(.task-summary-show-all)").length, 3, "the summary shows three sources");
  assert.equal(events.shown[0].length, 5, "View all hands over every source");
});

test("a task without a projection says so, and one without environments says that instead", async () => {
  await show([]);
  assert.equal(document.querySelector(".task-environment-empty").textContent, "Environment information unavailable");
  await show([], { runProjection: { workerEnvironments: [], workerEnvironment: { id: "" } } });
  assert.equal(document.querySelector(".task-environment-empty").textContent, "No environments attached to this task");
  assert.ok(!section("Related agents"));
});
