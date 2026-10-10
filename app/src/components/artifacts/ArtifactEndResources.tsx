import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ChatRenderMessage } from "../chat/types";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { OcMenuItem, OcMenuMessage, OcMenuSurface } from "../../design-system/components";
import { appToast } from "../../toasts";
import { ArtifactWorkbench } from "./ArtifactWorkbench";
import {
  downloadArtifactFileCopy,
  listArtifactFileOpenTargets,
  openArtifactFile,
  openArtifactFileWithApplication,
  resolveArtifactFile,
  startArtifactFileDrag,
  type ArtifactFile,
} from "./artifactFileOperations";
import { artifactKindHint } from "./rendererRegistry";
import type { ArtifactReference, ArtifactRendererKind } from "./types";

export type ArtifactEndResource = {
  filename: string;
  kind: ArtifactRendererKind;
  reference: ArtifactReference;
};

export function ArtifactEndResources({ resources }: { resources: ArtifactEndResource[] }) {
  const [expanded, setExpanded] = useState(false);
  const unique = useMemo(() => {
    const seen = new Set<string>();
    return resources.filter((resource) => {
      if (seen.has(resource.reference.artifactId)) return false;
      seen.add(resource.reference.artifactId);
      return true;
    });
  }, [resources]);
  const visible = expanded ? unique : unique.slice(0, 3);
  const hidden = unique.length - visible.length;
  if (!unique.length) return null;
  return (
    <div className="mt-3 flex w-full max-w-[536px] flex-col self-start overflow-hidden rounded-2xl border border-token-border-default bg-token-main-surface-primary shadow-[0_2px_6px_-1px_rgba(0,0,0,0.03)] divide-y divide-token-border">
      {visible.map((resource) => <ArtifactEndResourceCard key={resource.reference.artifactId} resource={resource} />)}
      {hidden > 0 ? (
        <button className="cursor-interaction border-0 bg-transparent px-4 py-2.5 text-left text-sm text-token-text-secondary hover:bg-token-list-hover-background/30" onClick={() => setExpanded(true)} type="button">
          Show {hidden} more
        </button>
      ) : null}
    </div>
  );
}

export function artifactEndResourceFromMessage(message: ChatRenderMessage): ArtifactEndResource | null {
  if (!message.artifactId || message.role === "user" || message.eventKind !== "event") return null;
  const event = record(message.eventData);
  const media = record(event.media);
  const explicitFilename = firstText(
    media.name,
    event.file_name,
    event.filename,
    event.name,
    basename(firstText(media.path, event.path, event.file_path)),
  );
  const candidate = explicitFilename || message.label || "";
  const kind = artifactKindHint(candidate);
  if (kind === "unsupported") return null;
  return {
    filename: basename(candidate) || candidate,
    kind,
    reference: {
      artifactId: message.artifactId,
      createdAtMs: message.createdAtMs,
      interfaceId: message.interfaceId,
      label: message.label || candidate,
      manifest: {
        direction: message.direction,
        event_data: message.eventData,
        event_kind: message.eventKind,
        message_id: message.id,
      },
      runId: message.runId,
      source: "ledger",
    },
  };
}

