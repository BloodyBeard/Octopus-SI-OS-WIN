import type { AdmittedClientSurfaceDocument, ClientSurfaceDocument } from "../../client-surfaces/types";
import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import { Glyph } from "../IconGlyphs";
import {
  primaryWorkerEnvironmentSurface,
  workerEnvironmentGlyph,
  workerEnvironmentPanelKind,
} from "./surfaceRegistry";
import { primaryWorkerEnvironmentClientSurface } from "./workerEnvironmentPane";
import "./worker-environment.css";

type Props = {
  compact?: boolean;
  environment: WorkerEnvironmentProjection;
  onOpen?: () => void;
  surfaces?: AdmittedClientSurfaceDocument[];
};

export function WorkerEnvironmentPreview({ compact = false, environment, onOpen, surfaces = [] }: Props) {
  const projectedSurface = primaryWorkerEnvironmentSurface(environment);
  const projectedDocument = primaryWorkerEnvironmentClientSurface(environment.id, surfaces);
  const preferDocument = Boolean(projectedDocument) && !projectedSurface;
  const surface = preferDocument ? null : projectedSurface;
  const document = preferDocument ? projectedDocument : null;
  const grantedRoot = environment.root.trim();
  const body = surface ? previewBody(surface) : document ? clientSurfacePreviewBody(document.document) : grantedRoot ? {
    eyebrow: "Granted root",
    lines: [grantedRoot, ...environment.capabilities.slice(0, 2)],
    title: humanize(environment.kind),
  } : {
    eyebrow: "No client surface",
    lines: environment.capabilities.slice(0, 3),
    title: "Preview unavailable",
  };
  if (compact) {
    const meta = uniqueStrings([
      surface?.label ?? document?.document.title ?? humanize(environment.kind),
      surface?.protocol || (document ? `${document.document.render.kind} surface` : environment.transport),
    ]);
    const detail = body.lines[0] || environment.detail || body.eyebrow;
    return (
      <section className="we-preview-compact">
        <span className="we-preview-compact-icon">
          <Glyph name={surface ? workerEnvironmentGlyph(environment) : document?.document.glyph ?? "ee"} size={14} />
        </span>
        <span className="we-preview-compact-content">
          <span className="we-preview-compact-heading">
            <strong>{environment.label}</strong>
            <span className="we-preview-status"><i className={`is-${previewStatus(environment)}`} />{environment.status}</span>
          </span>
          <strong className="we-preview-compact-title">{body.title}</strong>
          {detail ? <span className="we-preview-compact-detail">{detail}</span> : null}
          {meta.length ? <span className="we-preview-compact-meta">{meta.join(" · ")}</span> : null}
        </span>
      </section>
    );
  }
  return (
    <section className="we-preview">
      <header className="we-preview-header">
        <span className="we-preview-identity">
          <Glyph name={surface ? workerEnvironmentGlyph(environment) : document?.document.glyph ?? "ee"} size={16} />
          <span>
            <strong>{environment.label}</strong>
            <small>{surface?.label ?? document?.document.title ?? environment.kind}</small>
          </span>
        </span>
        <span className="we-preview-status"><i className={`is-${previewStatus(environment)}`} />{environment.status}</span>
      </header>
      <div className={`we-preview-canvas is-${surface ? workerEnvironmentPanelKind(surface) : "empty"}`}>
        <span className="we-preview-eyebrow">{body.eyebrow}</span>
        <strong>{body.title}</strong>
        <div className="we-preview-lines">
          {body.lines.length ? body.lines.map((line, index) => <span key={`${line}:${index}`}>{line}</span>) : <span>No projected evidence</span>}
        </div>
      </div>
      <footer className="we-preview-footer">
        <span>{surface?.protocol || (document ? `${document.document.render.kind} client surface` : environment.transport || "descriptor")}</span>
        {onOpen ? (
          <button onClick={onOpen} type="button">
            Open
            <Glyph name="chevronDown" size={12} />
          </button>
        ) : null}
      </footer>
    </section>
  );
}

function clientSurfacePreviewBody(document: ClientSurfaceDocument): { eyebrow: string; lines: string[]; title: string } {
  const lines = Object.entries(document.data ?? {})
    .flatMap(([key, value]) => typeof value === "string" || typeof value === "number" || typeof value === "boolean"
      ? [`${humanize(key)} · ${String(value)}`]
      : [])
    .slice(0, 4);
  return {
    eyebrow: `${document.render.kind} · revision ${document.revision}`,
    lines,
    title: document.title,
  };
}

function previewBody(surface: WorkerEnvironmentSurfaceProjection): { eyebrow: string; lines: string[]; title: string } {
  const kind = workerEnvironmentPanelKind(surface);
  const data = surface.data;
  if (kind === "browser") {
    return {
      eyebrow: text(data, "url") || surface.viewerUrl || "Browser surface",
      lines: [text(data, "heading"), text(data, "detail") || text(data, "body")].filter(Boolean),
      title: text(data, "title") || surface.label,
    };
  }
  if (kind === "terminal") {
    return {
      eyebrow: surface.protocol || "Terminal",
      lines: stringList(data.lines).slice(-4),
      title: surface.label,
    };
  }
  if (kind === "files") {
    const entries = objectList(data.entries).map((entry) => text(entry, "path")).filter(Boolean);
    return { eyebrow: surface.root || "Read-only files", lines: entries.slice(0, 4), title: surface.label };
  }
  if (kind === "changes") {
    const files = stringList(data.files);
    const diffs = objectList(data.diffs).map((entry) => text(entry, "path")).filter(Boolean);
    return {
      eyebrow: [text(data, "base"), text(data, "branch")].filter(Boolean).join(" → ") || "Projected review",
      lines: [...files, ...diffs].slice(0, 4),
      title: text(data, "title") || surface.label,
    };
  }
  if (kind === "desktop") {
    return {
      eyebrow: surface.protocol || "Desktop viewer",
      lines: [text(data, "detail")].filter(Boolean),
      title: text(data, "title") || surface.label,
    };
  }
  const pairs = Object.entries(data)
    .flatMap(([key, value]) => typeof value === "string" || typeof value === "number" ? [`${humanize(key)} · ${value}`] : [])
    .slice(0, 4);
  return { eyebrow: surface.protocol || surface.kind, lines: pairs, title: surface.label };
}

function previewStatus(environment: WorkerEnvironmentProjection): string {
  const value = environment.status.toLowerCase();
  if (environment.live) return "active";
  if (["failed", "blocked", "error", "denied"].includes(value)) return "error";
  return "muted";
}

function text(value: Record<string, unknown>, key: string): string {
  const item = value[key];
  return typeof item === "string" ? item.trim() : typeof item === "number" ? String(item) : "";
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function objectList(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((item): item is Record<string, unknown> => Boolean(item) && typeof item === "object" && !Array.isArray(item))
    : [];
}

function humanize(value: string): string {
  return value.replace(/[_-]+/g, " ");
}

function uniqueStrings(values: Array<string | undefined>): string[] {
  return values
    .map((value) => value?.trim() ?? "")
    .filter((value, index, rows) => Boolean(value) && rows.indexOf(value) === index);
}
