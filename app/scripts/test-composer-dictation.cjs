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
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.devicePixelRatio = 1;
w.HTMLElement.prototype.getBoundingClientRect = function () { return { x: 600, y: 10, left: 600, top: 10, right: 632, bottom: 42, width: 32, height: 32 }; };
for (const [name, value] of [["clientWidth", 60], ["clientHeight", 24]]) {
  Object.defineProperty(w.HTMLCanvasElement.prototype, name, { configurable: true, get() { return value; } });
}
const draws = [];
w.HTMLCanvasElement.prototype.getContext = function () {
  const bars = [];
  draws.push(bars);
  const gradient = { addColorStop() {} };
  return { setTransform() {}, clearRect() {}, save() {}, restore() {}, translate() {}, beginPath() {}, fill() {}, fillRect() {}, createLinearGradient: () => gradient,
    roundRect: (x, y, width, height) => bars.push({ x, height }) };
};
const frames = new Map();
let frameId = 0;
w.requestAnimationFrame = (callback) => (frames.set(++frameId, callback), frameId);
w.cancelAnimationFrame = (id) => frames.delete(id);
const runFrame = (now) => { const pending = [...frames.values()]; frames.clear(); pending.forEach((callback) => callback(now)); };

const media = { tracks: [], recorders: [], processors: [] };
Object.defineProperty(w.navigator, "mediaDevices", { configurable: true, value: {
  getUserMedia: async () => { const track = { stopped: false, stop() { track.stopped = true; } }; media.tracks.push(track); return { getTracks: () => [track] }; },
  enumerateDevices: async () => [],
} });
globalThis.MediaRecorder = class {
  constructor() { this.state = "inactive"; media.recorders.push(this); }
  start() { this.state = "recording"; }
  stop() { this.state = "inactive"; this.onstop?.(); }
};
globalThis.AudioContext = class {
  constructor() { this.sampleRate = 24_000; this.destination = {}; }
  createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
  createScriptProcessor() { const processor = { connect() {}, disconnect() {}, onaudioprocess: null }; media.processors.push(processor); return processor; }
  close() { return Promise.resolve(); }
};
const speak = (level) => media.processors.at(-1).onaudioprocess({ inputBuffer: { getChannelData: () => new Float32Array(2048).fill(level) } });

const React = require("react");
const { createRoot } = require("react-dom/client");
const stubbed = {
  ProviderModelPicker: "export function ProviderModelPicker() { return null; }",
  RichComposerEditor: 'import { forwardRef } from "react"; export const RichComposerEditor = forwardRef(function RichComposerEditor({ text }, ref) { return createElement("textarea", { readOnly: true, value: text }); });',
  ComposerUtilityBar: "export function ComposerUtilityBar() { return null; }",
  ProjectedFilePicker: "export function ProjectedFilePicker() { return null; }",
  ChatAttachmentCard: "export function ChatAttachmentStrip() { return null; }",
};
const compiled = build({
  stdin: {
    contents: 'export { ChatComposer } from "./src/components/chat/ChatComposer"; export { ClientPreferencesProvider } from "./src/clientPreferences";',
    resolveDir: path.join(__dirname, ".."), loader: "ts",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  plugins: [{
    name: "composer-stubs",
    setup(builder) {
      builder.onResolve({ filter: new RegExp(`/(${Object.keys(stubbed).join("|")})$`) }, (args) => ({ path: path.basename(args.path), namespace: "stub" }));
      builder.onLoad({ filter: /.*/, namespace: "stub" }, (args) => ({ loader: "js", contents: `import { createElement } from "react";\n${stubbed[args.path]}` }));
    },
  }],
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return module.exports;
});

const container = document.getElementById("root");
const root = createRoot(container);
after(() => { root.unmount(); w.close(); });
const settle = (ms = 20) => new Promise((resolve) => setTimeout(resolve, ms));
const query = (selector) => container.querySelector(selector);
const noop = () => {};
async function showComposer(texts) {
  const { ChatComposer, ClientPreferencesProvider } = await compiled;
  root.render(null);
  await settle();
  root.render(React.createElement(ClientPreferencesProvider, { profile: "lab" }, React.createElement(ChatComposer, {
    accessProfile: "lab", text: "", attachments: [], providerInterfaces: [], providerInterfaceId: "provider", providerModel: "model", reasoningEffort: "high",
    submissionMode: "send", canSubmit: false, onSubmit: noop, onKeyDown: noop, onTextChange: (text) => texts.push(text), onFilesPicked: noop, onRemoveAttachment: noop,
    onProviderInterfaceChange: noop, onProviderModelChange: noop, onReasoningEffortChange: noop, fileInputRef: { current: null },
  })));
  await settle();
}
const press = async (label) => {
  const button = query(`button[aria-label="${label}"]`);
  assert.ok(button, label);
  button.click();
  await settle();
};
const latestBars = () => draws.at(-1).splice(0).map(({ x, height }) => ({ x: Math.round(x * 100) / 100, height }));

test("a dictation draws one waveform bar per six pixels and advances a bar per 200ms of display frames, carrying audio heard before the first frame", async () => {
  await showComposer([]);
  await press("Dictate");
  assert.ok(query(".composer-voice-footer canvas.composer-waveform"));
  speak(0.2);
  const frameAt = performance.now() + 200;
  runFrame(frameAt);
  let bars = latestBars();
  assert.equal(bars.length, 11, "a 60px canvas holds a bar every 6px");
  assert.deepEqual(bars.slice(1).map((bar, index) => Math.round(bar.x - bars[index].x)), Array(10).fill(6));
  const newest = bars.at(-1).x;
  assert.ok(newest > 54 && newest <= 60, `the newest bar sits at the right edge: ${newest}`);
  assert.ok(bars.at(-1).height > bars[0].height * 3, "the newest bar carries the sound heard before the first frame");
  runFrame(frameAt + 100);
  bars = latestBars();
  assert.equal(Math.round((newest - bars.at(-1).x) * 10) / 10, 3, "half a step later the bars have scrolled half a bar");
  await settle(260);
  assert.deepEqual(latestBars(), [], "the waveform only moves on display frames");
  await press("Cancel dictation");
});

test("cancelling a dictation stops the microphone and discards the recording while stopping transcribes it", async () => {
  const texts = [];
  await showComposer(texts);
  await press("Dictate");
  const footer = query(".composer-voice-footer");
  assert.equal(footer.textContent, "", "a dictation shows no running duration");
  assert.ok(query('button[aria-label="Cancel dictation"] .glyph-close'));
  speak(0.2);
  await settle(280);
  await press("Cancel dictation");
  assert.ok(!query(".composer-voice-footer"));
  assert.ok(media.tracks.at(-1).stopped && media.recorders.at(-1).state === "inactive");
  await settle(750);
  assert.deepEqual(texts, [], "a cancelled dictation is never transcribed");
  await press("Dictate");
  speak(0.2);
  await settle(280);
  await press("Stop dictation");
  await settle(750);
  assert.deepEqual(texts, ["Review the active run and continue with the next implementation step."]);
});