function ArtifactEndResourceCard({ resource }: { resource: ArtifactEndResource }) {
  const [previewOpen, setPreviewOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [file, setFile] = useState<ArtifactFile | null>(null);
  const [fileLoading, setFileLoading] = useState(false);
  const [openTargets, setOpenTargets] = useState<Awaited<ReturnType<typeof listArtifactFileOpenTargets>> | null>(null);
  const [targetsLoading, setTargetsLoading] = useState(false);
  const [error, setError] = useState("");
  const [pending, setPending] = useState<"application" | "download" | "reveal" | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const targetRequestRef = useRef<Promise<Awaited<ReturnType<typeof listArtifactFileOpenTargets>>> | null>(null);
  const icon = resourceIcon(resource.kind);
  const subtitle = resourceSubtitle(resource.filename, resource.kind);

  async function ensureFile(): Promise<ArtifactFile> {
    if (file) return file;
    setFileLoading(true);
    setError("");
    try {
      const resolved = await resolveArtifactFile(resource.reference);
      setFile(resolved);
      return resolved;
    } catch (failure) {
      const message = failure instanceof Error ? failure.message : String(failure);
      setError(message);
      throw failure;
    } finally {
      setFileLoading(false);
    }
  }

  async function ensureOpenTargets() {
    if (openTargets) return openTargets;
    if (targetRequestRef.current) return targetRequestRef.current;
    setTargetsLoading(true);
    setError("");
    const request = ensureFile().then(listArtifactFileOpenTargets);
    targetRequestRef.current = request;
    try {
      const targets = await request;
      setOpenTargets(targets);
      return targets;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      throw failure;
    } finally {
      targetRequestRef.current = null;
      setTargetsLoading(false);
    }
  }

  async function act(action: "download" | "reveal") {
    if (pending) return;
    setPending(action);
    try {
      const resolved = await ensureFile();
      if (action === "download") {
        const outcome = await downloadArtifactFileCopy(resolved);
        if (outcome.saved) appToast.success("Saved a copy to Downloads");
      } else await openArtifactFile(resolved, action);
      setMenuOpen(false);
    } catch (failure) {
      appToast.danger(action === "download" ? "Could not download a copy" : failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(null);
    }
  }

  async function openWith(applicationId: string) {
    if (pending) return;
    setPending("application");
    try {
      await openArtifactFileWithApplication(await ensureFile(), applicationId);
      setMenuOpen(false);
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(null);
    }
  }

  return (
    <div className="group/end-resource relative flex min-h-16 w-full min-w-0 items-center gap-4 p-4">
      <button
        aria-label={`Open ${resource.filename}`}
        className="peer/end-resource absolute inset-0 cursor-interaction border-0 bg-transparent group-hover/end-resource:bg-token-list-hover-background/30 focus-visible:ring-1 focus-visible:ring-token-focus-border focus-visible:outline-none focus-visible:ring-inset"
        draggable={Boolean(window.octopusTransport?.startClientFileDrag)}
        onClick={() => setPreviewOpen(true)}
        onDragStart={(event) => {
          event.preventDefault();
          void ensureFile().then((resolved) => startArtifactFileDrag(resolved)).catch((failure) => {
            appToast.danger(failure instanceof Error ? failure.message : String(failure));
          });
        }}
        onPointerEnter={() => { if (!file && !fileLoading) void ensureFile().catch(() => undefined); }}
        type="button"
      />
      <span className="pointer-events-none relative z-10 flex size-10 shrink-0 items-center justify-center rounded-lg bg-token-bg-secondary text-token-text-secondary">
        <Glyph name={icon} size={24} />
      </span>
      <span className="pointer-events-none relative z-10 flex min-w-0 flex-1 flex-col">
        <span className="truncate text-base leading-5 text-token-text-primary">{resource.filename}</span>
        <span className="truncate text-sm leading-5 text-token-text-tertiary peer-hover/end-resource:[&_.end-resource-default-subtitle]:hidden peer-hover/end-resource:[&_.end-resource-hover-subtitle]:inline-flex">
          <span className="end-resource-default-subtitle">{subtitle}</span>
          <span className="end-resource-hover-subtitle hidden">Open preview</span>
        </span>
      </span>
      <button
        aria-expanded={menuOpen}
        aria-haspopup="menu"
        className="relative z-10 flex h-token-button-composer-sm cursor-interaction items-center gap-1 rounded-lg border border-token-border bg-transparent px-2 text-sm text-token-foreground hover:bg-token-list-hover-background data-[state=open]:bg-token-list-hover-background"
        data-state={menuOpen ? "open" : undefined}
        onClick={() => setMenuOpen((value) => {
          const next = !value;
          if (next) void ensureOpenTargets().catch(() => undefined);
          return next;
        })}
        onFocus={() => { void ensureOpenTargets().catch(() => undefined); }}
        onMouseEnter={() => { void ensureOpenTargets().catch(() => undefined); }}
        ref={triggerRef}
        type="button"
      >
        Open in <Glyph className="opacity-50" name="chevronDown" size={11} />
      </button>
      {menuOpen ? (
        <ArtifactEndResourceMenu
          error={error}
          loading={fileLoading || targetsLoading}
          onAction={(action) => void act(action)}
          onOpenWith={(applicationId) => void openWith(applicationId)}
          onClose={() => setMenuOpen(false)}
          openTargets={openTargets}
          pending={pending}
          trigger={triggerRef.current}
        />
      ) : null}
      {previewOpen ? <ArtifactWorkbench onClose={() => setPreviewOpen(false)} presentation="overlay" reference={resource.reference} /> : null}
    </div>
  );
}

function ArtifactEndResourceMenu({ error, loading, onAction, onClose, onOpenWith, openTargets, pending, trigger }: {
  error: string;
  loading: boolean;
  onAction: (action: "download" | "reveal") => void;
  onClose: () => void;
  onOpenWith: (applicationId: string) => void;
  openTargets: Awaited<ReturnType<typeof listArtifactFileOpenTargets>> | null;
  pending: "application" | "download" | "reveal" | null;
  trigger: HTMLButtonElement | null;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ left: 0, top: 0 });

  useEffect(() => {
    const update = () => {
      const rect = trigger?.getBoundingClientRect();
      if (!rect) return;
      setPosition({ left: Math.max(8, rect.right - 224), top: rect.bottom + 4 });
    };
    update();
    const dismiss = (event: PointerEvent) => {
      if (trigger?.contains(event.target as Node) || menuRef.current?.contains(event.target as Node)) return;
      onClose();
    };
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", keydown);
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", keydown);
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [onClose, trigger]);

  return createPortal(
    <OcMenuSurface aria-label="Open artifact" className="fixed z-100 w-56" role="menu" style={position} surfaceRef={menuRef}>
      {loading ? <OcMenuMessage>Loading available apps…</OcMenuMessage> : null}
      {error && !loading ? <OcMenuMessage>{error}</OcMenuMessage> : null}
      {openTargets?.applications.map((application) => (
        <OcMenuItem
          disabled={Boolean(pending)}
          key={application.id}
          label={<NativeOpenTargetLabel icon={application.icon} label={application.label} />}
          onClick={() => onOpenWith(application.id)}
          role="menuitem"
        />
      ))}
      {openTargets ? (
        <>
          {openTargets.applications.length ? <div className="my-1 h-px bg-token-border" /> : null}
          <OcMenuItem
            disabled={Boolean(pending)}
            label={<NativeOpenTargetLabel icon={openTargets.fileManagerIcon} label={revealLabel()} />}
            onClick={() => onAction("reveal")}
            role="menuitem"
          />
        </>
      ) : null}
      <div className="my-1 h-px bg-token-border" />
      <OcMenuItem
        disabled={Boolean(pending)}
        label={<NativeOpenTargetLabel icon={openTargets?.downloadsFolderIcon ?? null} label={pending === "download" ? "Downloading…" : "Download a copy"} />}
        onClick={() => onAction("download")}
        role="menuitem"
      />
    </OcMenuSurface>,
    document.body,
  );
}

function NativeOpenTargetLabel({ icon, label }: { icon: string | null; label: string }) {
  return (
    <span className="flex min-w-0 items-center gap-1.5">
      {icon ? <img alt="" className="size-4 shrink-0" draggable={false} src={icon} /> : null}
      <span className="truncate">{label}</span>
    </span>
  );
}

function resourceIcon(kind: ArtifactRendererKind): GlyphName {
  if (kind === "image") return "image";
  if (kind === "media") return "play";
  if (kind === "code" || kind === "notebook") return "terminal";
  return "file";
}

function resourceSubtitle(filename: string, kind: ArtifactRendererKind): string {
  const extension = fileExtension(filename).toUpperCase();
  if (kind === "document" || kind === "pdf" || ["MD", "MDX"].includes(extension)) return `Document · ${extension || "FILE"}`;
  if (["CSV", "TSV", "XLS", "XLSM", "XLSX"].includes(extension)) return `Spreadsheet · ${extension}`;
  if (["PPT", "PPTX"].includes(extension)) return `Slides · ${extension}`;
  if (kind === "image") return `Image · ${extension || "FILE"}`;
  return extension || "File";
}

function revealLabel(): string {
  if (/Mac|iPhone|iPad/.test(navigator.platform)) return "Show in Finder";
  if (/Win/.test(navigator.platform)) return "Show in File Explorer";
  return "Show in File Manager";
}

function fileExtension(value: string): string {
  const index = value.lastIndexOf(".");
  return index < 0 ? "" : value.slice(index + 1);
}

function basename(value: string): string {
  return value.split(/[\\/]/).filter(Boolean).at(-1) ?? "";
}

function firstText(...values: unknown[]): string {
  return values.find((value): value is string => typeof value === "string" && Boolean(value.trim()))?.trim() ?? "";
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
