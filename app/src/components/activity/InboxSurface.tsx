import { useEffect, useId, useState } from "react";
import { loadChatRunMessages, type ChatRunMessage } from "../../chatRuns";
import { OcButton, OcIconButton } from "../../design-system/components";
import { projectAssistantDirectives } from "../chat/assistantDirectives";
import { MessageContent } from "../chat/MessageContent";
import { requestPresentation } from "../chat/OperatorRequestSurface";
import { Glyph } from "../IconGlyphs";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import { ActivityMark } from "./ActivityMark";
import type { MarkActivityItem } from "./markActivity";
import { activityMoment, activityPresentation, activitySource, isActivityRequest } from "./markPresentation";
import type { MarkActivity } from "./useMarkActivity";
import "./inbox.css";

export type InboxSurfaceState = {
  activity: MarkActivity;
  selected: MarkActivityItem | null;
  selectedGroup?: readonly string[];
  onSelect: (item: MarkActivityItem, ids?: readonly string[]) => void;
  onOpen: (item: MarkActivityItem) => void;
};

export function InboxSurface({ activity, selected, selectedGroup = [], onOpen }: InboxSurfaceState) {
  const item = selected && activity.projectedItems.find((candidate) => candidate.id === selected.id);

  return (
    <section className="inbox-surface" aria-label="Inbox details">
      {activity.error ? (
        <div className="inbox-placeholder" role="alert"><Glyph name="warning" size={16} /><span>{activity.error}</span></div>
      ) : !activity.loaded ? (
        <OctopusLoadingState className="inbox-placeholder" label="Loading Inbox" />
      ) : selected && !item ? (
        <p className="inbox-placeholder" role="status">This notification is no longer in your Inbox</p>
      ) : item ? (
        <InboxPreview item={item} group={selectedGroup.includes(item.id) ? selectedGroup : [item.id]} activity={activity} onOpen={onOpen} />
      ) : (
        <p className="inbox-placeholder">{activity.listedCount ? "Select a notification to preview it" : "You're all caught up"}</p>
      )}
    </section>
  );
}

function InboxPreview({ item, group, activity, onOpen }: {
  item: MarkActivityItem;
  group: readonly string[];
  activity: MarkActivity;
  onOpen: (item: MarkActivityItem) => void;
}) {
  const [details, setDetails] = useState(false);
  const detailsId = useId();
  const now = Date.now();
  const request = isActivityRequest(item);
  const thread = useActivityThread(item);
  const source = thread?.message && request
    ? requestPresentation(item.kind as "approval" | "elicitation" | "user_input", thread.message.eventData, thread.message.text)
    : null;
  const presentation = activityPresentation(item);
  const homeRun = item.navigation.kind === "run" && item.navigation.clientSurfaceRole === "home";
  const title = item.kind === "message" ? item.sourceTitle ?? "Home" : source?.title ?? presentation.label;
  const action = item.kind === "approval" ? "Review in task"
    : request ? "Answer in task"
      : item.navigation.kind === "mark" ? "Open history" : homeRun ? "Open Home" : "Open task";
  const author = activitySource(item);
  const occurrences = group.map((id) => activity.projectedItems.find((candidate) => candidate.id === id)?.createdAt ?? null)
    .filter((at): at is number => at !== null).sort((left, right) => left - right);
  return (
    <article className="inbox-preview" key={item.id}>
      <header className="inbox-preview-head">
        <h1>{title}{item.kind === "mark" && group.length > 1 ? <span className="inbox-preview-count">{`×${group.length}`}</span> : null}</h1>
        <div className="inbox-preview-actions">
          <OcIconButton aria-controls={detailsId} aria-expanded={details} data-state={details ? "open" : "closed"} glyph="activityOptions" label="Technical details" onClick={() => setDetails(!details)} />
          <OcButton onClick={() => onOpen(item)} variant={request ? "primary" : "secondary"}>{action}</OcButton>
        </div>
      </header>
      <div className="inbox-preview-details" data-open={details || undefined} id={detailsId}>
        <div>
          <dl>
            <div><dt>{item.kind === "mark" ? "Mark" : "Event"}</dt><dd>{item.name}</dd></div>
            <div><dt>Worker</dt><dd>{item.worker}</dd></div>
            <div><dt>Run</dt><dd>{item.runId}</dd></div>
            {item.sequence !== null ? <div><dt>Movement</dt><dd>{item.sequence}</dd></div> : null}
            {item.artifactId ? <div><dt>Artifact</dt><dd>{item.artifactId}</dd></div> : null}
          </dl>
        </div>
      </div>
      <div className="inbox-preview-message">
        <span className="inbox-preview-avatar"><ActivityMark item={item} size={28} glyphSize={20} /></span>
        <div className="inbox-preview-body">
          <div className="inbox-preview-author">
            <span>{author}</span>
            {item.createdAt ? <time dateTime={new Date(item.createdAt).toISOString()}>{activityMoment(item.createdAt, now)}</time> : null}
            {request ? <span className="inbox-preview-badge">{presentation.label}</span> : null}
          </div>
          <div className="inbox-preview-content text-size-chat">
            {item.kind === "mark" ? (
              <p>
                {presentation.description}
                {occurrences.length > 1 ? ` It happened ${occurrences.length} times between ${clock(occurrences[0])} and ${clock(occurrences.at(-1) as number)}.` : ""}
              </p>
            ) : item.kind === "message" ? (
              <MessageReply item={item} thread={thread} />
            ) : (
              <RequestContent fallback={presentation.description} source={source} />
            )}
            {thread?.error ? <p className="inbox-preview-error" role="status">{thread.error}</p> : null}
          </div>
        </div>
      </div>
    </article>
  );
}

