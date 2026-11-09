import { OcSkeleton } from "../../design-system/LoadingSkeleton";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { OcButton, OcIconButton, OcMenuItem, OcMenuSectionLabel } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { MessageTooltip } from "../chat/MessageTooltip";
import { OctopusNavItem, OctopusNavSection } from "../navigation/OctopusNav";
import { describeSchedule, nextScheduledRunAtMs } from "./scheduleModel";
import { ScheduledRunRows, ScheduledTaskRow, ShowMoreButton, scheduledRowStatus, type ScheduledRowStatus } from "./ScheduledRows";
import type { ScheduledDelivery, ScheduledInterfaceProjection } from "./types";
import { useScheduledWorkspace } from "./ScheduledWorkspace";

export { scheduledCalendarTime } from "./ScheduledRows";

const STATUSES: ReadonlyArray<{ value: ScheduledRowStatus; label: string }> = [
  { value: "active", label: "Active" },
  { value: "paused", label: "Paused" },
  { value: "completed", label: "Completed" },
  { value: "invalid", label: "Needs attention" },
];
const STATUS_RANK: Record<ScheduledRowStatus, number> = { active: 0, paused: 1, completed: 2, invalid: 3 };
const VISIBLE_ROWS = 5;

export function ScheduledSidebar({ onNavigate, onSelectChat }: { onNavigate?: () => void; onSelectChat?: (runId: string) => void }) {
  const workspace = useScheduledWorkspace();
  const searchId = useId();
  const [search, setSearch] = useState<string | null>(null);
  const [statuses, setStatuses] = useState<ScheduledRowStatus[]>(STATUSES.map((status) => status.value));
  const [filterOpen, setFilterOpen] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [now, setNow] = useState(Date.now);
  const searchRef = useRef<HTMLInputElement>(null);
  const searchButtonRef = useRef<HTMLButtonElement>(null);
  const filterRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);
  useEffect(() => {
    if (workspace.searchRequest) { setSearch(""); searchRef.current?.focus(); }
  }, [workspace.searchRequest]);
  useEffect(() => { if (search !== null) searchRef.current?.focus(); }, [search === null]);
  const query = search?.trim().toLocaleLowerCase() ?? "";
  const allStatuses = statuses.length === STATUSES.length;
  const rows = useMemo(() => workspace.tasks
    .map((task) => ({ task, status: scheduledRowStatus(task), next: nextScheduledRunAtMs(task, now) }))
    .filter(({ task, status }) => statuses.includes(status) && (!query || [task.displayName, task.interfaceId, task.instructions, describeSchedule(task)]
      .some((value) => value.toLocaleLowerCase().includes(query))))
    .sort((left, right) => STATUS_RANK[left.status] - STATUS_RANK[right.status] || (left.next ?? Infinity) - (right.next ?? Infinity) || left.task.displayName.localeCompare(right.task.displayName)),
  [workspace.tasks, statuses, query, now]);
  const activeCount = rows.filter((row) => row.status === "active").length;
  const listed = !expanded && !query && allStatuses && activeCount > 0 && activeCount < VISIBLE_ROWS
    ? rows.filter((row) => row.status === "active" || row.task.interfaceId === workspace.selectedId) : rows;
  const selectedIndex = listed.findIndex((row) => row.task.interfaceId === workspace.selectedId);
  const shown = expanded || query ? listed : listed.slice(0, Math.max(VISIBLE_ROWS, selectedIndex + 1));
  const hasMore = !expanded && !query && (shown.length < listed.length || listed.length < rows.length);
  const heading = statuses.length === 1 && statuses[0] !== "active" ? STATUSES.find((status) => status.value === statuses[0])?.label : "Upcoming";

  function toggleStatus(status: ScheduledRowStatus) {
    const next = statuses.includes(status) ? statuses.filter((value) => value !== status) : [...statuses, status];
    setStatuses(next.length ? STATUSES.map((entry) => entry.value).filter((value) => next.includes(value)) : statuses);
  }

  const filter = (
    <>
      <MessageTooltip label="Filter">
        <OcIconButton appearance="sidebar" aria-expanded={filterOpen} aria-haspopup="menu" buttonRef={filterRef} glyph="filter" label="Filter tasks" onClick={() => setFilterOpen(!filterOpen)} selected={!allStatuses} title="" />
      </MessageTooltip>
      <OcDropdown align="start" anchorRef={filterRef} aria-label="Trigger status" onOpenChange={setFilterOpen} open={filterOpen}>
        <OcMenuSectionLabel>Filter by</OcMenuSectionLabel>
        {STATUSES.map(({ value, label }) => (
          <OcMenuItem aria-checked={statuses.includes(value)} key={value} label={label} onClick={(event) => { event.preventDefault(); toggleStatus(value); }} role="menuitemcheckbox" selected={statuses.includes(value)} />
        ))}
      </OcDropdown>
    </>
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col select-none" data-scheduled-sidebar="">
      <div className="octopus-sidebar-scroll sidebar-page-scroll vertical-scroll-fade-mask mb-(--sidebar-footer-height) flex min-h-0 flex-1 flex-col gap-2 overflow-x-hidden overflow-y-auto">
        <div className="relative z-10 flex shrink-0 flex-col px-row-x">
          <div className="@container/navigation-header ms-2 flex h-[var(--sidebar-navigation-header-height,calc(var(--spacing)*8))] items-center pe-1">
            <h2 className="m-0 min-w-0 flex-1 truncate text-[18px] leading-6 font-semibold text-default">
              <button className="max-w-full cursor-interaction truncate border-0 bg-transparent p-0 text-start [font:inherit] text-inherit focus-visible:rounded-sm focus-visible:outline-2 focus-visible:outline-ring" onClick={() => { workspace.selectTask(""); onNavigate?.(); }} type="button">Triggers</button>
            </h2>
            <div className="flex shrink-0 items-center gap-1">
              <MessageTooltip label="Search triggers">
                <OcIconButton appearance="sidebar" aria-controls={searchId} aria-expanded={search !== null} buttonRef={searchButtonRef} glyph="search" label="Search triggers" onClick={() => setSearch(search === null ? "" : null)} selected={search !== null} title="" />
              </MessageTooltip>
            </div>
          </div>
          {search !== null ? (
            <div className="pt-1" data-codex-escape-boundary="">
              <label className="no-drag flex h-9 shrink-0 items-center gap-2 rounded-full bg-background-secondary-soft-alpha px-3 py-0 text-base leading-[18px]" htmlFor={searchId}>
                <Glyph className="shrink-0 text-secondary" name="search" size={16} />
                <span className="sr-only">Search triggers</span>
                <input
                  className="min-w-0 flex-1 bg-transparent text-base leading-[18px] text-default outline-none select-text placeholder:text-tertiary [&::placeholder]:select-none"
                  id={searchId}
                  onChange={(event) => setSearch(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key !== "Escape") return;
                    event.preventDefault(); event.stopPropagation();
                    setSearch(null); searchButtonRef.current?.focus();
                  }}
                  placeholder="Search triggers"
                  ref={searchRef}
                  type="search"
                  value={search}
                />
              </label>
            </div>
          ) : null}
          <div className="group/nav-list mt-2 mb-2 flex flex-col gap-px">
            <OctopusNavItem
              disabled={Boolean(workspace.createUnavailableReason) || workspace.pending}
              glyph="plus"
              label="New trigger"
              onClick={() => { workspace.startCreate(); onNavigate?.(); }}
            />
          </div>
        </div>
        <div className="flex flex-col gap-4 px-row-x pb-row-x">
          <OctopusNavSection title={heading} titleActions={filter} titleRowClassName="ps-2 pe-1.5">
            <div aria-label="Triggers" className="flex flex-col gap-px" role="list">
              {shown.map(({ task }) => (
                <TriggerListItem
                  key={task.interfaceId}
                  now={now}
                  onNavigate={onNavigate}
                  onOpenRun={onSelectChat ? (delivery) => { onSelectChat(delivery.runId); onNavigate?.(); } : undefined}
                  task={task}
                />
              ))}
            </div>
            {hasMore ? <div className="flex gap-1 py-1"><ShowMoreButton onClick={() => setExpanded(true)}>Show more</ShowMoreButton></div> : null}
            {workspace.error ? (
              <div className="flex flex-col gap-2 p-2 text-sm text-secondary" role="alert">
                Triggers could not be loaded
                <OcButton className="self-start" onClick={() => void workspace.reload()} size="sm" variant="ghost">Try again</OcButton>
              </div>
            ) : workspace.loading && !workspace.tasks.length ? <LoadingRows />
              : !shown.length ? (
                <div className={query || !allStatuses ? "p-2 text-sm text-secondary" : "px-2 py-1 text-sm text-tertiary"} role="status">
                  {query || !allStatuses ? "No matching triggers" : "No triggers"}
                </div>
              ) : null}
          </OctopusNavSection>
        </div>
      </div>
    </div>
  );
}

