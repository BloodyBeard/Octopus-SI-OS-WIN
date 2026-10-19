import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import "./icon-glyphs/worker";
import { motion } from "motion/react";
import type { ChatRun } from "../chatRuns";
import type { RunProjection, SelectionRef, TopologySnapshot, WorkerEnvironmentProjection } from "../data/topology";
import type { OctopusClientAccessProfile } from "../electron";
import type { AdmittedClientSurfaceDocument } from "../client-surfaces/types";
import { OcButton, OcEmptyState, OcIconButton, OcSearchField, OcSelect, OcStatus, cx, type OcTone } from "../design-system/components";
import { useProjectionStructureGeneration } from "../projectionLive";
import { formatProjectionError, readProjection } from "../projectionClient";
import { shortId } from "../runtimeFormat";
import { Glyph, type GlyphName } from "./IconGlyphs";
import { baseDynamicToolId } from "./chat/toolPresentation";
import { formatEvidenceTime, relativeEvidenceTime } from "./scheduled/scheduleModel";
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
  chatRuns: readonly ChatRun[];
  snapshot: TopologySnapshot;
  searchFocusTick: number;
  onOpenRun?: (runId: string, role: "home" | "task") => void;
  onSelect?: (selection: SelectionRef) => void;
  surfaces: AdmittedClientSurfaceDocument[];
};

type RoomSection = "tasks" | "home" | "system";
type RoomTab = "all" | RoomSection;
type RoomStateFilter = "all" | "open" | "attention" | "closed";

type RoomState = {
  filter: Exclude<RoomStateFilter, "all"> | "other";
  label: string;
  tone: OcTone;
  detail: string;
};

type RoomRow = {
  key: string;
  room: WorkerEnvironmentProjection;
  runId: string;
  projectId: string;
  projectName: string;
  section: RoomSection;
  usedBy: string;
  openRole: "home" | "task" | null;
  title: string;
  detail: string;
  glyph: GlyphName;
  state: RoomState;
  searchText: string;
};

type ProjectPresentationProjection = {
  accessProfile: OctopusClientAccessProfile;
  error: string;
  names: Map<string, string>;
  status: "loading" | "ready" | "degraded";
};

const ROOM_TABS: Array<{ value: RoomTab; label: string; note: string }> = [
  { value: "all", label: "All", note: "" },
  { value: "tasks", label: "Tasks", note: "Rooms your tasks opened to do their work." },
  { value: "home", label: "Home", note: "Rooms opened for Home." },
  { value: "system", label: "System", note: "Rooms Octopus opens for its own upkeep rather than for a task. They come and go on their own and need nothing from you." },
];

const ROOM_STATE_OPTIONS = [
  { label: "Any state", value: "all" as const },
  { label: "Open", value: "open" as const },
  { label: "Needs attention", value: "attention" as const },
  { label: "Closed", value: "closed" as const },
];

const ROOM_ROW_HEIGHT = 60;

