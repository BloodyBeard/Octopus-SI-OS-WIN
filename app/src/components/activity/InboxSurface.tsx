import { useEffect, useState } from "react";
import { loadChatRunMessages, type ChatRunMessage } from "../../chatRuns";
import { OcButton } from "../../design-system/components";
import { projectAssistantDirectives } from "../chat/assistantDirectives";
import { MessageContent } from "../chat/MessageContent";
import { Glyph } from "../IconGlyphs";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import { ActivityMark } from "./ActivityMark";
import type { MarkActivityItem } from "./markActivity";
import { activityHeadline, activityMoment, activityPresentation, activitySource, isActivityRequest } from "./markPresentation";
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
  const now = Date.now();

  return (
    <section className="inbox-surface" aria-label="Inbox details">
      <div className="inbox-surface-content" data-view={item ? "detail" : "placeholder"}>
        {activity.error ? (
          <div className="inbox-placeholder" role="alert">
            <Glyph name="warning" size={16} />
            <span>{activity.error}</span>
          </div>
        ) : !activity.loaded ? (
          <OctopusLoadingState className="inbox-placeholder" label="Loading Inbox" />
        ) : selected && !item ? (
          <p className="inbox-placeholder" role="status">This item is no longer in your Inbox</p>
        ) : item ? (
          <InboxDetail item={item} group={selectedGroup.includes(item.id) ? selectedGroup : [item.id]} activity={activity} now={now} onOpen={onOpen} />
        ) : (
          <p className="inbox-placeholder">{activity.items.length ? "Select an item to view" : "You're all caught up"}</p>
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
  const presentation = activityPresentation(item);
  const request = isActivityRequest(item);
  const homeRun = item.navigation.kind === "run" && item.navigation.clientSurfaceRole === "home";
  const openLabel = item.navigation.kind === "mark" ? "Show in history"
    : homeRun ? "Open Home" : request ? "Review in task" : "Open task";
  const occurrences = group.map((id) => activity.projectedItems.find((candidate) => candidate.id === id)).filter((candidate): candidate is MarkActivityItem => Boolean(candidate));
  const times = occurrences.map((candidate) => candidate.createdAt).filter((at): at is number => at !== null).sort((left, right) => left - right);
  return (
    <article className="inbox-detail" key={item.id}>
      <header className="inbox-detail-header">
        <div className="inbox-detail-heading">
          <h1>{activityHeadline(item)}</h1>
          <div className="inbox-detail-meta">
            <ActivityMark item={item} size={14} />
            <span className="inbox-detail-meta-source">{activitySource(item)}</span>
            {item.createdAt ? <>
              <span aria-hidden="true">·</span>
              <time dateTime={new Date(item.createdAt).toISOString()}>{activityMoment(item.createdAt, now)}</time>
            </> : null}
            {item.kind !== "mark" ? <>
              <span aria-hidden="true">·</span>
              <span data-request={request || undefined}>{presentation.label}</span>
            </> : null}
          </div>
        </div>
        <OcButton leadingIcon={item.navigation.kind === "mark" ? "activity" : "openExternal"} onClick={() => onOpen(item)} variant={request ? "primary" : "secondary"}>{openLabel}</OcButton>
      </header>
      {item.kind === "mark" ? (
        <div className="inbox-detail-body">
          <p>{presentation.description}</p>
          {occurrences.length > 1 ? (
            <p className="inbox-detail-occurrences">
              {`Happened ${occurrences.length} times`}
              {times.length > 1 ? ` between ${clock(times[0])} and ${clock(times.at(-1) as number)}` : ""}.
            </p>
          ) : null}
        </div>
      ) : (
        <InboxThread item={item} />
      )}
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

type ActivityThread = { key: string; prompt: ChatRunMessage | null; reply: ChatRunMessage | null; error: string };

function useActivityThread(item: MarkActivityItem): ActivityThread | null {
  const [thread, setThread] = useState<ActivityThread | null>(null);
  useEffect(() => {
    let cancelled = false;
    loadChatRunMessages(item.runId).then((messages) => {
      if (cancelled) return;
      const reply = messages.filter((message) => message.artifactId === item.artifactId && message.movementSeq === item.sequence).at(-1) ?? null;
      const prompt = reply
        ? messages.filter((message) => message.role === "user" && message.movementSeq < reply.movementSeq && message.text.trim()).at(-1) ?? null
        : null;
      setThread({ key: item.id, prompt, reply, error: reply ? "" : "This message is no longer in the task." });
    }, (failure: unknown) => {
      if (!cancelled) setThread({ key: item.id, prompt: null, reply: null, error: failure instanceof Error ? failure.message : String(failure) });
    });
    return () => { cancelled = true; };
  }, [item.artifactId, item.id, item.runId, item.sequence]);
  return thread?.key === item.id ? thread : null;
}

function InboxThread({ item }: { item: MarkActivityItem }) {
  const thread = useActivityThread(item);
  const replyText = thread?.reply ? projectAssistantDirectives(thread.reply.text).markdown.trim() : "";
  return (
    <div className="inbox-thread text-size-chat" aria-busy={thread === null || undefined}>
      {thread?.prompt ? (
        <div className="inbox-thread-prompt">
          <div className="bg-token-foreground/5 max-w-[77%] min-w-0 overflow-hidden break-words rounded-2xl px-3 py-2 text-left">
            <div className="whitespace-pre-wrap"><MessageContent compactHeadings text={thread.prompt.text} variant="user" /></div>
          </div>
        </div>
      ) : null}
      {replyText ? (
        <div className="inbox-thread-reply"><MessageContent text={replyText} variant="assistant" /></div>
      ) : item.excerpt ? (
        <p className="inbox-thread-excerpt">{item.excerpt}</p>
      ) : !thread ? null : (
        <p className="inbox-thread-excerpt">{activityPresentation(item).description}</p>
      )}
      {thread?.error ? <p className="inbox-thread-error" role="status">{thread.error}</p> : null}
    </div>
  );
}

function clock(at: number): string {
  return new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(at);
}
