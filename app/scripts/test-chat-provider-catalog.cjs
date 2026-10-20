const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "KeyboardEvent", "MouseEvent", "localStorage", "navigator"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} disconnect() {} };
w.matchMedia = () => ({ matches: true, addEventListener() {}, removeEventListener() {} });
w.HTMLElement.prototype.scrollTo = function () {};
w.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 100, y: 100, left: 100, top: 100, right: 200, bottom: 120, width: 100, height: 20 });

const React = require("react");
const { createRoot } = require("react-dom/client");
const compiled = build({
  stdin: {
    contents: [
      'export { readRuntimeProviderInterfaces, refreshProviderModels } from "./src/projectionClient";',
      'export { useChatCapabilityCatalog } from "./src/components/chat/useChatCapabilityCatalog";',
      'export { chatProductError } from "./src/components/chat/chatProductError";',
      'export { ProviderModelPicker } from "./src/components/chat/ProviderModelPicker";',
    ].join("\n"),
    resolveDir: path.join(__dirname, ".."), loader: "ts",
  },
  bundle: true, platform: "node", format: "cjs", write: false, metafile: true, logLevel: "silent",
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return { ...module.exports, inputs: Object.keys(result.metafile.inputs) };
});

const root = createRoot(document.getElementById("root"));
after(() => { root.unmount(); w.close(); });
const settle = async (rounds = 4) => { for (let round = 0; round < rounds; round += 1) await new Promise((resolve) => setTimeout(resolve, 10)); };
const until = async (predicate, message) => {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail(typeof message === "function" ? message() : message);
};
const model = (id, efforts = ["high"]) => ({ model: id, displayName: id, description: "", defaultReasoningEffort: efforts[0], supportedReasoningEfforts: efforts, showInPicker: true, isDefault: false });
const providerFixture = (id, models, label = "chatgpt") => ({ id, label, descriptorPath: `registry/interface/${id}`, sourceNames: [label], supportedReasoningEfforts: ["high"], models });

test("the provider catalog is the bridge's declared interfaces, read without booting the runtime host", async () => {
  const { readRuntimeProviderInterfaces, refreshProviderModels } = await compiled;
  const calls = [];
  const declared = { repoRoot: "/repo", providers: [providerFixture("provider.a", [model("model-a")])] };
  w.octopusTransport = { providerInterfaces: async () => { calls.push("providerInterfaces"); return declared; }, bootStatus: async () => { calls.push("bootStatus"); return {}; } };
  assert.equal(await readRuntimeProviderInterfaces(), declared);
  assert.deepEqual(calls, ["providerInterfaces"]);
  w.octopusTransport = {};
  assert.deepEqual(await readRuntimeProviderInterfaces(), { repoRoot: null, providers: [], error: "no projection reader is available" });
  const request = { providerInterfaceId: "provider.a" };
  const refreshed = { providerInterfaceId: "provider.a", models: [model("model-b")] };
  w.octopusTransport = { refreshProviderModels: async (received) => { calls.push(received); return refreshed; } };
  assert.equal(await refreshProviderModels(request), refreshed);
  assert.deepEqual(calls.at(-1), request);
  w.octopusTransport = {};
  await assert.rejects(refreshProviderModels(request), /only in the live desktop client/);
});

test("a disabled surface holds no provider catalog and reads none", async () => {
  const { useChatCapabilityCatalog } = await compiled;
  let catalogReads = 0;
  w.octopusTransport = {
    onProjectionChanged: () => () => {},
    projectionRead: async () => ({}),
    providerInterfaces: async () => { catalogReads += 1; return { repoRoot: "/repo", providers: [providerFixture("provider.a", [model("model-a")])] }; },
  };
  const view = { current: null };
  function Harness({ enabled }) { view.current = useChatCapabilityCatalog("live", enabled, true, "/home/catalog"); return null; }
  root.render(React.createElement(Harness, { enabled: true }));
  await until(() => view.current?.providerInterfacesLoaded && view.current.providerInterfaces.length === 1, "the enabled catalog never loaded");
  const reads = catalogReads;
  root.render(React.createElement(Harness, { enabled: false }));
  await until(() => !view.current.providerInterfacesLoaded, "the disabled catalog still reports itself loaded");
  await view.current.refreshProviderInterfaces(true);
  await settle();
  assert.deepEqual([view.current.providerInterfaces, view.current.providerInterfacesLoaded, view.current.providerInterfaceError, catalogReads], [[], false, "", reads]);
  root.render(null);
  await settle();
});

test("the product error names an undeclared provider only for an Agent or an existing task once the catalog has loaded", async () => {
  const { chatProductError } = await compiled;
  const chat = { error: "", runsError: "", executionContextError: "", newTaskAuthoringError: "", providerInterfaceError: "", activeRunId: "", agentPresetId: "agent-writer",
    providerInterfacesLoaded: true, providerInterfaceId: "provider.gone", declaredProviderInterfaces: [{ id: "provider.kept" }], artifactWriterAvailable: true };
  assert.equal(chatProductError(chat), "provider.gone is not declared in the provider registry projection");
  assert.equal(chatProductError({ ...chat, agentPresetId: "", activeRunId: "run-1" }), "provider.gone is not declared in the provider registry projection");
  for (const patch of [{ agentPresetId: "" }, { providerInterfacesLoaded: false }, { providerInterfaceId: "" }, { declaredProviderInterfaces: [{ id: "provider.gone" }] }]) {
    assert.equal(chatProductError({ ...chat, ...patch }), "", JSON.stringify(patch));
  }
  assert.equal(chatProductError({ ...chat, providerInterfaceId: "", artifactWriterAvailable: false }), "Artifact writer unavailable");
  assert.equal(chatProductError({ ...chat, newTaskAuthoringError: "Exact authoring failure" }), "Exact authoring failure");
});

