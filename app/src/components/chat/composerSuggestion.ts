import { Plugin, PluginKey, TextSelection, type EditorState } from "prosemirror-state";
import { Decoration, DecorationSet, type EditorView } from "prosemirror-view";
import type { Node } from "prosemirror-model";

export type ComposerSuggestion = { id: string; text: string };
type SuggestionState = { suggestion: ComposerSuggestion | null; consumed: boolean; composing: boolean; acceptedId: string | null };
export const composerSuggestionKey = new PluginKey<SuggestionState>("composer-suggestion");

export function visibleComposerSuggestion(state: EditorState): ComposerSuggestion | null {
  const value = composerSuggestionKey.getState(state);
  const first = state.doc.firstChild;
  return value && !value.consumed && !value.composing && state.doc.childCount === 1
    && first?.type.name === "paragraph" && first.content.size === 0 ? value.suggestion : null;
}

export function composerSuggestionPlugin(parse: (text: string) => Node): Plugin<SuggestionState> {
  return new Plugin<SuggestionState>({
    key: composerSuggestionKey,
    state: {
      init: () => ({ suggestion: null, consumed: false, composing: false, acceptedId: null }),
      apply(transaction, previous) {
        const update = transaction.getMeta(composerSuggestionKey);
        if (!update) return previous;
        if (Object.hasOwn(update, "suggestion")) {
          return { ...previous, suggestion: update.suggestion,
            consumed: previous.acceptedId === update.suggestion?.id };
        }
        if (update.consumed) return { ...previous, consumed: true, acceptedId: previous.suggestion?.id ?? null };
        return { ...previous, ...update };
      },
    },
    props: {
      decorations(state) {
        const suggestion = visibleComposerSuggestion(state);
        if (!suggestion) return null;
        return DecorationSet.create(state.doc, [Decoration.widget(1, () => {
          const span = document.createElement("span");
          span.className = "composer-suggestion";
          span.textContent = suggestion.text;
          span.contentEditable = "false";
          span.setAttribute("aria-hidden", "true");
          span.inert = true;
          return span;
        }, { side: -1, ignoreSelection: true })]);
      },
      handleDOMEvents: {
        keydown(view, event) {
          if (view.composing || event.isComposing || event.keyCode === 229) return false;
          const suggestion = visibleComposerSuggestion(view.state);
          if (!suggestion || event.shiftKey || event.altKey || event.ctrlKey || event.metaKey
            || !["Tab", "ArrowRight", "Enter"].includes(event.key)) return false;
          event.preventDefault();
          const transaction = view.state.tr.replaceWith(0, view.state.doc.content.size, parse(suggestion.text).content);
          transaction.setSelection(TextSelection.atEnd(transaction.doc));
          view.dispatch(transaction.setMeta(composerSuggestionKey, { consumed: true }).scrollIntoView());
          return true;
        },
        compositionstart(view) { setComposing(view, true); return false; },
        compositionend(view) {
          requestAnimationFrame(() => { if (!view.isDestroyed) setComposing(view, false); });
          return false;
        },
        mousedown(view, event) {
          if (event.button !== 0 || !visibleComposerSuggestion(view.state)) return false;
          event.preventDefault();
          view.focus();
          return true;
        },
      },
    },
  });
}

function setComposing(view: EditorView, composing: boolean) {
  view.dispatch(view.state.tr.setMeta(composerSuggestionKey, { composing }));
}
