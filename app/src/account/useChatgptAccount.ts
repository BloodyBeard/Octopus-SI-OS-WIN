import { useEffect, useSyncExternalStore } from "react";
import { createChatgptAccountState, EMPTY_CHATGPT_ACCOUNT_STATE } from "./chatgptAccountState";
import { recordChatgptSignIn } from "../onboarding/chatgptSignIn";

let controller: ReturnType<typeof createChatgptAccountState> | null = null;
let connections = 0;
let disconnect: (() => void) | null = null;
const unavailable = { ...EMPTY_CHATGPT_ACCOUNT_STATE, loading: false, error: "The native ChatGPT account connection is unavailable." };
const noopSubscribe = () => () => {};

function accountController() {
  const bridge = window.octopusTransport;
  if (!bridge?.chatgptAccount) return null;
  controller ??= createChatgptAccountState(bridge);
  return controller;
}

export function useChatgptAccount(enabled = true) {
  const current = enabled ? accountController() : null;
  const state = useSyncExternalStore(current?.subscribe ?? noopSubscribe,
    current?.getSnapshot ?? (() => enabled ? unavailable : EMPTY_CHATGPT_ACCOUNT_STATE));
  useEffect(() => {
    if (!current) return;
    connections += 1;
    if (connections === 1) {
      const stopChanges = current.connect();
      const onFocus = () => { void current.refresh(); };
      window.addEventListener("focus", onFocus);
      disconnect = () => { stopChanges(); window.removeEventListener("focus", onFocus); };
      void current.refresh();
    }
    return () => {
      connections -= 1;
      if (connections === 0) { disconnect?.(); disconnect = null; }
    };
  }, [current]);
  useEffect(() => {
    if (state.account) recordChatgptSignIn(state.account.signedIn);
    else if (state.error) recordChatgptSignIn(false);
  }, [state.account, state.error]);
  return { ...state, refresh: current?.refresh, retrySignInStatus: current?.retrySignInStatus, signIn: current?.signIn, cancel: current?.cancel, logout: current?.logout };
}
