import { useMemo, useState, type CSSProperties } from "react";
import { MessageContent } from "../chat/MessageContent";
import { Glyph } from "../IconGlyphs";
import { payloadText } from "./rendererRegistry";
import type { ArtifactPreviewModel } from "./types";

const MAX_INLINE_MEDIA_BASE64_CHARS = Math.ceil((40 * 1024 * 1024) / 3) * 4;

type Props = {
  model: ArtifactPreviewModel;
  zoomPercent: number;
};

export function ArtifactPreview({ model, zoomPercent }: Props) {
  switch (model.kind) {
    case "code":
      return <TextPreview code language={model.language} text={payloadText(model.payload) || model.searchableText} zoomPercent={zoomPercent} />;
    case "document":
      return <DocumentPreview model={model} zoomPercent={zoomPercent} />;
    case "image":
      return <ImagePreview model={model} zoomPercent={zoomPercent} />;
    case "json":
      return <TextPreview code language="json" text={prettyJson(model.payload)} zoomPercent={zoomPercent} />;
    case "media":
      return <MediaPreview model={model} />;
    case "notebook":
      return <NotebookPreview model={model} />;
    case "pdf":
      return <PdfPreview model={model} />;
    case "text":
      return <TextPreview text={payloadText(model.payload) || model.searchableText} zoomPercent={zoomPercent} />;
    default:
      return <UnsupportedPreview model={model} />;
  }
}

