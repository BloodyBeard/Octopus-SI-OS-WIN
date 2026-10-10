import { File as CodeFile } from "@pierre/diffs/react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useAppearance } from "../../appearance";
import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import type {
  OctopusWorkerEnvironmentFile,
  OctopusWorkerEnvironmentFileEntry,
  OctopusWorkerEnvironmentFilePreview,
} from "../../electron";
import { formatProjectionError } from "../../projectionClient";
import { clientDialogOwnsCommands } from "../../keyboardShortcuts";
import { copyText } from "../../runtimeFormat";
import { appToast } from "../../toasts";
import { OcButton, OcDialog, OcDialogFooter, OcDialogHeader, OcIconButton } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { ShellTooltip } from "../shell/ShellTooltip";
import "./filePreview.css";
import { octopusDiffRawUnsafeCss } from "../settings/octopusDiffUnsafeCss";
import { octopusDiffThemeName, registerOctopusDiffThemes } from "../settings/octopusDiffThemes";
import {
  OctopusFileTree,
  OctopusFilterField,
  OctopusPanelNote,
  octopusTreeAncestors,
} from "../OctopusSurface";
import {
  dispatchWorkerEnvironmentLocalMutation,
  subscribeWorkerEnvironmentLocalMutations,
} from "./workerEnvironmentLocalMutation";
import {
  LiveSurfaceEmpty,
  dataText,
} from "./RunEnvironmentSurfacePrimitives";

const diffThemeRegistrationKey = Symbol.for("octopus.diff-themes.registered");
const diffThemeRegistry = globalThis as typeof globalThis & { [diffThemeRegistrationKey]?: boolean };
if (!diffThemeRegistry[diffThemeRegistrationKey]) {
  registerOctopusDiffThemes();
  diffThemeRegistry[diffThemeRegistrationKey] = true;
}

const fileRendererUnsafeCss = `${octopusDiffRawUnsafeCss}
[data-file] {
  --diffs-font-family: var(--font-mono);
  --diffs-font-size: var(--code-font-size, 12px);
  --diffs-line-height: round(nearest, calc(var(--diffs-font-size, 12px) * 1.8), 1px);
  line-height: var(--diffs-line-height);
}`;

export function FilesSurface({ active, environment, onOpenFile, surface }: {
  active: boolean;
  environment: WorkerEnvironmentProjection;
  onOpenFile: (filePath: string, permanent?: boolean, surface?: WorkerEnvironmentSurfaceProjection) => void;
  surface?: WorkerEnvironmentSurfaceProjection;
}) {
  if (surface && isDirectFilesystemSurface(surface)) {
    return <LiveFilesSurface active={active} environment={environment} onOpenFile={onOpenFile} surface={surface} />;
  }
  if (!surface || (surface.mode !== "fixture" && surface.mode !== "evidence")) {
    return (
      <div className="flex h-full min-h-0 w-full flex-col bg-token-main-surface-primary">
        <LiveSurfaceEmpty
          detail="Files appear when this room projects a bounded filesystem."
          glyph="folder"
          title="No files yet"
        />
      </div>
    );
  }
  return <ProjectedFilesSurface onOpenFile={onOpenFile} surface={surface} />;
}

