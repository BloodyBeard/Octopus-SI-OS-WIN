import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import { useSetClientPreferences, useClientPreferences } from "../../clientPreferences";
import { OcIconButton, OcMenuItem, OcMenuSectionLabel } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { OctopusNavSection } from "../navigation/OctopusNav";
import { OctopusSpinner } from "../navigation/OctopusSpinner";
import { OctopusMarquee } from "../navigation/OctopusMarquee";
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
    className="mark-activity-row sidebar-item group"
    data-active={active || undefined}
    data-activity-kind={item.kind}
    data-sidebar-nav-item=""
    data-title-aligned-trailing-rail=""
    data-multiline=""
    onClick={(event) => { if (!event.defaultPrevented && event.button === 0) onSelect(); }}
    onKeyDown={(event) => {
      if (event.target !== event.currentTarget || (event.key !== "Enter" && event.key !== " ")) return;
      event.preventDefault();
      onSelect();
    }}
    role="button"
    tabIndex={0}
  >
    <div className="flex h-full w-full items-center text-sm leading-4">
      <div className="flex min-w-0 flex-1 items-start gap-2.5">
        <span className="mark-activity-glyph"><ActivityMark item={item} size={16} /></span>
        <div className="flex min-w-0 flex-1 self-stretch flex-col items-start gap-0.5 text-base leading-5" data-thread-title-trigger="">
          <div className="flex w-full min-w-0 flex-1 items-center gap-1.5">
            <OctopusMarquee className="min-w-0 flex-1 select-none" animateOnGroupHover data-thread-title=""><span dir="auto">{title}</span></OctopusMarquee>
            {count > 1 ? <span className="mark-activity-count">{`×${count}`}</span> : null}
            {unread ? <div className="mark-activity-unread" aria-hidden="true"><span className="icon-xs relative scale-50"><span className="mark-activity-unread-dot" /></span></div> : null}
          </div>
          <span className="mark-activity-secondary flex w-full min-w-0 items-center gap-1 pe-row-y text-xs leading-4 select-none" data-thread-secondary-title="">
            <span className="min-w-0 truncate" data-request={isActivityRequest(item) || undefined}>{summary}</span>
            {time ? <><span aria-hidden="true">·</span><time className="shrink-0" dateTime={item.createdAt ? new Date(item.createdAt).toISOString() : undefined}>{time}</time></> : null}
          </span>
        </div>
      </div>
    </div>
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
    <OcIconButton appearance="sidebar" glyph="clearRead" label="Clear Inbox" title={!activity.loaded || activity.error ? "Inbox read state is unavailable" : !activity.unreadCount ? "No notifications to clear" : undefined} disabled={!activity.unreadCount || !activity.loaded || Boolean(activity.error)} onClick={activity.clearNotifications} />
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
