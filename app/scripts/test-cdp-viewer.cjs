const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { EventEmitter } = require("node:events");

function fixture(sourcePath = path.join(__dirname, "../electron/cdpViewer.cjs"), clockStep = 100) {
  const sockets = [];
  const methods = [];
  const events = [];
  let clock = 0;
  let now = 100;
  let navigationError = "";
  class Socket extends EventEmitter {
    static OPEN = 1;
    constructor() {
      super();
      this.readyState = 1;
      sockets.push(this);
      queueMicrotask(() => this.emit("open"));
    }
    send(text) {
      const { id, method, params } = JSON.parse(text);
      methods.push({ method, params });
      const result = method === "Page.getNavigationHistory"
        ? { currentIndex: 1, entries: [{ id: 1, url: "https://example.test/" }, { id: 2, url: "https://example.test/page", title: "Page" }] }
        : method === "Page.navigate" ? { frameId: "main", errorText: navigationError }
        : method === "Runtime.evaluate" ? { result: { value: true } } : {};
      queueMicrotask(() => this.emit("message", JSON.stringify({ id, result })));
    }
    close() { this.readyState = 3; this.emit("close"); }
    frame(data = "ZmFrZQ==", metadata = { deviceWidth: 1200, deviceHeight: 800, pageScaleFactor: 1 }) {
      this.emit("message", JSON.stringify({ method: "Page.screencastFrame", params: {
        sessionId: ++clock, data, metadata,
      } }));
    }
  }
  const context = {
    require: (name) => name === "ws" ? { WebSocket: Socket } : require(name),
    module: { exports: {} }, Buffer, URL, AbortController, setTimeout, clearTimeout,
    Date: class extends Date { static now() { now += clockStep; return now; } },
    fetch: async () => ({ ok: true, headers: { get: () => null }, text: async () => JSON.stringify([
      { id: "page-1", type: "page", url: "https://example.test/page", title: "Page", webSocketDebuggerUrl: "ws://127.0.0.1:9222/devtools/page/page-1" },
    ]) }),
  };
  vm.runInNewContext(fs.readFileSync(sourcePath, "utf8"), context, { filename: sourcePath });
  const descriptor = { kind: "browser", mode: "direct", protocol: "cdp", viewerUrl: "http://127.0.0.1:9222/", runId: "run-1", formationRevision: "1", readOnly: false, data: { target_id: "page-1" } };
  const sender = Object.assign(new EventEmitter(), { id: 1, isDestroyed: () => false, send: (_channel, event) => events.push(event) });
  const manager = context.module.exports.createCdpViewerManager({ resolveSurface: async () => descriptor });
  const open = (attachmentId, visible = true) => manager.open("operator", sender, { attachmentId, visible, workerEnvironmentId: "room-1", surfaceId: "browser" });
  const action = (sessionId, action, fields = {}) => manager.action("operator", sender, { sessionId, action, ...fields });
  return { action, context, descriptor, events, manager, methods, open, sender, sockets, setNavigationError: (value) => { navigationError = value; } };
}

async function hiddenMeasure(sourcePath) {
  const f = fixture(sourcePath);
  try {
    await f.open("measurement", false);
    for (let index = 0; index < 12; index++) f.sockets[0].frame();
    await new Promise(setImmediate);
    return {
      hiddenFramesForwarded: f.events.filter((event) => event.type === "frame").length,
      screencastStarts: f.methods.filter((call) => call.method === "Page.startScreencast").length,
      framesAcknowledged: f.methods.filter((call) => call.method === "Page.screencastFrameAck").length,
    };
  } finally { f.manager.closeAll(); }
}

