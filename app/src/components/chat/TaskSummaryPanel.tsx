import { AnimatePresence, motion } from "motion/react";
import { useMemo, useState } from "react";
import type { RunProjection } from "../../data/topology";
import type { AdmittedClientSurfaceDocument } from "../../client-surfaces/types";
import { useShouldReduceMotion } from "../../appearance";
import { TaskEnvironmentSummary } from "../worker-environment/TaskEnvironmentSummary";
import { OcIconButton } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { artifactEndResourceFromMessage, type ArtifactEndResource } from "../artifacts/ArtifactEndResources";
import { ArtifactWorkbench } from "../artifacts/ArtifactWorkbench";
import type { ArtifactReference } from "../artifacts/types";
import type { WorkerEnvironmentPanelKind } from "../worker-environment/surfaceRegistry";
import type { WorkerEnvironmentPaneTarget } from "../worker-environment/workerEnvironmentPane";
import {
  multiAgentIds,
  multiAgentStateIsTerminal,
  multiAgentStateStatus,
  normalizeMultiAgentState,
  normalizeSubagentActivity,
} from "./subagentActivity";
import { collectTaskSources, type TaskSourceRow } from "./taskSourcesPane";
import type { ChatRenderMessage } from "./types";

type Props = {
  active?: boolean;
  messages: ChatRenderMessage[];
  runProjection: RunProjection | null;
  surfaces?: AdmittedClientSurfaceDocument[];
  onOpenEnvironment: (environmentId: string, panelKind?: WorkerEnvironmentPanelKind, surfaceId?: string, clientSurface?: WorkerEnvironmentPaneTarget["clientSurface"]) => void;
  onRevealMessage: (messageId: string) => void;
  onShowAllSources: (sources: TaskSourceRow[]) => void;
};

type OutputRow = {
  message: ChatRenderMessage;
  resource: ArtifactEndResource;
};

type BackgroundProcessRow = {
  active: boolean;
  glyph: GlyphName;
  id: string;
  label: string;
  messageId: string;
  meta: string;
};

type SubagentSummaryRow = {
  displayName: string;
  id: string;
  messageId: string;
  status: string;
  state: "working" | "done" | "observed";
};

export function TaskSummaryPanel({
  messages,
  runProjection,
  surfaces = [],
  onOpenEnvironment,
  onRevealMessage,
  onShowAllSources,
}: Props) {
  const [artifactTarget, setArtifactTarget] = useState<ArtifactReference | null>(null);
  const plan = useMemo(() => [...messages].reverse().find((message) => message.eventKind === "plan" && message.text.trim()), [messages]);
  const outputs = useMemo(() => messages.flatMap((message): OutputRow[] => {
    const resource = artifactEndResourceFromMessage(message);
    return resource ? [{ message, resource }] : [];
  }), [messages]);
  const backgroundProcesses = useMemo(() => collectBackgroundProcesses(messages), [messages]);
  const subagents = useMemo(() => collectSubagents(messages), [messages]);
  const sources = useMemo(() => collectTaskSources(messages), [messages]);
  const workerEnvironments = runProjection?.workerEnvironments.length
    ? runProjection.workerEnvironments
    : runProjection?.workerEnvironment.id
      ? [runProjection.workerEnvironment]
      : [];
  const visibleSources = sources.slice(0, 3);
  const planTitle = plan ? taskSummaryPlanTitle(plan) : "";

  return (
    <div className="task-summary-shell">
      <div className="task-summary-scroll">
        <div className="task-summary-content">
        <section className="task-summary-section" aria-label="Environments">
          <div className="task-summary-section-body">
            {workerEnvironments.length ? (
              <TaskEnvironmentSummary
                environments={workerEnvironments}
                onOpen={onOpenEnvironment}
                surfaces={surfaces}
              />
            ) : null}
            {!workerEnvironments.length ? <p className="task-environment-empty">{runProjection ? "No environments attached to this task" : "Environment information unavailable"}</p> : null}
          </div>
        </section>

        {outputs.length ? (
          <SummarySection count={String(outputs.length)} title="Outputs">
            <div className="task-summary-list">
              {outputs.map(({ message, resource }) => (
                <div className="task-summary-artifact-row" key={message.id}>
                  <button className="task-summary-row" onClick={() => onRevealMessage(message.id)} type="button">
                    <span className="task-summary-row-leading"><Glyph name={message.error ? "warning" : "artifact"} size={16} /></span>
                    <span className="task-summary-row-copy">
                      <span className="task-summary-row-title">{resource.filename}</span>
                      <span className="task-summary-row-meta">{formatTime(message.createdAtMs)}</span>
                    </span>
                    <Glyph className="task-summary-row-chevron" name="chevronDown" size={13} />
                  </button>
                  <OcIconButton className="task-summary-artifact-action" glyph="artifact" label="Open artifact" onClick={() => setArtifactTarget(resource.reference)} size="xs" variant="ghost" />
                </div>
              ))}
            </div>
          </SummarySection>
        ) : null}

        {plan ? (
          <SummarySection title="Plan">
            <button className="task-summary-row task-summary-plan" onClick={() => onRevealMessage(plan.id)} type="button">
              <span className="task-summary-row-leading"><Glyph name="taskPlan" size={16} /></span>
              <span className="task-summary-plan-title" title={plan.text}>{planTitle}</span>
            </button>
          </SummarySection>
        ) : null}

        {subagents.length ? (
          <SummarySection count={String(subagents.length)} title="Related agents">
            <SubagentSummary items={subagents} />
            <div className="task-summary-list task-summary-related-agents">
              {subagents.map((item) => (
                <button className="task-summary-row is-compact" key={item.id} onClick={() => onRevealMessage(item.messageId)} title={item.id} type="button">
                  <span className="task-summary-row-leading"><Glyph name="user" size={16} /></span>
                  <span className="task-summary-row-copy is-inline">
                    <span className="task-summary-row-title">{item.displayName}</span>
                    <span className="task-summary-row-meta">{item.status}</span>
                  </span>
                </button>
              ))}
            </div>
          </SummarySection>
        ) : null}

        {backgroundProcesses.length ? (
          <SummarySection count={String(backgroundProcesses.length)} title="Background processes">
            <div className="task-summary-list">
              {backgroundProcesses.map((item) => (
                <button className="task-summary-row" key={item.id} onClick={() => onRevealMessage(item.messageId)} type="button">
                  <span className="task-summary-row-leading">
                    {item.active ? <span className="composer-spinner size-3.5" /> : <Glyph name={item.glyph} size={16} />}
                  </span>
                  <span className="task-summary-row-copy">
                    <span className="task-summary-row-title">{item.label}</span>
                    <span className="task-summary-row-meta">{item.meta}</span>
                  </span>
                  <Glyph className="task-summary-row-chevron" name="chevronDown" size={13} />
                </button>
              ))}
            </div>
          </SummarySection>
        ) : null}

        {sources.length ? (
          <SummarySection count={String(sources.length)} title="Sources">
            <div className="task-summary-list">
              {visibleSources.map((source) => (
                <ArtifactSourceRow key={source.id} onOpen={setArtifactTarget} source={source} />
              ))}
              <ViewAllButton onClick={() => onShowAllSources(sources)} />
            </div>
          </SummarySection>
        ) : null}
        </div>
      </div>
      {artifactTarget ? <ArtifactWorkbench onClose={() => setArtifactTarget(null)} presentation="overlay" reference={artifactTarget} /> : null}
    </div>
  );
}

