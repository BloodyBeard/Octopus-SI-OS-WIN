import type { NotificationSound } from "./clientPreferences";

type ClientNotification = {
  body: string;
  key: string;
  sound: NotificationSound;
  title: string;
  target?: ClientNotificationTarget;
  when?: "always" | "unfocused";
};

export type ClientNotificationTarget =
  | { kind: "run"; runId: string }
  | { kind: "mark"; movementId: string }
  | { kind: "surface"; surfaceId: string };

export const CLIENT_NOTIFICATION_ACTIVATE_EVENT = "octopus:client-notification-activate";

const delivered = new Map<string, number>();

export function notifyClientEvent({ body, key, sound, target, title, when = "unfocused" }: ClientNotification): boolean {
  if (when === "unfocused" && document.hasFocus()) return false;
  if (!("Notification" in window) || Notification.permission !== "granted") return false;
  const now = Date.now();
  const last = delivered.get(key) ?? 0;
  if (now - last < 30_000) return false;
  try {
    const notification = new Notification(title, { body, silent: sound === "silent" });
    notification.onclick = () => {
      window.focus();
      notification.close();
      if (target) window.dispatchEvent(new CustomEvent(CLIENT_NOTIFICATION_ACTIVATE_EVENT, { detail: target }));
    };
    delivered.set(key, now);
    if (delivered.size > 256) {
      for (const [candidate, timestamp] of delivered) {
        if (now - timestamp > 86_400_000) delivered.delete(candidate);
      }
    }
    return true;
  } catch {
    return false;
  }
}