function TriggerListItem({ now, onNavigate, onOpenRun, task }: {
  now: number;
  onNavigate?: () => void;
  onOpenRun?: (delivery: ScheduledDelivery) => void;
  task: ScheduledInterfaceProjection;
}) {
  const workspace = useScheduledWorkspace();
  const [collapsed, setCollapsed] = useState(false);
  const selected = workspace.selectedId === task.interfaceId;
  const canExpand = task.deliveries.length > 0;
  const expanded = selected && canExpand && !collapsed;
  const paused = task.status === "disabled";
  return (
    <div className="flex flex-col gap-px" data-selected-scheduled-task={selected || undefined} role="listitem">
      <ScheduledTaskRow
        actions={task.eventSource ? [] : [
          { glyph: "edit", label: "Edit", disabled: !workspace.canEdit || !task.valid || workspace.pending, onSelect: () => workspace.startEdit(task) },
          ...(scheduledRowStatus(task) === "completed" ? [] : [{ glyph: paused ? "play" as const : "pause" as const, label: paused ? "Resume" : "Pause", disabled: !workspace.canEdit || !task.valid || workspace.pending, onSelect: () => workspace.toggleTask(task) }]),
          { glyph: "trash", label: "Delete", disabled: !workspace.canEdit || workspace.pending, tone: "danger", onSelect: () => workspace.requestDelete(task) },
        ]}
        canExpand={canExpand}
        expanded={expanded}
        now={now}
        onActivate={() => {
          if (selected && canExpand) { setCollapsed(!collapsed); return; }
          setCollapsed(false);
          workspace.selectTask(task.interfaceId);
          onNavigate?.();
        }}
        selected={selected}
        task={task}
      />
      {expanded ? <div className="flex flex-col gap-px"><ScheduledRunRows deliveries={task.deliveries} onOpen={(delivery) => onOpenRun?.(delivery)} onUndo={workspace.requestUndo} openable={Boolean(onOpenRun)} /></div> : null}
    </div>
  );
}

function LoadingRows() {
  return (
    <div className="flex flex-col gap-px" role="status">
      <span className="sr-only">Loading triggers…</span>
      {[32, 24, 28].map((width) => (
        <div aria-hidden="true" className="flex h-[var(--height-token-row)] w-full items-center gap-3 px-2" key={width}>
          <OcSkeleton size="lg" width={width * 4} />
          <OcSkeleton className="ms-auto shrink-0" size="sm" width={48} />
        </div>
      ))}
    </div>
  );
}