async function browserLeafCheck(coordinates) {
  const { JSDOM } = require("jsdom");
  const React = require("react");
  const ts = require("typescript");
  const dom = new JSDOM("<div id='root'></div>", { url: "http://client.test", pretendToBeVisual: true });
  const saved = Object.fromEntries(["window", "document", "navigator", "IS_REACT_ACT_ENVIRONMENT"].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  for (const [key, value] of Object.entries({ window: dom.window, document: dom.window.document, navigator: dom.window.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  const { createRoot } = require("react-dom/client");
  const f = fixture();
  const listeners = new Set(), paints = [], decodes = [];
  f.sender.send = (_channel, event) => { f.events.push(event); for (const listener of listeners) listener(event); };
  dom.window.HTMLCanvasElement.prototype.getContext = () => ({ drawImage: (image) => paints.push(image.src) });
  class FrameImage {
    naturalWidth = 1200; naturalHeight = 800;
    decode() { return new Promise((resolve) => decodes.push(resolve)); }
  }
  dom.window.octopusTransport = {
    onCdpViewerEvent: (listener) => { listeners.add(listener); return () => listeners.delete(listener); },
    openCdpViewer: (request) => f.open(request.attachmentId, request.visible),
    cdpViewerAction: (request) => f.manager.action("operator", f.sender, request),
    closeCdpViewer: async (request) => f.manager.close("operator", f.sender, request),
  };
  const module = { exports: {} };
  const source = ts.transpileModule(fs.readFileSync(path.join(__dirname, "../src/components/worker-environment/RunEnvironmentBrowserSurfaces.tsx"), "utf8"), {
    compilerOptions: { jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }, reportDiagnostics: true,
  });
  assert.equal(source.diagnostics.length, 0);
  vm.runInNewContext(source.outputText, { module, exports: module.exports, window: dom.window, Image: FrameImage, Node: dom.window.Node, crypto: require("node:crypto").webcrypto,
    require: (name) => name.startsWith("react") ? require(name) : name.endsWith(".css") ? {} : name.endsWith("browserCoordinates.mjs") ? coordinates
      : name.endsWith("projectionClient") ? { formatProjectionError: (error) => error.message }
      : name.endsWith("IconGlyphs") ? { Glyph: () => null }
      : name.endsWith("components") ? { OcButton: "button", OcMenuItem: "button", OcMenuSurface: "div", OcIconButton: ({ label, glyph, appearance, ...props }) => React.createElement("button", { ...props, "aria-label": label }) }
      : name.endsWith("MessageTooltip") ? { MessageTooltip: ({ children }) => children }
      : name.endsWith("OctopusSurface") ? { OctopusEmptyState: () => null }
      : { LiveSurfaceEmpty: ({ title }) => React.createElement("div", null, title), ManualRoomControlWarning: () => null,
        dataText: (data, key) => typeof data[key] === "string" ? data[key] : "", isViewerUrl: (value) => /^https?:/.test(value) },
  });
  const root = createRoot(dom.window.document.getElementById("root"));
  const props = { active: true, environment: { id: "room-1", runId: "run-1", lifecycle: "active" }, surface: { id: "browser", kind: "browser", mode: "direct", protocol: "cdp", viewerUrl: f.descriptor.viewerUrl, data: { target_id: "page-1", url: "https://example.test/page" } } };
  try {
    await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, props)));
    assert.match(dom.window.document.body.textContent, /Waiting for browser frame/);
    const address = dom.window.document.querySelector('input[aria-label="Browser address"]');
    await React.act(async () => { address.focus(); Object.getOwnPropertyDescriptor(dom.window.HTMLInputElement.prototype, "value").set.call(address, ""); address.dispatchEvent(new dom.window.Event("input", { bubbles: true })); });
    assert.equal(address.value, "");
    await React.act(async () => address.dispatchEvent(new dom.window.KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
    assert.equal(address.value, "https://example.test/page");
    await React.act(async () => f.sockets[0].frame("Zmlyc3Q="));
    await React.act(async () => f.sockets[0].frame("c2Vjb25k"));
    await React.act(async () => f.sockets[0].frame("bGFzdA=="));
    assert.equal(decodes.length, 1);
    assert.match(dom.window.document.body.textContent, /Waiting for browser frame/);
    await React.act(async () => decodes[0]());
    assert.equal(decodes.length, 2);
    await React.act(async () => decodes[1]());
    assert.deepEqual(paints, ["data:image/jpeg;base64,Zmlyc3Q=", "data:image/jpeg;base64,bGFzdA=="]);
    assert.match(dom.window.document.body.textContent, /Live browser/);
    await React.act(async () => f.sockets[0].emit("close"));
    assert.equal(dom.window.document.querySelector("canvas"), null);
    const reconnect = [...dom.window.document.querySelectorAll("button")].find((button) => button.textContent === "Reconnect");
    assert.ok(reconnect);
    await React.act(async () => reconnect.click());
    assert.equal(f.sockets.length, 2);
    await React.act(async () => root.render(React.createElement(module.exports.BrowserSurface, { ...props, surface: { ...props.surface, data: { target_id: "" } } })));
    assert.equal(f.sockets[1].readyState, 3);
    assert.equal(dom.window.document.querySelector("canvas"), null);
    assert.match(dom.window.document.body.textContent, /has not projected an exact browser target/);
  } finally {
    await React.act(async () => root.unmount()); f.manager.closeAll(); dom.window.close();
    for (const [key, descriptor] of Object.entries(saved)) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key]; }
  }
}

async function main() {
  if (process.argv[2] === "--measure") {
    process.stdout.write(`${JSON.stringify(await hiddenMeasure(process.argv[3]))}\n`);
    return;
  }
  const { browserImagePoint } = await import("../src/components/worker-environment/browserCoordinates.mjs");
  const bounds = { left: 100, top: 200, width: 600, height: 300 };
  assert.deepEqual(browserImagePoint(bounds, 1200, 400, 350), { x: 600, y: 300, zoom: 0.5 });
  assert.equal(browserImagePoint(bounds, 1200, 10, 350), null);
  assert.equal(browserImagePoint(bounds, undefined, 400, 350), null);
  assert.deepEqual(browserImagePoint(bounds, 1200, 900, 900, true), { x: 1200, y: 600, zoom: 0.5 });
  const f = fixture();
  try {
    const first = await f.open("mount-1");
    const second = await f.open("mount-2");
    assert.equal(first.targetId, "page-1");
    assert.equal(second.runId, "run-1");
    assert.equal(f.sockets[0].readyState, 1);
    f.manager.close("operator", f.sender, { sessionId: first.sessionId });
    assert.equal(f.sockets[1].readyState, 1);
    assert.equal((await f.action(second.sessionId, "claimControl")).manualControl, true);
    f.setNavigationError("net::ERR_NAME_NOT_RESOLVED");
    await assert.rejects(f.action(second.sessionId, "navigate", { url: "https://unavailable.test/" }), /ERR_NAME_NOT_RESOLVED/);
    let state = await f.action(second.sessionId, "refreshState");
    assert.equal(state.navigationError, "net::ERR_NAME_NOT_RESOLVED");
    assert.equal(state.loading, false);
    assert.equal(state.connected, true);
    assert.equal((await f.action(second.sessionId, "stop")).loading, false);
    await f.action(second.sessionId, "key", { eventType: "keyDown", key: " ", code: "Space", text: " " });
    assert.equal(f.methods.find((call) => call.method === "Input.dispatchKeyEvent").params.text, " ");
    f.sockets[1].frame();
    await f.action(second.sessionId, "setVisible", { visible: false });
    const frameCount = f.events.filter((event) => event.type === "frame").length;
    f.sockets[1].frame();
    assert.equal(f.events.filter((event) => event.type === "frame").length, frameCount);
    await f.action(second.sessionId, "setVisible", { visible: true });
    assert.ok(f.events.filter((event) => event.type === "frame").length > frameCount);
    state = await f.action(second.sessionId, "refreshState");
    assert.equal(state.manualControl, false);
    await assert.rejects(f.action(second.sessionId, "reload"), /Enable manual/);
    f.descriptor.readOnly = true;
    await assert.rejects(f.action(second.sessionId, "claimControl"), /read-only|released/);
    f.descriptor.readOnly = false;
    f.descriptor.data.target_id = "new-target";
    await f.manager.revalidate();
    assert.equal(f.sockets[1].readyState, 3);
    assert.ok(f.events.some((event) => event.type === "error" && /identity changed/.test(event.error)));
  } finally { f.manager.closeAll(); }

  const stream = fixture(undefined, 1);
  try {
    await stream.open("stream");
    stream.sockets[0].frame("Zmlyc3Q=");
    stream.sockets[0].frame("c2Vjb25k");
    stream.sockets[0].frame("bGFzdA==");
    assert.equal(stream.events.filter((event) => event.type === "frame").length, 1);
    await new Promise((resolve) => setTimeout(resolve, 35));
    const frames = stream.events.filter((event) => event.type === "frame");
    assert.equal(frames.length, 2);
    assert.equal(frames.at(-1).data, "bGFzdA==");
    assert.equal(stream.methods.filter((call) => call.method === "Page.screencastFrameAck").length, 3);
    assert.doesNotThrow(() => stream.sockets[0].frame("ZmFrZQ==", { deviceWidth: -1, deviceHeight: 800 }));
    assert.equal(stream.sockets[0].readyState, 3);
    assert.ok(stream.events.some((event) => event.type === "error" && /numeric input/.test(event.error)));
  } finally { stream.manager.closeAll(); }

  const pending = fixture();
  let resolve;
  const descriptor = new Promise((done) => { resolve = done; });
  const manager = pending.context.module.exports.createCdpViewerManager({ resolveSurface: () => descriptor });
  const opening = manager.open("operator", pending.sender, { attachmentId: "pending-mount", visible: true, workerEnvironmentId: "room-1", surfaceId: "browser" });
  manager.close("operator", pending.sender, { sessionId: "pending-mount" });
  resolve(pending.descriptor);
  await assert.rejects(opening, /released/);
  assert.equal(pending.sockets.length, 0);
  const result = await hiddenMeasure();
  assert.equal(result.hiddenFramesForwarded, 0);
  assert.equal(result.screencastStarts, 0);
  assert.equal(result.framesAcknowledged, 12);
  await browserLeafCheck({ browserImagePoint });
  process.stdout.write(`CDP viewer protocol contracts passed; ${JSON.stringify(result)}\n`);
}

main().catch((error) => { process.stderr.write(`${error.stack}\n`); process.exitCode = 1; });
