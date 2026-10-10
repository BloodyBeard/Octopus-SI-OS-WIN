const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createRequire } = require("node:module");
const ts = require("typescript");
const React = require("react");
const { createRoot } = require("react-dom/client");

const app = path.resolve(__dirname, "..");
function load(file, mocks = {}, select) {
  const filename = path.join(app, file);
  let source = fs.readFileSync(filename, "utf8");
  if (select) {
    const tree = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    source = tree.statements.filter((node) => select.has(node.name?.text)
      || ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => select.has(item.name.text)))
      .map((node) => node.getText(tree)).join("\n");
    source = `const {useCallback,useEffect,useLayoutEffect,useMemo,useRef,useState} = require('react');
      const {flushSync} = require('react-dom');
      const {buildThreadVirtualizerLayout,threadVirtualizerRange,rangeAnchoredToTurn,distanceFromBottomAnchoredToTurn} = require('./threadVirtualizer');\n${source}
      exports.VirtualizedTurns = VirtualizedTurns;`;
  }
  const code = ts.transpileModule(source, { fileName: filename, compilerOptions: {
    jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
  } }).outputText;
  const module = { exports: {} }, localRequire = createRequire(filename);
  Function("require", "module", "exports", code)((id) => id in mocks ? mocks[id] : localRequire(id), module, module.exports);
  return module.exports;
}

class HostNode extends EventTarget {
  constructor(tag, document) {
    super();
    this.nodeType = tag === "#text" ? 3 : 1;
    this.nodeName = this.tagName = tag.toUpperCase();
    this.ownerDocument = document;
    this.namespaceURI = "http://www.w3.org/1999/xhtml";
    this.childNodes = [];
    this.attributes = {};
    this.style = { setProperty(name, value) { this[name] = value; } };
    this.scrollTop = 0;
    this.clientHeight = 600;
  }
  appendChild(node) { return this.insertBefore(node, null); }
  insertBefore(node, next) {
    node.parentNode?.removeChild(node);
    this.childNodes.splice(next ? this.childNodes.indexOf(next) : this.childNodes.length, 0, node);
    node.parentNode = this;
    return node;
  }
  removeChild(node) { this.childNodes.splice(this.childNodes.indexOf(node), 1); node.parentNode = null; }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  removeAttribute(name) { delete this.attributes[name]; }
  set textContent(value) { this.childNodes = []; this.text = String(value); }
  get textContent() { return this.text || this.childNodes.map((node) => node.textContent).join(""); }
  get firstChild() { return this.childNodes[0] ?? null; }
  getBoundingClientRect() { return { height: 220, top: 0, bottom: 220, left: 0, right: 800, width: 800 }; }
  scrollTo({ top }) { this.scrollTop = top; }
}
const document = new EventTarget();
document.nodeType = 9;
document.createElement = (tag) => new HostNode(tag, document);
document.createTextNode = (text) => Object.assign(new HostNode("#text", document), { text });
document.documentElement = document.createElement("html");
document.body = document.createElement("body");
document.activeElement = document.body;
const frames = new Map();
let frameId = 0;
global.window = { document, HTMLElement: HostNode, HTMLIFrameElement: class {},
  requestAnimationFrame(callback) { frames.set(++frameId, callback); return frameId; },
  cancelAnimationFrame(id) { frames.delete(id); },
};
global.document = document;
global.IS_REACT_ACT_ENVIRONMENT = true;
const observers = new Set();
global.ResizeObserver = class {
  constructor(callback) { this.callback = callback; this.nodes = new Set(); observers.add(this); }
  observe(node) { this.nodes.add(node); }
  unobserve(node) { this.nodes.delete(node); }
  disconnect() { this.nodes.clear(); observers.delete(this); }
};
function frame() { const batch = [...frames.values()]; frames.clear(); batch.forEach((callback) => callback(0)); }
function descendants(node) { return [node, ...node.childNodes.flatMap(descendants)]; }
const virtualizer = load("src/components/chat/threadVirtualizer.ts");
const { VirtualizedTurns } = load("src/components/chat/ChatMessageList.tsx", {
  "./threadVirtualizer": virtualizer,
}, new Set([
  "VirtualizedTurns", "MeasuredVirtualTurn", "turnKey", "estimatedTurnHeight",
  "initialThreadVirtualizerRange", "resizeObserverBlockSize", "reverseScrollDistance",
  "THREAD_VIRTUALIZATION_MIN_TURNS", "THREAD_VIRTUALIZATION_OVERSCAN_TURNS", "THREAD_TURN_GAP_PX", "threadTurnEstimateCache",
]));
const turns = Array.from({ length: 45 }, (_, index) => [
  { id: `input:${index}`, role: "user", text: `Input ${index}`, createdAtMs: index * 10_000, terminal: true },
]);

(async () => {
  const container = document.createElement("div");
  const root = createRoot(container);
  let scroll, pinned = true;
  const listRef = { current: null };
  const props = {
    scrollElementRef: listRef, turns, initialDistanceFromBottomPx: 0, initialRestoreState: null,
    onRevealReady() {}, onRestoreStateChange() {}, revealRequest: null,
    getLastScrollDistanceFromBottomPx: () => -scroll.scrollTop,
    scrollToDistanceFromBottomPx: (distance) => listRef.current?.scrollTo({ top: -distance }),
    shouldFollowLatest: () => pinned,
    renderTurn: (turn) => React.createElement("article", null, turn.map((message) => message.text).join("\n")),
  };
  await React.act(async () => {
    root.render(React.createElement("div", { ref: listRef }, React.createElement(VirtualizedTurns, props)));
  });
  scroll = listRef.current;
  await React.act(async () => frame());
  assert.ok([...observers].some((observer) => observer.nodes.has(scroll)), "mounted parent scroll node must be observed after its ref attaches");
  const mountedKeys = () => descendants(container).flatMap((node) => node.attributes["data-thread-turn-key"] ?? []);
  assert.ok(mountedKeys().includes("input:44"));
  assert.ok(!mountedKeys().includes("input:0"));
  pinned = false;
  const height = Number.parseFloat(descendants(container).find((node) => node.attributes["data-thread-virtualized"]).style.height);
  await React.act(async () => { scroll.scrollTop = -(height - scroll.clientHeight); scroll.dispatchEvent(new Event("scroll")); frame(); });
  assert.ok(mountedKeys().includes("input:0"), "scrolling into reserved history must mount its turns");
  await React.act(async () => { scroll.scrollTop = 0; scroll.dispatchEvent(new Event("scroll")); frame(); });
  assert.ok(mountedKeys().includes("input:44"));
  await React.act(async () => root.unmount());
  assert.equal(observers.size, 0);
  scroll.dispatchEvent(new Event("scroll"));
  assert.equal(frames.size, 0);
  console.log("Activity scroll: initial mount subscription, history/latest ranges and cleanup passed");
})().catch((error) => { console.error(error); process.exitCode = 1; });
