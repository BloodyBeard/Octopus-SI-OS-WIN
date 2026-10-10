import { settingsTargetProps } from "./settingsCatalog";
import { useEffect, useState } from "react";
import { useClientPreferences, useSetClientPreferences, type ClientPreferences, type NotificationSound } from "../../clientPreferences";
import { OcButton, OcCard, OcGroup, OcSelect, OcSettingsRow, OcStatus, OcSwitch, type OcTone } from "../../design-system/components";
import { useAppErrorToast } from "../../toasts";
import { SettingsHeading } from "./SettingsHeading";

export function MarkActivitySettingsRow() {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  return (
    <OcSettingsRow {...settingsTargetProps("notifications.mark-activity")}
      control={(
        <OcSelect<ClientPreferences["markNotifications"]>
          label="Inbox notifications"
          onChange={(markNotifications) => setClientPreferences({ markNotifications })}
          options={[
            { value: "never", label: "Never" },
            { value: "unfocused", label: "Only when unfocused" },
            { value: "always", label: "Always" },
          ]}
          value={preferences.markNotifications}
        />
      )}
      description="Notify about new messages, requests, and marks selected in Inbox."
      label="Inbox notifications"
    />
  );
}

export function NotificationSoundSettingsRow() {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  return (
    <OcSettingsRow {...settingsTargetProps("notifications.sound")}
      control={(
        <OcSelect<NotificationSound>
          label="Notification sound"
          onChange={(notificationSound) => setClientPreferences({ notificationSound })}
          options={[
            { value: "system", label: "System default" },
            { value: "silent", label: "Silent" },
          ]}
          value={preferences.notificationSound}
        />
      )}
      description="The sound played with desktop notifications."
      label="Sound"
    />
  );
}

export function NotificationsSection() {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const [permission, setPermission] = useState<NotificationPermission | "unsupported">(readNotificationPermission);
  const [notificationError, setNotificationError] = useState("");
  useAppErrorToast(notificationError);

  useEffect(() => {
    function refreshPermission() {
      setPermission(readNotificationPermission());
    }
    window.addEventListener("focus", refreshPermission);
    return () => window.removeEventListener("focus", refreshPermission);
  }, []);

  async function requestNotificationPermission() {
    setNotificationError("");
    try {
      if (!("Notification" in window)) throw new Error("Desktop notifications are unavailable in this environment.");
      const next = await Notification.requestPermission();
      setPermission(next);
    } catch (error) {
      setNotificationError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <section className="settings-section">
      <SettingsHeading
        description="When the desktop can notify you about run and interface activity."
        title="Notifications"
      />

      <OcGroup title="Runs" description="Notifications that follow chat work on this device.">
        <OcCard>
          <MarkActivitySettingsRow />
          <OcSettingsRow {...settingsTargetProps("notifications.operator-input")}
            control={(
              <OcSwitch
                checked={preferences.notifyOperatorInputRequired}
                label="Operator input required"
                onCheckedChange={(notifyOperatorInputRequired) => setClientPreferences({ notifyOperatorInputRequired })}
              />
            )}
            description="Notify when a run pauses on an operator request."
            label="Operator input required"
          />
          <OcSettingsRow {...settingsTargetProps("notifications.blocked")}
            control={(
              <OcSwitch
                checked={preferences.notifyBlockedOrDeliveryFailed}
                label="Blocked or delivery failed"
                onCheckedChange={(notifyBlockedOrDeliveryFailed) => setClientPreferences({ notifyBlockedOrDeliveryFailed })}
              />
            )}
            description="Notify when an artifact hand-in is rejected or a run reports a blocked checkpoint."
            label="Blocked or delivery failed"
          />
        </OcCard>
      </OcGroup>

      <OcGroup title="System" description="Environment and Home voice events.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("notifications.worker-environment")}
            control={(
              <OcSwitch
                checked={preferences.notifyWorkerEnvironmentDisconnected}
                label="Worker Environment disconnected"
                onCheckedChange={(notifyWorkerEnvironmentDisconnected) => setClientPreferences({ notifyWorkerEnvironmentDisconnected })}
              />
            )}
            description="Notify when an attached Worker Environment stops being projected."
            label="Worker Environment disconnected"
          />
          <OcSettingsRow {...settingsTargetProps("notifications.home-voice")}
            control={(
              <OcSwitch
                checked={preferences.notifyVoiceSession}
                label="Home voice"
                onCheckedChange={(notifyVoiceSession) => setClientPreferences({ notifyVoiceSession })}
              />
            )}
            description="Notify when Home voice ends while the window is unfocused."
            label="Home voice"
          />
          <NotificationSoundSettingsRow />
          <OcSettingsRow {...settingsTargetProps("notifications.permission")}
            control={(
              <div className="settings-inline-actions">
                <OcStatus
                  label={notificationPermissionLabel(permission)}
                  tone={notificationPermissionTone(permission)}
                />
                {permission === "default" ? (
                  <OcButton onClick={() => void requestNotificationPermission()}>Allow</OcButton>
                ) : null}
              </div>
            )}
            description={notificationPermissionDescription(permission)}
            label="System permission"
          />
        </OcCard>
      </OcGroup>
    </section>
  );
}

function readNotificationPermission(): NotificationPermission | "unsupported" {
  return "Notification" in window ? Notification.permission : "unsupported";
}

function notificationPermissionLabel(permission: NotificationPermission | "unsupported"): string {
  if (permission === "granted") return "Allowed";
  if (permission === "denied") return "Denied by system";
  if (permission === "default") return "Not requested";
  return "Unavailable";
}

function notificationPermissionTone(permission: NotificationPermission | "unsupported"): OcTone {
  if (permission === "granted") return "success";
  if (permission === "denied") return "danger";
  return "muted";
}

function notificationPermissionDescription(permission: NotificationPermission | "unsupported"): string {
  if (permission === "granted") return "The desktop is allowed to display notifications.";
  if (permission === "denied") return "macOS or the browser has denied notification permission.";
  if (permission === "default") return "Permission has not been requested from this environment yet.";
  return "This environment does not expose the Notification API.";
}
