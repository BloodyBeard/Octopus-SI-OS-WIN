import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import { useSetClientPreferences, useClientPreferences } from "../../clientPreferences";
import { OcIconButton, OcMenuItem, OcMenuSectionLabel } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { OctopusNavSection } from "../navigation/OctopusNav";
import { OctopusSpinner } from "../navigation/OctopusSpinner";
import { ActivityMark } from "./ActivityMark";
import type { MarkActivityItem } from "./markActivity";
import { activityHeadline, activityPresentation, activitySections, activitySummary, isActivityRequest, relativeActivityTime, type ActivityRowGroup } from "./markPresentation";
import type { MarkActivity } from "./useMarkActivity";
import "./markActivity.css";

export function MarkActivityView({ activity, activeId, onSelect }: {
  activity: MarkActivity;
  activeId: string;
  onSelect: (item: MarkActivityItem, ids: readonly string[]) => void;
}) {
  const [visibleCount, setVisibleCount] = useState(10);
  const now = useActivityClock();
  const items = activity.groups.flatMap((group) => group.items);
  const sections = activitySections(items, now);
  let remaining = visibleCount;
  const visible = sections.flatMap((section) => {
    const rows = section.rows.slice(0, Math.max(remaining, 0));
    remaining -= rows.length;
    return rows.length ? [{ ...section, rows }] : [];
  });
  const hasMore = sections.reduce((count, section) => count + section.rows.length, 0) > visibleCount;
  const options = <ActivityOptions activity={activity} />;
  return (
    <div className="mark-activity px-row-x" aria-label="Inbox">
      <div className="flex flex-col" role="list">
        {!visible.length ? (
          <ActivitySection heading={<OctopusNavSection title={<div className="py-0.5">Inbox</div>} titleActions={options}>{null}</OctopusNavSection>}>
            {activity.error ? <div className="mark-activity-message" role="alert">{activity.error}</div> : null}
            {!activity.loaded && !activity.error ? <div className="mark-activity-message" role="status">Loading Inbox…</div> : null}
            {activity.loaded && !activity.error ? <div className="mark-activity-empty">You're all caught up</div> : null}
          </ActivitySection>
        ) : visible.map((section, index) => (
          <Fragment key={section.key}>
            {index > 0 ? <div aria-hidden="true" className="h-3 shrink-0" /> : null}
            <ActivitySection heading={<OctopusNavSection title={<div className="py-0.5">{section.label}</div>} titleActions={index === 0 ? options : undefined}>{null}</OctopusNavSection>}>
              {index === 0 && activity.error ? <div className="mark-activity-message" role="alert">{activity.error}</div> : null}
              {section.rows.map((row) => (
                <div role="listitem" key={row.id}>
                  <ActivityRow active={row.items.some((item) => item.id === activeId)} now={now} row={row}
                    unread={row.items.some((item) => !activity.read.has(item.id))}
                    onSelect={() => onSelect(row.lead, row.items.map((item) => item.id))} />
                </div>
              ))}
            </ActivitySection>
          </Fragment>
        ))}
        {hasMore && !activity.error ? <ActivityNextPage key={visibleCount} onLoad={() => setVisibleCount((count) => count + 10)} /> : null}
      </div>
    </div>
  );
}

function useActivityClock() {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);
  return now;
}

function ActivitySection({ children, heading }: { children: ReactNode; heading: ReactNode }) {
  return <div className="flex flex-col">
    <div className="mark-activity-heading">
      <div aria-hidden="true" className="mark-activity-heading-blur" />
      {heading}
    </div>
    <div className="mark-activity-content"><div className="mark-activity-clip-window"><div className="mark-activity-clip-content flex flex-col gap-px">{children}</div></div></div>
  </div>;
}

