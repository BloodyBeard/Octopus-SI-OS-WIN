import type { ChatRenderMessage } from "./types";

export type ContextUsage = { usedTokens: number; contextWindow: number; percent: number };

export function latestContextUsage(messages: readonly ChatRenderMessage[]): ContextUsage | null {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const usage = messages[index].eventData?.context_window_usage;
    if (!usage || typeof usage !== "object" || Array.isArray(usage)) continue;
    const { used_tokens: used, context_window: limit } = usage as Record<string, unknown>;
    if (typeof used !== "number" || !Number.isFinite(used) || used < 0
      || typeof limit !== "number" || !Number.isFinite(limit) || limit <= 0) return null;
    const usedTokens = Math.min(used, limit);
    return { usedTokens, contextWindow: limit, percent: usedTokens / limit * 100 };
  }
  return null;
}