function LiveFilesSurface({ active, environment, onOpenFile, surface }: {
  active: boolean;
  environment: WorkerEnvironmentProjection;
  onOpenFile: (filePath: string, permanent?: boolean, surface?: WorkerEnvironmentSurfaceProjection) => void;
  surface: WorkerEnvironmentSurfaceProjection;
}) {
  const [directories, setDirectories] = useState<Record<string, OctopusWorkerEnvironmentFileEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [loading, setLoading] = useState<Set<string>>(() => new Set());
  const [query, setQuery] = useState("");
  const [selectedPath, setSelectedPath] = useState("");
  const [error, setError] = useState("");
  const [indexState, setIndexState] = useState<"idle" | "indexing" | "complete" | "bounded" | "error">("idle");
  const [indexedEntries, setIndexedEntries] = useState(0);
  const [searchEntries, setSearchEntries] = useState<OctopusWorkerEnvironmentFileEntry[]>([]);
  const rootRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const requestGenerationRef = useRef(0);
  const filterInputId = useId();
  const lastEnvironmentRevisionRef = useRef(environment.revision);
  const dirtyWhileInactiveRef = useRef(false);

  async function loadDirectory(relativePath: string, force = false) {
    if (!force && Object.prototype.hasOwnProperty.call(directories, relativePath)) return;
    const generation = requestGenerationRef.current;
    setLoading((current) => new Set(current).add(relativePath));
    setError("");
    try {
      const entries = await loadDirectFilesystemDirectory(environment.id, surface.id, relativePath);
      if (generation !== requestGenerationRef.current) return;
      setDirectories((current) => ({ ...current, [relativePath]: entries }));
    } catch (failure) {
      if (generation === requestGenerationRef.current) setError(formatProjectionError(failure));
    } finally {
      if (generation === requestGenerationRef.current) {
        setLoading((current) => {
          const next = new Set(current);
          next.delete(relativePath);
          return next;
        });
      }
    }
  }

  useEffect(() => {
    const generation = requestGenerationRef.current + 1;
    requestGenerationRef.current = generation;
    let cancelled = false;
    setDirectories({});
    setExpanded(new Set());
    setLoading(new Set());
    setSelectedPath("");
    setError("");
    setIndexState("idle");
    setIndexedEntries(0);
    setSearchEntries([]);
    lastEnvironmentRevisionRef.current = environment.revision;
    if (!active) {
      dirtyWhileInactiveRef.current = true;
      return;
    }
    dirtyWhileInactiveRef.current = false;
    void loadDirectFilesystemDirectory(environment.id, surface.id, "")
      .then((entries) => {
        if (!cancelled && generation === requestGenerationRef.current) setDirectories({ "": entries });
      })
      .catch((failure) => {
        if (!cancelled && generation === requestGenerationRef.current) setError(formatProjectionError(failure));
      });
    return () => { cancelled = true; };
  }, [environment.id, surface.id, surface.root]);

  useEffect(() => {
    const handleQuickOpen = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || !active || clientDialogOwnsCommands()) return;
      if (!event.metaKey || event.altKey || event.ctrlKey || event.shiftKey || event.key.toLowerCase() !== "p") return;
      const root = rootRef.current;
      const owner = root?.closest('.run-environment-view');
      if (!owner || !(event.target instanceof Element) || event.target.closest('.run-environment-view') !== owner) return;
      if (!root?.offsetParent || getComputedStyle(root).visibility !== "visible") return;
      event.preventDefault();
      searchInputRef.current?.focus();
      searchInputRef.current?.select();
    };
    window.addEventListener("keydown", handleQuickOpen);
    return () => window.removeEventListener("keydown", handleQuickOpen);
  }, [active]);

  const normalizedQuery = query.trim().toLowerCase();
  const loadedEntries = Object.values(directories).flat().filter((entry, index, all) => (
    all.findIndex((candidate) => candidate.path === entry.path) === index
  ));
  const searchUniverse = searchEntries.length || indexState !== "idle" ? searchEntries : loadedEntries;
  const matchedEntries = normalizedQuery
    ? searchUniverse
      .filter((entry) => `${entry.name} ${entry.path}`.toLowerCase().includes(normalizedQuery))
      .sort((left, right) => left.path.localeCompare(right.path))
    : [];
  const treePaths = useMemo(() => (
    [...new Set(loadedEntries.map((entry) => entry.kind === "directory" ? `${entry.path}/` : entry.path))].sort()
  ), [directories]);
  const searchTreePaths = useMemo(() => (
    [...new Set(matchedEntries.map((entry) => entry.kind === "directory" ? `${entry.path}/` : entry.path))]
  ), [normalizedQuery, searchUniverse]);
  const searchExpanded = useMemo(() => octopusTreeAncestors(searchTreePaths), [searchTreePaths]);
  const knownDirectories = useMemo(() => (
    loadedEntries.filter((entry) => entry.kind === "directory").map((entry) => entry.path)
  ), [directories]);

  useEffect(() => {
    if (!active || !normalizedQuery || indexState !== "idle") return;
    void indexWorkspace();
  }, [active, normalizedQuery, indexState]);

  const syncExpandedDirectories = useCallback((model: { getItem(path: string): { isDirectory(): boolean; isExpanded?: () => boolean } | null }) => {
    for (const directoryPath of knownDirectories) {
      const item = model.getItem(directoryPath);
      if (!item?.isDirectory() || !item.isExpanded?.()) continue;
      if (!expanded.has(directoryPath)) {
        setExpanded((current) => new Set(current).add(directoryPath));
        void loadDirectory(directoryPath);
      }
    }
  }, [expanded, knownDirectories]);

  async function refresh() {
    requestGenerationRef.current += 1;
    const generation = requestGenerationRef.current;
    const paths = ["", ...expanded];
    setLoading(new Set(paths));
    setError("");
    setIndexState("idle");
    setIndexedEntries(0);
    setSearchEntries([]);
    try {
      const results = await Promise.all(paths.map(async (relativePath) => ({
        relativePath,
        entries: await loadDirectFilesystemDirectory(environment.id, surface.id, relativePath),
      })));
      if (generation !== requestGenerationRef.current) return;
      setDirectories(Object.fromEntries(results.map((result) => [result.relativePath, result.entries])));
    } catch (failure) {
      if (generation === requestGenerationRef.current) setError(formatProjectionError(failure));
    } finally {
      if (generation === requestGenerationRef.current) setLoading(new Set());
    }
  }

  useEffect(() => {
    if (!active || lastEnvironmentRevisionRef.current === environment.revision) return;
    lastEnvironmentRevisionRef.current = environment.revision;
    const timer = window.setTimeout(() => void refresh(), 160);
    return () => window.clearTimeout(timer);
  }, [active, environment.revision]);

  useEffect(() => {
    if (!active || !dirtyWhileInactiveRef.current) return;
    dirtyWhileInactiveRef.current = false;
    void refresh();
  }, [active]);

  useEffect(() => {
    if (active) return;
    dirtyWhileInactiveRef.current = true;
    requestGenerationRef.current += 1;
    setLoading(new Set());
    setIndexState((current) => current === "indexing" ? "idle" : current);
  }, [active]);

  useEffect(() => {
    return subscribeWorkerEnvironmentLocalMutations((mutation) => {
      if (mutation.workerEnvironmentId !== environment.id) return;
      if (mutation.operation === "git.stage" || mutation.operation === "git.unstage") return;
      if (active) void refresh();
      else dirtyWhileInactiveRef.current = true;
    });
  }, [active, environment.id, expanded, surface.id]);

  async function indexWorkspace() {
    const transport = window.octopusTransport;
    if (!transport?.listWorkerEnvironmentFiles) {
      setIndexState("error");
      return;
    }
    const generation = requestGenerationRef.current;
    const queue = [""];
    const discovered: OctopusWorkerEnvironmentFileEntry[] = [];
    const visited = new Set<string>();
    let count = 0;
    let truncated = false;
    const limit = 4_000;
    setIndexState("indexing");
    setIndexedEntries(0);
    setSearchEntries([]);
    try {
      while (queue.length && count < limit) {
        const batch = queue.splice(0, 8).filter((path) => !visited.has(path));
        batch.forEach((path) => visited.add(path));
        const results = await Promise.all(batch.map(async (path) => ({
          entries: await loadDirectFilesystemDirectory(environment.id, surface.id, path),
          path,
        })));
        if (generation !== requestGenerationRef.current) return;
        for (const result of results) {
          const remaining = Math.max(0, limit - count);
          const accepted = result.entries.slice(0, remaining);
          if (accepted.length < result.entries.length) truncated = true;
          discovered.push(...accepted);
          count += accepted.length;
          for (const entry of accepted) {
            if (entry.kind === "directory" && count < limit) queue.push(entry.path);
          }
        }
        setIndexedEntries(count);
        setSearchEntries([...discovered]);
      }
      if (generation !== requestGenerationRef.current) return;
      setIndexState(queue.length || truncated ? "bounded" : "complete");
    } catch (failure) {
      if (generation !== requestGenerationRef.current) return;
      setIndexState("error");
      setError(formatProjectionError(failure));
    }
  }

  const searching = Boolean(normalizedQuery);
  const rootLoading = loading.has("");
  return (
    <div className="flex h-full min-h-0 w-full flex-col bg-token-main-surface-primary" ref={rootRef}>
      <div className="shrink-0 px-2 pt-2 pb-px">
        <FilesRootIdentity surface={surface} />
        <OctopusFilterField
          inputId={filterInputId}
          inputRef={searchInputRef}
          label="Filter files"
          onQueryChange={setQuery}
          placeholder="Filter files…"
          searchQuery={query}
          trailing={(
            <button
              aria-label="Refresh files"
              className="flex size-7 shrink-0 cursor-interaction items-center justify-center rounded-lg text-token-input-placeholder-foreground outline-none hover:text-token-foreground"
              onClick={() => void refresh()}
              type="button"
            >
              <Glyph className="icon-2xs" name="refresh" size={13} />
            </button>
          )}
        />
      </div>
      <div className="min-h-0 flex-1">
        {error ? (
          <div className="px-3 py-2 text-xs text-token-error-foreground">
            {error}
            <button
              className="ms-2 cursor-interaction text-token-text-secondary underline-offset-2 hover:underline"
              onClick={() => void refresh()}
              type="button"
            >Retry</button>
          </div>
        ) : searching ? (
          indexState === "indexing" && !searchTreePaths.length ? (
            <OctopusPanelNote>Searching files…</OctopusPanelNote>
          ) : !searchTreePaths.length ? (
            <OctopusPanelNote>No matching files</OctopusPanelNote>
          ) : (
            <div className="flex h-full min-h-0 flex-col">
              {indexState === "bounded" || indexState === "error" ? (
                <OctopusPanelNote>{indexState === "bounded" ? `Showing the first ${indexedEntries} indexed entries` : "Workspace index failed"}</OctopusPanelNote>
              ) : null}
              <div className="min-h-0 flex-1">
                <OctopusFileTree
                  flattenEmptyDirectories
                  initialExpandedPaths={searchExpanded}
                  onOpenPath={(path) => onOpenFile(path, true, surface)}
                  onSelectPath={(path) => {
                    setSelectedPath(path);
                    onOpenFile(path, false, surface);
                  }}
                  paths={searchTreePaths}
                  resetKey={normalizedQuery}
                />
              </div>
            </div>
          )
        ) : rootLoading && !treePaths.length ? (
          <OctopusPanelNote>Loading directory entries…</OctopusPanelNote>
        ) : !treePaths.length ? (
          <OctopusPanelNote>No files in this folder</OctopusPanelNote>
        ) : (
          <OctopusFileTree
            initialExpandedPaths={[...expanded]}
            initialSelectedPaths={selectedPath ? [selectedPath] : undefined}
            onModelEvent={syncExpandedDirectories}
            onOpenPath={(path) => onOpenFile(path, true, surface)}
            onSelectPath={(path) => {
              setSelectedPath(path);
              onOpenFile(path, false, surface);
            }}
            paths={treePaths}
          />
        )}
      </div>
    </div>
  );
}

