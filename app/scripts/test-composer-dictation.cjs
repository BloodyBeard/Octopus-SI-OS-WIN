const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");
const { decodeAudio } = require("../electron/clientDictation.cjs");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "navigator", "getComputedStyle", "localStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "getComputedStyle" ? w[key].bind(w) : key === "window" ? w : w[key] });
}
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.devicePixelRatio = 1;
const requests = [];
const responses = [];
const saved = [];
const streamListeners = new Set();
let activeAttempt;
const streamedAudio = [];
w.octopusTransport = {
  startDictationStream: async ({ attemptId }) => { activeAttempt = attemptId; return { ok: true, value: null }; },
  appendDictationAudio: (attemptId, bytes) => streamedAudio.push({ attemptId, bytes }),
  finishDictationStream: async () => {
    const response = responses.shift();
    return typeof response === "function" ? response() : response ?? { ok: true, value: { provider: "codex", model: "fixture", text: "A recognized dictation." } };
  },
  onDictationStreamEvent: (listener) => { streamListeners.add(listener); return () => streamListeners.delete(listener); },
  logClientEvent: async () => {},
  windowZoomFactor: () => 1,
  saveDictationRecording: async (request) => {
    const entry = { version: 1, id: require("node:crypto").randomUUID(), durationMs: request.durationMs, byteLength: request.bytes.byteLength, createdAtMs: Date.now(), text: "", status: "recorded" };
    saved.push({ entry, bytes: new Uint8Array(request.bytes) });
    return { ok: true, value: entry };
  },
  updateDictationRecording: async ({ id, ...update }) => {
    const recording = saved.find(({ entry }) => entry.id === id);
    Object.assign(recording.entry, update);
    return { ok: true, value: recording.entry };
  },
  readDictationRecordings: async () => ({ ok: true, value: { version: 1, enabled: true, maxRecordings: 20, recordings: saved.map(({ entry }) => entry).reverse() } }),
  readDictationRecording: async (id) => ({ ok: true, value: saved.find(({ entry }) => entry.id === id) }),
  transcribeDictation: async (request) => {
    requests.push(request);
    const response = responses.shift();
    return typeof response === "function" ? response(request) : response ?? { ok: true, value: { provider: "codex", model: "fixture", text: "A recognized dictation." } };
  },
  cancelDictation: async () => {},
};
w.HTMLElement.prototype.getBoundingClientRect = function () { return { x: 600, y: 10, left: 600, top: 10, right: 632, bottom: 42, width: 32, height: 32 }; };
w.HTMLCanvasElement.prototype.getBoundingClientRect = function () { return { x: 0, y: 0, left: 0, top: 0, right: 60, bottom: 24, width: 60, height: 24 }; };
for (const [name, value] of [["clientWidth", 60], ["clientHeight", 24]]) {
  Object.defineProperty(w.HTMLCanvasElement.prototype, name, { configurable: true, get() { return value; } });
}
const draws = [];
w.HTMLCanvasElement.prototype.getContext = function () {
  const bars = [];
  draws.push(bars);
  const gradient = { addColorStop() {} };
  return { setTransform() {}, clearRect() {}, save() {}, restore() {}, translate() {}, beginPath() {}, fill() {}, fillRect() {}, createLinearGradient: () => gradient,
    roundRect: (x, y, width, height) => bars.push({ x, width, height }) };
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
const speak = (level) => media.processors.at(-1).onaudioprocess({ inputBuffer: { getChannelData: () => new Float32Array(7200).fill(level) } });

const React = require("react");
const { createRoot } = require("react-dom/client");
const stubbed = {
  ProviderModelPicker: "export function ProviderModelPicker() { return null; }",
  RichComposerEditor: 'import { forwardRef, useImperativeHandle, useRef } from "react"; export const RichComposerEditor = forwardRef(function RichComposerEditor({ text, onKeyDown, onTextChange }, ref) { const field = useRef(null); useImperativeHandle(ref, () => ({ startDictation() {}, updateDictation(value) { field.current.dataset.dictationDraft = value; }, cancelDictation() { delete field.current.dataset.dictationDraft; }, finishDictation(value) { delete field.current.dataset.dictationDraft; onTextChange(value); return value; } })); return createElement("textarea", { ref: field, readOnly: true, value: text, onKeyDown }); });',
  ComposerUtilityBar: "export function ComposerUtilityBar() { return null; }",
  ProjectedFilePicker: "export function ProjectedFilePicker() { return null; }",
  ChatAttachmentCard: "export function ChatAttachmentStrip() { return null; }",
};
const compiled = build({
  stdin: {
    contents: 'export { ChatComposer } from "./src/components/chat/ChatComposer"; export { useLocalDictation } from "./src/components/chat/useLocalDictation"; export { ClientPreferencesProvider } from "./src/clientPreferences"; export { DictationRecordings } from "./src/components/chat/DictationRecordings";',
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
async function showComposer(texts, submissions = []) {
  const { ChatComposer, ClientPreferencesProvider } = await compiled;
  root.render(null);
  await settle();
  root.render(React.createElement(ClientPreferencesProvider, { profile: "live" }, React.createElement(ChatComposer, {
    accessProfile: "live", runtimeHome: "/fixture/home", text: "", attachments: [], providerInterfaces: [], providerInterfaceId: "provider", providerModel: "model", reasoningEffort: "high",
    submissionMode: "send", canSubmit: false, onSubmit: (...args) => submissions.push(args), onKeyDown: noop, onTextChange: (text) => texts.push(text), onFilesPicked: noop, onRemoveAttachment: noop,
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
const startDictation = async () => {
  await press("Dictate");
  const deadline = performance.now() + 2_000;
  while (!query('button[aria-label="Stop dictation"] canvas[data-mode="text"]') && performance.now() < deadline) await settle(10);
  assert.ok(query('button[aria-label="Stop dictation"] canvas[data-mode="text"]'), "dictation capture has rendered its Stop control");
};
const cancelDictation = async () => {
  query("textarea").dispatchEvent(new w.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle();
};
const latestBars = () => draws.at(-1).splice(0).map(({ x, width, height }) => ({ x: Math.round(x * 100) / 100, width, height }));

test("initial mount effects preserve dictation started immediately after commit", async () => {
  const { useLocalDictation, ClientPreferencesProvider } = await compiled;
  function CaptureOnCommit() {
    const dictation = useLocalDictation({ accessProfile: "live", composerId: "initial-capture", onTranscriptStart: noop,
      onTranscriptUpdate: noop, onTranscriptCancel: noop, onTranscriptInsert: noop, onTranscriptSend: noop });
    React.useLayoutEffect(() => { void dictation.startDictation(); }, [dictation.startDictation]);
    return React.createElement("span", { "data-capture": dictation.isDictating ? "recording" : dictation.isStarting ? "starting" : "idle" });
  }
  root.render(React.createElement(ClientPreferencesProvider, { profile: "live" }, React.createElement(CaptureOnCommit)));
  const deadline = performance.now() + 2_000;
  while (!query('[data-capture="recording"]') && performance.now() < deadline) await settle(10);
  assert.ok(query('[data-capture="recording"]'), "initial passive effects must preserve the microphone startup operation");
  assert.equal(media.tracks.at(-1).stopped, false);
  root.render(null);
  await settle();
  assert.equal(media.tracks.at(-1).stopped, true, "unmount still releases the microphone");
});

test("streaming dictation keeps the composer controls and shows six responsive bars inside Stop", async () => {
  const texts = [];
  await showComposer(texts);
  await startDictation();
  assert.ok(query('button[aria-label="Stop dictation"] canvas[data-mode="text"]'));
  assert.equal(query(".composer-voice-footer"), null);
  assert.ok(query('button[aria-label="Add files and more"]'));
  speak(0.2);
  runFrame(performance.now() + 100);
  const bars = latestBars();
  assert.equal(bars.length, 6);
  assert.ok(bars.every((bar) => bar.width === 2 && bar.height > 3));
  assert.ok(streamedAudio.at(-1).bytes instanceof ArrayBuffer);
  streamListeners.forEach((listener) => listener({ attemptId: activeAttempt, type: "transcript", text: "Words appearing while speaking" }));
  await settle();
  assert.equal(query("textarea").dataset.dictationDraft, "Words appearing while speaking");
  assert.deepEqual(texts, [], "intermediate speech is not published as a saved draft");
  await cancelDictation();
  assert.equal(query("textarea").dataset.dictationDraft, undefined);
});

test("cancelling dictation preserves its audio without inserting text while Stop finalizes the text", async () => {
  const texts = [];
  await showComposer(texts);
  await startDictation();
  assert.ok(query(".composer-footer"), "the normal footer remains in place");
  const countBefore = saved.length;
  assert.ok(query('button[aria-label="Add files and more"]'));
  assert.ok(query('.composer-footer button[aria-label="Send"]'));
  speak(0.2);
  await settle(280);
  await cancelDictation();
  assert.ok(!query(".composer-voice-footer"));
  assert.ok(media.tracks.at(-1).stopped);
  await settle(750);
  assert.deepEqual(texts, [], "a cancelled dictation is never transcribed");
  assert.equal(saved.length, countBefore + 1, "cancelled audio remains in Recent recordings");
  await startDictation();
  speak(0.2);
  await settle(280);
  await press("Stop dictation");
  await settle(750);
  assert.deepEqual(texts, ["A recognized dictation."]);
});

test("ten-minute dictation stops capture and its full WAV fits the transcription boundary", async () => {
  await showComposer([]);
  await startDictation();
  await settle(260);
  media.processors.at(-1).onaudioprocess({ inputBuffer: { getChannelData: () => new Float32Array(24_000 * 600 + 1) } });
  assert.ok(media.tracks.at(-1).stopped);
  const bytes = Buffer.alloc(44 + 24_000 * 600 * 2);
  bytes.write("RIFF"); bytes.write("WAVE", 8);
  const request = { bytes, durationMs: 600_000, mediaType: "audio/wav", sampleRateHz: 24_000, provider: "codex" };
  assert.equal(decodeAudio(request).byteLength, bytes.length);
  assert.throws(() => decodeAudio({ ...request, durationMs: 600_001 }), /10 minutes/);
  assert.throws(() => decodeAudio({ ...request, bytes: Buffer.alloc(bytes.length + 1) }), /10 minutes/);
  await settle(750);
});

test("a failed dictation opens its exact recording in Voice settings and retries without sending", async () => {
  const texts = [], submissions = [];
  await showComposer(texts, submissions);
  responses.push({ ok: false, error: { code: "empty-transcript", message: "No speech was recognized. Try again or record a new dictation." } });
  await startDictation();
  speak(0.2);
  await press("Stop dictation");
  assert.deepEqual(texts, []);
  assert.match(query(".composer-voice-recovery").textContent, /Dictation stopped/);
  const destinations = [];
  const receive = (event) => destinations.push(event.detail);
  w.addEventListener("octopus:client-command", receive);
  [...container.querySelectorAll("button")].find((button) => button.textContent === "View recording").click();
  await settle();
  w.removeEventListener("octopus:client-command", receive);
  assert.deepEqual(destinations, [{ id: "open_settings", source: "surface", settingsDestination: { sectionId: "voice-audio", dictationRecordingId: saved.at(-1).entry.id } }]);
  assert.equal(document.querySelector('[role="dialog"]'), null);
  assert.equal(query('button[aria-label="Dictation recordings"]'), null);
  const previous = saved.at(-1);
  await press("Retry dictation");
  assert.deepEqual(new Uint8Array(requests.at(-1).bytes), new Uint8Array(previous.bytes));
  assert.deepEqual(texts, ["A recognized dictation."]);
  assert.deepEqual(submissions, []);
  assert.equal(query(".composer-voice-recovery"), null, "successful transcription adds no saved notice");
});

async function pressDocument(label) {
  const button = document.querySelector(`button[aria-label="${label}"]`);
  assert.ok(button, label);
  button.click();
  await settle();
}

test("cancelling a pending transcription rejects its late result and releases the microphone", async () => {
  const texts = [];
  await showComposer(texts);
  let finish;
  responses.push(() => new Promise((resolve) => { finish = resolve; }));
  await startDictation();
  speak(0.2);
  await press("Stop dictation");
  await cancelDictation();
  finish({ ok: true, value: { provider: "codex", model: "fixture", text: "Late result" } });
  await settle();
  assert.deepEqual(texts, []);
  assert.ok(media.tracks.at(-1).stopped);
});

test("the dictation send arrow transcribes and submits its text while the stop button only inserts", async () => {
  const texts = [], submissions = [];
  await showComposer(texts, submissions);
  assert.equal(query('button[aria-label="Dictation recordings"]'), null);
  await startDictation();
  speak(0.2);
  await press("Send");
  assert.deepEqual(submissions, [[undefined, "A recognized dictation."]]);
  assert.deepEqual(texts, ["A recognized dictation."]);
});

test("recent recordings render inline in Voice settings with exact-record recovery and recording actions", async () => {
  const { DictationRecordings, ClientPreferencesProvider } = await compiled;
  const entry = { version: 1, id: require("node:crypto").randomUUID(), createdAtMs: Date.now(), durationMs: 1_000,
    byteLength: 48_044, text: "A saved dictation.", status: "transcribed" };
  saved.push({ entry, bytes: new Uint8Array(entry.byteLength) });
  const recordingId = entry.id;
  let showedAll = false;
  root.render(React.createElement(ClientPreferencesProvider, { profile: "live" }, React.createElement(DictationRecordings, { recordingId, onShowAll: () => { showedAll = true; } })));
  await settle();
  assert.match(container.textContent, /Recent recordings/);
  assert.match(container.textContent, /Your last 20 recordings are saved on this device/);
  assert.equal(container.querySelectorAll(".dictation-recording-row").length, 1);
  assert.ok(query(".dictation-recording-text.is-selected"));
  assert.equal(document.querySelector('[role="dialog"], audio, [role="switch"]'), null);
  const copied = [];
  w.octopusTransport.copyText = async (text) => { copied.push(text); };
  await press("Copy dictation");
  assert.deepEqual(copied, [entry.text]);
  assert.ok(query('button[aria-label="Copy dictation"] .glyph-check'));
  await press("Recording actions");
  assert.ok([...document.querySelectorAll('[role="menuitem"]')].some((item) => item.textContent.includes("Download recording")));
  assert.ok([...document.querySelectorAll('[role="menuitem"]')].some((item) => item.textContent.includes("Delete recording")));
  document.dispatchEvent(new w.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle();
  [...container.querySelectorAll("button")].find((button) => button.textContent === "Show all recordings").click();
  assert.equal(showedAll, true);
});
