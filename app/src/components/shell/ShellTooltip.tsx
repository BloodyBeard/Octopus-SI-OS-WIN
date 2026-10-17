import type { ReactNode } from "react";
import { MessageTooltip } from "../chat/MessageTooltip";
import "./shellTooltip.css";

export function ShellTooltip({ children, asChild, className, label, shortcut, disabled, side = "bottom" }: {
  children: ReactNode;
  asChild?: boolean;
  className?: string;
  label: string;
  shortcut?: string;
  disabled?: boolean;
  side?: "top" | "right" | "bottom";
}) {
  return <MessageTooltip asChild={asChild} className={className} delayMs={200} disabled={disabled || !label} side={side} variant="shell" label={
    label ? <span className="shell-tooltip-label">
      <span>{label}</span>
      {shortcut ? <kbd>{shortcut}</kbd> : null}
    </span> : null
  }>{children}</MessageTooltip>;
}
