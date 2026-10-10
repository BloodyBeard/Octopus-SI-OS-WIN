import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import { useSetClientPreferences, useClientPreferences } from "../../clientPreferences";
import { OcIconButton, OcMenuItem, OcMenuSectionLabel } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { OctopusNavSection } from "../navigation/OctopusNav";
import { OctopusSpinner } from "../navigation/OctopusSpinner";
import { OctopusMarquee } from "../navigation/OctopusMarquee";
import { AgentPresetLogo } from "../agent/AgentPresetLogo";
import type { MarkActivityItem } from "./markActivity";
import type { MarkActivity } from "./useMarkActivity";
import "./markActivity.css";

export function MarkActivityView({ activity, activeId, onSelect }: {
  activity: MarkActivity;
  activeId: string;
  onSelect: (item: MarkActivityItem) => void;
}) {
  const [visibleCount, setVisibleCount] = useState(10);
  let remaining = visibleCount;
  const groups = activity.groups.flatMap((group) => {
    const items = group.items.slice(0, remaining);
    remaining -= items.length;
    return group.key === "priority" || items.length ? [{ ...group, items }] : [];
  });
  const hasMore = activity.groups.reduce((count, group) => count + group.items.length, 0) > visibleCount;
  return (
    <div className="mark-activity px-row-x" aria-label="Inbox">
      <div className="flex flex-col" role="list">
        {groups.map((group, index) => (
          <Fragment key={group.key}>
            {index > 0 ? <div aria-hidden="true" className="h-4 shrink-0" /> : null}
            <ActivitySection heading={<OctopusNavSection title={group.key === "priority" ? <div className="py-0.5">{group.label}</div> : group.label} titleActions={index === 0 ? <ActivityOptions activity={activity} /> : undefined}>{null}</OctopusNavSection>}>
              {index === 0 && activity.error ? <div className="mark-activity-message" role="alert">{activity.error}</div> : null}
              {index === 0 && !activity.loaded && !activity.error ? <div className="mark-activity-message" role="status">Loading Inbox…</div> : null}
              {group.key === "priority" && activity.loaded && !activity.error && !group.items.length ? <div className="mark-activity-empty">Nothing needs attention</div> : null}
              {group.items.map((item) => (
                <div role="listitem" key={item.id}>
                  <ActivityRow active={activeId === item.id} item={item} unread={!activity.read.has(item.id)} onSelect={() => onSelect(item)} />
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

function ActivitySection({ children, heading }: { children: ReactNode; heading: ReactNode }) {
  return <div className="flex flex-col">
    <div className="mark-activity-heading">
      <div aria-hidden="true" className="mark-activity-heading-blur" />
      {heading}
    </div>
    <div className="mark-activity-content"><div className="mark-activity-clip-window"><div className="mark-activity-clip-content flex flex-col gap-px">{children}</div></div></div>
  </div>;
}

function ActivityRow({ active, item, unread, onSelect }: { active: boolean; item: MarkActivityItem; unread: boolean; onSelect: () => void }) {
  const agentName = item.agentName || item.worker;
  return <div
    aria-current={active ? "page" : undefined}
    aria-label={[item.title, agentName, unread && "Unread"].filter(Boolean).join(", ")}
    className="mark-activity-row sidebar-item group"
    data-active={active || undefined}
    data-sidebar-nav-item=""
    data-title-aligned-trailing-rail={Boolean(agentName) || undefined}
    data-multiline={Boolean(agentName) || undefined}
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
      <div className="flex min-w-0 flex-1 items-center gap-2">
        <div className="flex min-w-0 flex-1 self-stretch flex-col items-start gap-0.5 text-base leading-5" data-thread-title-trigger="">
          <div className="flex w-full min-w-0 flex-1 items-center gap-1">
            <OctopusMarquee className="min-w-0 flex-1 select-none" animateOnGroupHover data-thread-title=""><span dir="auto">{item.title}</span></OctopusMarquee>
            {unread ? <div className="mark-activity-unread" aria-hidden="true"><span className="icon-xs relative scale-50"><span className="mark-activity-unread-dot" /></span></div> : null}
          </div>
          {agentName ? <span className="mark-activity-secondary line-clamp-2 w-full pe-row-y text-xs leading-4 select-none" data-thread-secondary-title=""><span className="inline-flex items-center gap-1">{item.agentIcon ? <AgentPresetLogo color={item.agentColor} icon={item.agentIcon} size={12} /> : null}<span>{agentName}</span></span></span> : null}
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
      <OcMenuSectionLabel>Additional marks</OcMenuSectionLabel>
      {activity.kinds.map((name) => <OcMenuItem aria-checked={mode === "all" || mode === "custom" && Boolean(types?.includes(name))} key={name} label={name} onClick={() => toggle(name)} role="menuitemcheckbox" selected={mode === "all" || mode === "custom" && Boolean(types?.includes(name))} />)}
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
