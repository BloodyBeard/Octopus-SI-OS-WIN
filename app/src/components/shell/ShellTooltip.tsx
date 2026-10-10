import type { ReactNode } from "react";
import { MessageTooltip } from "../chat/MessageTooltip";
import "./shellTooltip.css";

export function ShellTooltip({ children, label, shortcut, disabled }: {
  children: ReactNode;
  label: string;
  shortcut?: string;
  disabled?: boolean;
}) {
  return <MessageTooltip delayMs={200} disabled={disabled} side="bottom" variant="shell" label={
    <span className="shell-tooltip-label">
      <span>{label}</span>
      {shortcut ? <kbd>{shortcut}</kbd> : null}
    </span>
  }>{children}</MessageTooltip>;
}
