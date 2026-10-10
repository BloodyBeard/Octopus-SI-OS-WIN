import { useRef } from "react";
import type { OctopusWorkerEnvironmentOperatorAction } from "../../electron";
import { OcButton, OcDialog, OcDialogFooter, OcDialogHeader } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";

export type WorktreeOperatorConfirmation = {
  action: Extract<OctopusWorkerEnvironmentOperatorAction, "apply" | "revert">;
  paths: string[];
};

type ConfirmProps = {
  confirmation: WorktreeOperatorConfirmation;
  pending: boolean;
  onCancel: () => void;
  onConfirm: () => void;
};

export function WorktreeOperatorConfirm({ confirmation, pending, onCancel, onConfirm }: ConfirmProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const applying = confirmation.action === "apply";
  return (
    <OcDialog
      ariaLabelledBy="we-operator-confirm-title"
      backdropClassName="we-operator-confirm-backdrop"
      busy={pending}
      initialFocusRef={cancelRef}
      onClose={onCancel}
      open
      panelClassName="we-operator-confirm"
    >
        <OcDialogHeader
          description={applying ? "This writes the selected projected content into the worktree." : "This removes the selected projected changes from the worktree."}
          leading={<span className={`we-operator-confirm-icon${applying ? "" : " is-danger"}`}><Glyph name={applying ? "changes" : "warning"} size={17} /></span>}
          title={applying ? "Apply projected changes?" : "Revert changes?"}
          titleId="we-operator-confirm-title"
        />
        <div className="we-operator-confirm-body">
          <p><Glyph name="warning" size={13} />This is a manual operator action. It is not an Octopus artifact, movement, mark, or model tool call.</p>
          <div className="we-operator-confirm-paths">
            {confirmation.paths.slice(0, 8).map((filePath) => <span key={filePath}>{filePath}</span>)}
            {confirmation.paths.length > 8 ? <em>+{confirmation.paths.length - 8} more files</em> : null}
          </div>
        </div>
        <OcDialogFooter>
          <OcButton buttonRef={cancelRef} disabled={pending} onClick={onCancel} variant="ghost">Cancel</OcButton>
          <OcButton disabled={pending} onClick={onConfirm} tone={applying ? "neutral" : "danger"} variant="primary">
            {pending ? "Working…" : applying ? "Apply" : "Confirm"}
          </OcButton>
        </OcDialogFooter>
    </OcDialog>
  );
}
