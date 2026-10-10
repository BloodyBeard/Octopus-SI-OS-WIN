import { OcButton } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { AgentPresetLogo } from "../agent/AgentPresetLogo";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import type { MarkActivityItem, ActivityKind } from "./markActivity";
import type { MarkActivity } from "./useMarkActivity";
import "./inbox.css";

export type InboxSurfaceState = {
  activity: MarkActivity;
  selected: MarkActivityItem | null;
  onSelect: (item: MarkActivityItem) => void;
  onOpen: (item: MarkActivityItem) => void;
};

const activityPresentation: Record<ActivityKind, { label: string; glyph: GlyphName; body: string }> = {
  message: { label: "New message", glyph: "chat", body: "Open the task to read the reply and continue the conversation." },
  approval: { label: "Approval requested", glyph: "lock", body: "Review the request in its task before deciding whether to approve it." },
  user_input: { label: "Your input is needed", glyph: "user", body: "Open the task to answer the request and continue the work." },
  elicitation: { label: "Information requested", glyph: "chat", body: "Open the task to review and answer the request." },
  mark: { label: "Recorded activity", glyph: "worktree", body: "View the recorded movement to inspect its evidence and context." },
};

export function InboxSurface({ activity, selected, onSelect, onOpen }: InboxSurfaceState) {
  const item = selected && activity.projectedItems.find((candidate) => candidate.id === selected.id);
  const presentation = item ? activityPresentation[item.kind] : null;
  const openLabel = item?.navigation.kind === "mark" ? "View movement"
    : item?.navigation.kind === "run" && item.navigation.clientSurfaceRole === "home" ? "Open Home" : "Open task";
  const unread = activity.items.filter((candidate) => !activity.read.has(candidate.id));
  const previewItems = (unread.length ? unread : activity.items).slice(0, 5);

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
          <div className="inbox-summary" role="status"><Glyph name="inbox" size={32} /><h1>Activity is no longer available</h1><p>Select another item from your Inbox.</p></div>
        ) : item && presentation ? (
          <article className="inbox-detail" key={item.id}>
            <div className="inbox-detail-kind"><Glyph name={presentation.glyph} size={18} /><span>{presentation.label}</span></div>
            <h1>{item.title}</h1>
            <div className="inbox-detail-sender">
              {item.agentIcon ? <AgentPresetLogo color={item.agentColor} icon={item.agentIcon} size={24} /> : <Glyph name="user" size={24} />}
              <span>{item.agentName || item.worker || "Octopus"}</span>
              {item.createdAt ? <time dateTime={new Date(item.createdAt).toISOString()}>{new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(item.createdAt)}</time> : null}
            </div>
            <p className="inbox-detail-description">{item.navigation.kind === "run" && item.navigation.clientSurfaceRole === "home" ? "Open Home to review this activity and continue the conversation." : presentation.body}</p>
            <div className="inbox-detail-actions">
              <OcButton leadingIcon={item.navigation.kind === "mark" ? "worktree" : "openExternal"} onClick={() => onOpen(item)} variant="primary">{openLabel}</OcButton>
            </div>
            {item.navigation.kind === "mark" ? (
              <dl className="inbox-detail-evidence">
                <div><dt>Mark</dt><dd>{item.name}</dd></div>
                {item.sequence !== null ? <div><dt>Movement</dt><dd>{item.sequence}</dd></div> : null}
                {item.artifactId ? <div><dt>Artifact</dt><dd>{item.artifactId}</dd></div> : null}
              </dl>
            ) : null}
          </article>
        ) : (
          <div className="inbox-summary">
            <span className="inbox-summary-icon"><Glyph name="inbox" size={32} /></span>
            <h1>{activity.unreadCount ? "Your Inbox" : "You're up to date"}</h1>
            <p>{activity.unreadCount ? "Messages and requests from across Octopus." : "New messages and requests will appear here as work progresses."}</p>
            <div className="inbox-summary-counts">
              <div><strong>{activity.unreadCount}</strong><span>Unread</span></div>
              <div><strong>{activity.items.filter((candidate) => !activity.read.has(candidate.id) && candidate.kind !== "message" && candidate.kind !== "mark").length}</strong><span>Requests</span></div>
            </div>
            {previewItems.length ? (
              <div className="inbox-overview">
                <h2>{unread.length ? "Unread activity" : "Recent activity"}</h2>
                {previewItems.map((candidate) => (
                  <button className="inbox-overview-row" key={candidate.id} onClick={() => onSelect(candidate)} type="button">
                    <Glyph name={activityPresentation[candidate.kind].glyph} size={20} />
                    <span className="inbox-overview-text"><strong>{candidate.title}</strong><span>{candidate.agentName || candidate.worker || activityPresentation[candidate.kind].label}</span></span>
                    {candidate.createdAt ? <time dateTime={new Date(candidate.createdAt).toISOString()}>{new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(candidate.createdAt)}</time> : null}
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        )}
      </div>
    </section>
  );
}
