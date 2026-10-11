import type { OctopusChatgptAccount } from "../electron";

export type ChatgptAccountBridge = Pick<NonNullable<Window["octopusTransport"]>,
  "chatgptAccount" | "chatgptSignInStart" | "chatgptSignInStatus" | "chatgptSignInCancel" | "chatgptLogout" | "onChatgptAccountChanged">;

export type ChatgptAccountState = {
  account: OctopusChatgptAccount | null;
  loading: boolean;
  action: "idle" | "starting" | "waiting" | "cancelling" | "signing-out";
  error: string;
  signInStatusError: string;
  signInUrl: string;
};

export const EMPTY_CHATGPT_ACCOUNT_STATE: ChatgptAccountState = { account: null, loading: true, action: "idle", error: "", signInStatusError: "", signInUrl: "" };

export function createChatgptAccountState(bridge: ChatgptAccountBridge) {
  let state = EMPTY_CHATGPT_ACCOUNT_STATE;
  let readGeneration = 0;
  let pendingRead: Promise<OctopusChatgptAccount | null> | null = null;
  let actionGeneration = 0;
  let loginId = "";
  let timer: ReturnType<typeof setTimeout> | null = null;
  const listeners = new Set<() => void>();
  const message = (error: unknown) => error instanceof Error ? error.message : String(error);
  function update(patch: Partial<ChatgptAccountState>) {
    state = { ...state, ...patch };
    for (const listener of listeners) listener();
  }
  function stopPolling() {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    actionGeneration += 1;
  }
  function refresh(): Promise<OctopusChatgptAccount | null> {
    const generation = ++readGeneration;
    update({ loading: true, error: "" });
    const request: Promise<OctopusChatgptAccount | null> = Promise.resolve().then(() => bridge.chatgptAccount()).then((account) => {
      if (generation !== readGeneration) return pendingRead !== request ? pendingRead : state.account;
      update({ account, loading: false });
      return account;
    }, (error) => {
      if (generation !== readGeneration) return pendingRead !== request ? pendingRead : state.account;
      update({ account: null, loading: false, error: message(error) });
      return null;
    });
    pendingRead = request;
    return request;
  }
  async function poll(generation: number) {
    let terminal = false;
    try {
      const result = await bridge.chatgptSignInStatus(loginId);
      if (generation !== actionGeneration || state.action !== "waiting") return;
      update({ signInStatusError: "" });
      if (result.state === "pending") {
        timer = setTimeout(() => void poll(generation), 1000);
        return;
      }
      terminal = true;
      if (result.state !== "completed") throw new Error(result.error || "ChatGPT sign-in did not complete.");
      const account = await refresh();
      if (generation !== actionGeneration) return;
      if (!account?.signedIn) throw new Error(state.error || "Sign in with ChatGPT to continue.");
      loginId = "";
      update({ action: "idle", signInUrl: "" });
    } catch (error) {
      if (generation !== actionGeneration) return;
      if (terminal) loginId = "";
      update(terminal
        ? { action: "idle", error: message(error), signInStatusError: "", signInUrl: "" }
        : { action: "waiting", signInStatusError: message(error) });
    }
  }
  return {
    getSnapshot: () => state,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    refresh,
    async retrySignInStatus() {
      if (state.action !== "waiting" || !loginId || !state.signInStatusError) return;
      stopPolling();
      update({ signInStatusError: "" });
      await poll(actionGeneration);
    },
    connect() { return bridge.onChatgptAccountChanged(() => { void refresh(); }); },
    async signIn() {
      if (state.action !== "idle") return;
      stopPolling();
      const generation = actionGeneration;
      update({ action: "starting", error: "", signInStatusError: "", signInUrl: "" });
      try {
        const result = await bridge.chatgptSignInStart();
        if (generation !== actionGeneration) return;
        loginId = result.loginId;
        update({ action: "waiting", signInUrl: result.authUrl });
        void poll(generation);
      } catch (error) {
        if (generation === actionGeneration) update({ action: "idle", error: message(error) });
      }
    },
    async cancel() {
      if (state.action !== "waiting" || !loginId) return;
      stopPolling();
      update({ action: "cancelling", error: "", signInStatusError: "" });
      try {
        await bridge.chatgptSignInCancel(loginId);
        loginId = "";
        update({ action: "idle", signInUrl: "" });
        await refresh();
      } catch (error) {
        update({ action: "waiting", error: message(error) });
        void poll(actionGeneration);
      }
    },
    async logout() {
      if (state.action !== "idle") return;
      stopPolling();
      readGeneration += 1;
      update({ action: "signing-out", error: "", signInStatusError: "", signInUrl: "" });
      try {
        const account = await bridge.chatgptLogout();
        if (account.signedIn) throw new Error("ChatGPT logout was not confirmed.");
        readGeneration += 1;
        update({ account, loading: false, action: "idle" });
      } catch (error) {
        await refresh();
        update({ action: "idle", error: message(error) });
      }
    },
  };
}