function ProjectedFilesSurface({ onOpenFile, surface }: {
  onOpenFile: (filePath: string, permanent?: boolean, surface?: WorkerEnvironmentSurfaceProjection) => void;
  surface: WorkerEnvironmentSurfaceProjection;
}) {
  const entries = fixtureFileEntries(surface);
  const filterInputId = useId();
  const [query, setQuery] = useState("");
  const [selectedPath, setSelectedPath] = useState("");
  const normalizedQuery = query.trim().toLowerCase();
  const allPaths = useMemo(() => (
    [...new Set(entries.map((entry) => entry.kind === "directory" ? `${entry.path}/` : entry.path))].sort()
  ), [surface.id, surface.data]);
  const matchedPaths = normalizedQuery
    ? allPaths.filter((path) => path.toLowerCase().includes(normalizedQuery))
    : allPaths;
  const searchExpanded = useMemo(() => octopusTreeAncestors(matchedPaths), [matchedPaths]);
  return (
    <div className="flex h-full min-h-0 w-full flex-col bg-token-main-surface-primary">
      <div className="shrink-0 px-2 pt-2 pb-px">
        <FilesRootIdentity surface={surface} />
        <OctopusFilterField
          inputId={filterInputId}
          label="Filter files"
          onQueryChange={setQuery}
          placeholder="Filter files…"
          searchQuery={query}
        />
      </div>
      <div className="min-h-0 flex-1">
        {!matchedPaths.length ? (
          <OctopusPanelNote>{entries.length ? "No matching files" : "No files in this folder"}</OctopusPanelNote>
        ) : (
          <OctopusFileTree
            flattenEmptyDirectories={Boolean(normalizedQuery)}
            initialExpandedPaths={normalizedQuery ? searchExpanded : undefined}
            initialSelectedPaths={selectedPath ? [selectedPath] : undefined}
            onOpenPath={(path) => onOpenFile(path, true, surface)}
            onSelectPath={(path) => {
              setSelectedPath(path);
              onOpenFile(path, false, surface);
            }}
            paths={matchedPaths}
            resetKey={normalizedQuery}
          />
        )}
      </div>
    </div>
  );
}

