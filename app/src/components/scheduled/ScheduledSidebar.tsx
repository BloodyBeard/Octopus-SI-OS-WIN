import { useEffect, useId, useMemo, useRef, useState } from "react";
import { OcButton, OcIconButton, OcMenuItem, OcSearchField } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { MessageTooltip } from "../chat/MessageTooltip";
import { describeSchedule, nextScheduledRunAtMs } from "./scheduleModel";
import type { ScheduledInterfaceProjection } from "./types";
import { useScheduledWorkspace } from "./ScheduledWorkspace";
import "./scheduled.css";

type StatusFilter = "all" | ScheduledInterfaceProjection["status"];
const statusFilters: Array<{ value: StatusFilter; label: string }> = [
  { value: "all", label: "All tasks" },
  { value: "enabled", label: "Active" },
  { value: "disabled", label: "Paused" },
  { value: "invalid", label: "Needs attention" },
];

export function ScheduledSidebar({ onNavigate }: { onNavigate?: () => void }) {
  const workspace = useScheduledWorkspace();
  const searchId = useId();
  const [search, setSearch] = useState<string | null>(null);
  const [filter, setFilter] = useState<StatusFilter>("all");
  const [filterOpen, setFilterOpen] = useState(false);
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
  const visible = useMemo(() => {
    const needle = search?.trim().toLocaleLowerCase() ?? "";
    return workspace.tasks.filter((task) => (filter === "all" || task.status === filter)
      && (!needle || [task.displayName, task.interfaceId, task.instructions, describeSchedule(task), task.timezone]
        .some((value) => value.toLocaleLowerCase().includes(needle))))
      .map((task) => ({ task, next: nextScheduledRunAtMs(task, now) }))
      .sort((a, b) => (a.next ?? Infinity) - (b.next ?? Infinity) || a.task.displayName.localeCompare(b.task.displayName));
  }, [workspace.tasks, filter, search, now]);

  return (
    <div className="scheduled-sidebar">
      <div className="scheduled-sidebar-actions">
        <OcButton className="scheduled-sidebar-new" disabled={Boolean(workspace.createUnavailableReason) || workspace.pending} leadingIcon="plus" onClick={() => { workspace.startCreate(); onNavigate?.(); }} title={workspace.createUnavailableReason || undefined} variant="ghost">New task</OcButton>
        <MessageTooltip label="Search scheduled tasks">
          <OcIconButton appearance="sidebar" aria-controls={searchId} aria-expanded={search !== null} buttonRef={searchButtonRef} glyph="search" label="Search scheduled tasks" onClick={() => setSearch(search === null ? "" : null)} selected={search !== null} title="" />
        </MessageTooltip>
      </div>
      {search !== null ? (
        <div className="scheduled-sidebar-search" onKeyDown={(event) => {
          if (event.key === "Escape") { event.stopPropagation(); setSearch(null); searchButtonRef.current?.focus(); }
        }}>
          <OcSearchField id={searchId} inputRef={searchRef} label="Search scheduled tasks" onChange={setSearch} placeholder="Search scheduled tasks" value={search} />
        </div>
      ) : null}
      <div className="scheduled-sidebar-section-title">
        <h2>{filter === "disabled" ? "Paused" : filter === "invalid" ? "Needs attention" : "Upcoming"}</h2>
        <MessageTooltip label="Filter tasks">
          <OcIconButton appearance="sidebar" buttonRef={filterRef} glyph="filter" label="Filter tasks" onClick={() => setFilterOpen(!filterOpen)} selected={filter !== "all"} title="" />
        </MessageTooltip>
        <OcDropdown anchorRef={filterRef} align="end" aria-label="Scheduled task status" onOpenChange={setFilterOpen} open={filterOpen}>
          {statusFilters.map(({ value, label }) => <OcMenuItem aria-checked={filter === value} key={value} label={label} onClick={() => { setFilter(value); setFilterOpen(false); }} role="menuitemradio" selected={filter === value} />)}
        </OcDropdown>
      </div>
      <div className="scheduled-sidebar-list octopus-sidebar-scroll" role="list" aria-label="Scheduled tasks">
        {workspace.error ? <div className="scheduled-sidebar-notice" role="alert"><p>Scheduled tasks could not be loaded.</p><OcButton onClick={() => void workspace.reload()} size="sm" variant="ghost">Retry</OcButton></div>
          : workspace.loading && !workspace.tasks.length ? <p className="scheduled-sidebar-notice" role="status">Loading scheduled tasks…</p>
            : visible.length ? (filter === "all" ? <>
              <ScheduleGroup rows={visible.filter(({ task }) => task.status === "enabled")} now={now} onNavigate={onNavigate} />
              <ScheduleGroup label="Paused" rows={visible.filter(({ task }) => task.status === "disabled")} now={now} onNavigate={onNavigate} />
              <ScheduleGroup label="Needs attention" rows={visible.filter(({ task }) => task.status === "invalid")} now={now} onNavigate={onNavigate} />
            </> : <ScheduleGroup rows={visible} now={now} onNavigate={onNavigate} />)
              : <p className="scheduled-sidebar-notice" role="status">{search?.trim() || filter !== "all" ? "No matching tasks" : "No scheduled tasks"}</p>}
      </div>
    </div>
  );
}

