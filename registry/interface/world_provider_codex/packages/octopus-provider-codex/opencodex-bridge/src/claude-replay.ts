import type { OcxMessage, OcxParsedRequest } from "../node_modules/@bitkyc08/opencodex/src/types.ts";
import { canonicalClaudeJson } from "../node_modules/@bitkyc08/opencodex/src/adapters/claude-cli/stable-replay.ts";
import { formatMessageForHistory, MAX_PROJECTED_HISTORY_CHARS } from "../node_modules/@bitkyc08/opencodex/src/adapters/coding-agent/protocol.ts";

function imageBlock(url: string): Record<string, unknown> {
  const data = /^data:(image\/(?:png|jpeg|gif|webp));base64,([A-Za-z0-9+/]+={0,2})$/.exec(url);
  if (data) return { type: "image", source: { type: "base64", media_type: data[1], data: data[2] } };
  let remote: URL;
  try { remote = new URL(url); } catch { throw new Error("Claude image has an invalid carrier."); }
  if (remote.protocol !== "https:" || remote.username || remote.password) throw new Error("Claude images require base64 data or an HTTPS URL without credentials.");
  return { type: "image", source: { type: "url", url } };
}

export function buildClaudeConversationInput(parsed: OcxParsedRequest, options: {
  maxHistoryChars?: number; vendorNameByWire?: ReadonlyMap<string, string>;
} = {}): string[] {
  for (const message of parsed.context.messages) {
    if (message.role === "developer" && Array.isArray(message.content) && message.content.some(part => part.type === "image")) {
      throw new Error("Claude developer instructions cannot contain images.");
    }
  }
  const messages = parsed.context.messages.filter(message => message.role !== "developer");
  const blocks = messages.map(message => {
    const stable: OcxMessage = message.role === "assistant" ? {
      ...message, content: message.content.map(part => part.type === "toolCall"
        ? { ...part, arguments: canonicalClaudeJson(part.arguments ?? {}) as Record<string, unknown> } : part),
    } : message;
    const content: Record<string, unknown>[] = [{ type: "text", text: formatMessageForHistory(stable, options.vendorNameByWire) }];
    if (Array.isArray(message.content)) {
      for (const part of message.content) if (part.type === "image") content.push(imageBlock(part.imageUrl));
    }
    return content;
  });
  const requested = options.maxHistoryChars ?? MAX_PROJECTED_HISTORY_CHARS;
  const limit = Number.isFinite(requested) && requested >= 0 ? Math.floor(requested) : MAX_PROJECTED_HISTORY_CHARS;
  const history = blocks.slice(0, -1);
  const size = history.reduce((sum, block) => sum + String(block[0]!.text).length, 0);
  let omitted = 0;
  if (size > limit) {
    const step = Math.max(1, Math.min(20_000, Math.floor(limit / 10)));
    const target = Math.ceil((size - limit) / step) * step;
    let removed = 0;
    while (omitted < history.length && removed < target) removed += String(history[omitted++]![0]!.text).length;
    while (omitted < history.length && messages[omitted]!.role === "toolResult") omitted++;
    const current = messages.at(-1);
    if (current?.role === "toolResult") {
      const call = messages.slice(0, -1).findLastIndex(message => message.role === "assistant"
        && message.content.some(part => part.type === "toolCall" && part.id === current.toolCallId));
      if (call >= 0 && omitted > call) omitted = call;
    }
  }
  const content = [
    ...(omitted ? [{ type: "text", text: "[Earlier conversation history omitted at a whole-message checkpoint.]" }] : []),
    ...blocks.slice(omitted).flat(),
  ];
  if (!content.length) content.push({ type: "text", text: "" });
  return [JSON.stringify({ type: "user", message: { role: "user", content } })];
}
