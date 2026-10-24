const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "HTMLInputElement", "Element", "SVGElement", "Node", "Event", "KeyboardEvent", "MouseEvent", "localStorage", "navigator"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.HTMLElement.prototype.scrollTo = function () {};
w.HTMLElement.prototype.scrollIntoView = function () {};
w.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 100, y: 100, left: 100, top: 100, right: 200, bottom: 120, width: 100, height: 20 });

const React = require("react");
const { createRoot } = require("react-dom/client");
const compiled = build({
  stdin: {
    contents: [
      'export { ComposerUtilityBar } from "./src/components/chat/ComposerUtilityBar";',
      'export { AgentPresetDialog } from "./src/components/agent/AgentPresetDialog";',
      'export { projectChatExecutionContexts } from "./src/chatRuns";',
      'export { resolveClientAgents } from "./src/agentPresets";',
    ].join("\n"),
    resolveDir: path.join(__dirname, ".."), loader: "ts",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return module.exports;
});

const root = createRoot(document.getElementById("root"));
after(() => { root.unmount(); w.close(); });
const settle = async (rounds = 3) => { for (let round = 0; round < rounds; round += 1) await new Promise((resolve) => setTimeout(resolve, 10)); };
const press = async (element) => {
  element.dispatchEvent(new w.MouseEvent("pointerdown", { bubbles: true, cancelable: true, button: 0 }));
  await settle();
};
const click = async (element) => { element.click(); await settle(); };
const text = (element) => element.textContent.replace(/\s+/g, " ").trim();
const menuRows = () => [...document.querySelectorAll('[role="menu"] [role="menuitem"]')];

const application = (id, name) => ({ id, name, category: "connector", description: `${name} access`, interfaces: [], artifact_bindings: [], task_capabilities: { tools: [] } });
const driverRows = [
  { name: "system_shell", capabilities: ["shell"], root_slots: ["host_filesystem_root"], door_interfaces: ["door.shell"], tools: [], host_slots: {} },
  { name: "system_browser", capabilities: ["browser"], root_slots: [], door_interfaces: ["door.browser"], tools: [], host_slots: {} },
];
const agentWorker = (id, agentId, name, createdAtMs) => ({ id, label: id, provider_interface_ids: ["provider.a"], interface_policy: { "provider.a": { models: ["alpha", "beta"] } },
  applications: [application("app.notes", "Notes"), application("app.mail", "Mail")], drivers: driverRows,
  client_agent_id: agentId, client_agent_name: name, client_agent_icon: "default", client_agent_created_at_ms: createdAtMs, client_agent_updated_at_ms: createdAtMs, client_agent_deleted_at_ms: 0,
  client_template_digest: `template-${agentId}`, client_template_provider_interface_id: "provider.a", client_template_provider_model: "beta", client_template_reasoning_effort: "high",
  client_template_application_ids: ["app.notes"], client_template_worker_environment_kinds: ["system_shell"] });
const contextRows = [
  { ee_id: "ee.root", is_os_root: true, display_name: "Root", agent_catalog_digest: "catalog-root", workers: [{ id: "worker.root", label: "worker.root" }] },
  { ee_id: "ee.alpha", display_name: "Alpha", agent_catalog_digest: "catalog-alpha", default_task_access_mode: "full_access", workers: [agentWorker("worker.alpha", "agent-alpha", "Analyst", 1)] },
  { ee_id: "ee.beta", display_name: "Beta", agent_catalog_digest: "catalog-beta", default_task_access_mode: "full_access", workers: [agentWorker("worker.beta", "agent-beta", "Builder", 2)] },
];

async function utilityBar(overrides = {}) {
  const { ComposerUtilityBar, projectChatExecutionContexts, resolveClientAgents } = await compiled;
  const contexts = projectChatExecutionContexts({ contexts: contextRows });
  root.render(null);
  await settle();
  const calls = [];
  const record = (name) => (...args) => calls.push([name, ...args]);
  root.render(React.createElement(ComposerUtilityBar, {
    contexts, agentPresets: resolveClientAgents(contexts), agentPresetId: "agent-alpha", accessMode: "project_access", fullAccessUnavailableReason: "",
    applicationIds: ["app.notes"], applicationLocked: false, applicationLockReason: "", driverNames: ["system_shell"], eeId: "ee.alpha", locked: false, rooms: [], runId: "",
    workerInterfaceId: "worker.alpha", onAgentChange: record("agent"), onAccessModeChange: record("access"), onEeChange: record("project"),
    onApplicationToggle: record("application"), onDriverToggle: record("driver"), ...overrides,
  }));
  await settle();
  const trigger = (target) => document.querySelector(`[data-composer-navigation-target="${target}"]`);
  return { calls, trigger, toggle: () => document.querySelector(".composer-agent-access-toggle") };
}
const close = async () => { w.dispatchEvent(new w.KeyboardEvent("keydown", { key: "Escape" })); await settle(); };

test("project selection keeps each project's configured logo and color", async () => {
  const { projectChatExecutionContexts } = await compiled;
  const contexts = projectChatExecutionContexts({ contexts: contextRows }, new Map(),
    new Map([["ee.alpha", "green"], ["ee.beta", "blue"]]),
    new Map([["ee.alpha", "code"], ["ee.beta", "rocket"]]));
  const bar = await utilityBar({ contexts, agentPresetId: "" });
  const selected = bar.trigger("workspace-project").querySelector('[data-project-icon="code"]');
  assert.ok(selected);
  await press(bar.trigger("workspace-project"));
  const icons = menuRows().map(row => row.querySelector('[data-project-icon]'));
  assert.deepEqual(icons.map(icon => icon?.dataset.projectIcon), [undefined, "code", "rocket"]);
  assert.equal(icons[1].style.color, selected.style.color);
  assert.notEqual(icons[1].style.color, icons[2].style.color);
  await close();
});

test("the Agent control lists Agents with the access each would run under, and the chosen Agent fixes the project", async () => {
  const bar = await utilityBar();
  assert.match(document.querySelector(".composer-utility-bar").getAttribute("style"), /--color-token-composer-utility-background/);
  assert.match(text(bar.trigger("workspace-agent")), /^Analyst$/);
  assert.equal(bar.trigger("workspace-project").disabled, true, "an Agent's project is not changed beside it");
  assert.ok(!bar.trigger("workspace-agent").querySelector(".composer-utility-dropdown-label-chevron"), "the Agent chip carries no dropdown chevron");
  assert.ok(bar.trigger("workspace-applications").querySelector(".composer-utility-dropdown-label-chevron"));
  await press(bar.trigger("workspace-agent"));
  const menu = document.querySelector('[role="menu"]');
  assert.deepEqual([...menu.querySelectorAll(".text-token-description-foreground")].map(text), ["Agents"]);
  assert.doesNotMatch(text(menu), /Agent type|Custom Agent|Built-in Agents|Your Agents/i);
  const rows = menuRows();
  assert.deepEqual(rows.map((row) => text(row)), ["AnalystAlpha", "BuilderBeta"]);
  const icon = (row) => row.querySelector("span.min-w-0 svg");
  assert.deepEqual(rows.map((row) => icon(row).classList.contains("text-token-warning")), [false, true], "the chosen Agent shows the task's access, another its project's default");
  const projectIcon = icon(rows[0]).innerHTML, fullIcon = icon(rows[1]).innerHTML;
  assert.notEqual(projectIcon, fullIcon);
  await click(rows[1]);
  assert.deepEqual(bar.calls, [["agent", "agent-beta"]]);
  assert.ok(!document.querySelector('[role="menu"]'));
  await utilityBar({ accessMode: "full_access" });
  await press(bar.trigger("workspace-agent"));
  assert.deepEqual(menuRows().map((row) => [icon(row).classList.contains("text-token-warning"), icon(row).innerHTML === fullIcon]), [[true, true], [true, true]]);
  await close();
  for (const [overrides, disabled] of [[{ agentPresetId: "" }, false], [{ agentPresetId: "", locked: true }, true], [{ agentPresetId: "", contexts: [] }, true]]) {
    const view = await utilityBar(overrides);
    assert.equal(view.trigger("workspace-project").disabled, disabled, JSON.stringify(overrides));
  }
});

test("the access toggle switches between project and full access and says why full access is unavailable", async () => {
  const bar = await utilityBar();
  const toggle = bar.toggle();
  assert.deepEqual([toggle.getAttribute("aria-pressed"), toggle.getAttribute("aria-label"), text(toggle), toggle.disabled], ["false", "Use full access", "Project access", false]);
  assert.deepEqual(["aria-haspopup", "aria-expanded", "title", "aria-description"].map((name) => toggle.getAttribute(name)), [null, null, null, null], "the toggle is a plain button, not a menu");
  assert.ok(toggle.querySelector("svg.text-token-text-tertiary"));
  await click(toggle);
  await utilityBar({ accessMode: "full_access", fullAccessUnavailableReason: "Needs a host root." });
  const full = bar.toggle();
  assert.deepEqual([full.getAttribute("aria-pressed"), full.getAttribute("aria-label"), text(full), full.disabled, full.getAttribute("aria-description")], ["true", "Use project access", "Full access", false, null]);
  assert.ok(full.querySelector("svg.text-token-warning"));
  assert.deepEqual(bar.calls.filter(([name]) => name === "access"), [["access", "full_access"]]);
  const blocked = await utilityBar({ fullAccessUnavailableReason: "Needs a host root." });
  assert.deepEqual([blocked.toggle().disabled, blocked.toggle().getAttribute("aria-description")], [true, "Needs a host root."]);
  for (const overrides of [{ locked: true }, { workerInterfaceId: "" }]) {
    assert.equal((await utilityBar(overrides)).toggle().disabled, true, JSON.stringify(overrides));
  }
  const switched = await utilityBar({ accessMode: "full_access" });
  await click(switched.toggle());
  assert.deepEqual(switched.calls, [["access", "project_access"]]);
});

test("a new task offers only its Agent's applications and Worker Environments, an open task only what it carries", async () => {
  const labels = async (trigger) => { await press(trigger); const rows = menuRows().map((row) => text(row.querySelector(".text-left"))); await close(); return rows; };
  const chosen = await utilityBar();
  assert.deepEqual([await labels(chosen.trigger("workspace-applications")), await labels(chosen.trigger("workspace-drivers"))], [["Notes"], ["Shell"]]);
  await press(chosen.trigger("workspace-applications"));
  await click(menuRows()[0]);
  assert.deepEqual(chosen.calls, [["application", "app.notes"]]);
  await close();
  const unchosen = await utilityBar({ agentPresetId: "", applicationIds: [], driverNames: [] });
  assert.deepEqual([unchosen.trigger("workspace-applications").disabled, unchosen.trigger("workspace-drivers").disabled], [true, true]);
  const room = { workerEnvironmentId: "we-1", runId: "run-1", eeId: "ee.alpha", workerInterfaceId: "worker.alpha", doorInterfaceId: "door.browser", kind: "system_browser", live: true, status: "ready", lifecycle: "active", root: "/", lastActivityMovementSeq: 1 };
  const open = await utilityBar({ agentPresetId: "", runId: "run-1", applicationIds: ["app.mail"], driverNames: [], rooms: [room, { ...room, runId: "run-2", kind: "system_shell", doorInterfaceId: "door.shell" }] });
  assert.deepEqual([await labels(open.trigger("workspace-applications")), await labels(open.trigger("workspace-drivers"))], [["Mail"], ["Browser"]]);
  root.render(null);
  await settle();
});

const model = (id, showInPicker = true) => ({ model: id, displayName: id, description: "", defaultReasoningEffort: "high", supportedReasoningEfforts: ["high"], showInPicker, isDefault: false });
const providerA = { id: "provider.a", label: "provider.a", descriptorPath: "registry/interface/provider.a", sourceNames: ["chatgpt"], supportedReasoningEfforts: ["high"],
  models: [model("alpha"), model("beta"), model("gamma", false), model("delta")] };
const sourceWorker = (id, authoringProfile = true) => ({ id, label: id, authoring_profile: authoringProfile, agent_source_template: true, provider_interface_ids: ["provider.a"],
  interface_policy: { "provider.a": { models: ["alpha", "beta", "gamma"] } }, applications: [application("app.notes", "Notes")], drivers: driverRows.slice(0, 1) });
const rootRow = (workers) => ({ ee_id: "ee.root", is_os_root: true, display_name: "Root", agent_catalog_digest: "catalog-root", workers });
async function dialog(workers, overrides = {}) {
  const { AgentPresetDialog, projectChatExecutionContexts } = await compiled;
  root.render(null);
  await settle();
  const submitted = [];
  root.render(React.createElement(AgentPresetDialog, { contexts: projectChatExecutionContexts({ contexts: [rootRow(workers)] }), initialProjectEeId: "ee.root", open: true, preset: null,
    presets: [], projects: [], providers: [providerA], onRefreshModels: async () => {}, onCancel: () => {}, onDelete: () => {}, onSubmit: (value) => submitted.push(value), ...overrides }));
  await settle(5);
  const panel = document.querySelector('[aria-labelledby="octopus-agent-preset-title"]');
  const select = (label) => [...panel.querySelectorAll(".oc-select-field")].find((field) => text(field.querySelector(".oc-field__label")) === label).querySelector("button");
  const name = async (value) => {
    const input = panel.querySelector("#octopus-agent-name");
    Object.getOwnPropertyDescriptor(w.HTMLInputElement.prototype, "value").set.call(input, value);
    input.dispatchEvent(new w.Event("input", { bubbles: true }));
    await settle();
  };
  return { submitted, panel, select, name, alert: () => text(panel.querySelector('[role="alert"]') ?? panel) };
}

test("a new Agent starts from its project's one reusable worker, offers only admitted visible models, and submits exactly the choice", async () => {
  const view = await dialog([sourceWorker("worker.source"), sourceWorker("worker.other", false)]);
  assert.doesNotMatch(text(view.panel), /Agent types?|\bRole\b|Built-in defaults|client projection/i);
  assert.equal(view.select("Project").disabled, false);
  await view.name("Scout");
  await click(view.select("Model"));
  const options = [...document.querySelectorAll('[data-oc-dropdown] [role="menuitem"]')];
  assert.deepEqual(options.map((option) => option.getAttribute("aria-label")), ["chatgpt · alpha", "chatgpt · beta"]);
  await click(options[1]);
  assert.equal(text(view.select("Model")), "chatgpt · beta");
  await click([...view.panel.querySelectorAll("button")].find((button) => text(button) === "Create agent"));
  assert.deepEqual(view.submitted, [{ name: "Scout", icon: "default", projectEeId: "ee.root", workerInterfaceId: "worker.source", providerInterfaceId: "provider.a",
    providerModel: "beta", reasoningEffort: "high", serviceTier: "default", applicationIds: ["app.notes"], driverNames: ["system_shell"] }]);
});

test("a project with two reusable workers names none, and editing an Agent keeps its project and shows its saved model", async () => {
  const ambiguous = await dialog([sourceWorker("worker.one"), sourceWorker("worker.two")]);
  await ambiguous.name("Scout");
  assert.equal(ambiguous.alert(), "The selected Project has no reusable worker template.");
  const { resolveClientAgents, projectChatExecutionContexts } = await compiled;
  const scout = { ...sourceWorker("worker.source"), client_agent_id: "agent-scout", client_agent_name: "Scout", client_agent_icon: "calm", client_agent_created_at_ms: 1,
    client_agent_updated_at_ms: 1, client_agent_deleted_at_ms: 0, client_template_digest: "template-scout", client_template_provider_interface_id: "provider.a",
    client_template_provider_model: "beta", client_template_reasoning_effort: "high" };
  const [preset] = resolveClientAgents(projectChatExecutionContexts({ contexts: [rootRow([scout])] }));
  const editing = await dialog([scout], { preset, presets: [preset] });
  assert.deepEqual([editing.select("Project").disabled, text(editing.select("Model"))], [true, "chatgpt · beta"]);
  root.render(null);
  await settle();
});
