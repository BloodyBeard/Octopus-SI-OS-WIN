import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import "./icon-glyphs/worker";
import { motion } from "motion/react";
import type { RunProjection, SelectionRef, TopologySnapshot, WorkerEnvironmentProjection } from "../data/topology";
import type { OctopusClientAccessProfile } from "../electron";
import type { AdmittedClientSurfaceDocument } from "../client-surfaces/types";
import { OcButton, OcEmptyState, OcIconButton, OcSearchField, OcSelect } from "../design-system/components";
import { useProjectionStructureGeneration } from "../projectionLive";
import { formatProjectionError, readProjection } from "../projectionClient";
import { jsonText, shortId } from "../runtimeFormat";
import { Glyph } from "./IconGlyphs";
import { primaryWorkerEnvironmentSurface, workerEnvironmentPanelKind, workerEnvironmentSurfaceGlyph } from "./worker-environment/surfaceRegistry";
import {
  defaultWorkerEnvironmentPaneTarget,
  dispatchOpenWorkerEnvironmentPane,
  primaryWorkerEnvironmentClientSurface,
  workerEnvironmentClientSurfaces,
} from "./worker-environment/workerEnvironmentPane";
import { useFixedVirtualRange } from "./useFixedVirtualRange";

type Props = {
  accessProfile: OctopusClientAccessProfile;
  snapshot: TopologySnapshot;
  searchFocusTick: number;
  onSelect?: (selection: SelectionRef) => void;
  surfaces: AdmittedClientSurfaceDocument[];
};

type WorkerEnvironmentRow = {
  key: string;
  environmentId: string;
  environmentName: string;
  run: RunProjection | null;
  workerEnvironment: WorkerEnvironmentProjection;
};

type WorkerEnvironmentGroup = {
  environmentId: string;
  environmentName: string;
  rows: WorkerEnvironmentRow[];
};

type WorkerEnvironmentStateFilter = "all" | "active" | "attention" | "released";

type ProjectPresentationProjection = {
  accessProfile: OctopusClientAccessProfile;
  error: string;
  names: Map<string, string>;
  status: "loading" | "ready" | "degraded";
};

const WORKER_ENVIRONMENT_STATE_OPTIONS = [
  { label: "All states", value: "all" as const },
  { label: "Active rooms", value: "active" as const },
  { label: "Needs attention", value: "attention" as const },
  { label: "Released rooms", value: "released" as const },
];

