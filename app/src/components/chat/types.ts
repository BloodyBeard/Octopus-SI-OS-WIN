export type ChatAttachment = {
  id: string;
  file?: File;
  stage?: import("../../electron").OctopusClientAttachmentStageHandle;
  name: string;
  size: number;
  type: string;
  label: string;
  lastModified: number;
};

export type ChatAttachmentView = Omit<ChatAttachment, "file" | "stage">;

export type ChatRenderMessage = {
  id: string;
  runId?: string;
  role: "user" | "assistant" | "system";
  text: string;
  createdAtMs: number;
  label?: string;
  direction?: "in" | "out";
  attachments?: ChatAttachmentView[];
  artifactId?: string;
  interfaceId?: string;
  eventKind?: import("../../chatRuns").ChatRunEventKind;
  eventData?: Record<string, unknown>;
  latencyMs?: number;
  pending?: boolean;
  error?: boolean;
  terminal?: boolean;
};

export type ChatToolPresentationDescriptor = {
  conversationVisible: boolean;
  kind: "command" | "file_change" | "list" | "read" | "search" | "tool";
  label: string;
  queryArguments: string[];
  targetArguments: string[];
};

export type ChatToolPresentationIndex = Readonly<Record<string, ChatToolPresentationDescriptor>>;

export type ChatOperatorResponse =
  | { kind: "approval"; decision: "accept" | "acceptForSession" | "decline" | "cancel" | Record<string, unknown> }
  | { kind: "elicitation"; answer: unknown }
  | { kind: "user_input"; answers: unknown };

export type ChatQueuedTurnStatus = "queued" | "editing" | "sending" | "accepted" | "unknown" | "failed";

export type ChatQueuedTurnView = {
  id: string;
  text: string;
  attachments: ChatAttachment[];
  createdAtMs: number;
  status: ChatQueuedTurnStatus;
  error?: string;
};

export type ChatSettings = {
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
};
