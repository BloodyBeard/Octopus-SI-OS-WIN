const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "Node", "Event", "MouseEvent", "FocusEvent", "KeyboardEvent"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
globalThis.ResizeObserver = w.ResizeObserver = class { observe() {} disconnect() {} };
w.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 100, y: 100, left: 100, top: 100, right: 200, bottom: 120, width: 100, height: 20 });

const React = require("react");
const { createRoot } = require("react-dom/client");
const compiled = build({
  entryPoints: [path.join(__dirname, "../src/components/chat/MessageTooltip.tsx")],
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent", loader: { ".css": "empty" },
  external: ["react", "react-dom", "react/jsx-runtime"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return module.exports.MessageTooltip;
});

const root = createRoot(document.getElementById("root"));
after(() => { React.act(() => root.unmount()); w.close(); });
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const tooltip = () => document.querySelector('[role="tooltip"]');
const trigger = () => document.querySelector(".chat-tooltip-trigger");
const fire = (target, type, init = {}) => React.act(async () => {
  const event = new w.MouseEvent(type, { bubbles: true, cancelable: true, ...init });
  if (init.pointerType) Object.defineProperty(event, "pointerType", { value: init.pointerType });
  target.dispatchEvent(event);
});
let shows = 0;
async function show(props) {
  const MessageTooltip = await compiled;
  await React.act(async () => root.render(React.createElement("div", { id: "outside", key: ++shows },
    React.createElement(MessageTooltip, { label: "Opens the file", delayMs: 30, ...props }, "report.md"))));
}
async function hover() {
  await fire(trigger(), "pointermove");
  await React.act(() => delay(50));
}

test("hovering opens the tooltip after its delay, once, and only for a mouse", async () => {
  await show();
  await fire(trigger(), "pointermove", { pointerType: "touch" });
  await React.act(() => delay(50));
  assert.ok(!tooltip(), "touch never opens a hover tooltip");
  await fire(trigger(), "pointermove");
  assert.ok(!tooltip(), "the tooltip waits for its delay");
  await React.act(() => delay(50));
  assert.equal(tooltip().textContent, "Opens the file");
  assert.equal(trigger().getAttribute("aria-describedby"), tooltip().id);
  await fire(trigger(), "pointerout", { relatedTarget: document.getElementById("outside") });
  assert.ok(!tooltip(), "leaving the trigger closes it");
});

test("leaving before the delay cancels the pending open", async () => {
  await show();
  await fire(trigger(), "pointermove");
  await fire(trigger(), "pointerout", { relatedTarget: document.getElementById("outside") });
  await React.act(() => delay(60));
  assert.ok(!tooltip());
});

test("a measurement queued while leaving cannot reopen a closed tooltip", async () => {
  await show();
  await hover();
  await React.act(async () => {
    trigger().dispatchEvent(new w.MouseEvent("pointerout", { bubbles: true, relatedTarget: document.body }));
    w.dispatchEvent(new w.Event("resize"));
  });
  assert.ok(!tooltip(), "a late geometry update must keep the tooltip closed");
});

test("moving between triggers leaves only the latest tooltip and dragging opens none", async () => {
  const MessageTooltip = await compiled;
  await React.act(async () => root.render(React.createElement("div", { key: ++shows },
    ...["First", "Second"].map(label => React.createElement(MessageTooltip, { key: label, label, delayMs: 30 }, label)))));
  const [first, second] = document.querySelectorAll(".chat-tooltip-trigger");
  await fire(first, "pointermove");
  await fire(second, "pointermove");
  await React.act(() => delay(50));
  assert.deepEqual([...document.querySelectorAll('[role="tooltip"]')].map(node => node.textContent), ["Second"]);
  await fire(second, "pointerout", { relatedTarget: document.body });
  await fire(first, "pointerout", { relatedTarget: document.body });
  await fire(first, "pointermove");
  await React.act(() => delay(50));
  await fire(second, "pointermove");
  await React.act(() => delay(50));
  assert.deepEqual([...document.querySelectorAll('[role="tooltip"]')].map(node => node.textContent), ["Second"]);
  await fire(second, "pointerout", { relatedTarget: document.body });
  await fire(second, "pointermove", { buttons: 1 });
  await React.act(() => delay(50));
  assert.equal(tooltip(), null);
});

test("an SVG binding keeps its group and exposes the shared tooltip on hover", async () => {
  const MessageTooltip = await compiled;
  await React.act(async () => root.render(React.createElement("svg", null,
    React.createElement(MessageTooltip, { asChild: true, label: "Run: reader · Interface: files", delayMs: 30 },
      React.createElement("g", { role: "img", tabIndex: 0, "aria-label": "Binding" }, React.createElement("line", { x1: 0, y1: 0, x2: 20, y2: 20 }))))));
  const group = document.querySelector("svg g");
  assert.equal(group.getAttribute("role"), "img");
  assert.equal(document.querySelector("svg span"), null);
  group.getBoundingClientRect = w.HTMLElement.prototype.getBoundingClientRect;
  await fire(group, "pointermove");
  await React.act(() => delay(50));
  assert.equal(tooltip().textContent, "Run: reader · Interface: files");
  assert.equal(group.getAttribute("aria-describedby"), tooltip().id);
});

test("pressing, clicking, blurring the window or scrolling closes an open tooltip", async () => {
  for (const close of [
    () => fire(trigger(), "pointerdown"),
    () => fire(trigger(), "click"),
    () => React.act(async () => w.dispatchEvent(new w.Event("blur"))),
    () => React.act(async () => document.getElementById("outside").dispatchEvent(new w.Event("scroll"))),
  ]) {
    await show();
    await fire(trigger(), "pointerout", { relatedTarget: document.getElementById("outside") });
    await hover();
    assert.ok(tooltip());
    await close();
    assert.ok(!tooltip(), close.toString());
  }
});

test("an activity target is a focusable link that activates without bubbling the click", async () => {
  let activations = 0, outerClicks = 0;
  const MessageTooltip = await compiled;
  await React.act(async () => root.render(React.createElement("div", { id: "outside", onClick: () => outerClicks++ },
    React.createElement(MessageTooltip, { label: "Open report.md", delayMs: 30, variant: "activity-target", onActivate: () => activations++ }, "report.md"))));
  const target = trigger();
  assert.deepEqual([target.getAttribute("role"), target.tabIndex, target.dataset.agentActivityFileLink], ["link", 0, "true"]);
  await fire(target, "click");
  assert.deepEqual([activations, outerClicks], [1, 0]);
  await React.act(async () => target.dispatchEvent(new w.KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true })));
  assert.equal(activations, 2);
  await show();
  assert.equal(trigger().getAttribute("role"), null);
  assert.equal(trigger().dataset.agentActivityFileLink, undefined);
});

test("a tooltip closed by a click stays closed until the pointer leaves and returns", async () => {
  await show();
  await hover();
  assert.ok(tooltip());
  await fire(trigger(), "click");
  await hover();
  assert.ok(!tooltip(), "moving over the same trigger does not reopen what the click closed");
  await fire(trigger(), "pointerout", { relatedTarget: document.getElementById("outside") });
  await hover();
  assert.ok(tooltip());
});