function ArtifactSourceRow({ onOpen, source }: { onOpen: (reference: ArtifactReference) => void; source: TaskSourceRow }) {
  const content = (
    <>
      <span className="task-summary-row-leading"><Glyph name={source.glyph} size={16} /></span>
      <span className="task-summary-row-copy">
        <span className="task-summary-row-title">{source.label}</span>
        {source.meta ? <span className="task-summary-row-meta">{source.meta}</span> : null}
      </span>
    </>
  );
  return source.reference ? (
    <button className="task-summary-row" onClick={() => onOpen(source.reference as ArtifactReference)} type="button">{content}</button>
  ) : (
    <div className="task-summary-row is-static">{content}</div>
  );
}

function SummarySection({ children, count, title }: { children: React.ReactNode; count?: string; title: string }) {
  const reduceMotion = useShouldReduceMotion();
  const [expanded, setExpanded] = useState(true);
  return (
    <section className="task-summary-section">
      <header className="task-summary-section-heading">
        <button aria-expanded={expanded} onClick={() => setExpanded((value) => !value)} type="button">
          <span>{title}</span>
          {!expanded && count ? <span className="task-summary-section-count">{count}</span> : null}
          <Glyph className={`task-summary-section-chevron${expanded ? " is-expanded" : ""}`} name="chevronDown" size={12} />
        </button>
      </header>
      {reduceMotion ? expanded && (
        <div className="task-summary-section-motion" style={{ marginTop: 2 }}>
          <div className="task-summary-section-body">{children}</div>
        </div>
      ) : <AnimatePresence initial={false}>
        {expanded ? (
          <motion.div
            animate={{ height: "auto", marginTop: 2, opacity: 1 }}
            className="task-summary-section-motion"
            exit={{ height: 0, marginTop: 0, opacity: 0 }}
            initial={{ height: 0, marginTop: 0, opacity: 0 }}
            key="content"
            transition={{ duration: 0.3, ease: [0.19, 1, 0.22, 1] }}
          >
            <div className="task-summary-section-body">{children}</div>
          </motion.div>
        ) : null}
      </AnimatePresence>}
    </section>
  );
}

