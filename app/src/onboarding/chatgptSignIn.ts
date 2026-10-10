export const CHATGPT_SIGN_IN_STORAGE_KEY = "octopus.onboarding.chatgpt.v1";

export function chatgptSignInAvailable(): boolean {
  return typeof window.octopusTransport?.chatgptAccount === "function";
}

export function chatgptSignInCompleted(): boolean {
  try {
    return window.localStorage.getItem(CHATGPT_SIGN_IN_STORAGE_KEY) === "signed-in";
  } catch {
    return false;
  }
}

export function recordChatgptSignIn(): void {
  window.localStorage.setItem(CHATGPT_SIGN_IN_STORAGE_KEY, "signed-in");
}
