import { useEffect, useMemo, useRef, useState } from "react";
import "../icon-glyphs/chat";
import { loadChatRunMessages, loadChatRuns, type ChatRun, type ChatRunMessage } from "../../chatRuns";
import type { OctopusClientAccessProfile } from "../../electron";
import type { RunProjection, WorkerEnvironmentProjection } from "../../data/topology";
import { useProjectionGeneration } from "../../projectionLive";
import { useRuntimeTopology } from "../../runtimeTopology";
import { appToast } from "../../toasts";
import { useClientSurfaceDocuments } from "../../client-surfaces/useClientSurfaceDocuments";
import { Glyph } from "../IconGlyphs";
import { defaultWorkerEnvironmentPaneTarget } from "../worker-environment/workerEnvironmentPane";
import { MessageContent } from "./MessageContent";
import "./chat.css";

export function TaskMonitorWindow({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  const runId = useMemo(() => new URLSearchParams(window.location.search).get("runId")?.trim() ?? "", []);
  const generation = useProjectionGeneration();
  const topology = useRuntimeTopology(Boolean(runId), runId ? [runId] : []);
  const clientSurfaces = useClientSurfaceDocuments(accessProfile);
  const [run, setRun] = useState<ChatRun | null>(null);
  const [messages, setMessages] = useState<ChatRunMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [pinned, setPinned] = useState(true);
  const scrollRef = useRef<HTMLDivElement>(null);
  const projection = useMemo(() => findRunProjection(topology, runId), [runId, topology]);
  const workerEnvironments = projection?.workerEnvironments.length
    ? projection.workerEnvironments
    : projection?.workerEnvironment.id
      ? [projection.workerEnvironment]
      : [];
  const visibleMessages = messages.filter((message) => (
    message.eventData._movement_evidence !== true
    && message.eventKind !== "usage"
    && message.eventKind !== "hook"
    && !["item_started", "turn_started", "turn_completed", "model_verification", "rate_limits"].includes(message.eventKind ?? "")
    && (message.text.trim() || message.attachments.length)
  )).slice(-14);
  const statusLabel = loading && !run
    ? "Reading task"
    : run?.turnState === "working"
      ? "Working"
      : run?.turnState === "waiting"
        ? "Waiting for output"
        : run?.turnState === "failed"
          ? "Needs attention"
      : error
        ? "Unavailable"
        : run
          ? "Ready"
          : visibleMessages.length
            ? "Recorded evidence"
            : "Unavailable";

  useEffect(() => {
    let cancelled = false;
    if (!runId) {
      setError("Task Monitor requires a projected run id.");
      setLoading(false);
      return () => { cancelled = true; };
    }
    setLoading(true);
    setError("");
    void Promise.all([loadChatRuns(), loadChatRunMessages(runId)])
      .then(([runs, projectedMessages]) => {
        if (cancelled) return;
        setRun(runs.find((candidate) => candidate.runId === runId) ?? null);
        setMessages(projectedMessages);
      })
      .catch((failure) => {
        if (!cancelled) setError(formatFailure(failure));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => { cancelled = true; };
  }, [generation, runId]);

  useEffect(() => {
    document.title = `${run?.title || "Task Monitor"} — Octopus`;
  }, [run?.title]);

  useEffect(() => {
    const node = scrollRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [visibleMessages.length, run?.turnState]);

  const openMain = () => {
    if (!runId) return;
    void window.octopusTransport?.openRunInMain?.(runId).catch((failure) => appToast.danger(formatFailure(failure)));
  };

  const togglePinned = () => {
    void window.octopusTransport?.setTaskMonitorWindowPinned?.(!pinned)
      .then((outcome) => setPinned(outcome.pinned))
      .catch((failure) => appToast.danger(formatFailure(failure)));
  };

  return (
    <main className="task-monitor-window">
      <header className="task-monitor-titlebar">
        <div className="task-monitor-titlebar-left">
          <button aria-label="Dismiss Task Monitor" onClick={closeWindow} title="Dismiss" type="button"><Glyph name="close" size={15} /></button>
        </div>
        <div className="task-monitor-titlebar-title"><span>{run?.title || "Task Monitor"}</span></div>
        <div className="task-monitor-titlebar-actions">
          <button aria-pressed={pinned} className={pinned ? "is-active" : ""} onClick={togglePinned} title={pinned ? "Stop keeping on top" : "Keep on top"} type="button"><Glyph name="pin" size={15} /></button>
          <button onClick={openMain} title="Open in main window" type="button"><Glyph name="openExternal" size={15} /></button>
        </div>
      </header>

      <section className="task-monitor-status">
        <span className={`task-monitor-status-dot${run?.turnState === "working" ? " is-working" : run?.turnState === "failed" || error || (!loading && !run && !visibleMessages.length) ? " is-error" : ""}`} />
        <strong>{statusLabel}</strong>
        {run?.providerInterfaceId ? <span>{run.providerInterfaceId}</span> : null}
        {run?.turnState === "working" ? <span className="composer-spinner" /> : null}
      </section>

      <div className="task-monitor-scroll" ref={scrollRef}>
        {loading && !visibleMessages.length ? <TaskMonitorState label="Reading projected task…" loading /> : null}
        {!loading && error ? <TaskMonitorState label={error} /> : null}
        {!loading && !error && !visibleMessages.length ? <TaskMonitorState label="No admitted messages are projected for this task." /> : null}
        {visibleMessages.map((message) => (
          <article className={`task-monitor-message is-${message.role}${message.error ? " is-error" : ""}`} key={message.id}>
            <header><span>{messageTitle(message)}</span><time>{formatTime(message.createdAtMs)}</time></header>
            {message.role === "assistant" ? <MessageContent text={message.text} /> : <p>{message.text}</p>}
            {message.attachments.length ? <small>{message.attachments.length} attachment{message.attachments.length === 1 ? "" : "s"}</small> : null}
          </article>
        ))}
        {run?.turnState === "working" ? <div className="task-monitor-live-edge"><span className="composer-spinner" />Receiving admitted output</div> : null}
      </div>

      {workerEnvironments.length ? (
        <section className="task-monitor-environments" aria-label="Worker Environments">
          <header><span>Worker environments</span><small>{workerEnvironments.length}</small></header>
          <div>
            {workerEnvironments.map((environment) => (
              <button key={environment.id} onClick={() => openWorkerEnvironment(runId, environment, clientSurfaces.workerEnvironmentSurfaces)} type="button">
                <Glyph name="environment" size={14} />
                <span><strong>{environment.label}</strong><small>{environment.status}</small></span>
                <Glyph name="openExternal" size={12} />
              </button>
            ))}
          </div>
        </section>
      ) : null}
    </main>
  );
}

export function TaskMonitorAccessUnavailable() {
  return (
    <main className="task-monitor-window">
      <header className="task-monitor-titlebar">
        <div className="task-monitor-titlebar-left"><button aria-label="Dismiss Task Monitor" onClick={closeWindow} type="button"><Glyph name="close" size={15} /></button></div>
        <div className="task-monitor-titlebar-title"><span>Task Monitor</span></div>
      </header>
      <TaskMonitorState label="Unlock the main application before opening a projected task monitor." />
    </main>
  );
}

function TaskMonitorState({ label, loading = false }: { label: string; loading?: boolean }) {
  return <div className="task-monitor-state">{loading ? <span className="composer-spinner" /> : <Glyph name="pulse" size={18} />}<span>{label}</span></div>;
}

function findRunProjection(topology: ReturnType<typeof useRuntimeTopology>, runId: string): RunProjection | null {
  for (const environment of topology.environments) {
    const run = environment.runs.find((candidate) => candidate.id === runId);
    if (run) return run;
  }
  return null;
}

function openWorkerEnvironment(
  runId: string,
  environment: WorkerEnvironmentProjection,
  surfaces: ReturnType<typeof useClientSurfaceDocuments>["workerEnvironmentSurfaces"],
) {
  const target = defaultWorkerEnvironmentPaneTarget(environment, surfaces);
  void window.octopusTransport?.openWorkerEnvironmentWindow?.({
    runId,
    workerEnvironmentId: environment.id,
    panelKind: target.panelKind === "file" ? "files" : target.panelKind ?? "overview",
    ...(target.surfaceId ? { surfaceId: target.surfaceId } : {}),
    ...(target.clientSurface ? { clientSurface: target.clientSurface } : {}),
  }).catch((failure) => appToast.danger(formatFailure(failure)));
}

function messageTitle(message: ChatRunMessage): string {
  if (message.eventKind === "steering") return "Steer";
  if (message.eventKind === "tool") return "Tool activity";
  if (message.eventKind === "plan") return "Plan";
  if (message.eventKind === "web_search") return "Web search";
  if (message.eventKind === "approval" || message.eventKind === "elicitation" || message.eventKind === "user_input") return "Operator request";
  return message.role === "user" ? "Operator" : message.error ? "Error" : "Octopus";
}

function formatTime(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "";
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(value);
}

function closeWindow() {
  window.octopusTransport?.windowControl("close");
}

function formatFailure(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}