export function WorkerEnvironmentsView({ accessProfile, snapshot, searchFocusTick, onSelect, surfaces }: Props) {
  const searchRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [stateFilter, setStateFilter] = useState<WorkerEnvironmentStateFilter>("all");
  const [kindFilter, setKindFilter] = useState("all");
  const [projectFilter, setProjectFilter] = useState("all");
  const [projectProjection, setProjectProjection] = useState<ProjectPresentationProjection>(() => ({
    accessProfile,
    error: "",
    names: new Map(),
    status: "loading",
  }));
  const [projectProjectionRetry, setProjectProjectionRetry] = useState(0);
  const projectionGeneration = useProjectionStructureGeneration();

  useEffect(() => {
    let disposed = false;
    setProjectProjection((current) => ({
      accessProfile,
      error: current.accessProfile === accessProfile ? current.error : "",
      names: current.accessProfile === accessProfile ? current.names : new Map(),
      status: "loading",
    }));
    void readProjection<unknown>("projection.operator.os.scope")
      .then((value) => {
        if (!disposed) {
          setProjectProjection({
            accessProfile,
            error: "",
            names: projectPresentationNames(value),
            status: "ready",
          });
        }
      })
      .catch((failure) => {
        if (!disposed) {
          setProjectProjection((current) => ({
            accessProfile,
            error: formatProjectionError(failure),
            names: current.accessProfile === accessProfile ? current.names : new Map(),
            status: "degraded",
          }));
        }
      });
    return () => {
      disposed = true;
    };
  }, [accessProfile, projectionGeneration, projectProjectionRetry]);

  const rows = useMemo(() => flattenWorkerEnvironments(snapshot, projectProjection.names), [projectProjection.names, snapshot]);
  const kindOptions = useMemo(() => [
    { label: "All kinds", value: "all" },
    ...Array.from(new Set(rows.map((row) => row.workerEnvironment.kind).filter(Boolean)))
      .sort((left, right) => left.localeCompare(right))
      .map((kind) => ({ label: humanizeIdentifier(kind), value: kind })),
  ], [rows]);
  const projectOptions = useMemo(() => [
    { label: "All projects", value: "all" },
    ...Array.from(new Map(rows.map((row) => [row.environmentId, row.environmentName])).entries())
      .sort((left, right) => left[1].localeCompare(right[1]))
      .map(([value, label]) => ({ label, value })),
  ], [rows]);
  const filteredRows = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return rows.filter((row) => (
      (kindFilter === "all" || row.workerEnvironment.kind === kindFilter)
      && (projectFilter === "all" || row.environmentId === projectFilter)
      && workerEnvironmentMatchesState(row.workerEnvironment, stateFilter, snapshot.projection.mode === "live")
      && (!needle || jsonText({
        project: row.environmentName,
        run: row.run,
        workerEnvironment: row.workerEnvironment,
        clientSurfaces: workerEnvironmentClientSurfaces(row.workerEnvironment.id, surfaces)
          .map((surface) => ({ title: surface.document.title, data: surface.document.data })),
      }).toLowerCase().includes(needle))
    ));
  }, [kindFilter, projectFilter, query, rows, snapshot.projection.mode, stateFilter, surfaces]);
  const groups = useMemo(() => groupWorkerEnvironments(filteredRows), [filteredRows]);

  useEffect(() => {
    if (searchFocusTick <= 0) return;
    window.requestAnimationFrame(() => searchRef.current?.focus());
  }, [searchFocusTick]);

  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <div className="plugins-surface-scroll relative h-full min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]">
        <div className="flex h-full min-h-0 w-full flex-col">
          <motion.div animate={{ opacity: 1, transform: "translateY(0)" }} initial={{ opacity: 0, transform: "translateY(-4px)" }} transition={{ duration: 0.2 }}>
            <div className="mx-auto w-full max-w-[var(--thread-content-max-width)] px-panel pt-panel">
              <div className="flex items-start justify-between gap-4 px-2">
                <div className="flex min-w-0 flex-col gap-2">
                  <h1 className="heading-xl font-normal text-token-foreground">Worker environments</h1>
                  <div className="text-lg leading-6 text-token-text-secondary">Technical rooms formed for tasks — browsers, worktrees, terminals, containers, and every tool surface their policy permits</div>
                </div>
                <span className="mt-1 shrink-0 text-sm tabular-nums text-token-description-foreground">{filteredRows.length}</span>
              </div>
            </div>
          </motion.div>
          <div className="sticky top-0 z-30 bg-token-main-surface-primary after:pointer-events-none after:absolute after:top-full after:right-0 after:left-0 after:h-8 after:bg-linear-to-b after:from-token-main-surface-primary after:to-transparent after:content-['']">
            <div className="mx-auto flex w-full max-w-[var(--thread-content-max-width)] flex-wrap items-center gap-2 px-panel pt-panel pb-2">
              <EnvironmentSearch inputRef={searchRef} query={query} onChange={setQuery} />
              <OcSelect
                className="min-w-36"
                label="Room state"
                onChange={setStateFilter}
                options={WORKER_ENVIRONMENT_STATE_OPTIONS}
                value={stateFilter}
              />
              {kindOptions.length > 2 ? (
                <OcSelect className="min-w-36" label="Room kind" onChange={setKindFilter} options={kindOptions} value={kindFilter} />
              ) : null}
              {projectOptions.length > 2 ? (
                <OcSelect className="min-w-36" label="Project" onChange={setProjectFilter} options={projectOptions} value={projectFilter} />
              ) : null}
            </div>
          </div>
          {projectProjection.status === "degraded" ? (
            <div className="mx-auto w-full max-w-[var(--thread-content-max-width)] px-panel pt-4">
              <div className="flex flex-wrap items-start justify-between gap-3 rounded-lg border border-token-border bg-token-main-surface-secondary p-3" role="alert">
                <div className="min-w-0 flex-1">
                  <div className="text-sm font-medium text-token-foreground">Project labels could not be refreshed</div>
                  <div className="mt-1 break-words text-xs leading-5 text-token-text-secondary">
                    The room projection remains available. Project names use the last successful presentation or their registry fallback until this read succeeds. {projectProjection.error}
                  </div>
                </div>
                <OcButton
                  leadingIcon="retry"
                  onClick={() => setProjectProjectionRetry((value) => value + 1)}
                  size="sm"
                  variant="secondary"
                >Retry</OcButton>
              </div>
            </div>
          ) : null}
          {rows.length > 0 && snapshot.projection.mode !== "live" ? (
            <div className="mx-auto w-full max-w-[var(--thread-content-max-width)] px-panel pt-4">
              <div className="rounded-lg border border-token-border bg-token-main-surface-secondary p-3" role="alert">
                <div className="text-sm font-medium text-token-foreground">{snapshot.projection.label}</div>
                <div className="mt-1 break-words text-xs leading-5 text-token-text-secondary">
                  {accessProfile === "lab"
                    ? "Client Lab rooms are fixture projections and are never reported as currently active."
                    : "Rooms below are the last successful projection and are not being shown as currently active."} {snapshot.projection.detail}
                </div>
              </div>
            </div>
          ) : null}
          {rows.length === 0 ? (
            <div className="mx-auto flex min-h-0 w-full max-w-[var(--thread-content-max-width)] flex-1 flex-col px-panel pt-5 pb-panel electron:min-w-[calc(320px*var(--octopus-window-zoom))]">
              <EnvironmentEmptyState
                detail={snapshot.projection.mode === "live"
                  ? "When a run forms a browser session, worktree, terminal, container, or another execution room, it appears here."
                  : snapshot.projection.detail}
                title={snapshot.projection.mode === "live" ? "No worker environments" : snapshot.projection.label}
              />
            </div>
          ) : groups.length === 0 ? (
            <div className="mx-auto flex min-h-0 w-full max-w-[var(--thread-content-max-width)] flex-1 flex-col px-panel pt-5 pb-panel electron:min-w-[calc(320px*var(--octopus-window-zoom))]">
              <EnvironmentEmptyState
                detail="Try a different task, project, room kind, state, capability, or filesystem root."
                title="No matching environments"
              />
            </div>
          ) : (
            <motion.div
              animate={{ opacity: 1 }}
              className="mx-auto flex min-h-0 w-full max-w-[var(--thread-content-max-width)] flex-1 flex-col px-panel pt-5 pb-panel electron:min-w-[calc(320px*var(--octopus-window-zoom))]"
              initial={{ opacity: 0 }}
              key="worker-environment-gallery"
              transition={{ duration: 0.2 }}
            >
              <WorkerEnvironmentVirtualGallery
                fixture={accessProfile === "lab"}
                groups={groups}
                onSelect={onSelect}
                projectionLive={snapshot.projection.mode === "live"}
                surfaces={surfaces}
              />
            </motion.div>
          )}
        </div>
      </div>
    </section>
  );
}

