import { OcButton } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import { ActivityMark } from "./ActivityMark";
import type { MarkActivityItem } from "./markActivity";
import { activityMoment, activityPresentation, activityRows, activitySource, isActivityRequest, relativeActivityTime, type ActivityRowGroup } from "./markPresentation";
import type { MarkActivity } from "./useMarkActivity";
import "./inbox.css";

export type InboxSurfaceState = {
  activity: MarkActivity;
  selected: MarkActivityItem | null;
  selectedGroup?: readonly string[];
  onSelect: (item: MarkActivityItem, ids?: readonly string[]) => void;
  onOpen: (item: MarkActivityItem) => void;
};

export function InboxSurface({ activity, selected, selectedGroup = [], onSelect, onOpen }: InboxSurfaceState) {
  const item = selected && activity.projectedItems.find((candidate) => candidate.id === selected.id);
  const now = Date.now();

  return (
    <section className="inbox-surface" aria-label="Inbox details">
      <div className="inbox-surface-content" data-view={item ? "detail" : "summary"}>
        {activity.error ? (
          <div className="inbox-summary" role="alert">
            <Glyph name="warning" size={28} />
            <h1>Inbox is unavailable</h1>
            <p>{activity.error}</p>
          </div>
        ) : !activity.loaded ? (
          <OctopusLoadingState className="inbox-summary" label="Loading Inbox" />
        ) : selected && !item ? (
          <div className="inbox-summary" role="status"><Glyph name="inbox" size={28} /><h1>This activity is gone</h1><p>Pick another item from your Inbox.</p></div>
        ) : item ? (
          <InboxDetail item={item} group={selectedGroup.includes(item.id) ? selectedGroup : [item.id]} activity={activity} now={now} onOpen={onOpen} onSelect={onSelect} />
        ) : (
          <InboxOverview activity={activity} now={now} onSelect={onSelect} />
        )}
      </div>
    </section>
  );
}

function InboxOverview({ activity, now, onSelect }: {
  activity: MarkActivity;
  now: number;
  onSelect: (item: MarkActivityItem, ids?: readonly string[]) => void;
}) {
  const unread = activity.items.filter((candidate) => !activity.read.has(candidate.id));
  const requests = unread.filter(isActivityRequest);
  const replies = unread.filter((candidate) => candidate.kind === "message");
  const listed = unread.length ? [...requests, ...unread.filter((candidate) => !isActivityRequest(candidate))] : activity.items;
  const rows = activityRows(listed).slice(0, 6);
  const heading = requests.length
    ? `${requests.length} ${requests.length === 1 ? "thing needs" : "things need"} you`
    : unread.length ? "Catch up on your work" : "You're all caught up";
  return (
    <div className="inbox-summary">
      <span className="inbox-summary-icon"><Glyph name="inbox" size={28} /></span>
      <h1>{heading}</h1>
      <p>{unread.length ? "Replies, requests and what Octopus did while you were away." : "New replies and requests will show up here as your work runs."}</p>
      <dl className="inbox-summary-counts">
        <div data-attention={requests.length > 0 || undefined}><dt>Needs you</dt><dd>{requests.length}</dd></div>
        <div><dt>New replies</dt><dd>{replies.length}</dd></div>
        <div><dt>Unread</dt><dd>{unread.length}</dd></div>
      </dl>
      {rows.length ? (
        <section className="inbox-list" aria-label={unread.length ? "Unread" : "Recent"}>
          <h2>{unread.length ? "Unread" : "Recent"}</h2>
          {rows.map((row) => (
            <InboxItemRow key={row.id} now={now} row={row} secondary={rowSecondary(row.lead, true)}
              onSelect={() => onSelect(row.lead, row.items.map((candidate) => candidate.id))} />
          ))}
        </section>
      ) : null}
    </div>
  );
}

