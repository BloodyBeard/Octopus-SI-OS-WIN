import { useSyncExternalStore } from "react";
import { OcButton } from "../../design-system/components";
import { SetupDialogShell } from "../SetupDialogShell";
import { filesPaneGeneration, pendingFilesPaneClose, resolveFilesPaneClose, subscribeFilesPaneDialogs } from "./filesPaneLifecycle";

export function FilesPaneDialogs() {
  useSyncExternalStore(subscribeFilesPaneDialogs, filesPaneGeneration);
  const pending = pendingFilesPaneClose();
  if (!pending) return null;
  const { key, state } = pending;
  const resolve = (decision: "save" | "discard" | "cancel") => {
    void resolveFilesPaneClose(key, decision, window.octopusTransport);
  };
  return <SetupDialogShell ariaLabel="Save changes before closing" busy={state.saving} onClose={() => resolve("cancel")} open size="compact">
    <div className="px-6 pt-8 pb-6">
      <h2 className="heading-lg text-token-foreground">Save changes to {state.snapshot.file?.name}?</h2>
      <p className="mt-2 text-sm leading-5 text-token-text-secondary">Save this file, discard its unsaved changes, or cancel closing it.</p>
      {state.error ? <p className="mt-3 text-sm text-token-text-danger" role="alert">{state.error}</p> : null}
    </div>
    <div className="flex min-h-[76px] items-center justify-end gap-2 border-t border-token-border/40 px-6 pb-6">
      <OcButton disabled={state.saving} onClick={() => resolve("cancel")} variant="ghost">Cancel</OcButton>
      <OcButton disabled={state.saving} onClick={() => resolve("discard")} tone="danger" variant="secondary">Discard</OcButton>
      <OcButton disabled={state.saving} onClick={() => resolve("save")} variant="primary">{state.saving ? "Saving…" : "Save"}</OcButton>
    </div>
  </SetupDialogShell>;
}