type WorkerEnvironmentVirtualItem =
  | { group: WorkerEnvironmentGroup; kind: "heading" }
  | { group: WorkerEnvironmentGroup; kind: "row"; row: WorkerEnvironmentRow };

const WORKER_ENVIRONMENT_VIRTUAL_ITEM_HEIGHT = 76;

function WorkerEnvironmentVirtualGallery({ fixture, groups, onSelect, projectionLive, surfaces }: {
  fixture: boolean;
  groups: WorkerEnvironmentGroup[];
  onSelect?: (selection: SelectionRef) => void;
  projectionLive: boolean;
  surfaces: AdmittedClientSurfaceDocument[];
}) {
  const items = useMemo<WorkerEnvironmentVirtualItem[]>(() => groups.flatMap((group) => [
    { group, kind: "heading" as const },
    ...group.rows.map((row) => ({ group, kind: "row" as const, row })),
  ]), [groups]);
  const virtualRows = useFixedVirtualRange({ count: items.length, itemSize: WORKER_ENVIRONMENT_VIRTUAL_ITEM_HEIGHT });
  const rowIndexes = useMemo(() => items.flatMap((item, index) => item.kind === "row" ? [index] : []), [items]);
  const rowIndexSignature = rowIndexes.join(":");
  const [focusedIndex, setFocusedIndex] = useState(() => rowIndexes[0] ?? 0);

  useEffect(() => {
    setFocusedIndex(rowIndexes[0] ?? 0);
  }, [rowIndexSignature]);

  const focusRow = (nextRowPosition: number) => {
    const itemIndex = rowIndexes[Math.max(0, Math.min(rowIndexes.length - 1, nextRowPosition))];
    if (itemIndex === undefined) return;
    const viewport = virtualRows.scrollRef.current;
    if (!viewport) return;
    const top = itemIndex * WORKER_ENVIRONMENT_VIRTUAL_ITEM_HEIGHT;
    const bottom = top + WORKER_ENVIRONMENT_VIRTUAL_ITEM_HEIGHT;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (bottom > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = bottom - viewport.clientHeight;
    setFocusedIndex(itemIndex);
    window.requestAnimationFrame(() => {
      viewport.querySelector<HTMLButtonElement>(`[data-worker-environment-index="${itemIndex}"]`)?.focus();
    });
  };

  const onRowKeyDown = (event: KeyboardEvent<HTMLButtonElement>, itemIndex: number) => {
    const rowPosition = rowIndexes.indexOf(itemIndex);
    const page = Math.max(1, Math.floor(virtualRows.viewportHeight / WORKER_ENVIRONMENT_VIRTUAL_ITEM_HEIGHT));
    const target = event.key === "ArrowDown" ? rowPosition + 1
      : event.key === "ArrowUp" ? rowPosition - 1
        : event.key === "PageDown" ? rowPosition + page
          : event.key === "PageUp" ? rowPosition - page
            : event.key === "Home" ? 0
              : event.key === "End" ? rowIndexes.length - 1
                : null;
    if (target === null) return;
    event.preventDefault();
    focusRow(target);
  };

  return (
    <div className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]" onScroll={virtualRows.onScroll} ref={virtualRows.setScrollRef}>
      <div style={{ height: virtualRows.totalSize, position: "relative" }}>
        <div style={{ position: "absolute", top: virtualRows.start * WORKER_ENVIRONMENT_VIRTUAL_ITEM_HEIGHT, width: "100%" }}>
          {items.slice(virtualRows.start, virtualRows.end).map((item, offset) => {
            const itemIndex = virtualRows.start + offset;
            if (item.kind === "heading") {
              return (
                <GallerySectionHeader
                  count={item.group.rows.length}
                  key={`${item.group.environmentId}:heading`}
                  onFocusEnvironment={onSelect ? () => onSelect({ kind: "environment", id: item.group.environmentId }) : undefined}
                  title={item.group.environmentName}
                />
              );
            }
            return (
              <WorkerEnvironmentListRow
                fixture={fixture}
                key={item.row.key}
                onFocus={() => setFocusedIndex(itemIndex)}
                onKeyDown={(event) => onRowKeyDown(event, itemIndex)}
                onOpen={() => dispatchOpenWorkerEnvironmentPane(defaultWorkerEnvironmentPaneTarget(item.row.workerEnvironment, surfaces))}
                onOpenOverview={() => dispatchOpenWorkerEnvironmentPane({ workerEnvironmentId: item.row.workerEnvironment.id, panelKind: "overview" })}
                projectionLive={projectionLive}
                row={item.row}
                surfaces={surfaces}
                tabIndex={focusedIndex === itemIndex ? 0 : -1}
                virtualIndex={itemIndex}
              />
            );
          })}
        </div>
      </div>
    </div>
  );
}

