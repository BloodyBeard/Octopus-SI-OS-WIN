import { OcLoadingRows } from "../../design-system/LoadingSkeleton";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { OcIconButton, OcMenuItem, OcMenuMessage } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { containModalFocus, isolateModalLayer, isTopmostModal, restoreModalFocus } from "../../design-system/modalFocus";
import { appToast } from "../../toasts";
import { Glyph } from "../IconGlyphs";
import { ArtifactPreview } from "./ArtifactPreview";
import { prepareArtifactContext } from "./artifactContext";
import {
  listArtifactFileOpenTargets,
  openArtifactFile,
  openArtifactFileWithApplication,
  type ArtifactFile,
} from "./artifactFileOperations";
import { resolveArtifactPayload } from "./artifactResolver";
import { createArtifactPreviewModel, previewDownload } from "./rendererRegistry";
import type { ArtifactReference, ResolvedArtifactPayload } from "./types";
import "./artifact-workbench.css";

type Props = {
  onClose?: () => void;
  presentation?: "embedded" | "overlay";
  reference: ArtifactReference;
};

type SelectionAction = {
  text: string;
  x: number;
  y: number;
};

export function ArtifactWorkbench({ onClose, presentation = "embedded", reference }: Props) {
  const [resolved, setResolved] = useState<ResolvedArtifactPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [zoomPercent, setZoomPercent] = useState(100);
  const [selection, setSelection] = useState<SelectionAction | null>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  const overlayRef = useRef<HTMLDivElement>(null);
  const previousFocus = useMemo(() => document.activeElement instanceof HTMLElement ? document.activeElement : null, [presentation]);

  useLayoutEffect(() => {
    const layer = overlayRef.current;
    if (presentation !== "overlay" || !layer) return;
    const release = isolateModalLayer(layer);
    layer.focus({ preventScroll: true });
    return () => { release(); restoreModalFocus(previousFocus); };
  }, [presentation, previousFocus]);

  useEffect(() => {
    let cancelled = false;
    setResolved(null);
    setLoading(true);
    setError("");
    setZoomPercent(100);
    setSelection(null);
    void resolveArtifactPayload(reference)
      .then((payload) => {
        if (!cancelled) setResolved(payload);
      })
      .catch((failure) => {
        if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => { cancelled = true; };
  }, [reference.artifactId, reference.inlinePayload, reference.interfaceId, reference.label, reference.runId, reference.source]);

  useEffect(() => {
    if (presentation !== "overlay" || !onClose) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented && isTopmostModal(overlayRef.current)) {
        event.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [onClose, presentation]);

  useEffect(() => {
    if (!selection) return;
    const clear = () => setSelection(null);
    window.addEventListener("resize", clear);
    window.addEventListener("scroll", clear, true);
    return () => {
      window.removeEventListener("resize", clear);
      window.removeEventListener("scroll", clear, true);
    };
  }, [selection]);

  const payload = resolved?.payload ?? reference.inlinePayload;
  const model = useMemo(() => createArtifactPreviewModel({
    ...reference,
    createdAtMs: resolved?.createdAtMs || reference.createdAtMs,
    interfaceId: resolved?.interfaceId || reference.interfaceId,
    runId: resolved?.runId || reference.runId,
  }, payload), [payload, reference, resolved]);
  const file = useMemo(() => previewDownload(model), [model]);

  const content = (
    <section className={`artifact-workbench is-${presentation}`} aria-label={`Preview ${model.title}`}>
      <header className="artifact-workbench-header">
        <div className="artifact-workbench-identity">
          <h2 title={model.title}>{model.title}</h2>
          <span>{rendererLabel(model.kind)}</span>
        </div>
        <div className="artifact-workbench-center">
          {zoomable(model.kind) ? <ArtifactZoomControl onChange={setZoomPercent} value={zoomPercent} /> : null}
        </div>
        <div className="artifact-workbench-actions">
          {file ? <ArtifactOpenControl file={file} key={`${file.filename}:${file.mediaType}`} /> : null}
          {onClose ? <OcIconButton glyph="close" label="Close preview" onClick={onClose} size="xs" variant="ghost" /> : null}
        </div>
      </header>

      <div
        className="artifact-workbench-content"
        onMouseUp={() => setSelection(selectionFromPreview(previewRef.current))}
        ref={previewRef}
      >
        {loading ? <WorkbenchState glyph="pulse" title="Preparing preview…" />
          : error ? <WorkbenchState detail={error} glyph="warning" title="This preview could not be loaded" />
            : <ArtifactPreview model={model} zoomPercent={zoomPercent} />}
      </div>

      {selection ? <SelectedTextAction model={model} reference={reference} selection={selection} onDone={() => setSelection(null)} /> : null}
    </section>
  );

  if (presentation !== "overlay") return content;
  return createPortal(
    <div aria-label={`Preview ${model.title}`} className="artifact-workbench-overlay" data-oc-dialog data-settings-layer="true" onKeyDown={(event) => containModalFocus(event, overlayRef.current)} ref={overlayRef} role="dialog" aria-modal="true" tabIndex={-1}>
      <button aria-label="Close preview" className="artifact-workbench-backdrop" onClick={onClose} tabIndex={-1} type="button" />
      {content}
    </div>,
    document.body,
  );
}

function ArtifactZoomControl({ onChange, value }: { onChange: (value: number) => void; value: number }) {
  const [open, setOpen] = useState(false);
  const anchorRef = useRef<HTMLButtonElement>(null);
  const options = [25, 50, 100, 150, 200];

  return (
    <div className="artifact-zoom-control">
      <button aria-expanded={open} aria-haspopup="menu" aria-label={`Zoom ${value}%`} onClick={() => setOpen((current) => !current)} ref={anchorRef} type="button">
        <span>{value}%</span>
        <Glyph name="chevronDown" size={12} />
      </button>
        <OcDropdown align="center" anchorRef={anchorRef} aria-label="Zoom" className="artifact-zoom-menu" onOpenChange={setOpen} open={open} sideOffset={4}>
          {options.map((option) => (
            <OcMenuItem
              key={option}
              label={`${option}%`}
              onClick={() => {
                onChange(option);
                setOpen(false);
              }}
              role="menuitemradio"
              aria-checked={option === value}
              trailing={option === value ? <Glyph name="check" size={13} /> : undefined}
            />
          ))}
        </OcDropdown>
    </div>
  );
}

function ArtifactOpenControl({ file }: { file: ArtifactFile }) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [pending, setPending] = useState<"open" | "reveal" | null>(null);
  const [targets, setTargets] = useState<Awaited<ReturnType<typeof listArtifactFileOpenTargets>> | null>(null);
  const [targetsLoading, setTargetsLoading] = useState(false);
  const [targetsError, setTargetsError] = useState("");
  const anchorRef = useRef<HTMLButtonElement>(null);
  const targetRequestRef = useRef<Promise<Awaited<ReturnType<typeof listArtifactFileOpenTargets>>> | null>(null);

  async function ensureTargets() {
    if (targets) return targets;
    if (targetRequestRef.current) return targetRequestRef.current;
    setTargetsLoading(true);
    setTargetsError("");
    const request = listArtifactFileOpenTargets(file);
    targetRequestRef.current = request;
    try {
      const resolved = await request;
      setTargets(resolved);
      return resolved;
    } catch (failure) {
      setTargetsError(failure instanceof Error ? failure.message : String(failure));
      throw failure;
    } finally {
      targetRequestRef.current = null;
      setTargetsLoading(false);
    }
  }

  async function openPrimary() {
    if (pending) return;
    setPending("open");
    setMenuOpen(false);
    try {
      const catalog = await ensureTargets();
      const preferred = catalog.applications.find((application) => application.id === catalog.preferredApplicationId);
      if (preferred) await openArtifactFileWithApplication(file, preferred.id);
      else await openArtifactFile(file, "reveal");
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(null);
    }
  }

  async function openWith(applicationId: string) {
    if (pending) return;
    setPending("open");
    setMenuOpen(false);
    try {
      await openArtifactFileWithApplication(file, applicationId);
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(null);
    }
  }

  async function reveal() {
    if (pending) return;
    setPending("reveal");
    setMenuOpen(false);
    try {
      await openArtifactFile(file, "reveal");
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(null);
    }
  }

  const preferred = targets?.applications.find((application) => application.id === targets.preferredApplicationId) ?? null;
  const primaryLabel = preferred ? `Open in ${preferred.label}` : "Open";

  return (
    <div className="artifact-file-actions">
      <div className="artifact-open-control">
        <button aria-busy={pending === "open"} aria-label={primaryLabel} className="artifact-open-primary" disabled={Boolean(pending)} onClick={() => void openPrimary()} title={primaryLabel} type="button">
          {pending === "open" ? <span className="composer-spinner size-3.5" /> : preferred?.icon ? <img alt="" className="size-4" draggable={false} src={preferred.icon} /> : null}
          {preferred ? null : <span>Open</span>}
        </button>
        <button
          aria-expanded={menuOpen}
          aria-haspopup="menu"
          aria-label="Open options"
          className="artifact-open-options"
          disabled={Boolean(pending)}
          onClick={() => setMenuOpen((value) => {
            const next = !value;
            if (next) void ensureTargets().catch(() => undefined);
            return next;
          })}
          onFocus={() => { void ensureTargets().catch(() => undefined); }}
          onMouseEnter={() => { void ensureTargets().catch(() => undefined); }}
          ref={anchorRef}
          type="button"
        >
          <Glyph name="chevronDown" size={13} />
        </button>
      </div>
        <OcDropdown align="end" anchorRef={anchorRef} aria-label="Open options" className="artifact-open-menu" onOpenChange={setMenuOpen} open={menuOpen} sideOffset={4}>
          {targets?.applications.map((application) => (
            <OcMenuItem
              disabled={Boolean(pending)}
              key={application.id}
              label={<ArtifactApplicationLabel icon={application.icon} label={application.label} />}
              onClick={() => void openWith(application.id)}
              role="menuitem"
            />
          ))}
          {targetsLoading ? <OcLoadingRows label="Loading available apps…" rows={2} /> : null}
          {targetsError && !targetsLoading ? <OcMenuMessage>{targetsError}</OcMenuMessage> : null}
          {targetsLoading || targetsError || targets?.applications.length ? <div className="my-1 h-px bg-token-border" /> : null}
          <OcMenuItem disabled={Boolean(pending)} label="Open in folder" onClick={() => void reveal()} role="menuitem" />
        </OcDropdown>
    </div>
  );
}

function ArtifactApplicationLabel({ icon, label }: { icon: string | null; label: string }) {
  return (
    <span className="flex min-w-0 items-center gap-1.5">
      {icon ? <img alt="" className="size-4 shrink-0" draggable={false} src={icon} /> : null}
      <span className="truncate">{label}</span>
    </span>
  );
}

function SelectedTextAction({ model, onDone, reference, selection }: {
  model: ReturnType<typeof createArtifactPreviewModel>;
  onDone: () => void;
  reference: ArtifactReference;
  selection: SelectionAction;
}) {
  return createPortal(
    <div className="pointer-events-none fixed z-[1200] -translate-x-1/2 -translate-y-[calc(100%+8px)]" data-settings-layer="true" style={{ left: selection.x, top: selection.y }}>
      <div className="pointer-events-auto flex w-fit max-w-full overflow-hidden rounded-lg border border-token-border/70 bg-token-dropdown-background/90 shadow-lg backdrop-blur-sm" onMouseDown={(event) => event.preventDefault()}>
        <button
          className="h-token-button-composer-sm cursor-interaction border-0 bg-transparent px-3 text-sm text-token-foreground hover:bg-token-list-hover-background"
          onClick={() => {
            window.getSelection()?.removeAllRanges();
            prepareArtifactContext({ model, reference, selectionText: selection.text });
            onDone();
          }}
          type="button"
        >
          Add to task
        </button>
      </div>
    </div>,
    document.body,
  );
}

function selectionFromPreview(root: HTMLDivElement | null): SelectionAction | null {
  const selection = window.getSelection();
  if (!root || !selection || selection.isCollapsed || selection.rangeCount === 0) return null;
  const range = selection.getRangeAt(0);
  if (!root.contains(range.commonAncestorContainer)) return null;
  const text = selection.toString().trim().slice(0, 8000);
  if (!text) return null;
  const rect = range.getBoundingClientRect();
  return { text, x: rect.left + rect.width / 2, y: rect.top };
}

function WorkbenchState({ detail, glyph, title }: { detail?: string; glyph: "pulse" | "warning"; title: string }) {
  return <div className="artifact-workbench-state"><Glyph name={glyph} size={24} /><strong>{title}</strong>{detail ? <span>{detail}</span> : null}</div>;
}

function zoomable(kind: ReturnType<typeof createArtifactPreviewModel>["kind"]): boolean {
  return kind === "document" || kind === "image";
}

function rendererLabel(kind: ReturnType<typeof createArtifactPreviewModel>["kind"]): string {
  const labels = {
    code: "Code",
    document: "Document",
    image: "Image",
    json: "JSON",
    media: "Media",
    notebook: "Notebook",
    pdf: "PDF",
    text: "Text",
    unsupported: "Artifact",
  };
  return labels[kind];
}
