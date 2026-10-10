import { useEffect, useId, useMemo, useRef, useState } from "react";
import { Glyph } from "../IconGlyphs";
import type {
  AssistantCodeCommentDirective,
  AssistantInlineVisualizationDirective,
  AssistantVisualizationTarget,
} from "./assistantDirectives";

type Props = {
  codeComments: AssistantCodeCommentDirective[];
  inlineVisualizations: AssistantInlineVisualizationDirective[];
  onOpenReview: () => void;
  onOpenVisualization: (path: string) => void;
  resolveVisualization: (path: string) => AssistantVisualizationTarget | null;
};

const MAX_VISUALIZATION_BYTES = 2 * 1024 * 1024;

export function AssistantDirectiveSurfaces({ codeComments, inlineVisualizations, onOpenReview, onOpenVisualization, resolveVisualization }: Props) {
  if (!codeComments.length && !inlineVisualizations.length) return null;
  return (
    <div className="mt-3 flex min-w-0 flex-col gap-2">
      {codeComments.length ? (
        <button
          className="group/directive flex min-h-10 w-full cursor-interaction items-center gap-2 rounded-xl border border-token-border bg-token-foreground/[0.025] px-3 py-2 text-left text-sm outline-none hover:bg-token-list-hover-background focus-visible:ring-1 focus-visible:ring-token-focus-border"
          onClick={onOpenReview}
          type="button"
        >
          <span className="flex size-6 shrink-0 items-center justify-center rounded-md bg-token-list-hover-background text-token-description-foreground"><Glyph name="changes" size={14} /></span>
          <span className="min-w-0 flex-1"><strong className="block font-medium">Code review</strong><span className="block truncate text-xs text-token-description-foreground">{codeComments.length} inline {codeComments.length === 1 ? "comment" : "comments"}</span></span>
          <Glyph className="shrink-0 text-token-description-foreground" name="openExternal" size={13} />
        </button>
      ) : null}
      {inlineVisualizations.map((visualization) => (
        <InlineVisualization
          key={`${visualization.directiveIndex}:${visualization.path}`}
          onOpen={() => onOpenVisualization(visualization.path)}
          path={visualization.path}
          target={resolveVisualization(visualization.path)}
        />
      ))}
    </div>
  );
}