function WorkerEnvironmentListRow({ fixture, row, onOpen, onOpenOverview, projectionLive, surfaces, virtualIndex, tabIndex, onFocus, onKeyDown }: {
  fixture: boolean;
  row: WorkerEnvironmentRow;
  onOpen: () => void;
  onOpenOverview: () => void;
  projectionLive: boolean;
  surfaces: AdmittedClientSurfaceDocument[];
  virtualIndex?: number;
  tabIndex?: number;
  onFocus?: () => void;
  onKeyDown?: (event: KeyboardEvent<HTMLButtonElement>) => void;
}) {
  const workerEnvironment = row.workerEnvironment;
  const projectedSurface = primaryWorkerEnvironmentSurface(workerEnvironment);
  const document = primaryWorkerEnvironmentClientSurface(workerEnvironment.id, surfaces);
  const surface = document && (!projectedSurface || workerEnvironmentPanelKind(projectedSurface) === "connection")
    ? null
    : projectedSurface;
  const fact = workerEnvironmentFact(row, surface, document);
  const statusTone = workerEnvironmentStatusTone(workerEnvironment, projectionLive);
  const stateLabel = workerEnvironmentStateLabel(workerEnvironment, projectionLive, fixture);
  const title = workerEnvironmentTaskTitle(row);
  return (
    <div className="group flex h-[76px] w-full items-center border-b border-token-border last:border-b-0 hover:bg-token-list-hover-background focus-within:bg-token-list-hover-background">
      <button
        aria-label={`Open ${title} room`}
        className="flex min-w-0 flex-1 cursor-interaction items-center gap-3 px-3 py-2.5 text-left focus-visible:outline-none"
        data-worker-environment-index={virtualIndex}
        onClick={onOpen}
        onFocus={onFocus}
        onKeyDown={onKeyDown}
        tabIndex={tabIndex}
        type="button"
      >
        <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-token-border text-token-text-secondary">
          <Glyph name={surface ? workerEnvironmentSurfaceGlyph(surface) : document?.document.glyph ?? "ee"} size={16} />
        </span>
        <span className="flex min-w-0 flex-1 flex-col">
          <span className="flex min-w-0 items-center gap-2">
            <span className="min-w-0 truncate text-sm font-medium text-token-foreground">{title}</span>
            <span className="flex shrink-0 items-center gap-1.5 text-xs text-token-text-secondary">
              <span className={`size-1.5 rounded-full ${statusTone === "active" ? "bg-token-charts-green" : statusTone === "error" ? "bg-token-charts-red" : "bg-token-description-foreground"}`} />
              {stateLabel}
            </span>
          </span>
          {fact ? <span className="min-w-0 truncate text-xs text-token-text-secondary">{fact}</span> : null}
        </span>
        {workerEnvironment.kind ? (
          <span className="flex max-w-56 shrink-0 items-center gap-1.5 rounded-full border border-token-border bg-transparent px-2 py-0.5 text-sm leading-5 text-token-text-primary max-[680px]:hidden">
            <span className="min-w-0 truncate">{humanizeIdentifier(workerEnvironment.kind)}</span>
          </span>
        ) : null}
        <Glyph className="icon-2xs shrink-0 -rotate-90 text-token-input-placeholder-foreground" name="chevronDown" size={12} />
      </button>
      <OcIconButton
        className="mr-1 shrink-0 text-token-text-secondary"
        glyph="viewAll"
        label={`Open ${title} room overview`}
        onClick={onOpenOverview}
        size="xs"
        variant="ghost"
      />
    </div>
  );
}

