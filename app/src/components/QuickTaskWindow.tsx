import { AnimatePresence, motion } from "motion/react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { OctopusClientAccessProfile } from "../electron";
import { dispatchClientCommand } from "../clientActions";
import {
  clientOsRootEeId,
  clientTaskTitle,
  recordClientTaskOpened,
  useClientTaskOrganization,
} from "../shell/clientWorkspaces";
import { appToast } from "../toasts";
import { ChatComposer } from "./chat/ChatComposer";
import { MessageContent } from "./chat/MessageContent";
import { QueuedMessageList } from "./chat/QueuedMessageList";
import { useChatWorkspace } from "./chat/useChatWorkspace";
import { Glyph } from "./IconGlyphs";
import "./chat/chat.css";

export function QuickTaskWindow({ accessProfile, runtimeReady, runtimeHome }: {
  accessProfile: OctopusClientAccessProfile;
  runtimeReady: boolean;
  runtimeHome: string;
}) {
  const chat = useChatWorkspace(accessProfile, true, runtimeReady, "quick", "task", runtimeHome);
  const organization = useClientTaskOrganization(accessProfile);
  const [historyOpen, setHistoryOpen] = useState(false);
  const historyRef = useRef<HTMLDivElement>(null);
  const historyButtonRef = useRef<HTMLButtonElement>(null);
  const recentRank = useMemo(() => new Map(organization.recentRunIds.map((runId, index) => [runId, index])), [organization.recentRunIds]);
  const recentRuns = useMemo(() => [...chat.runs]
    .filter((run) => !run.archived)
    .sort((left, right) => {
      const leftRank = recentRank.get(left.runId) ?? Number.MAX_SAFE_INTEGER;
      const rightRank = recentRank.get(right.runId) ?? Number.MAX_SAFE_INTEGER;
      return leftRank - rightRank || right.updatedAtMs - left.updatedAtMs;
    })
    .slice(0, 8), [chat.runs, recentRank]);
  const visibleMessages = useMemo(() => chat.renderedMessages.filter((message) => (
    message.eventKind !== "usage"
    && message.eventKind !== "hook"
    && (message.text.trim() || message.pending)
  )).slice(-8), [chat.renderedMessages]);
  const error = chat.error || chat.runsError || chat.newTaskAuthoringError || chat.providerInterfaceError;
  const osRootEeId = useMemo(() => clientOsRootEeId(chat.runs), [chat.runs]);
  const startNewTask = () => {
    chat.createChat();
    if (osRootEeId) chat.setEeId(osRootEeId);
  };

  useEffect(() => window.octopusTransport?.onNativeClientCommand?.((command) => {
    if (command === "new_chat") {
      startNewTask();
      return;
    }
    if (command === "open_model_picker" || command === "attach_files" || command === "start_dictation") {
      dispatchClientCommand(command, "keyboard");
    }
  }), [startNewTask]);

  useEffect(() => {
    if (!historyOpen) return;
    const dismiss = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!historyRef.current?.contains(target) && !historyButtonRef.current?.contains(target)) setHistoryOpen(false);
    };
    const dismissWithKeyboard = (event: KeyboardEvent) => {
      if (event.key === "Escape") setHistoryOpen(false);
    };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", dismissWithKeyboard);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", dismissWithKeyboard);
    };
  }, [historyOpen]);

  const activeTitle = chat.activeRun
    ? clientTaskTitle(chat.activeRun.runId, chat.activeRun.title, organization)
    : "Quick task";

  return (
    <main className="quick-task-window">
      <header className="quick-task-titlebar">
        <div className="quick-task-brand">
          <img alt="" src="./octopus-logo.png" />
          <span className="quick-task-title-copy">
            <strong>{activeTitle}</strong>
            <small>{chat.activeRun ? "Octopus OS task" : "New Octopus OS task"}</small>
          </span>
        </div>
        <div className="quick-task-title-actions">
          <button
            aria-expanded={historyOpen}
            aria-haspopup="menu"
            aria-label="Recent tasks"
            onClick={() => setHistoryOpen((current) => !current)}
            ref={historyButtonRef}
            title="Recent tasks"
            type="button"
          >
            <Glyph name="viewAll" size={15} />
          </button>
          <button aria-label="Start another quick task" onClick={startNewTask} title="New task" type="button"><Glyph name="newTask" size={15} /></button>
          {chat.activeRunId ? (
            <button
              aria-label="Open task in main Octopus window"
              onClick={() => void window.octopusTransport?.openRunInMain?.(chat.activeRunId).catch((failure) => appToast.danger(formatFailure(failure)))}
              title="Open in Octopus"
              type="button"
            >
              <Glyph name="openExternal" size={15} />
            </button>
          ) : null}
          <button aria-label="Close quick task" onClick={() => window.octopusTransport?.windowControl("close")} title="Close" type="button"><Glyph name="close" size={15} /></button>
        </div>
        {historyOpen ? (
          <div aria-label="Recent tasks" className="quick-task-history" ref={historyRef} role="menu">
            <div className="quick-task-history-heading"><span>Recent tasks</span><small>{recentRuns.length}</small></div>
            {recentRuns.map((run) => (
              <button
                aria-current={run.runId === chat.activeRunId ? "page" : undefined}
                key={run.runId}
                onClick={() => {
                  chat.selectRun(run.runId);
                  recordClientTaskOpened(accessProfile, run.runId);
                  setHistoryOpen(false);
                }}
                role="menuitem"
                type="button"
              >
                <span>{clientTaskTitle(run.runId, run.title, organization)}</span>
                <small>{run.turnState === "working" ? "Working" : run.messageCount ? `${run.messageCount} messages` : "No messages"}</small>
              </button>
            ))}
            {!recentRuns.length ? <div className="quick-task-history-empty">No projected tasks yet</div> : null}
          </div>
        ) : null}
      </header>

      <section aria-live="polite" className={`quick-task-transcript${visibleMessages.length ? " has-messages" : ""}`}>
        {!visibleMessages.length ? (
          <motion.div animate={{ opacity: 1, y: 0 }} className="quick-task-empty" initial={{ opacity: 0, y: 4 }}>
            <Glyph name="chat" size={19} />
            <div><strong>What should Octopus work on?</strong><span>This opens a normal task through the declared operator interface.</span></div>
          </motion.div>
        ) : (
          <AnimatePresence initial={false}>
            {visibleMessages.map((message) => (
              <motion.article
                animate={{ opacity: 1, y: 0 }}
                className={`quick-task-message is-${message.role}${message.error ? " is-error" : ""}`}
                exit={{ opacity: 0 }}
                initial={{ opacity: 0, y: 3 }}
                key={message.id}
                transition={{ duration: 0.14 }}
              >
                {message.pending && !message.text ? (
                  <span className="quick-task-working"><i /><span>Working</span></span>
                ) : message.role === "assistant" ? (
                  <MessageContent text={message.text} />
                ) : (
                  <p>{message.text}</p>
                )}
              </motion.article>
            ))}
          </AnimatePresence>
        )}
      </section>

      {error ? <div className="quick-task-error" role="alert"><Glyph name="warning" size={14} /><span>{error}</span></div> : null}
      {!runtimeReady ? <div className="quick-task-runtime-state"><Glyph name="warning" size={14} />Runtime unavailable</div> : null}

      <footer className="quick-task-composer">
        <QueuedMessageList
          notice={chat.queueNotice}
          messages={chat.queuedTurns}
          paused={chat.queuePaused}
          sendNowAvailable={chat.canSendQueued}
          onDelete={chat.deleteQueuedTurn}
          onEdit={chat.editQueuedTurn}
          onSendNow={chat.steerQueuedTurn}
          onReorder={chat.reorderQueuedTurns}
          onResume={chat.resumeQueuedTurns}
          onRetry={chat.retryQueuedTurn}
        />
        <ChatComposer
          accessProfile={chat.accessProfile}
          activeProvider={chat.activeProvider}
          agentPresetId={chat.agentPresetId}
          accessMode={chat.accessMode}
          agentPresets={chat.composerAgentPresets}
          attachments={chat.attachments}
          applicationIds={chat.applicationIds}
          applicationSelectionLocked={chat.applicationSelectionLocked}
          applicationSelectionLockReason={chat.applicationSelectionLockReason}
          canSubmit={chat.canSubmit}
          canQueue={chat.canQueue}
          onQueue={chat.queueCurrentTurn}
          editingQueuedTurn={chat.editingQueuedTurn}
          onCancelQueuedEdit={chat.cancelQueuedEdit}
          driverNames={chat.driverNames}
          eeId={chat.eeId}
          executionContextLocked={Boolean(chat.activeRunId)}
          executionContexts={chat.executionContexts}
          fileInputRef={chat.fileInputRef}
          interruptPending={chat.turnControlPending}
          isSubmitting={chat.submitting}
          onFilesPicked={chat.onFilesPicked}
          onAgentPresetChange={chat.setAgentPresetId}
          onAccessModeChange={chat.setAccessMode}
          onApplicationToggle={chat.toggleApplication}
          onDriverToggle={chat.toggleDriver}
          onEeChange={chat.setEeId}
          onKeyDown={chat.onComposerKeyDown}
          onInterrupt={chat.interruptActiveTurn}
          onResume={chat.resumeActiveTurn}
          onProviderInterfaceChange={chat.setProviderInterfaceId}
          onProviderModelChange={chat.setProviderModel}
          onProviderModelsRefresh={chat.refreshProviderModels}
          onReasoningEffortChange={chat.setReasoningEffort}
          onRemoveAttachment={chat.removeAttachment}
          onResolvePausedQueueSubmission={chat.resolvePausedQueueSubmission}
          onSubmit={chat.submit}
          onTextChange={chat.setText}
          providerInterfaceId={chat.providerInterfaceId}
          pausedQueueSubmissionCount={chat.pausedQueueSubmissionCount}
          providerModel={chat.providerModel}
          providerInterfaces={chat.providerInterfaces}
          providerInterfacesLoaded={chat.providerInterfacesLoaded}
          reasoningEffort={chat.reasoningEffort}
          resumeAvailable={chat.resumeAvailable}
          runId={chat.activeRunId}
          submissionMode={chat.busy ? "steer" : "send"}
          submitBlockReason={chat.submitBlockReason}
          text={chat.text}
          turnOpen={chat.turnOpen}
          workerEnvironmentRooms={chat.workerEnvironmentRooms}
          workerInterfaceId={chat.workerInterfaceId}
        />
      </footer>
    </main>
  );
}

export function QuickTaskAccessUnavailable() {
  return (
    <main className="quick-task-window is-unavailable">
      <header className="quick-task-titlebar"><div className="quick-task-brand"><img alt="" src="./octopus-logo.png" /><span>Quick task</span></div><button aria-label="Close quick task" onClick={() => window.octopusTransport?.windowControl("close")} type="button"><Glyph name="close" size={15} /></button></header>
      <div className="quick-task-empty"><Glyph name="lock" size={19} /><div><strong>Octopus is locked</strong><span>Unlock the main application before opening Quick task.</span></div></div>
    </main>
  );
}

function formatFailure(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}
