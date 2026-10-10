import { adoptFilesStyles } from "./filesPierreSetup";
import { createElement, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useAppearance } from "../../appearance";
import { DIFFS_TAG_NAME, Virtualizer as FileVirtualizer, VirtualizedFile as VirtualFile, type FileOptions as VirtualFileOptions, type FileContents as EditorFile } from "@octopus/files-diffs";
import { readFileMetrics as readMetrics, equalFileMetrics as equalMetrics, fileLanguage, fileContentHash, type FileMetrics } from "./fileSupport";
import { filesCodeSurfaceCss } from "./filesCodeStyles";
import { octopusDiffThemeName } from "../settings/octopusDiffThemes";

export default function FilesReadonlyViewer({ documentId, filename, label, value, wordWrap }: {
  documentId: string;
  filename: string;
  label: string;
  value: string;
  wordWrap: boolean;
}) {
  const appearance = useAppearance();
  const rootRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const probeRef = useRef<HTMLSpanElement>(null);
  const [virtualizer] = useState(() => new FileVirtualizer());
  const [metrics, setMetrics] = useState<FileMetrics | null>(null);
  const file = useMemo(() => ({ name: filename, lang: fileLanguage(filename), contents: value, cacheKey: `${documentId}:${value.length}:${fileContentHash(value)}` }), [documentId, filename, value]);
  const options = useMemo<VirtualFileOptions<undefined, undefined>>(() => ({
    onPostRender: (host, _instance, reason) => { if (reason !== "unmount") adoptFilesStyles(host); },
    disableFileHeader: true,
    enableLineSelection: true,
    overflow: wordWrap ? "wrap" : "scroll",
    theme: { dark: octopusDiffThemeName("dark", appearance.darkCodeTheme), light: octopusDiffThemeName("light", appearance.lightCodeTheme) },
    themeType: appearance.themeMode,
    unsafeCSS: filesCodeSurfaceCss,
  }), [appearance.darkCodeTheme, appearance.lightCodeTheme, appearance.themeMode, wordWrap]);

  useLayoutEffect(() => {
    const root = rootRef.current;
    const content = contentRef.current;
    if (!root || !content) return;
    virtualizer.setup(root, content);
    return () => virtualizer.cleanUp();
  }, [virtualizer]);

  useLayoutEffect(() => {
    const probe = probeRef.current;
    if (!probe) return;
    const measure = () => {
      const next = readMetrics(probe);
      setMetrics(current => current && equalMetrics(current, next) ? current : next);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(probe);
    return () => observer.disconnect();
  }, [appearance.codeFontSize]);

  return <div aria-label={label} className="os-files-code-viewer" ref={rootRef} role="region">
    <span aria-hidden="true" className="os-files-code-metrics-probe" data-review-file-source-metrics-probe="" ref={probeRef} />
    <div data-review-path={filename} ref={contentRef}>
      {metrics ? <ReadonlyFile documentId={documentId} file={file} key={`${documentId}:${metrics.lineHeight}`} metrics={metrics} options={options} virtualizer={virtualizer} /> : null}
    </div>
  </div>;
}

function ReadonlyFile({ documentId, file, metrics, options, virtualizer }: {
  documentId: string;
  file: EditorFile;
  metrics: FileMetrics;
  options: VirtualFileOptions<undefined, undefined>;
  virtualizer: FileVirtualizer;
}) {
  const hostRef = useRef<HTMLElement>(null);
  const instanceRef = useRef<VirtualFile | null>(null);
  const currentRef = useRef({ file, metrics, options });
  currentRef.current = { file, metrics, options };

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const current = currentRef.current;
    const instance = new VirtualFile(current.options, virtualizer, current.metrics, undefined, true);
    instanceRef.current = instance;
    adoptFilesStyles(host);
    instance.hydrate({ file: current.file, fileContainer: host });
    return () => { instanceRef.current = null; instance.cleanUp(); };
  }, [documentId, file.name, virtualizer]);

  useLayoutEffect(() => {
    const instance = instanceRef.current;
    if (!instance) return;
    instance.setOptions(options);
    instance.setMetrics(metrics);
    instance.render({ file, forceRender: true });
  }, [documentId, file, metrics, options]);

  return createElement(DIFFS_TAG_NAME, { ref: hostRef, style: { display: "block" } });
}
