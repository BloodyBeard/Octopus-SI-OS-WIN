import type { ChatRun, ChatRunMessage } from "../../chatRuns";
import type { OctopusOperatorChatArtifact, OctopusOperatorChatMovementArtifact } from "../../electron";

function value(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function orderedMessages(messages: readonly ChatRunMessage[]): ChatRunMessage[] {
  return [...messages].sort((left, right) => left.movementSeq - right.movementSeq);
}

export function isChatReplyTerminal(eventKind: string, eventData: Record<string, unknown>, terminal: boolean): boolean {
  return eventKind === "turn_control"
    ? value(eventData.status) === "acknowledged" && ["stop", "interrupt"].includes(value(eventData.action))
    : terminal;
}

export function operatorTurnControlReceipt(
  run: Pick<ChatRun, "runId" | "providerBoundInterfaceId" | "chatInterfaceId">,
  messages: readonly ChatRunMessage[],
  control: { controlId: string; action: string },
): ChatRunMessage | undefined {
  return orderedMessages(messages).reverse().find((message) => {
    const movement = message.eventData._movement as { from?: { Interface?: string }; to?: { Interface?: string } } | undefined;
    return message.runId === run.runId && message.direction === "out" && Boolean(message.artifactId)
      && message.eventKind === "turn_control" && message.interfaceId === run.chatInterfaceId
      && movement?.from?.Interface === run.providerBoundInterfaceId && movement?.to?.Interface === run.chatInterfaceId
      && message.eventData.control_id === control.controlId && message.eventData.action === control.action
      && ["acknowledged", "failed"].includes(value(message.eventData.status));
  });
}

export function operatorTurnResumeAcknowledgedOpen(messages: readonly ChatRunMessage[], afterSequence = -1): boolean {
  let open = false;
  for (const message of orderedMessages(messages)) {
    if (message.movementSeq <= afterSequence) continue;
    if (message.eventKind === "turn_control") {
      if (value(message.eventData.status) !== "acknowledged") continue;
      const action = value(message.eventData.action);
      if (action === "resume") open = true;
      else if (action === "interrupt" || action === "stop") open = false;
      continue;
    }
    if (message.role === "assistant"
      && (message.terminal || message.eventKind === "turn_completed" || message.eventKind === "interrupted_result")) open = false;
  }
  return open;
}

export function reconcileChatRunReceipts(run: ChatRun, artifacts: readonly OctopusOperatorChatArtifact[]): ChatRun {
  let next = run;
  const movements = artifacts.filter((artifact): artifact is OctopusOperatorChatMovementArtifact => artifact.source === "movement" && artifact.run_id === run.runId)
    .sort((left, right) => left.movement_seq - right.movement_seq);
  for (const artifact of movements) {
    if (artifact.movement_seq <= next.lastMovementSeq) continue;
    let turnOpen = next.turnOpen;
    let turnState = next.turnState;
    const providerOwned = artifact.from_interface_id === run.providerBoundInterfaceId
      && artifact.interface_id === run.chatInterfaceId;
    const acknowledgedAction = artifact.event_kind === "turn_control"
      && value(artifact.event_data.status) === "acknowledged"
      ? value(artifact.event_data.action) : "";
    if (providerOwned) {
      if (artifact.event_kind === "turn_started" || acknowledgedAction === "resume") {
        turnOpen = true;
        turnState = "working";
      } else if (artifact.event_kind === "turn_completed") {
        const completed = artifact.event_data.turn as Record<string, unknown> | undefined;
        turnOpen = false;
        turnState = value(artifact.event_data.status ?? completed?.status) === "failed" ? "failed" : "idle";
      } else if ((artifact.terminal && artifact.event_kind !== "turn_control") || artifact.event_kind === "interrupted_result"
        || acknowledgedAction === "interrupt" || acknowledgedAction === "stop") {
        turnOpen = false;
        turnState = artifact.error || value(artifact.event_data.native_terminal_status) === "failed" ? "failed" : "idle";
      } else if (turnOpen && artifact.event_kind !== "rate_limits") {
        turnState = "working";
      }
    }
    next = { ...next, turnOpen, turnState,
      lastMovementSeq: artifact.movement_seq,
      updatedAtMs: Math.max(next.updatedAtMs, artifact.recorded_at_unix_ms) };
  }
  return next;
}

export function operatorTurnResumeAvailable(
  messages: readonly ChatRunMessage[],
  turnOpen: boolean,
): boolean {
  if (turnOpen) return false;
  let available = false;
  for (const message of orderedMessages(messages)) {
    if (message.role === "user" && message.eventKind !== "steering") {
      available = false;
      continue;
    }
    if (message.eventKind === "turn_control") {
      if (value(message.eventData.status) !== "acknowledged") continue;
      const action = value(message.eventData.action);
      if (action === "resume") available = false;
      else if (action === "interrupt" || action === "stop") available = true;
      continue;
    }
    if (message.eventKind === "interrupted_result" && message.terminal) {
      available = value(message.eventData.native_terminal_status) !== "completed";
      continue;
    }
    if (message.terminal && message.error) {
      available = true;
      continue;
    }
    if (message.role === "assistant" && message.eventKind === "message" && message.terminal) {
      available = false;
    }
  }
  return available;
}
