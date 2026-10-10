import { adoptFilesStyles } from "./filesPierreSetup";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useAppearance } from "../../appearance";
import { Editor } from "@octopus/files-diffs/edit";
import { CodeView, type CodeViewOptions } from "@octopus/files-diffs";
import { fileLanguage, fileContentHash } from "./fileSupport";
import { filesCodeSurfaceCss } from "./filesCodeStyles";
import { octopusDiffThemeName } from "../settings/octopusDiffThemes";
import type { FilesEditorViewState } from "./filesPaneSnapshot";

const editorCss = `${filesCodeSurfaceCss}
[data-file] {
  --diffs-line-height: round(nearest, calc(var(--diffs-font-size, 12px) * 1.8), 1px);
  line-height: var(--diffs-line-height);
  --diffs-editor-line-highlight-bg: var(--color-octopus-editor-line-highlight);
  --diffs-editor-line-number-active-bg: var(--diffs-editor-line-highlight-bg);
  --diffs-editor-line-highlight-border: transparent;
}
[data-line][data-editor-active-line] { --diffs-line-bg: var(--diffs-editor-line-highlight-bg); }
[data-selection-range] { opacity: 0.5; }
`;

export default function FilesPierreEditor({ documentId, filename, initialViewState, label, onChange, onViewStateChange, value, wordWrap }: {
  documentId: string;
  filename: string;
  label: string;
  onChange: (value: string) => void;
  initialViewState?: FilesEditorViewState;
  onViewStateChange?: (state: FilesEditorViewState) => void;
  value: string;
  wordWrap: boolean;
}) {
  const appearance = useAppearance();
  const containerRef = useRef<HTMLDivElement>(null);
  const instanceRef = useRef<{ editor: Editor | null; view: CodeView; value: string } | null>(null);
  const changeRef = useRef(onChange);
  changeRef.current = onChange;
  const valueRef = useRef(value);
  valueRef.current = value;
  const initialViewRef = useRef(initialViewState);
  const viewChangeRef = useRef(onViewStateChange);
  viewChangeRef.current = onViewStateChange;
  const [history, setHistory] = useState({ canUndo: false, canRedo: false });
  const theme = useMemo(() => ({ dark: octopusDiffThemeName("dark", appearance.darkCodeTheme), light: octopusDiffThemeName("light", appearance.lightCodeTheme) }), [appearance.darkCodeTheme, appearance.lightCodeTheme]);

  function options(owner: NonNullable<typeof instanceRef.current>): CodeViewOptions<undefined, undefined> {
    return {
      createEditor: (editorType, options, editStateKey) => {
        const editor = new Editor(editorType, { ...options, enabledSelectionAction: false }, editStateKey);
        owner.editor = editor;
        return editor;
      },
      onItemEditChange: ({ file, editor }, item) => {
        if (item.id !== documentId || instanceRef.current !== owner || owner.editor !== editor) return;
        if (owner.value !== file.contents) {
          owner.value = file.contents;
          changeRef.current(file.contents);
        }
        setHistory({ canUndo: editor.canUndo, canRedo: editor.canRedo });
      },
      onPostRender: (root, _instance, reason) => {
        if (reason !== "unmount") {
          adoptFilesStyles(root);
          root.shadowRoot?.querySelector('[role="textbox"]')?.setAttribute("aria-label", label);
        }
      },
      disableFileHeader: true,
      overflow: wordWrap ? "wrap" : "scroll",
      theme,
      themeType: appearance.themeMode,
      unsafeCSS: editorCss,
      useTokenTransformer: true,
      itemMetrics: { lineHeight: Math.round(appearance.codeFontSize * 1.8), spacing: 0 },
      layout: { gap: 0, paddingBottom: 16, paddingTop: 0 },
    };
  }
  const optionsRef = useRef(options);
  optionsRef.current = options;

  function sourceFile(contents: string) {
    return { name: filename, lang: fileLanguage(filename), contents, cacheKey: `${documentId}:${contents.length}:${fileContentHash(contents)}` };
  }

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const view = new CodeView(undefined, undefined, true);
    const owner = { editor: null as Editor | null, view, value: valueRef.current };
    instanceRef.current = owner;
    view.setOptions(optionsRef.current(owner));
    view.setup(container);
    view.setItems([{ id: documentId, type: "file", edit: true, file: sourceFile(valueRef.current) }]);
    view.render(true);
    if (initialViewRef.current) {
      view.scrollTo({ type: "position", position: initialViewRef.current.scrollTop });
      container.scrollLeft = initialViewRef.current.scrollLeft;
    }
    const rememberScroll = () => viewChangeRef.current?.({ scrollTop: view.getScrollTop(), scrollLeft: container.scrollLeft });
    container.addEventListener("scroll", rememberScroll);
    setHistory({ canUndo: false, canRedo: false });
    return () => { container.removeEventListener("scroll", rememberScroll); instanceRef.current = null; view.cleanUp(); };
  }, [documentId, filename]);

  useLayoutEffect(() => {
    const instance = instanceRef.current;
    if (!instance) return;
    instance.view.setOptions(optionsRef.current(instance));
    instance.view.render(true);
  }, [appearance.codeFontSize, appearance.themeMode, label, theme, wordWrap]);

  useLayoutEffect(() => {
    const instance = instanceRef.current;
    if (!instance || instance.value === value) return;
    const item = instance.view.getItem(documentId);
    if (!item || item.type !== "file") throw new Error("The Files editor lost its document target");
    instance.value = value;
    instance.view.updateItem({ ...item, file: sourceFile(value), version: (item.version ?? 0) + 1 });
    instance.view.render(true);
    setHistory({ canUndo: instance.editor?.canUndo ?? false, canRedo: instance.editor?.canRedo ?? false });
  }, [documentId, filename, value]);

  return <div className="os-files-pierre-surface" data-pierre-editor-surface="">
    <div aria-label={label} className="os-files-pierre-viewport" ref={containerRef} />
    {history.canUndo || history.canRedo ? <div className="os-files-pierre-history">
      <button aria-label="Undo" disabled={!history.canUndo} onClick={() => instanceRef.current?.editor?.undo()} title="Undo" type="button"><UndoIcon /></button>
      <button aria-label="Redo" disabled={!history.canRedo} onClick={() => instanceRef.current?.editor?.redo()} title="Redo" type="button"><UndoIcon redo /></button>
    </div> : null}
  </div>;
}

function UndoIcon({ redo = false }: { redo?: boolean }) {
  return <svg aria-hidden="true" height="16" style={redo ? { transform: "scaleX(-1)" } : undefined} viewBox="0 0 16 16" width="16"><path d="M4.96785 2.29515C5.17276 2.09034 5.50502 2.0905 5.71003 2.29515C5.91503 2.50014 5.91496 2.8323 5.71003 3.03733L3.88875 4.85862H9.33894C11.82 4.85862 13.8584 6.86166 13.8585 9.35179C13.8585 11.8441 11.8292 13.8655 9.33894 13.8655H6.63875C6.34891 13.8653 6.11338 13.6299 6.11336 13.3401C6.11336 13.0502 6.34889 12.8148 6.63875 12.8147H9.33894C11.2513 12.8147 12.8077 11.2621 12.8077 9.35179C12.8077 7.45057 11.2492 5.9094 9.33894 5.9094H3.99226L5.71003 7.62815C5.91497 7.83309 5.9148 8.1653 5.71003 8.37034C5.50501 8.57537 5.17287 8.57537 4.96785 8.37034L2.47761 5.88108C2.17544 5.57851 2.17542 5.08794 2.47761 4.78538L4.96785 2.29515Z" fill="currentColor" /></svg>;
}
