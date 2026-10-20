const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "SVGElement", "Node", "Event", "MouseEvent", "KeyboardEvent", "WheelEvent", "navigator", "getComputedStyle", "localStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "getComputedStyle" ? w[key].bind(w) : key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
globalThis.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
w.scrollTo = () => {};

const geometry = new WeakMap();
const box = (element) => geometry.get(element) ?? {};
const setBox = (element, values) => geometry.set(element, { ...box(element), ...values });
Object.defineProperty(w.HTMLElement.prototype, "scrollTop", { configurable: true, get() { return box(this).scrollTop ?? 0; }, set(value) { setBox(this, { scrollTop: value }); } });
for (const name of ["scrollHeight", "clientHeight"]) {
  Object.defineProperty(w.HTMLElement.prototype, name, { configurable: true, get() { return box(this)[name] ?? 0; } });
}
w.HTMLElement.prototype.scrollTo = function (options) { this.scrollTop = options.top; };
w.HTMLElement.prototype.scrollIntoView = function () {};
w.HTMLElement.prototype.getBoundingClientRect = function () {
  const height = box(this).height ?? 0;
  return { x: 0, y: 0, left: 0, top: 0, right: 720, bottom: height, width: 720, height };
};

const resizeObservers = new Set();
globalThis.ResizeObserver = w.ResizeObserver = class {
  constructor(callback) { this.callback = callback; this.targets = new Set(); resizeObservers.add(this); }
  observe(target) { this.targets.add(target); }
  unobserve(target) { this.targets.delete(target); }
  disconnect() { this.targets.clear(); resizeObservers.delete(this); }
};
const resize = (sizes) => {
  for (const [element, height] of sizes) setBox(element, { height });
  for (const observer of [...resizeObservers]) {
    const entries = sizes.filter(([element]) => observer.targets.has(element)).map(([target, height]) => ({ target, borderBoxSize: [{ blockSize: height }], contentRect: { height } }));
    if (entries.length) observer.callback(entries, observer);
  }
};
const observedTurnCount = () => new Set([...resizeObservers].flatMap((observer) => [...observer.targets].filter((target) => target.parentElement?.dataset.threadTurnKey))).size;

