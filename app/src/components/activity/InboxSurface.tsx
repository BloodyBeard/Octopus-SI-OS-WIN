import { useState } from "react";
import { OcButton } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { AgentPresetLogo } from "../agent/AgentPresetLogo";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import type { MarkActivityItem } from "./markActivity";
import { activityPresentation, activitySource, activityTitle, isActivityRequest, relativeActivityTime } from "./markPresentation";
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
  const unread = activity.items.filter((candidate) => !activity.read.has(candidate.id));
  const requests = unread.filter(isActivityRequest);
  const previewItems = [...requests, ...unread.filter((candidate) => !isActivityRequest(candidate))].slice(0, 5);
  const now = Date.now();

  return (
    <section className="inbox-surface" aria-label="Inbox details">
      <div className="inbox-surface-content">
        {activity.error ? (
          <div className="inbox-summary" role="alert">
            <Glyph name="warning" size={32} />
            <h1>Inbox is unavailable</h1>
            <p>{activity.error}</p>
          </div>
        ) : !activity.loaded ? (
          <OctopusLoadingState className="inbox-summary" label="Loading Inbox" />
        ) : selected && !item ? (
          <div className="inbox-summary" role="status"><Glyph name="inbox" size={32} /><h1>This activity is gone</h1><p>Pick another item from your Inbox.</p></div>
        ) : item ? (
          <InboxDetail item={item} group={selectedGroup.includes(item.id) ? selectedGroup : [item.id]} activity={activity} now={now} onOpen={onOpen} />
        ) : (
          <div className="inbox-summary">
            <span className="inbox-summary-icon"><Glyph name="inbox" size={32} /></span>
            <h1>{requests.length ? `${requests.length} ${requests.length === 1 ? "thing needs" : "things need"} you` : unread.length ? "Catch up on your work" : "You're all caught up"}</h1>
            <p>{unread.length ? "Replies, requests and what Octopus did while you were away." : "New replies and requests will show up here as your work runs."}</p>
            {previewItems.length ? (
              <div className="inbox-overview">
                {previewItems.map((candidate) => {
                  const presentation = activityPresentation(candidate);
                  return (
                    <button className="inbox-overview-row" data-request={isActivityRequest(candidate) || undefined} key={candidate.id} onClick={() => onSelect(candidate)} type="button">
                      <span className="inbox-overview-glyph"><Glyph name={presentation.glyph} size={16} /></span>
                      <span className="inbox-overview-text"><strong>{activityTitle(candidate)}</strong><span>{activitySource(candidate)}</span></span>
                      {candidate.createdAt ? <time dateTime={new Date(candidate.createdAt).toISOString()}>{relativeActivityTime(candidate.createdAt, now)}</time> : null}
                    </button>
                  );
                })}
              </div>
            ) : null}
          </div>
        )}
      </div>
    </section>
  );
}

function InboxDetail({ item, group, activity, now, onOpen }: {
  item: MarkActivityItem;
  group: readonly string[];
  activity: MarkActivity;
  now: number;
  onOpen: (item: MarkActivityItem) => void;
}) {
  const [technical, setTechnical] = useState(false);
  const presentation = activityPresentation(item);
  const occurrences = group.map((id) => activity.projectedItems.find((candidate) => candidate.id === id)).filter((candidate): candidate is MarkActivityItem => Boolean(candidate));
  const times = occurrences.map((candidate) => candidate.createdAt).filter((at): at is number => at !== null).sort((left, right) => left - right);
  const source = activitySource(item);
  const homeRun = item.navigation.kind === "run" && item.navigation.clientSurfaceRole === "home";
  const openLabel = item.navigation.kind === "mark" ? "Show in history"
    : homeRun ? "Open Home" : isActivityRequest(item) ? "Review in task" : "Open task";
  return (
    <article className="inbox-detail" key={item.id}>
      <div className="inbox-detail-kind" data-request={isActivityRequest(item) || undefined}><Glyph name={presentation.glyph} size={16} /><span>{presentation.category}</span></div>
      <h1>{activityTitle(item)}</h1>
      <div className="inbox-detail-sender">
        {item.agentIcon ? <AgentPresetLogo color={item.agentColor} icon={item.agentIcon} size={22} /> : <span className="inbox-detail-source-glyph"><Glyph name={presentation.glyph} size={14} /></span>}
        <span>{source}</span>
        {item.createdAt ? <time dateTime={new Date(item.createdAt).toISOString()} title={new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(item.createdAt)}>{relativeActivityTime(item.createdAt, now)}</time> : null}
      </div>
      <p className="inbox-detail-description">{homeRun ? "Open Home to read this and continue the conversation." : presentation.description}</p>
      {occurrences.length > 1 ? (
        <p className="inbox-detail-occurrences">
          {`Happened ${occurrences.length} times`}
          {times.length > 1 ? ` between ${clock(times[0])} and ${clock(times.at(-1) as number)}` : ""}.
        </p>
      ) : null}
      <div className="inbox-detail-actions">
        <OcButton leadingIcon={item.navigation.kind === "mark" ? "activity" : "openExternal"} onClick={() => onOpen(item)} variant="primary">{openLabel}</OcButton>
      </div>
      <div className="inbox-detail-technical">
        <button aria-expanded={technical} className="inbox-detail-technical-toggle" onClick={() => setTechnical(!technical)} type="button">
          <Glyph name={technical ? "chevronUp" : "chevronDown"} size={14} />
          <span>Technical details</span>
        </button>
        {technical ? (
          <dl className="inbox-detail-evidence">
            <div><dt>{item.kind === "mark" ? "Mark" : "Event"}</dt><dd>{item.name}</dd></div>
            <div><dt>Worker</dt><dd>{item.worker}</dd></div>
            <div><dt>Run</dt><dd>{item.runId}</dd></div>
            {item.sequence !== null ? <div><dt>Movement</dt><dd>{item.sequence}</dd></div> : null}
            {item.artifactId ? <div><dt>Artifact</dt><dd>{item.artifactId}</dd></div> : null}
          </dl>
        ) : null}
      </div>
    </article>
  );
}

function clock(at: number): string {
  return new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(at);
}
