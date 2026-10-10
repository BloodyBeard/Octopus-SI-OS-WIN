import type { ReactNode } from "react";
import { OcDialog, type OcDialogSize } from "../design-system/components";

export function SetupDialogShell({
  ariaLabel,
  busy,
  children,
  onClose,
  open,
  panelClassName,
  size,
}: {
  ariaLabel: string;
  busy: boolean;
  children: ReactNode;
  onClose: () => void;
  open: boolean;
  panelClassName?: string;
  size?: OcDialogSize;
}) {
  return (
    <OcDialog
      ariaLabel={ariaLabel}
      busy={busy}
      onClose={onClose}
      open={open}
      panelClassName={panelClassName}
      size={size}
    >
      {children}
    </OcDialog>
  );
}