function TextPreview({ code = false, language = "", text, zoomPercent }: {
  code?: boolean;
  language?: string;
  text: string;
  zoomPercent: number;
}) {
  const lines = useMemo(() => text.split("\n"), [text]);
  return (
    <div className={`artifact-text-preview ${code ? "is-code" : ""}`} style={{ "--artifact-preview-scale": zoomPercent / 100 } as CSSProperties}>
      {code ? <div className="artifact-code-language">{language || "code"}</div> : null}
      <ol>
        {lines.map((line, index) => (
          <li key={index}>
            <span className="artifact-line-number" aria-hidden="true">{index + 1}</span>
            <span className="artifact-line-content">{line || " "}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}

function DocumentPreview({ model, zoomPercent }: Props) {
  const pages = documentPages(model.payload, model.searchableText);
  if (!pages.some((page) => page.trim())) {
    return <PreviewUnavailable glyph="file" title="Document text is not exposed by the admitted payload" detail="Binary office documents require renderer-ready text or structured pages from the interface." />;
  }
  return (
    <div className="artifact-document-preview" style={{ "--artifact-preview-scale": zoomPercent / 100 } as CSSProperties}>
      {pages.map((page, pageIndex) => (
        <article className="artifact-document-page" key={pageIndex}>
          <span className="artifact-document-page-number">{pageIndex + 1}</span>
          <div>{page}</div>
        </article>
      ))}
    </div>
  );
}

function ImagePreview({ model, zoomPercent }: { model: ArtifactPreviewModel; zoomPercent: number }) {
  const [failed, setFailed] = useState(false);
  if (!model.dataUrl || failed) {
    return <PreviewUnavailable glyph="image" title={failed ? "Image could not be decoded" : "Image bytes are not exposed by the admitted payload"} detail={model.mediaType || model.filename} />;
  }
  return (
    <div className="artifact-image-preview">
      <img
        alt={model.title}
        draggable={false}
        onError={() => setFailed(true)}
        src={model.dataUrl}
        style={{ width: `${zoomPercent}%` }}
      />
    </div>
  );
}

function PdfPreview({ model }: { model: ArtifactPreviewModel }) {
  if (!model.dataUrl) {
    return <PreviewUnavailable glyph="file" title="PDF bytes are not exposed by the admitted payload" detail="The admitted artifact cannot be rendered as a document preview." />;
  }
  return (
    <div className="artifact-pdf-preview">
      <object aria-label={`PDF preview of ${model.title}`} data={model.dataUrl} type="application/pdf">
        <PreviewUnavailable glyph="file" title="The embedded PDF renderer is unavailable" detail={model.filename} />
      </object>
    </div>
  );
}

function MediaPreview({ model }: { model: ArtifactPreviewModel }) {
  if (!model.dataUrl) {
    return <PreviewUnavailable glyph="play" title="Media bytes are not exposed by the admitted payload" detail={model.mediaType || model.filename} />;
  }
  if (model.mediaType.toLocaleLowerCase().startsWith("video/")) {
    return <div className="artifact-media-preview"><video controls preload="metadata" src={model.dataUrl} /></div>;
  }
  return <div className="artifact-media-preview is-audio"><audio controls preload="metadata" src={model.dataUrl} /></div>;
}

function NotebookPreview({ model }: { model: ArtifactPreviewModel }) {
  const notebook = notebookRecord(model.payload);
  if (!notebook) return <PreviewUnavailable glyph="file" title="Notebook payload is malformed" detail="A notebook preview requires a JSON object with a cells array." />;
  if (!notebook.cells.length) return <PreviewUnavailable glyph="file" title="This notebook does not contain any cells" detail="Read-only notebook preview" />;
  return (
    <div className="artifact-notebook-preview">
      <div className="artifact-notebook-status">
        <span>IPYNB · {notebook.cells.length} {notebook.cells.length === 1 ? "cell" : "cells"}</span>
        <span className="artifact-notebook-readonly">Read only</span>
      </div>
      <div className="artifact-notebook-cells">
        {notebook.cells.map((cell, index) => <NotebookCell cell={cell} index={index} key={cellKey(cell, index)} total={notebook.cells.length} />)}
      </div>
    </div>
  );
}

function NotebookCell({ cell, index, total }: { cell: unknown; index: number; total: number }) {
  const row = object(cell);
  const type = text(row.cell_type) || "raw";
  const source = joinedText(row.source);
  const outputs = array(row.outputs);
  const title = notebookCellTitle(row, type, index + 1);
  return (
    <details className="artifact-notebook-cell" open>
      <summary>
        <span className="artifact-notebook-cell-title"><Glyph name="chevronDown" size={12} /><strong>{title}</strong></span>
        <span>{type === "code" && row.execution_count != null ? `Run ${String(row.execution_count)} · ` : ""}Cell {index + 1} of {total}</span>
      </summary>
      <div className="artifact-notebook-cell-body">
        {type === "markdown" ? (
          source.trim() ? <MessageContent text={source} /> : <NotebookEmpty label="Empty Markdown cell" />
        ) : type === "code" ? (
          <>
            {source.trim() ? <pre className="artifact-notebook-code"><code>{source}</code></pre> : <NotebookEmpty label="Empty code cell" />}
            {outputs.length ? <div className="artifact-notebook-outputs">{outputs.map((output, outputIndex) => <NotebookOutput output={output} key={outputIndex} outputIndex={outputIndex} />)}</div> : null}
          </>
        ) : source.trim() ? <pre className="artifact-notebook-code"><code>{source}</code></pre> : <NotebookEmpty label="Empty raw cell" />}
      </div>
    </details>
  );
}

function NotebookOutput({ output, outputIndex }: { output: unknown; outputIndex: number }) {
  const row = object(output);
  const outputType = text(row.output_type);
  if (outputType === "stream") return <pre className="artifact-notebook-output-text">{joinedText(row.text)}</pre>;
  if (outputType === "error") {
    const name = text(row.ename) || "Error";
    const value = text(row.evalue);
    const traceback = joinedText(row.traceback);
    return <div className="artifact-notebook-error"><strong>{name}{value ? `: ${value}` : ""}</strong>{traceback ? <pre>{traceback}</pre> : null}</div>;
  }
  const data = object(row.data);
  const image = notebookImage(data);
  if (image) return <div className="artifact-notebook-image"><img alt={`Notebook output ${outputIndex + 1}`} src={image} /></div>;
  const html = joinedText(data["text/html"]);
  if (html) return <iframe className="artifact-notebook-html" sandbox="" srcDoc={sandboxedNotebookHtml(html)} title={`Notebook HTML output ${outputIndex + 1}`} />;
  const markdown = joinedText(data["text/markdown"]);
  if (markdown) return <div className="artifact-notebook-markdown"><MessageContent text={markdown} /></div>;
  const plain = joinedText(data["text/plain"]);
  if (plain) return <pre className="artifact-notebook-output-text">{plain}</pre>;
  return <pre className="artifact-notebook-output-text">{prettyJson(data)}</pre>;
}

function NotebookEmpty({ label }: { label: string }) {
  return <div className="artifact-notebook-empty">{label}</div>;
}

function UnsupportedPreview({ model }: { model: ArtifactPreviewModel }) {
  return <PreviewUnavailable glyph="artifact" title="No renderer accepted this payload" detail={`${model.mediaType || "Unknown carrier"} · ${model.filename}`} />;
}

function PreviewUnavailable({ detail, glyph, title }: { detail: string; glyph: "artifact" | "file" | "image" | "play"; title: string }) {
  return (
    <div className="artifact-preview-unavailable">
      <span><Glyph name={glyph} size={22} /></span>
      <strong>{title}</strong>
      <small>{detail}</small>
    </div>
  );
}

function documentPages(payload: unknown, fallback: string): string[] {
  const record = object(payload);
  const pages = array(record.pages).map((page) => {
    if (typeof page === "string") return page;
    const row = object(page);
    return text(row.text) || text(row.content) || prettyJson(page);
  }).filter(Boolean);
  if (pages.length) return pages;
  const source = payloadText(payload) || fallback;
  const split = source.split("\f").filter((page) => page.trim());
  return split.length ? split : [source];
}

function notebookRecord(payload: unknown): { cells: unknown[] } | null {
  const direct = object(payload);
  if (Array.isArray(direct.cells)) return { cells: direct.cells };
  const nested = object(direct.notebook);
  if (Array.isArray(nested.cells)) return { cells: nested.cells };
  const document = object(direct.document);
  return Array.isArray(document.cells) ? { cells: document.cells } : null;
}

function notebookCellTitle(row: Record<string, unknown>, type: string, number: number): string {
  const metadata = object(row.metadata);
  const configured = text(metadata.title) || text(metadata.cellTitle) || text(metadata.cell_title);
  if (configured) return configured;
  if (type === "markdown") {
    const heading = /^\s*#{1,6}\s+(.+)$/m.exec(joinedText(row.source))?.[1]?.trim();
    if (heading) return heading;
  }
  return `${type === "code" ? "Code" : type === "markdown" ? "Markdown" : "Raw"} cell ${number}`;
}

function notebookImage(data: Record<string, unknown>): string {
  for (const [mime, value] of [["image/png", data["image/png"]], ["image/jpeg", data["image/jpeg"]]] as const) {
    const base64 = joinedText(value).replaceAll(/\s/g, "");
    if (base64 && base64.length <= MAX_INLINE_MEDIA_BASE64_CHARS && /^[A-Za-z0-9+/]*={0,2}$/.test(base64)) return `data:${mime};base64,${base64}`;
  }
  const svg = joinedText(data["image/svg+xml"]);
  return svg && svg.length <= 40 * 1024 * 1024 ? `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}` : "";
}

function sandboxedNotebookHtml(html: string): string {
  const policy = "default-src 'none'; img-src data:; media-src data:; style-src 'unsafe-inline'; font-src data:";
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${policy}"><meta name="color-scheme" content="light dark"><style>html,body{margin:0;background:transparent;color:CanvasText;font:13px -apple-system,BlinkMacSystemFont,Segoe UI,sans-serif}body{padding:12px}img,svg,canvas,video{max-width:100%;height:auto}table{border-collapse:collapse}th,td{border:1px solid color-mix(in srgb,CanvasText 18%,transparent);padding:4px 6px}</style></head><body>${html}</body></html>`;
}

function prettyJson(value: unknown): string {
  try {
    if (typeof value === "string") {
      const parsed = JSON.parse(value);
      return JSON.stringify(parsed, null, 2);
    }
    return JSON.stringify(value, null, 2);
  } catch {
    return typeof value === "string" ? value : String(value);
  }
}

function object(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function array(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function joinedText(value: unknown): string {
  if (typeof value === "string") return value;
  return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string").join("") : "";
}

function cellKey(cell: unknown, index: number): string {
  return text(object(cell).id) || String(index);
}