function FilesRootIdentity({ surface }: { surface: WorkerEnvironmentSurfaceProjection }) {
  const label = surface.label.trim() || "Files";
  const root = surface.root.trim();
  return (
    <div className="flex min-w-0 items-center gap-1.5 px-1 pb-1.5 text-xs text-token-text-secondary">
      <Glyph className="shrink-0" name="folder" size={13} />
      <span className="shrink-0 font-medium text-token-foreground">{label}</span>
      {root ? <span className="min-w-0 truncate font-mono" title={root}>{root}</span> : null}
    </div>
  );
}

export function FilePreviewSurface({ active, environment, path, surface }: {
  active: boolean;
  environment: WorkerEnvironmentProjection;
  path: string;
  surface?: WorkerEnvironmentSurfaceProjection;
}) {
  const appearance = useAppearance();
  const projectedFile = surface && (surface.mode === "fixture" || surface.mode === "evidence")
    ? fixtureFileEntries(surface).find((entry) => entry.kind === "file" && entry.path === path) ?? null
    : null;
  const [liveFile, setLiveFile] = useState<OctopusWorkerEnvironmentFile | null>(null);
  const [liveState, setLiveState] = useState<"idle" | "loading" | "error">("idle");
  const [liveError, setLiveError] = useState("");
  const [binaryPreview, setBinaryPreview] = useState<(OctopusWorkerEnvironmentFilePreview & { url: string }) | null>(null);
  const [binaryPreviewState, setBinaryPreviewState] = useState<"idle" | "loading" | "error">("idle");
  const [binaryPreviewError, setBinaryPreviewError] = useState("");
  const [binaryPreviewGeneration, setBinaryPreviewGeneration] = useState(0);
  const [editMode, setEditMode] = useState(false);
  const [editDraft, setEditDraft] = useState("");
  const [editWarningOpen, setEditWarningOpen] = useState(false);
  const [saveState, setSaveState] = useState<"idle" | "saving" | "error">("idle");
  const [saveError, setSaveError] = useState("");
  const [fileReadGeneration, setFileReadGeneration] = useState(0);
  const fileMutationOriginIdRef = useRef(crypto.randomUUID());
  const lastFileEnvironmentRevisionRef = useRef(environment.revision);
  const direct = Boolean(surface && isDirectFilesystemSurface(surface));

  useEffect(() => {
    if (!direct || !surface) {
      setLiveFile(null);
      setLiveState("idle");
      setLiveError("");
      return;
    }
    if (!active) return;
    const transport = window.octopusTransport;
    if (!transport?.readWorkerEnvironmentFile) {
      setLiveFile(null);
      setLiveState("error");
      setLiveError("Direct filesystem viewing is available only in the Octopus desktop client.");
      return;
    }
    let cancelled = false;
    setLiveState("loading");
    setLiveError("");
    void transport.readWorkerEnvironmentFile({ workerEnvironmentId: environment.id, surfaceId: surface.id, path })
      .then((result) => {
        if (cancelled) return;
        assertWorkerEnvironmentFileResponse(result, environment.id, surface.id, path);
        setLiveFile(result);
        setLiveState("idle");
      })
      .catch((failure) => {
        if (cancelled) return;
        setLiveFile(null);
        setLiveState("error");
        setLiveError(formatProjectionError(failure));
      });
    return () => { cancelled = true; };
  }, [active, direct, environment.id, fileReadGeneration, path, surface?.id, surface?.root]);

  useEffect(() => {
    lastFileEnvironmentRevisionRef.current = environment.revision;
    setEditMode(false);
    setEditDraft("");
    setEditWarningOpen(false);
    setSaveState("idle");
    setSaveError("");
  }, [environment.id, path, surface?.id]);

  useEffect(() => {
    if (!active || editMode || lastFileEnvironmentRevisionRef.current === environment.revision) return;
    lastFileEnvironmentRevisionRef.current = environment.revision;
    const timer = window.setTimeout(() => setFileReadGeneration((generation) => generation + 1), 160);
    return () => window.clearTimeout(timer);
  }, [active, editMode, environment.revision]);

  useEffect(() => {
    if (!active) return;
    return subscribeWorkerEnvironmentLocalMutations((mutation) => {
      if (mutation.workerEnvironmentId !== environment.id || mutation.originId === fileMutationOriginIdRef.current) return;
      if (mutation.operation === "git.stage" || mutation.operation === "git.unstage") return;
      setFileReadGeneration((generation) => generation + 1);
    });
  }, [active, environment.id]);

  useEffect(() => {
    setBinaryPreview(null);
    setBinaryPreviewError("");
    if (!active || !direct || !surface || liveFile?.kind !== "binary" || !isBinaryPreviewPath(path)) {
      setBinaryPreviewState("idle");
      return;
    }
    const transport = window.octopusTransport;
    if (!transport?.readWorkerEnvironmentFilePreview) {
      setBinaryPreviewState("error");
      setBinaryPreviewError("Binary preview is available only in the Octopus desktop client.");
      return;
    }
    let cancelled = false;
    let objectUrl = "";
    setBinaryPreviewState("loading");
    void transport.readWorkerEnvironmentFilePreview({ workerEnvironmentId: environment.id, surfaceId: surface.id, path })
      .then((result) => {
        assertWorkerEnvironmentFileResponse(result, environment.id, surface.id, path);
        objectUrl = URL.createObjectURL(new Blob([result.bytes], { type: result.mimeType }));
        if (cancelled) {
          URL.revokeObjectURL(objectUrl);
          return;
        }
        setBinaryPreview({ ...result, url: objectUrl });
        setBinaryPreviewState("idle");
      })
      .catch((failure) => {
        if (cancelled) return;
        setBinaryPreviewState("error");
        setBinaryPreviewError(formatProjectionError(failure));
      });
    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [active, binaryPreviewGeneration, direct, environment.id, liveFile?.kind, liveFile?.modifiedAt, path, surface?.id]);

  const file = projectedFile ?? (liveFile?.kind === "text" ? { path: liveFile.path, kind: "file" as const, text: liveFile.text } : null);
  const editable = Boolean(
    direct
    && surface
    && !surface.readOnly
    && surface.protocol.toLowerCase() === "filesystem.readwrite"
    && liveFile?.kind === "text"
    && !liveFile.truncated
    && liveFile.digest,
  );

  const pathParts = path.split(/[\\/]/).filter(Boolean);
  const fileRendererOptions = useMemo(() => ({
    disableFileHeader: true,
    overflow: "scroll" as const,
    theme: {
      dark: octopusDiffThemeName("dark", appearance.darkCodeTheme),
      light: octopusDiffThemeName("light", appearance.lightCodeTheme),
    },
    themeType: appearance.themeMode,
    unsafeCSS: fileRendererUnsafeCss,
  }), [appearance.darkCodeTheme, appearance.lightCodeTheme, appearance.themeMode]);

  async function performFileSystemAction(action: "resolve" | "reveal" | "open") {
    if (!direct || !surface) return;
    const transport = window.octopusTransport;
    if (!transport?.performWorkerEnvironmentFileSystemAction) {
      appToast.warning("File system actions are available only in the Octopus desktop client.");
      return;
    }
    try {
      const outcome = await transport.performWorkerEnvironmentFileSystemAction({
        action,
        path,
        surfaceId: surface.id,
        workerEnvironmentId: environment.id,
      });
      assertWorkerEnvironmentFileResponse(outcome, environment.id, surface.id, path);
      if (outcome.action !== action) throw new Error("File system action receipt does not match the requested action.");
      if (action === "resolve") {
        if (!await copyText(outcome.absolutePath)) throw new Error("Clipboard access is unavailable.");
        appToast.success("File path copied");
      }
    } catch (failure) {
      appToast.warning(formatProjectionError(failure));
    }
  }

  function beginManualEdit() {
    if (!editable || !liveFile) return;
    setEditWarningOpen(true);
  }

  function enterManualEdit() {
    if (!editable || !liveFile) return;
    setEditDraft(liveFile.text);
    setSaveState("idle");
    setSaveError("");
    setEditMode(true);
    setEditWarningOpen(false);
  }

  function cancelManualEdit() {
    setEditDraft(liveFile?.kind === "text" ? liveFile.text : "");
    setSaveState("idle");
    setSaveError("");
    setEditMode(false);
  }

  async function saveManualEdit() {
    if (!editable || !surface || !liveFile || saveState === "saving" || editDraft === liveFile.text) return;
    const transport = window.octopusTransport;
    if (!transport?.writeWorkerEnvironmentFile) {
      setSaveState("error");
      setSaveError("Manual file editing is available only in the Octopus desktop client.");
      return;
    }
    setSaveState("saving");
    setSaveError("");
    try {
      const result = await transport.writeWorkerEnvironmentFile({
        workerEnvironmentId: environment.id,
        surfaceId: surface.id,
        path,
        text: editDraft,
        expectedDigest: liveFile.digest,
        expectedModifiedAt: liveFile.modifiedAt,
        expectedSize: liveFile.size,
      });
      assertWorkerEnvironmentFileResponse(result, environment.id, surface.id, path);
      if (!result.outsideOctopusEvidence) throw new Error("Manual file write returned an invalid boundary receipt.");
      setLiveFile(result);
      setEditDraft(result.text);
      setEditMode(false);
      setSaveState("idle");
      dispatchWorkerEnvironmentLocalMutation({
        operation: "file.write",
        originId: fileMutationOriginIdRef.current,
        paths: [path],
        surfaceId: surface.id,
        workerEnvironmentId: environment.id,
      });
      appToast.success("File saved outside Octopus evidence");
    } catch (failure) {
      const message = formatProjectionError(failure);
      setSaveState("error");
      setSaveError(message);
      appToast.warning(message);
    }
  }

  return (
    <>
    <div className="we-file-preview">
      <header className="we-file-preview-toolbar">
        <nav aria-label="File path" className="we-file-preview-breadcrumb" title={path}>
          <ol>
            {pathParts.map((part, index) => (
              <li key={`${index}:${part}`}>
                <span aria-current={index === pathParts.length - 1 ? "page" : undefined} className={index === pathParts.length - 1 ? "is-current" : ""}>{part}</span>
                {index < pathParts.length - 1 ? <Glyph className="-rotate-90" name="chevronDown" size={10} /> : null}
              </li>
            ))}
          </ol>
        </nav>
        {direct ? (
          <div aria-label="File actions" className="we-file-preview-actions" role="group">
            {editable && !editMode ? <FileActionButton glyph="edit" label="Edit file manually" onClick={beginManualEdit} /> : null}
            {editMode ? (
              <span className="we-file-edit-actions">
                {saveState === "error" ? <small title={saveError}>Save failed</small> : editDraft !== liveFile?.text ? <small>Unsaved</small> : null}
                <OcButton disabled={saveState === "saving"} onClick={cancelManualEdit} size="sm" variant="secondary">Cancel</OcButton>
                <OcButton disabled={saveState === "saving" || editDraft === liveFile?.text} onClick={() => void saveManualEdit()} size="sm" variant="primary">{saveState === "saving" ? "Saving…" : "Save"}</OcButton>
              </span>
            ) : null}
            <FileActionButton glyph="copy" label="Copy absolute path" onClick={() => void performFileSystemAction("resolve")} />
            <FileActionButton glyph="folderOpen" label="Reveal in file manager" onClick={() => void performFileSystemAction("reveal")} />
            <FileActionButton glyph="openExternal" label="Open in default app" onClick={() => void performFileSystemAction("open")} />
          </div>
        ) : null}
      </header>
      <div className="we-file-preview-content">
        {editMode ? (
          <textarea
            aria-label={`Edit ${path}`}
            autoFocus
            className="we-file-text-editor"
            onChange={(event) => {
              setEditDraft(event.currentTarget.value);
              if (saveState === "error") {
                setSaveState("idle");
                setSaveError("");
              }
            }}
            onKeyDown={(event) => {
              if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === "s") {
                event.preventDefault();
                void saveManualEdit();
              }
              if (event.key === "Escape") {
                event.preventDefault();
                cancelManualEdit();
              }
            }}
            spellCheck={false}
            value={editDraft}
          />
        ) : liveState === "loading" ? (
          <div className="we-file-preview-state">Reading projected file…</div>
        ) : liveState === "error" ? (
          <div className="we-file-preview-state is-error">{liveError}</div>
        ) : binaryPreviewState === "loading" ? (
          <div className="we-file-preview-state">Loading bounded binary preview…</div>
        ) : binaryPreviewState === "error" ? (
          <div className="we-file-unsupported" role="alert">
            <strong>Preview could not be loaded</strong>
            <small>{binaryPreviewError}</small>
            <OcButton onClick={() => setBinaryPreviewGeneration((generation) => generation + 1)} size="sm" variant="secondary">Retry</OcButton>
          </div>
        ) : binaryPreview?.mimeType === "application/pdf" ? (
          <object aria-label={`PDF preview of ${binaryPreview.name}`} className="we-file-binary-preview is-pdf" data={binaryPreview.url} type={binaryPreview.mimeType}>
            <div className="we-file-unsupported"><strong>PDF preview unavailable</strong><small>{formatFileSize(binaryPreview.size)} · read only</small></div>
          </object>
        ) : binaryPreview ? (
          <div className="we-file-binary-preview is-image">
            <img alt={binaryPreview.name} src={binaryPreview.url} />
          </div>
        ) : liveFile?.kind === "binary" ? (
          <div className="we-file-unsupported"><strong>Binary preview unavailable</strong><small>{formatFileSize(liveFile.size)} · read only</small></div>
        ) : file ? (
          <article className="we-file-document">
            {liveFile?.truncated ? <div className="we-file-truncated-note">Showing a bounded text preview of {formatFileSize(liveFile.size)}</div> : null}
            <CodeFile
              className="we-file-code"
              file={{ contents: file.text || " ", name: path }}
              options={fileRendererOptions}
            />
          </article>
        ) : (
          <div className="we-file-preview-state">This file has no projected text snapshot.</div>
        )}
      </div>
    </div>
    <ManualFileEditWarning
      onCancel={() => setEditWarningOpen(false)}
      onContinue={enterManualEdit}
      open={editWarningOpen}
    />
    </>
  );
}