function ScheduleGroup({ label, rows, now, onNavigate }: { label?: string; rows: Array<{ task: ScheduledInterfaceProjection; next: number | null }>; now: number; onNavigate?: () => void }) {
  const { selectedId, selectTask, editor, pending } = useScheduledWorkspace();
  const [limit, setLimit] = useState(5);
  const count = Math.max(limit, rows.findIndex(({ task }) => task.interfaceId === selectedId) + 1);
  if (!rows.length) return null;
  return (
    <div className="scheduled-sidebar-group">
      {label ? <h3>{label}</h3> : null}
      {rows.slice(0, count).map(({ task, next }) => {
        const selected = !editor && selectedId === task.interfaceId;
        const timing = task.status === "disabled" ? "Paused" : task.status === "invalid" ? "Needs attention"
          : next ? `${task.nextRunSource !== "" && task.nextRunAtMs === next ? "" : "Estimated · "}${scheduledCalendarTime(next, now, task.timezone)}` : task.schedule?.type === "once" && task.lastRunAtMs ? "Triggered" : "Next run not recorded";
        return <div key={task.interfaceId} role="listitem">
          <button aria-current={selected ? "true" : undefined} className="scheduled-sidebar-row" data-sidebar-nav-item="scheduled" data-selected-scheduled-task={selected || undefined} disabled={pending}
            onClick={() => { selectTask(task.interfaceId); onNavigate?.(); }} title={task.status === "invalid" ? task.error : `${task.displayName}\n${timing} · ${describeSchedule(task)} · ${task.timezone}`} type="button">
            <span className="scheduled-sidebar-row-title">{task.displayName}</span>
            <span className="scheduled-sidebar-row-detail">{timing}{task.schedule ? ` · ${describeSchedule(task)}` : ""}</span>
          </button>
        </div>;
      })}
      {rows.length > count ? <button className="scheduled-sidebar-more" onClick={() => setLimit(count + 10)} type="button">Show more</button> : null}
    </div>
  );
}

export function scheduledCalendarTime(timestamp: number, now: number, timezone: string): string {
  const timeZone = timezone && timezone !== "local" ? timezone : undefined;
  try {
    const day = new Intl.DateTimeFormat("en-CA", { timeZone, year: "numeric", month: "2-digit", day: "2-digit" });
    const calendarDay = (value: number) => {
      const parts = Object.fromEntries(day.formatToParts(new Date(value)).map((part) => [part.type, part.value]));
      return Date.UTC(Number(parts.year), Number(parts.month) - 1, Number(parts.day));
    };
    const dayDifference = (calendarDay(timestamp) - calendarDay(now)) / 86_400_000;
    const dateLabel = dayDifference === 0 ? "Today" : dayDifference === 1 ? "Tomorrow" : new Intl.DateTimeFormat(undefined, { timeZone, month: "short", day: "numeric" }).format(new Date(timestamp));
    return `${dateLabel} ${new Intl.DateTimeFormat(undefined, { timeZone, hour: "numeric", minute: "2-digit" }).format(new Date(timestamp))}`;
  } catch {
    return "Time unavailable";
  }
}