function SubagentSummary({ items }: { items: SubagentSummaryRow[] }) {
  const active = items.filter((item) => item.state === "working");
  const done = items.filter((item) => item.state === "done");
  const observed = items.filter((item) => item.state === "observed");
  return (
    <div className="task-summary-subagents">
      <span className="task-summary-avatar-group" aria-hidden="true">
        {items.slice(0, 4).map((item) => (
          <span className={`task-summary-agent-avatar${item.state === "working" ? " is-active" : ""}`} key={item.id} style={{ "--agent-hue": String(identityHue(item.id)) } as React.CSSProperties} title={item.displayName}>
            {item.state === "working" ? <span className="composer-spinner size-3" /> : <Glyph name="user" size={11} />}
          </span>
        ))}
      </span>
      <span className="task-summary-subagent-counts">
        {active.length ? <span>{active.length} working</span> : null}
        {done.length ? <span>{done.length} finished</span> : null}
        {observed.length ? <span>{observed.length} observed</span> : null}
      </span>
    </div>
  );
}

function ViewAllButton({ onClick }: { onClick: () => void }) {
  return (
    <button className="task-summary-row task-summary-show-all" onClick={onClick} type="button">
      <span className="task-summary-row-leading"><Glyph name="viewAll" size={16} /></span>
      <span className="task-summary-plan-title">View all</span>
    </button>
  );
}

function collectBackgroundProcesses(messages: ChatRenderMessage[]): BackgroundProcessRow[] {
  const rows = new Map<string, BackgroundProcessRow>();
  for (const message of messages) {
    const kind = message.eventKind ?? "";
    if (kind !== "tool" && kind !== "terminal_interaction") continue;
    const data = record(message.eventData);
    const processId = stringValue(data.process_id) || stringValue(data.processId);
    if (!processId) continue;
    const command = stringValue(data.command) || firstLine(message.text) || "Background process";
    const status = stringValue(data.status) || stringValue(data.lifecycle) || "projected";
    rows.set(`process:${processId}`, {
      active: backgroundStatusActive(status),
      glyph: "terminal",
      id: `process:${processId}`,
      label: command,
      messageId: message.id,
      meta: readableStatus(status),
    });
  }
  return [...rows.values()].sort((left, right) => Number(right.active) - Number(left.active));
}

function collectSubagents(messages: ChatRenderMessage[]): SubagentSummaryRow[] {
  const rows = new Map<string, SubagentSummaryRow>();
  const update = (id: string, state: SubagentSummaryRow["state"], messageId: string, status: string, displayName = "Agent") => {
    const previous = rows.get(id);
    rows.set(id, {
      displayName: displayName === "Agent" ? previous?.displayName ?? displayName : displayName,
      id,
      messageId,
      status: state === "observed" && previous ? previous.status : status,
      state: state === "observed" && previous ? previous.state : state,
    });
  };
  for (const message of messages) {
    const multiAgent = normalizeMultiAgentState(message);
    if (multiAgent) {
      for (const agentThreadId of multiAgentIds(multiAgent)) {
        const agentState = record(multiAgent.agentsStates[agentThreadId]);
        const status = multiAgentStateStatus(agentState.status);
        const state: SubagentSummaryRow["state"] = multiAgentStateIsTerminal(status)
          ? "done"
          : status === "pendingInit" || status === "running"
            ? "working"
            : "observed";
        update(agentThreadId, state, message.id, readableStatus(status || "observed"));
      }
      continue;
    }
    const subagent = normalizeSubagentActivity(message);
    if (!subagent) continue;
    update(
      subagent.agentThreadId,
      subagent.displayStatus === "interrupted" ? "done" : "working",
      message.id,
      readableStatus(subagent.displayStatus),
      subagent.displayName,
    );
  }
  return [...rows.values()].sort((left, right) => subagentStateRank(left.state) - subagentStateRank(right.state) || left.id.localeCompare(right.id));
}

function subagentStateRank(state: SubagentSummaryRow["state"]): number {
  return state === "working" ? 0 : state === "done" ? 1 : 2;
}

function backgroundStatusActive(status: string): boolean {
  return ["active", "in_progress", "inprogress", "pending", "running", "started", "starting", "working"].includes(status.toLocaleLowerCase());
}

function readableClassifier(value: string): string {
  return value.trim().split(/[._\s-]+/).filter(Boolean).map((segment) => `${segment[0]?.toLocaleUpperCase() ?? ""}${segment.slice(1)}`).join(" ");
}

function readableStatus(value: string): string {
  return readableClassifier(value) || "Projected";
}

function identityHue(value: string): number {
  let hash = 0;
  for (let index = 0; index < value.length; index += 1) hash = (hash * 31 + value.charCodeAt(index)) | 0;
  return Math.abs(hash) % 360;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function firstLine(value: string): string {
  return value.trim().split(/\r?\n/, 1)[0]?.slice(0, 180) ?? "";
}

function taskSummaryPlanTitle(message: ChatRenderMessage): string {
  const data = record(message.eventData);
  return stringValue(data.title)
    || stringValue(data.plan_title)
    || firstLine(message.text)
    || "Plan";
}

function formatTime(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "Projected output";
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(value);
}
