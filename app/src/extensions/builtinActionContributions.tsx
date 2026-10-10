import { useMemo, useRef, useSyncExternalStore } from "react";
import {
  OcButton,
  OcDialog,
  OcIconButton,
} from "../design-system/components";
import {
  beginClientExtensionIncidentAction,
  clientExtensionIncidentSnapshot,
  completeClientExtensionIncidentAction,
  dismissClientExtensionIncident,
  failClientExtensionIncidentAction,
  subscribeClientExtensionIncidents,
  type ClientExtensionIncidentRecord,
} from "./incidents";
import { runClientExtensionAction } from "./transport";
import type { ClientExtensionActionRegistration } from "./types";

const PROVIDER_CREDENTIAL_REPAIR_ACTION = "provider.credentials.refresh";
const OCTOPUS_DIALOG_OUTLINE_ACTION = "!border-token-border !bg-token-bg-fog !text-token-button-tertiary-foreground enabled:hover:!bg-token-list-hover-background";

export function hasBuiltinClientExtensionAction(actionId: string): boolean {
  return actionId === PROVIDER_CREDENTIAL_REPAIR_ACTION;
}

export function BuiltinClientExtensionActionHost({
  actions,
}: {
  actions: ClientExtensionActionRegistration[];
}) {
  const incidents = useSyncExternalStore(
    subscribeClientExtensionIncidents,
    clientExtensionIncidentSnapshot,
    clientExtensionIncidentSnapshot,
  );
  const match = useMemo(() => {
    for (const incident of incidents) {
      const action = actions.find((candidate) => candidate.triggers.some((trigger) => (
        trigger.incident_code === incident.code
        && trigger.interface_id === incident.interfaceId
      )));
      if (action && hasBuiltinClientExtensionAction(action.actionId)) return { action, incident };
    }
    return null;
  }, [actions, incidents]);
  if (!match) return null;
  return (
    <ProviderCredentialRepairDialog
      action={match.action}
      incident={match.incident}
    />
  );
}

function ProviderCredentialRepairDialog({
  action,
  incident,
}: {
  action: ClientExtensionActionRegistration;
  incident: ClientExtensionIncidentRecord;
}) {
  const actionPendingRef = useRef(false);
  const operation = incident.operation;
  const running = operation.status === "running";
  const succeeded = operation.status === "succeeded";
  const failed = operation.status === "failed";

  function close() {
    if (!running) dismissClientExtensionIncident(incident.id);
  }

  function repair() {
    if (running || actionPendingRef.current) return;
    actionPendingRef.current = true;
    beginClientExtensionIncidentAction(incident.id);
    void runClientExtensionAction({
      extensionId: action.extensionId,
      actionId: action.actionId,
      incident: {
        id: incident.id,
        code: incident.code,
        interfaceId: incident.interfaceId,
      },
    }).then((outcome) => {
      completeClientExtensionIncidentAction(incident.id, outcome);
    }).catch((error) => {
      failClientExtensionIncidentAction(incident.id, error);
    }).finally(() => {
      actionPendingRef.current = false;
    });
  }

  const title = succeeded
    ? "Codex sign-in is ready"
    : failed
      ? "Codex sign-in isn't ready"
      : running
        ? "Checking Codex sign-in"
        : "Check Codex sign-in?";
  const description = succeeded
    ? "Octopus now uses the current Codex sign-in on this Mac. You can send the draft again or resume the stopped task."
    : failed
      ? operation.message
      : running
        ? "Checking the machine-wide Codex sign-in. Your draft and the running Octopus runtime stay unchanged."
        : "Octopus uses the same Codex sign-in as the Codex app and CLI on this Mac. Sign in there first, then check it here. Your draft stays unchanged.";

  return (
    <OcDialog
      ariaLabelledBy="provider-credential-repair-title"
      busy={running}
      onClose={close}
      open
      panelClassName="relative p-5"
      size="compact"
    >
      <OcIconButton
        className="absolute top-4 right-4"
        disabled={running}
        glyph="close"
        label="Close repair dialog"
        onClick={close}
        size="xs"
        variant="ghost"
      />
      <div className="flex flex-col gap-0 text-base leading-normal tracking-normal">
        <div className="flex min-w-0 flex-1 flex-col gap-1 self-stretch pr-7">
          <h2
            className="heading-dialog m-0 min-w-0 font-semibold text-token-foreground"
            id="provider-credential-repair-title"
          >
            {title}
          </h2>
          <p
            aria-live="polite"
            className={`m-0 mt-2 text-base leading-normal tracking-normal ${failed ? "text-token-error-foreground" : "text-token-description-foreground"}`}
          >
            {description}
          </p>
        </div>
        <div className="flex w-full items-center justify-end gap-3 pt-3">
          {succeeded ? (
            <OcButton className="w-full justify-center" onClick={close} variant="primary">Done</OcButton>
          ) : failed ? (
            <>
              <OcButton
                className={`${OCTOPUS_DIALOG_OUTLINE_ACTION}${operation.retryable ? "" : " w-full justify-center"}`}
                onClick={close}
                variant="secondary"
              >
                Close
              </OcButton>
              {operation.retryable ? <OcButton onClick={repair} variant="primary">Try again</OcButton> : null}
            </>
          ) : (
            <>
              <OcButton className={OCTOPUS_DIALOG_OUTLINE_ACTION} disabled={running} onClick={close} variant="secondary">Cancel</OcButton>
              <OcButton disabled={running} onClick={repair} variant="primary">
                {running ? "Checking…" : "Check sign-in"}
              </OcButton>
            </>
          )}
        </div>
      </div>
    </OcDialog>
  );
}
