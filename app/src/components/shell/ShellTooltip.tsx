import type { ReactNode } from "react";
import { MessageTooltip } from "../chat/MessageTooltip";
import "./shellTooltip.css";

export function ShellTooltip({ children, asChild, className, label, shortcut, disabled }: {
  children: ReactNode;
  asChild?: boolean;
  className?: string;
  label: string;
  shortcut?: string;
  disabled?: boolean;
}) {
  return <MessageTooltip asChild={asChild} className={className} delayMs={200} disabled={disabled || !label} side="bottom" variant="shell" label={
    label ? <span className="shell-tooltip-label">
      <span>{label}</span>
      {shortcut ? <kbd>{shortcut}</kbd> : null}
    </span> : null
  }>{children}</MessageTooltip>;
}