export function WorkerEnvironmentsView({ accessProfile, chatRuns, snapshot, searchFocusTick, onOpenRun, onSelect, surfaces }: Props) {
  const searchRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<RoomTab>("all");
  const [stateFilter, setStateFilter] = useState<RoomStateFilter>("all");
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
  const projectionLive = snapshot.projection.mode === "live";
  const fixture = accessProfile === "lab";

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

  const rows = useMemo(
    () => projectRooms(snapshot, projectProjection.names, chatRuns, surfaces, projectionLive, fixture),
    [chatRuns, fixture, projectProjection.names, projectionLive, snapshot, surfaces],
  );
  const tabCounts = useMemo(() => {
    const counts: Record<RoomTab, number> = { all: rows.length, tasks: 0, home: 0, system: 0 };
    for (const row of rows) counts[row.section] += 1;
    return counts;
  }, [rows]);
  const kindOptions = useMemo(() => [
    { label: "Any kind", value: "all" },
    ...Array.from(new Set(rows.map((row) => row.room.kind).filter(Boolean)))
      .sort((left, right) => left.localeCompare(right))
      .map((kind) => ({ label: sentenceCase(kind), value: kind })),
  ], [rows]);
  const projectOptions = useMemo(() => [
    { label: "Any project", value: "all" },
    ...Array.from(new Map(rows.map((row) => [row.projectId, row.projectName])).entries())
      .sort((left, right) => left[1].localeCompare(right[1]))
      .map(([value, label]) => ({ label, value })),
  ], [rows]);
  const visibleRows = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return rows.filter((row) => (
      (tab === "all" || row.section === tab)
      && (stateFilter === "all" || row.state.filter === stateFilter)
      && (kindFilter === "all" || row.room.kind === kindFilter)
      && (projectFilter === "all" || row.projectId === projectFilter)
      && (!needle || row.searchText.includes(needle))
    ));
  }, [kindFilter, projectFilter, query, rows, stateFilter, tab]);
  const filtered = Boolean(query.trim()) || stateFilter !== "all" || kindFilter !== "all" || projectFilter !== "all";
  const tabNote = ROOM_TABS.find((candidate) => candidate.value === tab)?.note ?? "";

  useEffect(() => {
    if (searchFocusTick <= 0) return;
    window.requestAnimationFrame(() => searchRef.current?.focus());
  }, [searchFocusTick]);

  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <div className="flex h-full min-h-0 w-full flex-col">
        <motion.div animate={{ opacity: 1, transform: "translateY(0)" }} initial={{ opacity: 0, transform: "translateY(-4px)" }} transition={{ duration: 0.2 }}>
          <div className="mx-auto w-full max-w-6xl px-panel pt-panel">
            <div className="flex flex-wrap items-center justify-between gap-x-6 gap-y-3 px-2">
              <h1 className="heading-xl m-0 font-normal text-token-foreground">Worker environments</h1>
              <OcSearchField
                className="w-full min-w-0 sm:w-72"
                id="worker-environment-search"
                inputRef={searchRef}
                label="Search rooms"
                onChange={setQuery}
                placeholder="Search rooms"
                value={query}
              />
            </div>
            <p className="m-0 mt-2 max-w-3xl px-2 text-base leading-6 text-token-text-secondary">
              Rooms where work touches the world: a browser, a code checkout, a terminal, a desktop. Octopus opens one when a task's tools need it.
              Open a room to watch it live, review its changes, or browse its files.
            </p>
          </div>
        </motion.div>
        <div className="mx-auto flex w-full max-w-6xl flex-wrap items-center justify-between gap-x-4 gap-y-2 px-panel pt-5 pb-3">
          <div aria-label="Rooms by owner" className="hide-scrollbar flex min-w-0 items-center gap-1 overflow-x-auto px-1" role="tablist">
            {ROOM_TABS.map((candidate) => {
              const selected = candidate.value === tab;
              return (
                <button
                  aria-selected={selected}
                  className={cx(
                    "no-drag flex h-8 shrink-0 cursor-interaction items-center gap-1.5 rounded-full px-3.5 text-base whitespace-nowrap focus:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    selected ? "bg-text/5 text-default hover:bg-text/10" : "text-tertiary hover:bg-primary-ghost-hover hover:text-default",
                  )}
                  key={candidate.value}
                  onClick={() => setTab(candidate.value)}
                  role="tab"
                  type="button"
                >
                  {candidate.label}
                  <span className="text-sm tabular-nums text-token-description-foreground">{tabCounts[candidate.value]}</span>
                </button>
              );
            })}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <OcSelect className="min-w-32" label="Room state" onChange={setStateFilter} options={ROOM_STATE_OPTIONS} value={stateFilter} />
            {kindOptions.length > 2 ? <OcSelect className="min-w-32" label="Room kind" onChange={setKindFilter} options={kindOptions} value={kindFilter} /> : null}
            {projectOptions.length > 2 ? <OcSelect className="min-w-32" label="Project" onChange={setProjectFilter} options={projectOptions} value={projectFilter} /> : null}
          </div>
        </div>
        {tabNote ? (
          <div className="mx-auto w-full max-w-6xl px-panel pb-2">
            <p className="m-0 px-2 text-sm leading-5 text-token-text-secondary">{tabNote}</p>
          </div>
        ) : null}
        {projectProjection.status === "degraded" ? (
          <RoomNotice
            action={<OcButton leadingIcon="retry" onClick={() => setProjectProjectionRetry((value) => value + 1)} size="sm" variant="secondary">Retry</OcButton>}
            detail={`Rooms are still listed. Project names use the last successful read until this one succeeds. ${projectProjection.error}`}
            title="Project names could not be refreshed"
          />
        ) : null}
        {rows.length > 0 && !projectionLive ? (
          <RoomNotice
            detail={`${fixture ? "Client Lab rooms are samples and are never shown as open." : "These rooms are from the last successful read and are not shown as open."} ${snapshot.projection.detail}`}
            title={snapshot.projection.label}
          />
        ) : null}
        <div className="mx-auto flex min-h-0 w-full max-w-6xl flex-1 flex-col px-panel pt-1 pb-panel electron:min-w-[calc(320px*var(--octopus-window-zoom))]">
          {rows.length === 0 ? (
            <RoomEmptyState
              detail={projectionLive
                ? "When a task needs a browser, a code checkout, a terminal or a desktop, Octopus opens a room for it and it appears here."
                : snapshot.projection.detail}
              title={projectionLive ? "No rooms yet" : snapshot.projection.label}
            />
          ) : visibleRows.length === 0 ? (
            <RoomEmptyState
              detail={filtered ? "Try another search, state, kind or project." : tab === "tasks" ? "None of your tasks has opened a room." : tab === "home" ? "Home has no rooms." : "Octopus has no upkeep rooms."}
              title={filtered ? "No matching rooms" : "Nothing here"}
            />
          ) : (
            <RoomTable onOpenRun={onOpenRun} onSelect={onSelect} rows={visibleRows} surfaces={surfaces} />
          )}
        </div>
      </div>
    </section>
  );
}