function workerEnvironmentFact(
  row: WorkerEnvironmentRow,
  surface: ReturnType<typeof primaryWorkerEnvironmentSurface>,
  document: AdmittedClientSurfaceDocument | null,
): string {
  const workerEnvironment = row.workerEnvironment;
  const data = surface?.data ?? {};
  const roots = Array.from(new Set(workerEnvironment.surfaces
    .map((candidate) => candidate.root.trim())
    .filter(Boolean)));
  const capability = workerEnvironment.surfaces
    .filter((candidate) => workerEnvironmentPanelKind(candidate) !== "connection")
    .map((candidate) => candidate.label.trim())
    .find(Boolean);
  const candidates = Array.from(new Set([
    typeof data.url === "string" ? data.url : "",
    surface?.viewerUrl ?? "",
    typeof data.branch === "string" ? data.branch : "",
    roots[0] ?? surface?.root ?? workerEnvironment.root ?? "",
    surface?.protocol ?? "",
    document?.document.title ?? "",
    workerEnvironment.transport ?? "",
  ].map((value) => value.trim()).filter(Boolean)));
  return [row.environmentName, capability, candidates[0], roots.length > 1 ? `+${roots.length - 1} roots` : ""]
    .filter(Boolean)
    .join(" · ");
}

function workerEnvironmentStatusTone(environment: WorkerEnvironmentProjection, projectionLive: boolean): "active" | "error" | "muted" {
  const lifecycle = environment.lifecycle.toLowerCase();
  const status = environment.status.toLowerCase();
  if ([lifecycle, status].some((value) => ["failed", "blocked", "error", "denied"].includes(value))) return "error";
  if (lifecycle === "active" && ["closed", "released", "offline"].includes(status)) return "error";
  if (projectionLive && environment.live) return "active";
  return "muted";
}

