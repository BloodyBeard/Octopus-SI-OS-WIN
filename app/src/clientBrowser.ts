import { appToast } from "./toasts";

export type ClientBrowserState = {
  id: string; url: string; title: string; loading: boolean;
  canGoBack: boolean; canGoForward: boolean; error: string;
};
export type ClientBrowserRequest =
  | { action: "open"; url: string }
  | { action: "navigate"; id: string; url: string }
  | { action: "layout"; id: string; visible: boolean; bounds?: { x: number; y: number; width: number; height: number } }
  | { action: "state" | "close" | "back" | "forward" | "reload" | "stop"; id: string };
export const OPEN_WEB_LINK_EVENT = "octopus:open-web-link";

export function openWebLink(url: string) {
  const event = new CustomEvent(OPEN_WEB_LINK_EVENT, { cancelable: true, detail: { url } });
  if (window.dispatchEvent(event)) appToast.danger("The in-app browser is unavailable in this window.");
}