function FileActionButton({ glyph, label, onClick }: { glyph: GlyphName; label: string; onClick: () => void }) {
  return <ShellTooltip label={label}>
    <OcIconButton appearance="compact" glyph={glyph} label={label} onClick={onClick} title="" />
  </ShellTooltip>;
}

function ManualFileEditWarning({ onCancel, onContinue, open }: {
  onCancel: () => void;
  onContinue: () => void;
  open: boolean;
}) {
  return (
    <OcDialog
      ariaLabelledBy="we-manual-file-edit-title"
      backdropClassName="we-manual-control-scrim"
      onClose={onCancel}
      open={open}
      panelClassName="we-manual-control-dialog !static !transform-none"
    >
        <div className="we-manual-control-dialog__icon"><Glyph name="file" size={20} /></div>
        <OcDialogHeader
          className="we-manual-control-dialog__copy p-0"
          description="The file changes on disk immediately when you save. This operator action is not recorded as an Octopus artifact, movement, mark, or model action."
          title="Edit this room file manually?"
          titleId="we-manual-file-edit-title"
        />
        <OcDialogFooter className="we-manual-control-dialog__actions">
          <OcButton onClick={onCancel} variant="secondary">Cancel</OcButton>
          <OcButton onClick={onContinue} variant="primary">Continue manually</OcButton>
        </OcDialogFooter>
    </OcDialog>
  );
}