function RoomTable({ onOpenRun, onSelect, rows, surfaces }: {
  onOpenRun?: (runId: string, role: "home" | "task") => void;
  onSelect?: (selection: SelectionRef) => void;
  rows: RoomRow[];
  surfaces: AdmittedClientSurfaceDocument[];
}) {
  const virtualRows = useFixedVirtualRange({ count: rows.length, itemSize: ROOM_ROW_HEIGHT });
  const rowSignature = rows.map((row) => row.key).join(":");
  const [focusedIndex, setFocusedIndex] = useState(0);

  useEffect(() => {
    setFocusedIndex(0);
  }, [rowSignature]);

  const focusRow = (target: number) => {
    const index = Math.max(0, Math.min(rows.length - 1, target));
    const viewport = virtualRows.scrollRef.current;
    if (!viewport) return;
    const top = index * ROOM_ROW_HEIGHT;
    const bottom = top + ROOM_ROW_HEIGHT;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (bottom > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = bottom - viewport.clientHeight;
    setFocusedIndex(index);
    window.requestAnimationFrame(() => {
      viewport.querySelector<HTMLButtonElement>(`[data-worker-environment-index="${index}"]`)?.focus();
    });
  };

  const onRowKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const page = Math.max(1, Math.floor(virtualRows.viewportHeight / ROOM_ROW_HEIGHT));
    const target = event.key === "ArrowDown" ? index + 1
      : event.key === "ArrowUp" ? index - 1
        : event.key === "PageDown" ? index + page
          : event.key === "PageUp" ? index - page
            : event.key === "Home" ? 0
              : event.key === "End" ? rows.length - 1
                : null;
    if (target === null) return;
    event.preventDefault();
    focusRow(target);
  };

  return (
    <div aria-label="Worker environments" aria-rowcount={rows.length} className="@container/rooms flex min-h-0 flex-1 flex-col" role="grid">
      <div className="flex h-11 shrink-0 items-center gap-4 border-b border-token-border px-3 text-sm leading-5 text-token-text-secondary" role="row">
        <span className="min-w-0 flex-1 ps-11" role="columnheader">Room</span>
        <span className="hidden w-56 shrink-0 @2xl/rooms:block" role="columnheader">Used by</span>
        <span className="hidden w-36 shrink-0 @lg/rooms:block" role="columnheader">State</span>
        <span className="hidden w-32 shrink-0 @xl/rooms:block" role="columnheader">Last activity</span>
        <span aria-hidden="true" className="w-16 shrink-0" />
      </div>
      <div className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]" onScroll={virtualRows.onScroll} ref={virtualRows.setScrollRef}>
        <div style={{ height: virtualRows.totalSize, position: "relative" }}>
          <div style={{ position: "absolute", top: virtualRows.start * ROOM_ROW_HEIGHT, width: "100%" }}>
            {rows.slice(virtualRows.start, virtualRows.end).map((row, offset) => {
              const index = virtualRows.start + offset;
              return (
                <RoomTableRow
                  index={index}
                  key={row.key}
                  onFocus={() => setFocusedIndex(index)}
                  onKeyDown={(event) => onRowKeyDown(event, index)}
                  onOpenRun={onOpenRun}
                  onSelect={onSelect}
                  row={row}
                  surfaces={surfaces}
                  tabIndex={focusedIndex === index ? 0 : -1}
                />
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}

function RoomTableRow({ index, onFocus, onKeyDown, onOpenRun, onSelect, row, surfaces, tabIndex }: {
  index: number;
  onFocus: () => void;
  onKeyDown: (event: KeyboardEvent<HTMLButtonElement>) => void;
  onOpenRun?: (runId: string, role: "home" | "task") => void;
  onSelect?: (selection: SelectionRef) => void;
  row: RoomRow;
  surfaces: AdmittedClientSurfaceDocument[];
  tabIndex: number;
}) {
  const openRun = row.openRole && onOpenRun ? () => onOpenRun(row.runId, row.openRole!) : null;
  const lastActivity = relativeEvidenceTime(row.room.lastActivityAtMs);
  return (
    <div
      aria-rowindex={index + 1}
      className="group relative flex items-center gap-4 border-b border-token-border px-3 last:border-b-0 hover:bg-token-list-hover-background focus-within:bg-token-list-hover-background"
      role="row"
      style={{ height: ROOM_ROW_HEIGHT }}
    >
      <span className="flex min-w-0 flex-1 items-center gap-3" role="gridcell">
        <span className="relative size-8 shrink-0">
          <span className="flex size-full items-center justify-center overflow-hidden rounded-lg border border-token-border bg-token-main-surface-primary text-token-text-secondary">
            <Glyph name={row.glyph} size={16} />
          </span>
          <span
            aria-hidden="true"
            className={cx(
              "absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-token-main-surface-primary",
              row.state.tone === "success" ? "bg-token-charts-green" : row.state.tone === "danger" ? "bg-token-charts-red" : "hidden",
            )}
          />
        </span>
        <button
          aria-label={`Open ${row.title} room${row.openRole ? ` used by ${row.usedBy}` : ""}`}
          className="flex min-w-0 flex-1 cursor-interaction flex-col text-left after:absolute after:inset-0 after:z-0 focus-visible:outline-none"
          data-worker-environment-index={index}
          onClick={() => dispatchOpenWorkerEnvironmentPane(defaultWorkerEnvironmentPaneTarget(row.room, surfaces))}
          onFocus={onFocus}
          onKeyDown={onKeyDown}
          tabIndex={tabIndex}
          type="button"
        >
          <span className="min-w-0 truncate text-base text-token-foreground">{row.title}</span>
          {row.detail ? <span className="min-w-0 truncate text-sm leading-5 text-token-text-secondary">{row.detail}</span> : null}
        </button>
      </span>
      <span className="hidden w-56 shrink-0 items-center text-sm leading-5 text-token-text-secondary @2xl/rooms:flex" role="gridcell">
        {openRun ? (
          <button
            className="relative z-10 min-w-0 cursor-interaction truncate text-left hover:text-token-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            onClick={openRun}
            tabIndex={-1}
            title={row.projectName && row.section === "tasks" ? `${row.usedBy} · ${row.projectName}` : row.usedBy}
            type="button"
          >{row.usedBy}</button>
        ) : (
          <span className="min-w-0 truncate" title={row.usedBy}>{row.usedBy}</span>
        )}
      </span>
      <span className="hidden w-36 shrink-0 items-center @lg/rooms:flex" role="gridcell">
        <OcStatus className="relative z-10" detail={row.state.detail} label={row.state.label} tone={row.state.tone} />
      </span>
      <span className="hidden w-32 shrink-0 truncate text-sm leading-5 text-token-text-secondary @xl/rooms:block" role="gridcell" title={row.room.lastActivityAtMs ? formatEvidenceTime(row.room.lastActivityAtMs) : undefined}>
        {lastActivity || "—"}
      </span>
      <span className="relative z-10 flex w-16 shrink-0 items-center justify-end gap-0.5 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 pointer-coarse:opacity-100" role="gridcell">
        {openRun ? (
          <OcIconButton glyph={row.openRole === "home" ? "home" : "chat"} label={row.openRole === "home" ? "Open Home" : `Open ${row.usedBy}`} onClick={openRun} size="xs" tabIndex={-1} variant="ghost" />
        ) : onSelect ? (
          <OcIconButton glyph="topology" label="Show its run in topology" onClick={() => onSelect({ kind: "run", id: row.runId })} size="xs" tabIndex={-1} variant="ghost" />
        ) : null}
        <OcIconButton
          glyph="viewAll"
          label={`${row.title} room details`}
          onClick={() => dispatchOpenWorkerEnvironmentPane({ workerEnvironmentId: row.room.id, panelKind: "overview" })}
          size="xs"
          tabIndex={-1}
          variant="ghost"
        />
      </span>
    </div>
  );
}

function RoomNotice({ action, detail, title }: { action?: ReactNode; detail: string; title: string }) {
  return (
    <div className="mx-auto w-full max-w-6xl px-panel pb-3">
      <div className="flex flex-wrap items-start justify-between gap-3 rounded-lg border border-token-border bg-token-main-surface-secondary p-3" role="alert">
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium text-token-foreground">{title}</div>
          <div className="mt-1 break-words text-xs leading-5 text-token-text-secondary">{detail}</div>
        </div>
        {action}
      </div>
    </div>
  );
}

function RoomEmptyState({ title, detail }: { title: string; detail: string }) {
  return (
    <OcEmptyState
      className="mt-4 min-h-32 rounded-xl border border-token-border p-6"
      detail={detail}
      glyph="ee"
      size="compact"
      title={title}
    />
  );
}

function projectRooms(
  snapshot: TopologySnapshot,
  projectNames: Map<string, string>,
  chatRuns: readonly ChatRun[],
  surfaces: AdmittedClientSurfaceDocument[],
  projectionLive: boolean,
  fixture: boolean,
): RoomRow[] {
  const chats = new Map(chatRuns.map((run) => [run.runId, run]));
  const homeEeIds = new Set(chatRuns.filter((run) => run.clientSurfaceRole === "home").map((run) => run.eeId).filter(Boolean));
  const taskEeIds = new Set(chatRuns.filter((run) => run.clientSurfaceRole === "task" && !homeEeIds.has(run.eeId)).map((run) => run.eeId).filter(Boolean));
  const formed = new Map<string, { projectId: string; projectName: string; run: RunProjection | null; room: WorkerEnvironmentProjection }>();
  for (const environment of snapshot.environments) {
    const projectName = projectNames.get(environment.id) || environment.name;
    for (const run of environment.runs) {
      for (const room of run.workerEnvironments) {
        if (room.id) formed.set(room.id, { projectId: environment.id, projectName, run, room });
      }
    }
    for (const room of environment.workerEnvironments) {
      if (!room.id || formed.has(room.id)) continue;
      const run = room.runId ? environment.runs.find((candidate) => candidate.id === room.runId) ?? null : null;
      formed.set(room.id, { projectId: environment.id, projectName, run, room });
    }
  }
  return [...formed.values()].map(({ projectId, projectName, run, room }): RoomRow => {
    const runId = run?.id || room.runId;
    const chat = chats.get(runId);
    const eeId = run?.eeId || room.eeId || projectId;
    const section: RoomSection = chat ? (chat.clientSurfaceRole === "home" ? "home" : "tasks") : taskEeIds.has(eeId) ? "tasks" : "system";
    const usedBy = chat ? (chat.clientSurfaceRole === "home" ? "Home" : chat.title) : runId ? `Run ${shortId(runId, 12, 5)}` : "No run";
    const presentation = roomPresentation(room, surfaces);
    const state = roomState(room, projectionLive, fixture);
    const clientSurfaceTitles = workerEnvironmentClientSurfaces(room.id, surfaces).map((surface) => surface.document.title);
    return {
      key: `${projectId}:${room.id}`,
      room,
      runId,
      projectId,
      projectName,
      section,
      usedBy,
      openRole: chat ? chat.clientSurfaceRole : null,
      ...presentation,
      state,
      searchText: [
        presentation.title, presentation.detail, usedBy, projectName, room.kind, room.root, room.status, room.lifecycle, state.label,
        ...room.tools, ...room.capabilities, ...room.surfaces.map((surface) => `${surface.label} ${surface.root}`), ...clientSurfaceTitles,
      ].join("\n").toLowerCase(),
    };
  }).sort((left, right) => roomRecency(right.room) - roomRecency(left.room));
}

function roomPresentation(room: WorkerEnvironmentProjection, surfaces: AdmittedClientSurfaceDocument[]): { title: string; detail: string; glyph: GlyphName } {
  const projectedSurface = primaryWorkerEnvironmentSurface(room);
  const document = primaryWorkerEnvironmentClientSurface(room.id, surfaces);
  const surface = projectedSurface && workerEnvironmentPanelKind(projectedSurface) !== "connection" ? projectedSurface : null;
  const data = surface?.data ?? {};
  const location = [
    typeof data.url === "string" ? data.url : "",
    surface?.viewerUrl ?? "",
    typeof data.branch === "string" ? data.branch : "",
    document?.document.title ?? "",
  ].map((value) => value.trim()).find(Boolean);
  const roots = Array.from(new Set(room.surfaces.map((candidate) => candidate.root.trim()).filter(Boolean)));
  const detail = location || toolSummary(room.tools) || roots[0] || room.root;
  return {
    title: sentenceCase(room.kind) || "Room",
    detail: detail && roots.length > 1 && detail === roots[0] ? `${detail} +${roots.length - 1} more` : detail,
    glyph: surface ? workerEnvironmentSurfaceGlyph(surface) : document?.document.glyph ?? "ee",
  };
}

function toolSummary(tools: string[]): string {
  const groups = new Map<string, string[]>();
  for (const tool of tools) {
    const base = baseDynamicToolId(tool.trim());
    if (!base) continue;
    const separator = base.indexOf(".");
    const family = separator > 0 ? base.slice(0, separator) : base;
    const operation = separator > 0 ? base.slice(separator + 1) : "";
    const operations = groups.get(family) ?? [];
    if (operation && !operations.includes(operation)) operations.push(operation);
    groups.set(family, operations);
  }
  if (groups.size === 0) return "";
  if (groups.size === 1) {
    const [[family, operations]] = [...groups.entries()];
    return operations.length ? `${sentenceCase(family)}: ${operations.map(spacedIdentifier).join(", ")}` : sentenceCase(family);
  }
  return `${[...groups.keys()].map(spacedIdentifier).join(", ")} tools`.replace(/^./, (letter) => letter.toUpperCase());
}

function roomState(room: WorkerEnvironmentProjection, projectionLive: boolean, fixture: boolean): RoomState {
  const lifecycle = room.lifecycle.toLowerCase();
  const status = room.status.toLowerCase();
  const detail = Array.from(new Set([lifecycle, status].filter(Boolean))).map(sentenceCase).join(" · ");
  const failed = [lifecycle, status].some((value) => ["failed", "blocked", "error", "denied"].includes(value));
  const disconnected = lifecycle === "active" && ["closed", "released", "offline"].includes(status);
  if (failed) return { filter: "attention", label: "Failed", tone: "danger", detail };
  if (disconnected) return { filter: "attention", label: "Disconnected", tone: "danger", detail };
  if (lifecycle === "released") return { filter: "closed", label: "Closed", tone: "neutral", detail };
  if (fixture) return { filter: "other", label: "Sample", tone: "neutral", detail };
  if (!projectionLive) return { filter: "other", label: "Last seen", tone: "neutral", detail };
  if (room.live) return { filter: "open", label: "Open", tone: "success", detail };
  return { filter: "other", label: sentenceCase(status || lifecycle) || "Unknown", tone: "neutral", detail };
}

function roomRecency(room: WorkerEnvironmentProjection): number {
  const numeric = Number(room.revision);
  if (Number.isFinite(numeric)) return numeric;
  const timestamp = Date.parse(room.revision);
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

function spacedIdentifier(value: string): string {
  return value.trim().replace(/[._-]+/g, " ");
}

function sentenceCase(value: string): string {
  return spacedIdentifier(value).toLowerCase().replace(/^./, (letter) => letter.toUpperCase());
}