async function picker(props) {
  const { ProviderModelPicker } = await compiled;
  const chosen = [];
  root.render(null);
  await settle(1);
  root.render(React.createElement(ProviderModelPicker, { providerInterfaceId: "", providerModel: "", reasoningEffort: "high", listenToGlobalCommands: false,
    onModelChange: (...args) => chosen.push(args), onReasoningEffortChange: () => {}, ...props }));
  await settle();
  const trigger = document.querySelector("[data-provider-picker-trigger]");
  if (!trigger.disabled) {
    trigger.dispatchEvent(new w.KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }));
    await settle();
  }
  const rows = [...document.querySelectorAll('[role="menuitemradio"]')];
  return { chosen, trigger, rows, menu: document.querySelector("[data-provider-picker-menu]"), snapshot: () => JSON.stringify({ label: trigger.getAttribute("aria-label"),
    rows: rows.map((row) => [row.dataset.providerId, row.dataset.modelId, row.textContent, row.getAttribute("aria-checked")]) }) };
}

test("the picker offers every declared provider's models, only the bound provider's when locked, and cannot open with nothing to offer", async () => {
  const first = providerFixture("provider.one", [model("alpha"), model("beta")]);
  const second = providerFixture("provider.two", [model("gamma")], "router");
  const open = await picker({ providers: [first, second], activeProvider: first, providerInterfaceId: first.id, providerModel: "alpha" });
  assert.deepEqual(open.rows.map((row) => [row.dataset.providerId, row.dataset.modelId]), [[first.id, "alpha"], [first.id, "beta"], [second.id, "gamma"]]);
  assert.ok(![...open.menu.querySelectorAll("button, [role]")].some((item) => /^(Interface|Effort)$/.test(item.textContent.trim())), "the menu has no provider or effort submenu");
  const locked = await picker({ providers: [first, second], activeProvider: first, providerInterfaceId: first.id, providerModel: "alpha", interfaceLocked: true });
  assert.deepEqual(locked.rows.map((row) => row.dataset.providerId), [first.id, first.id]);
  const unbound = await picker({ providers: [first, second], interfaceLocked: true });
  assert.deepEqual([unbound.trigger.disabled, unbound.rows.length], [true, 0]);
  const empty = await picker({ providers: [] });
  assert.equal(empty.trigger.disabled, true);
  const retry = await picker({ providers: [], onRefreshModels: () => {} });
  assert.equal(retry.trigger.disabled, false);
  root.render(null);
  await settle();
});

test("the picker treats provider identities as opaque and keeps a model id whole", async () => {
  const shape = (ids, family) => [providerFixture(ids[0], [model("gpt-6"), model(`${family}/glm-5`)]), providerFixture(ids[1], [model("gpt-6")])];
  const branded = shape(["world_provider_codex", "world_provider_openrouter"], "z-ai");
  const neutral = shape(["provider.one", "provider.two"], "vendor");
  const brandedView = await picker({ providers: branded, activeProvider: branded[0], providerInterfaceId: branded[0].id, providerModel: "gpt-6" });
  const brandedSnapshot = brandedView.snapshot();
  brandedView.rows.find((row) => row.dataset.modelId === "z-ai/glm-5").click();
  assert.deepEqual(brandedView.chosen, [["world_provider_codex", "z-ai/glm-5", "high"]]);
  const neutralView = await picker({ providers: neutral, activeProvider: neutral[0], providerInterfaceId: neutral[0].id, providerModel: "gpt-6" });
  assert.equal(brandedSnapshot.replaceAll("world_provider_codex", "provider.one").replaceAll("world_provider_openrouter", "provider.two").replaceAll("z-ai/", "vendor/"), neutralView.snapshot());
  assert.deepEqual(neutralView.rows.map((row) => row.textContent), ["6 · provider.one", "vendor/glm-5", "6 · provider.two"], "a model offered twice under one source label is told apart by provider identity");
  const labelled = await picker({ providers: [neutral[0], { ...neutral[1], label: "router" }], activeProvider: neutral[0], providerInterfaceId: neutral[0].id, providerModel: "gpt-6" });
  assert.deepEqual(labelled.rows.map((row) => row.textContent), ["6 · chatgpt", "vendor/glm-5", "6 · router"]);
  root.render(null);
  await settle();
});

test("the picker ships its own stylesheet", async () => {
  const { inputs } = await compiled;
  assert.ok(inputs.includes("src/components/chat/providerModelPicker.css"), inputs.filter((input) => input.endsWith(".css")).join(", "));
});