function InboxDetail({ item, group, activity, now, onOpen, onSelect }: {
  item: MarkActivityItem;
  group: readonly string[];
  activity: MarkActivity;
  now: number;
  onOpen: (item: MarkActivityItem) => void;
  onSelect: (item: MarkActivityItem, ids?: readonly string[]) => void;
}) {
  const presentation = activityPresentation(item);
  const occurrences = group.map((id) => activity.projectedItems.find((candidate) => candidate.id === id)).filter((candidate): candidate is MarkActivityItem => Boolean(candidate));
  const times = occurrences.map((candidate) => candidate.createdAt).filter((at): at is number => at !== null).sort((left, right) => left - right);
  const source = activitySource(item);
  const homeRun = item.navigation.kind === "run" && item.navigation.clientSurfaceRole === "home";
  const openLabel = item.navigation.kind === "mark" ? "Show in history"
    : homeRun ? "Open Home" : isActivityRequest(item) ? "Review in task" : "Open task";
  const related = activityRows(activity.projectedItems.filter((candidate) => candidate.runId === item.runId && !group.includes(candidate.id))).slice(0, 5);
  return (
    <article className="inbox-detail" key={item.id}>
      <div className="inbox-detail-kind" data-request={isActivityRequest(item) || undefined}>
        <Glyph name={presentation.glyph} size={14} />
        <span>{presentation.category}</span>
      </div>
      <h1>{presentation.label}</h1>
      <div className="inbox-detail-source">
        <ActivityMark item={item} size={16} />
        <span className="inbox-detail-source-name">{source}</span>
        {item.createdAt ? <>
          <span aria-hidden="true">·</span>
          <time dateTime={new Date(item.createdAt).toISOString()}>{activityMoment(item.createdAt, now)}</time>
        </> : null}
      </div>
      <div className="inbox-detail-card" data-quote={item.excerpt ? true : undefined}>
        {item.excerpt
          ? <blockquote className="inbox-detail-excerpt">{item.excerpt}</blockquote>
          : <p className="inbox-detail-description">{homeRun ? "Open Home to read this and continue the conversation." : presentation.description}</p>}
        {occurrences.length > 1 ? (
          <p className="inbox-detail-occurrences">
            {`Happened ${occurrences.length} times`}
            {times.length > 1 ? ` between ${clock(times[0])} and ${clock(times.at(-1) as number)}` : ""}.
          </p>
        ) : null}
        <div className="inbox-detail-actions">
          <OcButton leadingIcon={item.navigation.kind === "mark" ? "activity" : "openExternal"} onClick={() => onOpen(item)} variant="primary">{openLabel}</OcButton>
        </div>
      </div>
      {related.length ? (
        <section className="inbox-list" aria-label={`More from ${source}`}>
          <h2>More from {source}</h2>
          {related.map((row) => (
            <InboxItemRow key={row.id} now={now} row={row} secondary={rowSecondary(row.lead, false)}
              onSelect={() => onSelect(row.lead, row.items.map((candidate) => candidate.id))} />
          ))}
        </section>
      ) : null}
      <details className="inbox-detail-technical">
        <summary>
          <span>Technical details</span>
          <Glyph name="chevronDown" size={12} />
        </summary>
        <dl className="inbox-detail-evidence">
          <div><dt>{item.kind === "mark" ? "Mark" : "Event"}</dt><dd>{item.name}</dd></div>
          <div><dt>Worker</dt><dd>{item.worker}</dd></div>
          <div><dt>Run</dt><dd>{item.runId}</dd></div>
          {item.sequence !== null ? <div><dt>Movement</dt><dd>{item.sequence}</dd></div> : null}
          {item.artifactId ? <div><dt>Artifact</dt><dd>{item.artifactId}</dd></div> : null}
        </dl>
      </details>
    </article>
  );
}

function InboxItemRow({ now, row, secondary, onSelect }: { now: number; row: ActivityRowGroup; secondary: string; onSelect: () => void }) {
  const item = row.lead;
  const count = row.items.length;
  return (
    <button className="inbox-item-row" data-request={isActivityRequest(item) || undefined} onClick={onSelect} type="button">
      <ActivityMark item={item} size={16} />
      <span className="inbox-item-text">
        <strong>{activityPresentation(item).label}</strong>
        {secondary ? <span>{secondary}</span> : null}
      </span>
      {count > 1 ? <span className="inbox-item-count">{`×${count}`}</span> : null}
      {item.createdAt ? <time dateTime={new Date(item.createdAt).toISOString()}>{relativeActivityTime(item.createdAt, now)}</time> : null}
    </button>
  );
}

function rowSecondary(item: MarkActivityItem, withSource: boolean): string {
  const source = withSource ? activitySource(item) : "";
  if (!item.excerpt) return source;
  return source ? `${source} — ${item.excerpt}` : item.excerpt;
}

function clock(at: number): string {
  return new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(at);
}
