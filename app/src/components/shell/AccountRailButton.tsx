import { useLayoutEffect, useRef, useState } from "react";
import { OcMenuItem } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { formatKeyboardShortcuts, useKeyboardShortcuts } from "../../keyboardShortcuts";
import type { RuntimeHostView } from "../../runtimeHost";
import { MessageTooltip } from "../chat/MessageTooltip";

export function AccountRailButton({ accessProfile, runtimeHost, onLogOut, onOpenSettings }: {
  accessProfile: "live" | "lab";
  runtimeHost: RuntimeHostView;
  onLogOut: () => void;
  onOpenSettings: () => void;
}) {
  const shortcuts = useKeyboardShortcuts(accessProfile);
  const trigger = useRef<HTMLButtonElement>(null);
  const openingSettings = useRef(false);
  const [open, setOpen] = useState(false);
  useLayoutEffect(() => {
    if (open || !openingSettings.current) return;
    openingSettings.current = false;
    document.getElementById("settings-search")?.focus({ preventScroll: true });
  }, [open]);
  const name = accessProfile === "lab" ? "Test" : "Administrator";
  const connected = runtimeHost.available && runtimeHost.ready;
  const settingsShortcut = formatKeyboardShortcuts(shortcuts.open_settings ?? []);
  const avatar = <span className="navigation-rail-avatar" aria-hidden="true">{name.slice(0, 1)}</span>;

  return (
    <div className="navigation-rail-account">
      <MessageTooltip label="Account" side="right" delayMs={300} disabled={open}>
        <button
          aria-expanded={open}
          aria-haspopup="menu"
          aria-label={`Account: ${name}`}
          className="oc-octopus-icon-control no-drag"
          data-appearance="rail"
          data-state={open ? "open" : "closed"}
          onClick={() => setOpen(!open)}
          onKeyDown={(event) => {
            if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
            event.preventDefault();
            event.stopPropagation();
            setOpen(true);
          }}
          ref={trigger}
          type="button"
        >{avatar}</button>
      </MessageTooltip>
      {accessProfile === "live" ? <span
        aria-label={connected ? "Runtime connected" : "Runtime unavailable"}
        className={`sidebar-runtime-dot navigation-rail-account-status${connected ? " is-ready" : runtimeHost.phase === "degraded" ? " is-error" : ""}`}
        role="status"
        title={connected ? "Runtime connected" : runtimeHost.error || runtimeHost.phase}
      /> : null}
      <OcDropdown align="end" anchorRef={trigger} aria-label="Account" contentWidth="sidebar" open={open} onOpenChange={setOpen} onCloseAutoFocus={(event) => {
        if (openingSettings.current) event.preventDefault();
      }} side="right" sideOffset={8}>
        <div className="navigation-account-heading">{avatar}<span>{name}</span></div>
        <div className="navigation-account-separator" role="separator" />
        <OcMenuItem label="Settings" leadingIcon="settings" onClick={() => { openingSettings.current = true; trigger.current?.focus(); onOpenSettings(); }} role="menuitem" trailing={settingsShortcut ? <span className="text-xs text-token-description-foreground">{settingsShortcut}</span> : undefined} />
        <OcMenuItem label="Log out" leadingIcon="logOut" onClick={onLogOut} role="menuitem" />
      </OcDropdown>
    </div>
  );
}