function InlineVisualization({ onOpen, path, target }: { onOpen: () => void; path: string; target: AssistantVisualizationTarget | null }) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const channelId = useId();
  const [height, setHeight] = useState(240);
  const [source, setSource] = useState("");
  const [state, setState] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState("");

  useEffect(() => {
    if (!target) {
      setSource("");
      setState("error");
      setError("This task has no Files surface that can resolve the visualization.");
      return;
    }
    const transport = window.octopusTransport;
    if (!transport?.readWorkerEnvironmentFile) {
      setSource("");
      setState("error");
      setError("Interactive visualizations require the desktop Worker Environment file surface.");
      return;
    }
    let cancelled = false;
    setState("loading");
    setError("");
    void transport.readWorkerEnvironmentFile({
      path: target.path,
      surfaceId: target.surfaceId,
      workerEnvironmentId: target.workerEnvironmentId,
    })
      .then((file) => {
        if (cancelled) return;
        if (file.workerEnvironmentId !== target.workerEnvironmentId || file.surfaceId !== target.surfaceId || file.path !== target.path) {
          throw new Error("The visualization file response no longer matches this task surface.");
        }
        if (file.kind !== "text") throw new Error("The visualization must be a standalone text HTML file.");
        if (file.truncated || file.size > MAX_VISUALIZATION_BYTES) throw new Error("The visualization exceeds the bounded 2 MiB inline preview.");
        setSource(file.text);
        setState("ready");
      })
      .catch((failure) => {
        if (cancelled) return;
        setSource("");
        setState("error");
        setError(failure instanceof Error ? failure.message : String(failure));
      });
    return () => { cancelled = true; };
  }, [target?.path, target?.revision, target?.surfaceId, target?.workerEnvironmentId]);

  useEffect(() => {
    const receive = (event: MessageEvent) => {
      if (event.source !== frameRef.current?.contentWindow || !event.data || typeof event.data !== "object") return;
      if (event.data.type !== "octopus.inline-visualization.resize" || event.data.channelId !== channelId) return;
      const next = Number(event.data.height);
      if (Number.isFinite(next)) setHeight(Math.max(160, Math.min(1_200, Math.ceil(next))));
    };
    window.addEventListener("message", receive);
    return () => window.removeEventListener("message", receive);
  }, [channelId]);

  const documentSource = useMemo(() => source ? visualizationDocument(source, channelId) : "", [channelId, source]);
  const label = basename(path);
  return (
    <section className="overflow-hidden rounded-xl border border-token-border bg-token-foreground/[0.025]">
      <header className="flex min-h-10 items-center gap-2 border-b border-token-border px-3 py-2">
        <span className="flex size-6 shrink-0 items-center justify-center rounded-md bg-token-list-hover-background text-token-description-foreground"><Glyph name="insights" size={14} /></span>
        <span className="min-w-0 flex-1"><strong className="block truncate text-sm font-medium">{label || "Interactive visualization"}</strong><span className="block truncate text-xs text-token-description-foreground">Interactive visualization</span></span>
        <button aria-label={`Open ${path} in Files`} className="flex size-7 cursor-interaction items-center justify-center rounded-lg text-token-description-foreground outline-none hover:bg-token-list-hover-background hover:text-token-foreground focus-visible:ring-1 focus-visible:ring-token-focus-border" onClick={onOpen} title="Open in Files" type="button"><Glyph name="openExternal" size={13} /></button>
      </header>
      {state === "loading" ? (
        <div className="flex h-60 items-center justify-center gap-2 text-sm text-token-description-foreground"><Glyph className="is-spinning" name="refresh" size={14} />Loading visualization…</div>
      ) : state === "error" ? (
        <div className="flex min-h-32 flex-col items-center justify-center gap-2 px-4 py-6 text-center"><Glyph className="text-token-description-foreground" name="warning" size={18} /><strong className="text-sm font-medium">Visualization unavailable</strong><span className="max-w-lg text-xs text-token-description-foreground">{error}</span></div>
      ) : (
        <iframe
          className="block w-full border-0 bg-transparent"
          key={`${target?.workerEnvironmentId}:${target?.surfaceId}:${target?.path}:${target?.revision}:${source.length}`}
          ref={frameRef}
          sandbox="allow-scripts"
          srcDoc={documentSource}
          style={{ height }}
          title={`Interactive visualization: ${label}`}
        />
      )}
    </section>
  );
}

function visualizationDocument(source: string, channelId: string): string {
  const body = stripFrontMatter(source);
  const policy = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data: blob:; media-src data: blob:; font-src data:; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'\">";
  const bridge = `<script>(()=>{const send=()=>parent.postMessage({type:"octopus.inline-visualization.resize",channelId:${JSON.stringify(channelId)},height:Math.ceil(document.documentElement.getBoundingClientRect().height)},"*");new ResizeObserver(send).observe(document.documentElement);addEventListener("load",send);send()})()</script>`;
  let documentSource = body;
  if (/<head(?:\s[^>]*)?>/i.test(documentSource)) documentSource = documentSource.replace(/<head(?:\s[^>]*)?>/i, (head) => `${head}${policy}`);
  else if (/<html(?:\s[^>]*)?>/i.test(documentSource)) documentSource = documentSource.replace(/<html(?:\s[^>]*)?>/i, (html) => `${html}<head>${policy}</head>`);
  else documentSource = `<!doctype html><html><head>${policy}</head><body>${documentSource}</body></html>`;
  return /<\/body\s*>/i.test(documentSource)
    ? documentSource.replace(/<\/body\s*>/i, `${bridge}</body>`)
    : `${documentSource}${bridge}`;
}

function stripFrontMatter(value: string): string {
  if (!value.startsWith("---\n")) return value;
  const end = value.indexOf("\n---", 4);
  if (end < 0) return value;
  return value.slice(end + 4).replace(/^\n/, "");
}

function basename(value: string): string {
  return value.replaceAll("\\", "/").split("/").filter(Boolean).at(-1) ?? value;
}