function workerEnvironmentStateLabel(environment: WorkerEnvironmentProjection, projectionLive: boolean, fixture: boolean): string {
  const lifecycle = humanizeIdentifier(environment.lifecycle || "");
  const status = humanizeIdentifier(environment.status || "unknown");
  if (fixture) return `Fixture · ${status || lifecycle}`;
  if (!projectionLive) return `Last projected · ${status || lifecycle}`;
  if (lifecycle && lifecycle.toLowerCase() !== status.toLowerCase()) {
    return `${lifecycle} · ${status}`;
  }
  return lifecycle || status;
}

function workerEnvironmentTaskTitle(row: WorkerEnvironmentRow): string {
  const title = row.run?.label.trim();
  if (title) return title;
  const runId = row.run?.id || row.workerEnvironment.runId;
  return runId ? `Task ${shortId(runId, 12, 5)}` : `Technical room ${shortId(row.workerEnvironment.id, 12, 5)}`;
}

function workerEnvironmentMatchesState(environment: WorkerEnvironmentProjection, filter: WorkerEnvironmentStateFilter, projectionLive: boolean): boolean {
  if (filter === "all") return true;
  const lifecycle = environment.lifecycle.toLowerCase();
  const status = environment.status.toLowerCase();
  const attention = [lifecycle, status].some((value) => ["failed", "blocked", "error", "denied"].includes(value))
    || (lifecycle === "active" && ["closed", "released", "offline"].includes(status));
  if (filter === "attention") return attention;
  if (filter === "released") return lifecycle === "released";
  return projectionLive && environment.live;
}

function EnvironmentSearch({ inputRef, query, onChange }: {
  inputRef: RefObject<HTMLInputElement | null>;
  query: string;
  onChange: (value: string) => void;
}) {
  return (
    <OcSearchField
      className="min-w-0 flex-1"
      id="worker-environment-search"
      inputRef={inputRef}
      label="Search worker environments"
      onChange={onChange}
      placeholder="Search worker environments"
      value={query}
    />
  );
}