function MessageReply({ item, thread }: { item: MarkActivityItem; thread: ActivityThread | null }) {
  const text = thread?.message ? projectAssistantDirectives(thread.message.text).markdown.trim() : "";
  if (text) return <MessageContent text={text} variant="assistant" />;
  return item.excerpt ? <p>{item.excerpt}</p> : null;
}

function RequestContent({ fallback, source }: { fallback: string; source: ReturnType<typeof requestPresentation> | null }) {
  if (!source) return <p>{fallback}</p>;
  const code = source.context.filter((entry) => entry.code);
  const facts = source.context.filter((entry) => !entry.code);
  return (
    <>
      {source.detail ? <p>{source.detail}</p> : null}
      {facts.map((entry) => <p key={entry.label}><span className="inbox-preview-label">{entry.label}</span> {entry.value}</p>)}
      {code.map((entry) => <pre className="inbox-preview-code" key={entry.label} title={entry.label}>{entry.value}</pre>)}
      {!source.detail && !source.context.length ? <p>{fallback}</p> : null}
    </>
  );
}

type ActivityThread = { key: string; message: ChatRunMessage | null; error: string };

function useActivityThread(item: MarkActivityItem): ActivityThread | null {
  const [thread, setThread] = useState<ActivityThread | null>(null);
  useEffect(() => {
    if (item.kind === "mark") return;
    let cancelled = false;
    loadChatRunMessages(item.runId).then((messages) => {
      if (cancelled) return;
      const message = messages.filter((candidate) => candidate.artifactId === item.artifactId && candidate.movementSeq === item.sequence).at(-1) ?? null;
      setThread({ key: item.id, message, error: message ? "" : "This message is no longer in the task." });
    }, (failure: unknown) => {
      if (!cancelled) setThread({ key: item.id, message: null, error: failure instanceof Error ? failure.message : String(failure) });
    });
    return () => { cancelled = true; };
  }, [item.artifactId, item.id, item.kind, item.runId, item.sequence]);
  return thread?.key === item.id ? thread : null;
}

function clock(at: number): string {
  return new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(at);
}
