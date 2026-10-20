const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "navigator", "getComputedStyle", "localStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "getComputedStyle" ? w[key].bind(w) : key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };
globalThis.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.HTMLElement.prototype.scrollTo = function () {};
w.HTMLElement.prototype.scrollIntoView = function () {};
w.HTMLElement.prototype.getBoundingClientRect = function () { return { x: 600, y: 10, left: 600, top: 10, right: 632, bottom: 42, width: 32, height: 32 }; };

const React = require("react");
const { createRoot } = require("react-dom/client");
const compiled = build({
  entryPoints: [path.join(__dirname, "../src/components/ApplicationsView.tsx")],
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl", ".webm": "dataurl", ".mp4": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return module.exports.ApplicationsView;
});

const container = document.getElementById("root");
const root = createRoot(container);
after(() => { root.unmount(); w.close(); });
const settle = (ms = 30) => new Promise((resolve) => setTimeout(resolve, ms));
const digest = (letter) => letter.repeat(64);
const application = (id, extra = {}) => ({
  id, name: `Application ${id}`, description: "Keeps notes", category: "connector", developer: "Octopus", version: "1.0.0", icon: "", logo: "", logo_dark: "", logo_data_url: "",
  logo_dark_data_url: "", interfaces: [], artifact_bindings: [], installed: false, materialized: true, partially_materialized: false, valid: true, missing_interface_ids: [],
  descriptor_path: "", manifest_path: "", manifest_digest: "", package_id: `package.${id}`, installed_digest: digest("b"), source_available: true, source_kind: "local",
  source_digest: digest("a"), install_packages: [], source_file_count: 1, receipt_state: "stale", receipt_digest: digest("c"), managed: true, install_block_reason: "",
  removal_allowed: false, removal_block_reason: "", connected_run_ids: [], project_states: [{ project_id: "root", ee_id: "ee.root", policy_allowed: true, connected: false }],
  ...extra,
});

test("repairing a stale or receipt-less application asks the host to repair exactly that package against its observed digests", async () => {
  const ApplicationsView = await compiled;
  const mutations = [];
  const projections = {
    "projection.operator.os.applications": { applications: [application("stale"), application("missing", { receipt_state: "missing", receipt_digest: "" })], projects: [], errors: [] },
    "projection.operator.os.local_packages": { packs: [], errors: [] },
  };
  w.octopusTransport = new Proxy({
    projectionRead: async (request) => structuredClone(projections[request.method] ?? {}),
    mutateOctopusOsWorkspace: async (request) => { mutations.push(structuredClone(request)); return { status: "complete" }; },
  }, { get: (target, name) => target[name] ?? (typeof name === "string" && name.startsWith("on") ? () => () => {} : () => new Promise(() => {})) });
  root.render(React.createElement(ApplicationsView, { accessProfile: "live", searchFocusTick: 0 }));
  await settle(80);
  const repair = (name) => {
    const button = [...container.querySelectorAll("button")].find((candidate) => {
      if (candidate.textContent !== "Repair") return false;
      let card = candidate.parentElement;
      while (card && !card.textContent.includes("Application ")) card = card.parentElement;
      return card?.textContent.includes(name);
    });
    assert.ok(button, `${name} offers Repair: ${container.textContent.slice(0, 300)}`);
    return button;
  };
  for (const name of ["Application stale", "Application missing"]) {
    repair(name).click();
    await settle(60);
  }
  assert.deepEqual(mutations.map(({ operationId, ...request }) => request), [
    { action: "repair_package", packageIds: ["package.stale"], expectedRepairs: { "package.stale": { sourceDigest: digest("a"), installedDigest: digest("b"), receiptState: "stale", receiptDigest: digest("c") } } },
    { action: "repair_package", packageIds: ["package.missing"], expectedRepairs: { "package.missing": { sourceDigest: digest("a"), installedDigest: digest("b"), receiptState: "missing", receiptDigest: null } } },
  ]);
  assert.ok(mutations.every(({ operationId }) => /^[0-9a-f-]{36}$/.test(operationId)) && mutations[0].operationId !== mutations[1].operationId);
});