function isDirectFilesystemSurface(surface: WorkerEnvironmentSurfaceProjection): boolean {
  return surface.mode === "direct"
    && (
      surface.protocol.toLowerCase() === "filesystem.readonly" && surface.readOnly
      || surface.protocol.toLowerCase() === "filesystem.readwrite" && !surface.readOnly
    )
    && Boolean(surface.root);
}

function isBinaryPreviewPath(value: string): boolean {
  return /\.(?:avif|bmp|gif|jpe?g|pdf|png|webp)$/i.test(value);
}

async function loadDirectFilesystemDirectory(workerEnvironmentId: string, surfaceId: string, path: string) {
  const transport = window.octopusTransport;
  if (!transport?.listWorkerEnvironmentFiles) throw new Error("Direct filesystem viewing is unavailable");
  const result = await transport.listWorkerEnvironmentFiles({ workerEnvironmentId, surfaceId, path });
  assertWorkerEnvironmentFileResponse(result, workerEnvironmentId, surfaceId, path);
  return result.entries;
}

function assertWorkerEnvironmentFileResponse(
  response: { workerEnvironmentId: string; surfaceId: string; path: string },
  workerEnvironmentId: string,
  surfaceId: string,
  path: string,
): void {
  if (
    response.workerEnvironmentId !== workerEnvironmentId
    || response.surfaceId !== surfaceId
    || response.path !== path
  ) {
    throw new Error("Worker Environment file response no longer matches the requested room surface.");
  }
}

function formatFileSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 102.4) / 10} KB`;
  return `${Math.round(bytes / (1024 * 102.4)) / 10} MB`;
}


type FixtureFileEntry = {
  path: string;
  kind: "directory" | "file";
  text: string;
};

export function fixtureFileEntries(surface: WorkerEnvironmentSurfaceProjection): FixtureFileEntry[] {
  const entries = Array.isArray(surface.data.entries) ? surface.data.entries : [];
  return entries.flatMap((value) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return [];
    const record = value as Record<string, unknown>;
    const path = dataText(record, "path");
    const kind = dataText(record, "kind");
    if (!path || (kind !== "directory" && kind !== "file")) return [];
    return [{ path, kind, text: kind === "file" ? dataText(record, "text") : "" }];
  });
}