function GallerySectionHeader({ title, count, onFocusEnvironment }: {
  title: string;
  count: number;
  onFocusEnvironment?: () => void;
}) {
  return (
    <header className="flex h-[76px] min-w-0 items-center justify-between gap-4">
        <div className="flex min-w-0 items-center gap-2">
          <h2 className="m-0 min-w-0 truncate text-base font-medium text-token-text-primary">{title}</h2>
          {onFocusEnvironment ? (
            <OcIconButton
              glyph="topology"
              label={`Focus ${title} in topology`}
              onClick={onFocusEnvironment}
              size="sm"
              variant="ghost"
            />
          ) : null}
        </div>
        <span className="text-sm text-token-input-placeholder-foreground">{count}</span>
    </header>
  );
}

function EnvironmentEmptyState({ title, detail }: { title: string; detail: string }) {
  return (
    <OcEmptyState
      className="min-h-32 rounded-xl border border-token-border p-6"
      detail={detail}
      glyph="ee"
      size="compact"
      title={title}
    />
  );
}

function flattenWorkerEnvironments(snapshot: TopologySnapshot, projectNames: Map<string, string>): WorkerEnvironmentRow[] {
  return snapshot.environments.flatMap((environment) => {
    const formed = new Map<string, WorkerEnvironmentRow>();
    const environmentName = projectNames.get(environment.id) || environment.name;
    for (const run of environment.runs) {
      for (const workerEnvironment of run.workerEnvironments) {
        if (!workerEnvironment.id) continue;
        formed.set(workerEnvironment.id, {
          key: `${environment.id}:${workerEnvironment.id}`,
          environmentId: environment.id,
          environmentName,
          run,
          workerEnvironment,
        });
      }
    }
    for (const workerEnvironment of environment.workerEnvironments) {
      if (!workerEnvironment.id || formed.has(workerEnvironment.id)) continue;
      const ownerRun = workerEnvironment.runId
        ? environment.runs.find((candidate) => candidate.id === workerEnvironment.runId) ?? null
        : null;
      formed.set(workerEnvironment.id, {
        key: `${environment.id}:${workerEnvironment.id}`,
        environmentId: environment.id,
        environmentName,
        run: ownerRun,
        workerEnvironment,
      });
    }
    return [...formed.values()];
  }).sort((left, right) => workerEnvironmentRecency(right.workerEnvironment) - workerEnvironmentRecency(left.workerEnvironment));
}

function workerEnvironmentRecency(environment: WorkerEnvironmentProjection): number {
  const numeric = Number(environment.revision);
  if (Number.isFinite(numeric)) return numeric;
  const timestamp = Date.parse(environment.revision);
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function projectPresentationNames(value: unknown): Map<string, string> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return new Map();
  const rows = (value as Record<string, unknown>).project_presentations;
  if (!Array.isArray(rows)) return new Map();
  return new Map(rows.flatMap((candidate): Array<[string, string]> => {
    if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) return [];
    const record = candidate as Record<string, unknown>;
    const id = typeof record.ee_id === "string" ? record.ee_id.trim() : "";
    const name = typeof record.name === "string" ? record.name.trim() : "";
    return id && name ? [[id, name]] : [];
  }));
}

function humanizeIdentifier(value: string): string {
  return value.trim().replace(/[._-]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function groupWorkerEnvironments(rows: WorkerEnvironmentRow[]): WorkerEnvironmentGroup[] {
  const groups = new Map<string, WorkerEnvironmentGroup>();
  for (const row of rows) {
    const group = groups.get(row.environmentId);
    if (group) {
      group.rows.push(row);
      continue;
    }
    groups.set(row.environmentId, {
      environmentId: row.environmentId,
      environmentName: row.environmentName,
      rows: [row],
    });
  }
  return [...groups.values()];
}
