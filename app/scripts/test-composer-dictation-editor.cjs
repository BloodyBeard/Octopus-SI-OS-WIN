const assert = require("node:assert/strict");
const path = require("node:path");
const { test } = require("node:test");
const { build } = require("esbuild");
const { EditorState, TextSelection } = require("prosemirror-state");
const { schema } = require("prosemirror-markdown");
const { history, undo } = require("prosemirror-history");

test("live speech stays out of the persisted document, preserves typing and selection, and commits as one undoable edit", async () => {
  const result = await build({ entryPoints: [path.join(__dirname, "../src/components/chat/composerDictation.ts")], bundle: true, platform: "node", format: "cjs", write: false, external: ["prosemirror-state", "prosemirror-view"] });
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  const { composerDictationKey, composerDictationPlugin, startComposerDictation, updateComposerDictation, cancelComposerDictation, finishComposerDictation } = module.exports;
  const doc = schema.topNodeType.create(null, schema.nodes.paragraph.create(null, schema.text("Before after")));
  const view = {
    state: EditorState.create({ doc, plugins: [composerDictationPlugin(), history()], selection: TextSelection.create(doc, 7) }),
    dispatch(transaction) { this.state = this.state.apply(transaction); },
    focus() {},
  };
  startComposerDictation(view);
  updateComposerDictation(view, "interim words");
  assert.equal(view.state.doc.textContent, "Before after");
  assert.equal(composerDictationKey.getState(view.state).text, "interim words");
  view.dispatch(view.state.tr.insertText("Typed ", 1));
  updateComposerDictation(view, "final words");
  finishComposerDictation(view, "final words");
  assert.equal(view.state.doc.textContent, "Typed Before final words after");
  assert.equal(composerDictationKey.getState(view.state), null);
  assert.equal(undo(view.state, (transaction) => view.dispatch(transaction)), true);
  assert.equal(view.state.doc.textContent, "Typed Before after");
  view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, 7, 13)));
  startComposerDictation(view);
  updateComposerDictation(view, "replacement");
  cancelComposerDictation(view);
  assert.equal(view.state.doc.textContent, "Typed Before after");
  startComposerDictation(view);
  finishComposerDictation(view, "replacement");
  assert.equal(view.state.doc.textContent, "Typed replacement after");
});
