import { useEffect, useSyncExternalStore } from "react";
import type { OctopusChatgptAccount } from "../electron";
import { accountUsageIdentity, createChatgptUsageState, EMPTY_CHATGPT_USAGE_STATE } from "./chatgptUsageState";

let controller: ReturnType<typeof createChatgptUsageState> | null = null;
const noopSubscribe = () => () => {};
const unavailable = { ...EMPTY_CHATGPT_USAGE_STATE, error: "The native usage connection is unavailable." };

export function useChatgptUsage(account: OctopusChatgptAccount | null, enabled = true) {
  const bridge = window.octopusTransport;
  if (enabled && bridge?.chatgptRateLimits) controller ??= createChatgptUsageState(bridge);
  const current = enabled ? controller : null;
  const snapshot = useSyncExternalStore(current?.subscribe ?? noopSubscribe,
    current?.getSnapshot ?? (() => enabled ? unavailable : EMPTY_CHATGPT_USAGE_STATE));
  const identity = accountUsageIdentity(account);
  useEffect(() => {
    if (current) void current.observe(account);
  }, [account, current]);
  const state = !enabled || identity === null ? EMPTY_CHATGPT_USAGE_STATE
    : !current ? unavailable
      : snapshot.identity === identity ? snapshot : { ...EMPTY_CHATGPT_USAGE_STATE, loading: true };
  return { ...state, refresh: current?.refresh };
}
