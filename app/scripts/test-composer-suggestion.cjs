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
buildSync({ entryPoints: [path.join(__dirname, "../src/components/chat/composerSuggestion.ts")], outfile: bundle,
  bundle: true, platform: "node", format: "cjs", packages: "external" });
const Module = require("node:module");
const compiled = new Module(bundle, module);
compiled.filename = bundle;
compiled.paths = module.paths;
compiled._compile(fs.readFileSync(bundle, "utf8"), bundle);
const { composerSuggestionKey: key, composerSuggestionPlugin, visibleComposerSuggestion } = compiled.exports;
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
    assert.equal(view.dom.querySelector(".composer-suggestion").textContent, "Покажи, как это работает.");
    assert.equal(view.dom.querySelector(".composer-suggestion").getAttribute("aria-hidden"), "true");
    press("Enter");
    assert.equal(submits, 0);
    assert.equal(view.state.doc.textContent, "Покажи, как это работает.");
    assert.equal(visibleComposerSuggestion(view.state), null);
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
    set("ime", "Не вводить за пользователя");
    view.dom.dispatchEvent(new dom.window.CompositionEvent("compositionstart", { bubbles: true }));
    assert.equal(visibleComposerSuggestion(view.state), null);
    assert.equal(plugin.props.handleDOMEvents.keydown(view, new dom.window.KeyboardEvent("keydown", { key: "Enter", isComposing: true })), false);
    assert.equal(view.state.doc.textContent, "");
    view.dispatch(view.state.tr.setMeta(key, { suggestion: null }));
    assert.equal(visibleComposerSuggestion(view.state), null);
  } finally { view.destroy(); dom.window.close(); }
});