function ActivityRow({ active, now, row, unread, onSelect }: { active: boolean; now: number; row: ActivityRowGroup; unread: boolean; onSelect: () => void }) {
  const item = row.lead;
  const title = activityHeadline(item);
  const summary = activitySummary(item);
  const time = relativeActivityTime(item.createdAt, now);
  const count = row.items.length;
  return <div
    aria-current={active ? "page" : undefined}
    aria-label={[title, count > 1 && `${count} times`, summary, unread && "Unread"].filter(Boolean).join(", ")}
    className="mark-activity-row"
    data-active={active || undefined}
    data-activity-kind={item.kind}
    data-read={!unread || undefined}
    data-sidebar-nav-item=""
    onClick={(event) => { if (!event.defaultPrevented && event.button === 0) onSelect(); }}
    onKeyDown={(event) => {
      if (event.target !== event.currentTarget || (event.key !== "Enter" && event.key !== " ")) return;
      event.preventDefault();
      onSelect();
    }}
    role="button"
    tabIndex={0}
  >
    <span className="mark-activity-icon"><ActivityMark item={item} size={20} glyphSize={18} /></span>
    <span className="mark-activity-text">
      <span className="mark-activity-top">
        <span className="mark-activity-title" dir="auto">{title}</span>
        {count > 1 ? <span className="mark-activity-count">{`×${count}`}</span> : null}
        {time ? <time dateTime={item.createdAt ? new Date(item.createdAt).toISOString() : undefined}>{time}</time> : null}
      </span>
      <span className="mark-activity-sub">
        <span data-request={isActivityRequest(item) || undefined}>{summary}</span>
        {unread ? <i className="mark-activity-dot" aria-hidden="true" /> : null}
      </span>
    </span>
  </div>;
}

function ActivityOptions({ activity }: { activity: MarkActivity }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const mode = preferences.markActivityMode;
  const types = preferences.markActivityTypes;
  const toggle = (name: string) => {
    const selected = new Set(mode === "all" ? activity.kinds : mode === "custom" ? types ?? [] : []);
    if (selected.has(name)) selected.delete(name); else selected.add(name);
    setClientPreferences({ markActivityMode: "custom", markActivityTypes: [...selected] });
  };
  return <div className="flex items-center gap-1">
    <OcIconButton appearance="sidebar" glyph="clearRead" label="Clear Inbox" title={!activity.loaded || activity.error ? "Inbox read state is unavailable" : !activity.listedCount ? "No notifications to clear" : undefined} disabled={!activity.listedCount || !activity.loaded || Boolean(activity.error)} onClick={activity.clearNotifications} />
    <OcIconButton appearance="sidebar" glyph="activityOptions" label="Inbox options" aria-expanded={open} aria-haspopup="menu" data-state={open ? "open" : "closed"} onClick={() => setOpen(!open)} buttonRef={anchor} />
    <OcDropdown anchorRef={anchor} align="start" contentWidth="sidebar" modal open={open} onOpenChange={setOpen} aria-label="Inbox options">
      <OcMenuSectionLabel>Show</OcMenuSectionLabel>
      <OcMenuItem aria-checked={mode === "attention"} label="Messages and requests" role="menuitemradio" selected={mode === "attention"} onClick={() => setClientPreferences({ markActivityMode: "attention" })} />
      <OcMenuItem aria-checked={mode === "all"} label="All activity" role="menuitemradio" selected={mode === "all"} onClick={() => setClientPreferences({ markActivityMode: "all" })} />
      <OcMenuSectionLabel>Also show</OcMenuSectionLabel>
      {activity.kinds.map((name) => <OcMenuItem aria-checked={mode === "all" || mode === "custom" && Boolean(types?.includes(name))} key={name} label={markKindLabel(name)} description={name} onClick={() => toggle(name)} role="menuitemcheckbox" selected={mode === "all" || mode === "custom" && Boolean(types?.includes(name))} />)}
    </OcDropdown>
  </div>;
}

function ActivityNextPage({ onLoad }: { onLoad: () => void }) {
  const sentinel = useRef<HTMLDivElement>(null);
  const callback = useRef(onLoad);
  callback.current = onLoad;
  useEffect(() => {
    const element = sentinel.current;
    if (!element) return;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      observer.disconnect();
      callback.current();
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return <div role="listitem"><div className="flex w-full justify-center py-3" ref={sentinel}><OctopusSpinner className="icon-xs text-secondary" /></div></div>;
}

function markKindLabel(name: string): string {
  return activityPresentation({ kind: "mark", name } as MarkActivityItem).label;
}
