import { Plugin, PluginKey, type EditorState } from "prosemirror-state";
import { Decoration, DecorationSet, type EditorView } from "prosemirror-view";

type DictationDraft = { from: number; to: number; text: string };
export const composerDictationKey = new PluginKey<DictationDraft | null>("composer-dictation");

export function composerDictationPlugin() {
  return new Plugin<DictationDraft | null>({
    key: composerDictationKey,
    state: {
      init: () => null,
      apply(transaction, current) {
        const update = transaction.getMeta(composerDictationKey);
        if (update !== undefined) return update;
        if (!current) return null;
        return { ...current, from: transaction.mapping.map(current.from, 1), to: transaction.mapping.map(current.to, 1) };
      },
    },
    props: {
      decorations(state) {
        const draft = composerDictationKey.getState(state);
        if (!draft?.text) return null;
        return DecorationSet.create(state.doc, [Decoration.widget(draft.from, () => {
          const span = document.createElement("span");
          span.className = "composer-dictation-draft";
          span.textContent = spacedTranscript(state, draft);
          return span;
        }, { side: -1, key: draft.text })]);
      },
    },
  });
}

function spacedTranscript(state: EditorState, draft: DictationDraft) {
  const before = state.doc.textBetween(Math.max(0, draft.from - 1), draft.from);
  const after = state.doc.textBetween(draft.to, Math.min(state.doc.content.size, draft.to + 1));
  return `${before && !/\s$/.test(before) ? " " : ""}${draft.text}${after && !/^\s/.test(after) ? " " : ""}`;
}

export function startComposerDictation(view: EditorView) {
  const { from, to } = view.state.selection;
  view.dispatch(view.state.tr.setMeta(composerDictationKey, { from, to, text: "" }).setMeta("addToHistory", false));
}

export function updateComposerDictation(view: EditorView, text: string) {
  const draft = composerDictationKey.getState(view.state);
  if (draft) view.dispatch(view.state.tr.setMeta(composerDictationKey, { ...draft, text }).setMeta("addToHistory", false).scrollIntoView());
}

export function cancelComposerDictation(view: EditorView) {
  view.dispatch(view.state.tr.setMeta(composerDictationKey, null).setMeta("addToHistory", false));
}

export function finishComposerDictation(view: EditorView, text: string) {
  const draft = composerDictationKey.getState(view.state) ?? { from: view.state.selection.from, to: view.state.selection.to, text: "" };
  const transaction = view.state.tr.setMeta(composerDictationKey, null);
  if (text.trim()) transaction.insertText(spacedTranscript(view.state, { ...draft, text }), draft.from, draft.to);
  view.dispatch(transaction.scrollIntoView());
  view.focus();
}