const React = require("react");
const { createRoot } = require("react-dom/client");
const contentRenders = new Map();
const contentStub = {
  name: "message-content",
  setup(bundle) {
    bundle.onResolve({ filter: /\/MessageContent$/ }, () => ({ path: "MessageContent", namespace: "fixture" }));
    bundle.onLoad({ filter: /.*/, namespace: "fixture" }, () => ({
      loader: "js",
      contents: `import { createElement } from "react";
export function MessageContent({ text, variant, streaming, streamKey }) {
  globalThis.__contentRenders.set(text, (globalThis.__contentRenders.get(text) ?? 0) + 1);
  return createElement("span", { "data-content": variant ?? "", "data-streaming": streaming ? "true" : undefined, "data-stream-key": streamKey }, text);
}`,
    }));
  },
};
globalThis.__contentRenders = contentRenders;
const compiled = build({
  entryPoints: [path.join(__dirname, "../src/components/chat/ChatMessageList.tsx")],
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent", plugins: [contentStub],
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react", "@pierre/diffs", "@pierre/diffs/*", "shiki", "@shikijs/*", "mermaid", "katex"],
}).then((result) => {
  const module = { exports: {} };
  const diffs = new Proxy({}, { get: (_, name) => name === "__esModule" ? true : name === "preloadHighlighter" ? async () => {} : () => null });
  const load = (specifier) => specifier.startsWith("@pierre/diffs") ? diffs : require(specifier);
  Function("require", "module", "exports", result.outputFiles[0].text)(load, module, module.exports);
  return module.exports;
});

const container = document.getElementById("root");
const root = createRoot(container);
const errors = [];
w.addEventListener("error", (event) => errors.push(event.error));
after(() => { root.unmount(); assert.deepEqual(errors, []); w.close(); });

const settle = async () => { for (let pass = 0; pass < 3; pass += 1) await new Promise((resolve) => setTimeout(resolve, 15)); };
const query = (selector) => container.querySelector(selector);
const user = (id, text, createdAtMs = 1) => ({ id, role: "user", text, createdAtMs });
const answer = (id, text, extra = {}) => ({ id, role: "assistant", text, createdAtMs: 2, eventKind: "message", terminal: true, ...extra });
const event = (id, eventKind, text, eventData = {}) => ({ id, role: "assistant", text, createdAtMs: 2, eventKind, eventData });
const tool = (id, status) => event(id, "dynamic_tool", "", { tool: "run_list", call_id: id, status, ok: status === "completed", arguments: {}, _activity_origin: { interface_role: "provider" } });
const turns = (count, prefix = "t") => Array.from({ length: count }, (_, index) => [user(`${prefix}u${index}`, `${prefix} question ${index}`, index * 10 + 1), answer(`${prefix}a${index}`, `${prefix} answer ${index}`, { createdAtMs: index * 10 + 2 })]).flat();

let headerEdge = [];
const noop = () => {};
const resolved = async () => {};
const unresolvedVisualization = () => null;
const accepted = () => true;
const steered = async () => true;
const reportHeaderEdge = (scrolled) => headerEdge.push(scrolled);
const toolPresentations = {};
const composer = React.createElement("div", { id: "composer" }, "Composer");
const props = (overrides) => ({
  activeMessageCount: overrides.messages?.length ?? 0, conversationId: "task-a", findQuery: "", findActiveIndex: 0, onFindMatchCountChange: noop,
  onHeaderEdgeScrollChange: reportHeaderEdge, dragOver: false, onDragOver: noop, onDragLeave: noop, onDrop: noop,
  footer: composer, onOperatorRequestResponse: resolved, onOpenAssistantReview: noop,
  onOpenAssistantVisualization: noop, onOpenFile: noop, resolveAssistantVisualization: unresolvedVisualization, pinnedArtifactIds: [], pinningArtifactIds: [],
  toolPresentations, onTogglePinnedMessage: resolved, onPinMessageExcerpt: resolved, onContinueInNewTask: noop,
  onEditUserMessage: accepted, onSteerUserMessage: steered, steerableMessageId: "", revealPinnedContext: null, responseStreaming: false,
  onPinnedContextRevealed: noop, messages: [], ...overrides,
});
const show = async (overrides) => {
  const { ChatMessageList } = await compiled;
  root.render(React.createElement(ChatMessageList, props(overrides)));
  await settle();
};
const list = () => query(".thread-scroll-container");
const messageGroup = (id) => query(`[data-message-id="${id}"]`)?.closest(".group");
const userScroll = async (distanceFromBottom) => {
  list().dispatchEvent(new w.WheelEvent("wheel", { deltaY: -120, bubbles: true }));
  list().scrollTop = -distanceFromBottom;
  list().dispatchEvent(new w.Event("scroll"));
  await settle();
};
const absent = (element, message) => assert.ok(element === null || element === undefined, message ?? "expected no element");
const mountedTurnKeys = () => [...container.querySelectorAll("[data-thread-turn-key]")].map((element) => element.dataset.threadTurnKey);

test("a settled answer carries its actions; a streaming or non-terminal reply does not, and latency is never shown", async () => {
  await show({ messages: [user("u1", "Summarize the report"), answer("a1", "The report balances.", { latencyMs: 1500 })] });
  assert.ok(messageGroup("a1").querySelector('[aria-label="Copy"]'));
  assert.doesNotMatch(container.textContent, /1\.5\s?s|1500/);
  await show({ messages: [user("u1", "Summarize the report"), { ...event("p1", "message_delta", "The report bal", { phase: "final_answer" }), terminal: false }] });
  assert.ok(messageGroup("p1"), "an unfinished answer still shows its text");
  absent(messageGroup("p1").querySelector('[aria-label="Copy"]'));
  const streaming = event("d1", "message_delta", "The report bal", { phase: "final_answer" });
  await show({ messages: [user("u1", "Summarize the report"), streaming], responseStreaming: true });
  assert.equal(query('[data-message-id="d1"] [data-content]').dataset.streaming, "true");
  absent(messageGroup("d1").querySelector('[aria-label="Copy"]'));
});

test("only the latest turn works while a response streams", async () => {
  const messages = [user("u1", "First"), tool("c1", "in_progress"), user("u2", "Second", 20), tool("c2", "in_progress")];
  await show({ messages, responseStreaming: true });
  const states = [...container.querySelectorAll(".octopus-turn-activity")].map((element) => element.dataset.state);
  assert.equal(states.at(-1), "working");
  assert.ok(states.slice(0, -1).every((state) => state !== "working"), states.join());
  await show({ messages, responseStreaming: false });
  assert.ok([...container.querySelectorAll(".octopus-turn-activity")].every((element) => element.dataset.state !== "working"));
});

test("commentary and answers stream under their own identity only while they are deltas", async () => {
  await show({ messages: [user("u1", "Plan it"), event("c1", "commentary_delta", "Looking at the plan")], responseStreaming: true, activityPresentation: "ledger" });
  const streaming = query('[data-message-id="c1"] [data-content]');
  assert.deepEqual([streaming.dataset.streaming, streaming.dataset.streamKey], ["true", "c1"]);
  await show({ messages: [user("u1", "Plan it"), event("c1", "commentary", "Looking at the plan")], responseStreaming: true, activityPresentation: "ledger" });
  const settled = query('[data-message-id="c1"] [data-content]');
  assert.deepEqual([settled.dataset.streaming, settled.dataset.streamKey], [undefined, undefined]);
  const answerDelta = event("d1", "message_delta", "Draft", { phase: "final_answer" });
  await show({ messages: [user("u1", "Plan it"), answerDelta], responseStreaming: true });
  assert.equal(query('[data-message-id="d1"] [data-content]').dataset.streamKey, "d1");
});

test("tool calls carry the mark of their surface", async () => {
  await show({ messages: [user("u1", "Go back"), { ...tool("b1", "completed"), eventData: { ...tool("b1", "completed").eventData, tool: "browser.back" } }, tool("g1", "completed")], activityPresentation: "ledger" });
  query('[aria-label="Used the browser, run list"]').click();
  await settle();
  assert.ok(query('[data-message-id$=":b1"] .glyph-browserUse'), "browser tools show the browser mark");
  assert.ok(query('[data-message-id$=":g1"] .glyph-plugins'), "other tools show the plugin mark");
});

test("a new task shows the home surface, an empty task its label, a loading task neither", async () => {
  await show({ messages: [], emptyState: "new-task", conversationId: "" });
  assert.ok(query(".octopus-chat-home"));
  absent(list());
  await show({ messages: [], emptyState: "empty-task", emptyLabel: "Nothing here yet" });
  absent(query(".octopus-chat-home"));
  assert.match(list().textContent, /Nothing here yet/);
  await show({ messages: [], emptyState: "loading-task" });
  assert.doesNotMatch(list().textContent, /Nothing here yet|No activity yet/);
});

test("the composer floats over the thread and reserves its own height at the bottom", async () => {
  await show({ messages: [user("u1", "Hello"), answer("a1", "Hi")] });
  const footer = query('[data-thread-scroll-footer="true"]');
  assert.ok(footer.querySelector("#composer"));
  assert.equal(list().firstElementChild.contains(footer), false, "the composer is not part of the scrolled content");
  assert.ok(footer.classList.contains("absolute") && footer.classList.contains("bottom-0") && !footer.classList.contains("sticky"));
  const reserve = list().firstElementChild.lastElementChild;
  assert.equal(reserve.getAttribute("aria-hidden"), "true");
  assert.match(reserve.className, /--thread-scroll-padding-bottom/);
  resize([[footer, 120]]);
  assert.equal(list().style.getPropertyValue("--thread-scroll-padding-bottom"), "136px");
  await show({ messages: [user("u1", "Hello"), answer("a1", "Hi")], footer: null });
  absent(query('[data-thread-scroll-footer="true"]'));
  absent(list().firstElementChild.lastElementChild.getAttribute("aria-hidden"));
});

test("moved files are labeled by their destination", async () => {
  const change = event("f1", "file_change", "", { status: "completed", changes: [
    { path: "draft.md", kind: { type: "update", move_path: "final.md" } },
    { path: "notes.md", kind: { type: "update", movePath: "archive/kept.md" } },
  ] });
  await show({ messages: [user("u1", "Rename them"), change], activityPresentation: "ledger" });
  query('[aria-label="Edited files"]').click();
  await settle();
  assert.ok(query('[aria-label="Toggle diff for final.md"]'));
  assert.ok(query('[aria-label="Toggle diff for kept.md"]'));
  absent(query('[aria-label="Toggle diff for draft.md"]') ?? query('[aria-label="Toggle diff for notes.md"]'));
});

test("settled turns do not render again while the latest turn streams", async () => {
  const settled = turns(3, "s").map((message) => message.role === "assistant" ? { ...message, eventData: { phase: "final_answer", usage: { tokens: [12, 40] } } } : message);
  await show({ messages: [...settled, user("u9", "Next", 100), event("d1", "message_delta", "Wor", { phase: "final_answer" })], responseStreaming: true, conversationId: "memo" });
  const before = new Map(contentRenders);
  await show({ messages: [...structuredClone(settled), user("u9", "Next", 100), event("d1", "message_delta", "Working on it", { phase: "final_answer" })], responseStreaming: true, conversationId: "memo" });
  for (const message of settled) assert.equal(contentRenders.get(message.text), before.get(message.text), `${message.id} rendered again`);
  assert.equal(contentRenders.get("Working on it"), 1);
  assert.match(container.textContent, /Working on it/);
});

test("the reader's place belongs to the task and new replies never pull them down", async () => {
  await show({ messages: turns(4, "p"), conversationId: "task-of-equal-length" });
  await show({ messages: turns(4, "a"), conversationId: "task-a" });
  setBox(list(), { clientHeight: 500, scrollHeight: 2000 });
  await userScroll(400);
  const latest = () => query('button[aria-label^="Scroll to bottom"]');
  assert.equal(latest().getAttribute("aria-hidden"), "false");
  const grown = [...turns(4, "a"), user("n1", "More", 900), answer("n2", "Reply", { createdAtMs: 901 })];
  await show({ messages: grown, conversationId: "task-a" });
  const kept = -list().scrollTop;
  assert.ok(kept > 400, "the new turn below the reader extends the distance instead of shifting what they read");
  assert.equal(latest().getAttribute("aria-label"), "Scroll to bottom · 2 new messages");
  await show({ messages: turns(2, "b"), conversationId: "task-b" });
  assert.equal(list().scrollTop, 0);
  assert.equal(latest().getAttribute("aria-hidden"), "true");
  await show({ messages: grown, conversationId: "task-a" });
  assert.equal(-list().scrollTop, kept);
  assert.equal(latest().getAttribute("aria-hidden"), "false");
});

test("the header edge reports whether the thread is scrolled away from its top", async () => {
  await show({ messages: turns(2, "h"), conversationId: "edge" });
  setBox(list(), { clientHeight: 500, scrollHeight: 2000 });
  headerEdge = [];
  await userScroll(1500);
  assert.deepEqual(headerEdge, [false]);
  await userScroll(100);
  assert.deepEqual(headerEdge, [false, true]);
});

test("a long thread mounts a window of turns, measures them, and keeps the window across tasks", async () => {
  await show({ messages: turns(30, "m"), conversationId: "measured" });
  assert.equal(mountedTurnKeys().length, 30);
  assert.equal(observedTurnCount(), 30, "short threads measure every turn so the window starts from real heights");
  await show({ messages: turns(60, "v"), conversationId: "long" });
  setBox(list(), { clientHeight: 800, scrollHeight: 60 * 172 });
  await settle();
  const latestWindow = mountedTurnKeys();
  assert.ok(latestWindow.length < 20, `${latestWindow.length} turns mounted`);
  assert.ok(latestWindow.includes("vu59"));
  await userScroll(60 * 1200);
  const topWindow = mountedTurnKeys();
  assert.ok(topWindow.includes("vu0"), topWindow.join());
  assert.equal(topWindow.includes("vu59"), false);
  await show({ messages: turns(2, "o"), conversationId: "other" });
  await show({ messages: turns(60, "v"), conversationId: "long" });
  assert.ok(mountedTurnKeys().includes("vu0"), "returning to the task restores its window");
});

test("a turn growing below the reader keeps their place in the same frame", async () => {
  await show({ messages: turns(60, "g"), conversationId: "grow" });
  setBox(list(), { clientHeight: 800, scrollHeight: 60 * 172 });
  await settle();
  await userScroll(500);
  const below = query('[data-thread-turn-key="gu59"]');
  assert.ok(below, mountedTurnKeys().join());
  const turnElements = [...container.querySelectorAll("[data-thread-turn-key] > div")];
  resize(turnElements.map((element) => [element, 160]));
  await settle();
  const before = -list().scrollTop;
  resize([[query('[data-thread-turn-key="gu59"] > div'), 460]]);
  assert.equal(-list().scrollTop, before + 300, "the measurement is committed and the scroll anchored before the frame ends");
});

test("returning to a long task shows the same turns at their measured heights", async () => {
  await show({ messages: turns(60, "r"), conversationId: "restored" });
  setBox(list(), { clientHeight: 800, scrollHeight: 60 * 400 });
  await settle();
  await userScroll(6000);
  for (let pass = 0; pass < 3; pass += 1) {
    resize([...container.querySelectorAll("[data-thread-turn-key] > div")].map((element) => [element, 400]));
    await settle();
  }
  const shown = mountedTurnKeys();
  assert.equal(shown.includes("ru59"), false);
  await show({ messages: turns(60, "q"), conversationId: "elsewhere" });
  await show({ messages: turns(60, "r"), conversationId: "restored" });
  assert.deepEqual(mountedTurnKeys(), shown);
});
