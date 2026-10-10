import type { ChatRenderMessage } from "./types";

export type MultiAgentAction = "spawnAgent" | "sendInput" | "resumeAgent" | "closeAgent";
export type MultiAgentStatus = "inProgress" | "completed" | "failed";
export type MultiAgentStateStatus = "pendingInit" | "running" | "interrupted" | "shutdown" | "completed" | "errored" | "notFound";

export type MultiAgentActivityItem = {
  action: MultiAgentAction;
  agentsStates: Record<string, unknown>;
  id: string;
  model: string;
  prompt: string;
  receiverThreadIds: string[];
  status: MultiAgentStatus;
};

export type MultiAgentStateItem = Pick<
  MultiAgentActivityItem,
  "agentsStates" | "id" | "receiverThreadIds"
>;

export type SubagentActivityItem = {
  agentThreadId: string;
  displayName: string;
  displayStatus: "active" | "updated" | "interrupted";
  id: string;
};

export function normalizeMultiAgentActivity(message: ChatRenderMessage | undefined): MultiAgentActivityItem | null {
  if (!message) return null;
  const state = normalizeMultiAgentState(message);
  if (!state) return null;
  const data = record(message.eventData);
  const action = multiAgentAction(data.tool);
  const status = multiAgentStatus(data.status);
  if (!action || !status) return null;
  return {
    action,
    ...state,
    model: text(data.model),
    prompt: typeof data.prompt === "string" ? data.prompt : "",
    status,
  };
}

export function normalizeMultiAgentState(message: ChatRenderMessage | undefined): MultiAgentStateItem | null {
  if (message?.eventKind !== "collaboration") return null;
  const data = record(message.eventData);
  if (!multiAgentAction(data.tool) && text(data.tool) !== "wait") return null;
  return {
    agentsStates: record(data.agentsStates ?? data.agents_states),
    id: text(data.id) || message.id,
    receiverThreadIds: stringArray(data.receiverThreadIds ?? data.receiver_thread_ids),
  };
}

export function normalizeSubagentActivity(message: ChatRenderMessage | undefined): SubagentActivityItem | null {
  if (message?.eventKind !== "subagent_activity") return null;
  const data = record(message.eventData);
  const agentThreadId = text(data.agentThreadId ?? data.agent_thread_id);
  const displayStatus = subagentDisplayStatus(data.kind);
  if (!agentThreadId || !displayStatus) return null;
  return {
    agentThreadId,
    displayName: subagentDisplayName(text(data.agentPath ?? data.agent_path)) ?? "Agent",
    displayStatus,
    id: text(data.id) || message.id,
  };
}

export function multiAgentAction(value: unknown): MultiAgentAction | null {
  switch (text(value)) {
    case "spawnAgent":
    case "sendInput":
    case "resumeAgent":
    case "closeAgent":
      return text(value) as MultiAgentAction;
    default:
      return null;
  }
}

export function multiAgentStatus(value: unknown): MultiAgentStatus | null {
  switch (text(value)) {
    case "inProgress":
    case "completed":
    case "failed":
      return text(value) as MultiAgentStatus;
    default:
      return null;
  }
}

export function multiAgentStateStatus(value: unknown): MultiAgentStateStatus | null {
  switch (text(value)) {
    case "pendingInit":
    case "running":
    case "interrupted":
    case "shutdown":
    case "completed":
    case "errored":
    case "notFound":
      return text(value) as MultiAgentStateStatus;
    default:
      return null;
  }
}

export function multiAgentIds(item: MultiAgentStateItem): string[] {
  return [...new Set([...item.receiverThreadIds, ...Object.keys(item.agentsStates)])].sort();
}

export function multiAgentStateIsTerminal(status: MultiAgentStateStatus | null): boolean {
  return status === "interrupted"
    || status === "shutdown"
    || status === "completed"
    || status === "errored"
    || status === "notFound";
}

export function subagentDisplayStatus(value: unknown): SubagentActivityItem["displayStatus"] | null {
  switch (text(value)) {
    case "started": return "active";
    case "interacted": return "updated";
    case "interrupted": return "interrupted";
    default: return null;
  }
}

function subagentDisplayName(agentPath: string): string | null {
  const leaf = agentPath.split("/").map((part) => part.trim()).filter((part) => part && part !== "root").at(-1);
  if (!leaf) return null;
  const normalized = leaf.replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim().toLowerCase();
  return normalized ? normalized[0].toUpperCase() + normalized.slice(1) : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.map(text).filter(Boolean) : [];
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
