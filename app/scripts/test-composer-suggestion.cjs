const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { JSDOM } = require("jsdom");
const { buildSync } = require("esbuild");
const { EditorState, Plugin } = require("prosemirror-state");
const { EditorView } = require("prosemirror-view");
const { schema } = require("prosemirror-markdown");

const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-composer-suggestion-"));
const bundle = path.join(root, "suggestion.cjs");
buildSync({ stdin: { contents: 'export * from "./composerSuggestion"; export * from "./useComposerSuggestion"; export * from "./RichComposerEditor";', resolveDir: path.join(__dirname, "../src/components/chat"), loader: "ts" }, outfile: bundle,
  bundle: true, platform: "node", format: "cjs", packages: "external" });
const Module = require("node:module");
const compiled = new Module(bundle, module);
compiled.filename = bundle;
compiled.paths = module.paths;
compiled._compile(fs.readFileSync(bundle, "utf8"), bundle);
const { composerSuggestionKey: key, composerSuggestionPlugin, visibleComposerSuggestion, useComposerSuggestion, RichComposerEditor } = compiled.exports;
fs.rmSync(root, { recursive: true });

test("ghost text stays outside the draft; acceptance consumes the first Enter, not the second", () => {
  const dom = new JSDOM("<div id='editor'></div>", { pretendToBeVisual: true });
  for (const name of ["window", "document", "Node", "HTMLElement", "MutationObserver", "getSelection", "getComputedStyle", "requestAnimationFrame"]) {
    global[name] = typeof dom.window[name] === "function" && ["getSelection", "getComputedStyle", "requestAnimationFrame"].includes(name)
      ? dom.window[name].bind(dom.window) : dom.window[name];
  }
  dom.window.Range.prototype.getClientRects = () => [];
  dom.window.Range.prototype.getBoundingClientRect = () => ({ top: 0, left: 0, bottom: 0, right: 0 });
  const parse = (text) => schema.topNodeType.create(null, schema.nodes.paragraph.create(null, text ? schema.text(text) : null));
  let submits = 0;
  const plugin = composerSuggestionPlugin(parse);
  const view = new EditorView(document.querySelector("#editor"), { state: EditorState.create({
    doc: parse(""), plugins: [plugin, new Plugin({ props: { handleDOMEvents: { keydown(_view, event) {
      if (event.key !== "Enter") return false;
      submits += 1; event.preventDefault(); return true;
    } } } })],
  }) });
  const set = (id, text) => view.dispatch(view.state.tr.setMeta(key, { suggestion: { id, text } }));
  const press = (keyName, extra = {}) => view.dom.dispatchEvent(new dom.window.KeyboardEvent("keydown", { key: keyName, bubbles: true, cancelable: true, ...extra }));
  try {
    set("first", "Покажи, как это работает.");
    assert.equal(view.state.doc.textContent, "");
    assert.equal(view.dom.querySelector("[data-suggestion]").dataset.suggestion, "Покажи, как это работает.");
    assert.equal(view.dom.textContent, "");
    assert.ok(view.dom.querySelector("p.composer-suggestion-line > .ProseMirror-trailingBreak"));
    assert.match(view.dom.getAttribute("aria-description"), /Покажи, как это работает/);
    view.focus();
    press("Enter");
    assert.equal(submits, 0);
    assert.equal(view.state.doc.textContent, "Покажи, как это работает.");
    assert.equal(visibleComposerSuggestion(view.state), null);
    assert.equal(view.dom.querySelector(".composer-suggestion-line"), null);
    assert.equal(view.dom.hasAttribute("aria-description"), false);
    press("Enter");
    assert.equal(submits, 1);
    view.dispatch(view.state.tr.delete(0, view.state.doc.content.size));
    set("first", "Покажи, как это работает.");
    assert.equal(visibleComposerSuggestion(view.state), null);
    for (const accept of ["Tab", "ArrowRight"]) {
      set(accept, "Ещё вариант");
      view.dispatch(view.state.tr.insertText("мой текст"));
      assert.equal(visibleComposerSuggestion(view.state), null);
      view.dispatch(view.state.tr.delete(0, view.state.doc.content.size));
      assert.equal(visibleComposerSuggestion(view.state).text, "Ещё вариант");
      assert.equal(plugin.props.handleDOMEvents.keydown(view, new dom.window.KeyboardEvent("keydown", { key: accept, ctrlKey: true })), false);
      assert.equal(view.state.doc.textContent, "");
      press(accept);
      assert.equal(view.state.doc.textContent, "Ещё вариант");
      view.dispatch(view.state.tr.delete(0, view.state.doc.content.size));
    }
    set("dismiss", "A suggestion\nwith multiple lines");
    view.dom.querySelector(".composer-suggestion-line").dispatchEvent(new dom.window.MouseEvent("mousedown", { button: 0, bubbles: true, cancelable: true }));
    assert.equal(view.state.doc.textContent, "");
    assert.equal(visibleComposerSuggestion(view.state).text, "A suggestion\nwith multiple lines");
    assert.equal(document.activeElement, view.dom);
    set("dismiss", "A suggestion\nwith multiple lines");
    assert.equal(visibleComposerSuggestion(view.state).text, "A suggestion\nwith multiple lines");
    view.dom.blur();
    assert.equal(visibleComposerSuggestion(view.state).text, "A suggestion\nwith multiple lines");
    view.focus();
    assert.equal(visibleComposerSuggestion(view.state).text, "A suggestion\nwith multiple lines");
    view.dispatch(view.state.tr.insertText("My own message"));
    assert.equal(view.state.doc.textContent, "My own message");
    view.dispatch(view.state.tr.delete(0, view.state.doc.content.size));
    set("ime", "Не вводить за пользователя");
    view.dom.dispatchEvent(new dom.window.CompositionEvent("compositionstart", { bubbles: true }));
    assert.equal(visibleComposerSuggestion(view.state), null);
    assert.equal(plugin.props.handleDOMEvents.keydown(view, new dom.window.KeyboardEvent("keydown", { key: "Enter", isComposing: true })), false);
    assert.equal(view.state.doc.textContent, "");
    view.dispatch(view.state.tr.setMeta(key, { suggestion: null }));
    assert.equal(visibleComposerSuggestion(view.state), null);
  } finally { view.destroy(); dom.window.close(); }
});

