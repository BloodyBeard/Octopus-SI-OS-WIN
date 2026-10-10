export const CHAT_MESSAGE_REVEAL_EVENT = "octopus:chat-message-reveal";

export type ChatMessageRevealRequest = {
  runId: string;
  messageId: string;
};

let pendingReveal: ChatMessageRevealRequest | null = null;

export function requestChatMessageReveal(request: ChatMessageRevealRequest): void {
  pendingReveal = request;
  window.dispatchEvent(new CustomEvent(CHAT_MESSAGE_REVEAL_EVENT));
}

export function consumeChatMessageReveal(runId: string): ChatMessageRevealRequest | null {
  if (!pendingReveal || pendingReveal.runId !== runId) return null;
  const request = pendingReveal;
  pendingReveal = null;
  return request;
}
