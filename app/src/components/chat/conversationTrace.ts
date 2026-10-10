import { dynamicToolPresentation } from "./toolPresentation";
import type { ChatRenderMessage, ChatToolPresentationIndex } from "./types";

export type ConversationTraceAdmission = "hidden" | "state" | "visible";

export function isFinalAssistantResponse(message: ChatRenderMessage): boolean {
  if (message.role !== "assistant" || message.error) return false;
  const kind = message.eventKind ?? "message";
  const phase = text(record(message.eventData).phase);
  if (kind === "message_delta") return phase === "final_answer";
  return kind === "message" && message.terminal !== false && phase !== "commentary" && phase !== "partial_answer";
}

export function conversationTraceAdmission(
  message: ChatRenderMessage,
  toolPresentations: ChatToolPresentationIndex,
): ConversationTraceAdmission {
  const data = record(message.eventData);
  if (data._movement_evidence === true) return "hidden";
  if (message.role === "user") return "visible";
  const kind = message.eventKind ?? "message";
  if (kind === "event") {
    return ["submitting", "working"].includes(text(data.work_status)) ? "state" : "hidden";
  }
  if ([
    "message",
    "message_delta",
    "commentary",
    "commentary_delta",
    "reasoning",
    "reasoning_delta",
    "tool",
    "tool_output_delta",
    "terminal_interaction",
    "file_change",
    "file_change_delta",
    "diff",
    "mcp_tool",
    "mcp_progress",
    "mcp_status",
    "collaboration",
    "subagent_activity",
    "image_view",
    "image_generation",
    "compaction",
    "approval",
    "elicitation",
    "user_input",
    "warning",
    "error",
  ].includes(kind)) return "visible";
  if (kind === "web_search") {
    return text(data.query) || text(data.action) || message.text.trim() ? "visible" : "hidden";
  }
  if (kind === "dynamic_tool") {
    const originRole = text(record(data._activity_origin).interface_role).toLocaleLowerCase();
    // Tool movements are product trace only when their declared source is the
    // selected model provider. System authoring and unresolved provenance stay
    // available in ledger diagnostics but must not masquerade as model actions.
    if (originRole !== "provider") return "hidden";
    return dynamicToolPresentation(message, toolPresentations).conversationVisible ? "visible" : "hidden";
  }
  if (["review_entered", "review_exited", "auto_review_started", "auto_review_completed"].includes(kind)) {
    const status = text(data.status) || text(record(data.review).status);
    return status.toLocaleLowerCase().replace(/[_-]/g, "") === "approved" ? "hidden" : "visible";
  }
  if (kind === "model_rerouted") {
    const reason = text(data.reason).toLocaleLowerCase().replace(/[_-]/g, "");
    return reason === "highriskcyberactivity" ? "visible" : "hidden";
  }
  if (kind === "auth_recovery_started" || kind === "auth_recovery_completed") return "visible";
  if ([
    "turn_started",
    "turn_completed",
    "turn_control",
    "interrupted_result",
    "item_started",
    "model_verification",
  ].includes(kind)) return "state";
  return "hidden";
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