test("suggestions survive temporary composer modes and refresh only for a new completed reply", async (t) => {
  const React = require("react");
  const { createRoot } = require("react-dom/client");
  const dom = new JSDOM("<div id='root'></div>", { pretendToBeVisual: true });
  Object.assign(global, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  for (const name of ["Node", "Element", "HTMLElement", "MutationObserver"]) global[name] = dom.window[name];
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let calls = 0, finish, visible;
  window.octopusTransport = { requestComposerPrediction: () => { calls++; return new Promise(resolve => { finish = resolve; }); } };
  function Probe({ source, enabled = true, text = "" }) {
    visible = useComposerSuggestion("home", source, enabled);
    return React.createElement(RichComposerEditor, { text, suggestion: visible, placeholder: "Message Home", onTextChange() {}, onKeyDown() {}, onFilesPicked() {} });
  }
  const root = createRoot(document.getElementById("root"));
  const render = async props => React.act(async () => root.render(React.createElement(Probe, props)));
  const tick = async () => React.act(async () => t.mock.timers.tick(501));
  try {
    await render({ source: 10 }); await tick();
    assert.equal(calls, 1);
    await render({ source: 10, enabled: false });
    await React.act(async () => finish({ id: "first", text: "Continue the example" }));
    await render({ source: 10 }); await tick();
    assert.equal(visible.text, "Continue the example");
    const first = visible;
    const editor = document.querySelector(".ProseMirror");
    await React.act(async () => editor.focus());
    assert.equal(editor.querySelector("[data-suggestion]").dataset.suggestion, first.text);
    assert.equal(editor.querySelector(".placeholder"), null);
    await render({ source: 10, text: "An external draft" });
    await render({ source: 10 });
    assert.equal(editor.querySelector("[data-suggestion]").dataset.suggestion, first.text);
    await React.act(async () => editor.blur());
    assert.equal(editor.querySelector("[data-suggestion]").dataset.suggestion, first.text);
    for (let i = 0; i < 5; i++) await render({ source: 10 });
    await render({ source: 10, enabled: false });
    assert.equal(visible, null);
    await render({ source: 10 }); await tick();
    assert.equal(visible, first);
    assert.equal(calls, 1);
    await render({ source: 20 });
    assert.equal(visible, null);
    await tick();
    await React.act(async () => finish({ id: "second", text: "Explain the next step" }));
    assert.equal(visible.text, "Explain the next step");
    assert.equal(calls, 2);
  } finally { await React.act(async () => root.unmount()); dom.window.close(); }
});
