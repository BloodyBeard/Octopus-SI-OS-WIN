import { settingsTargetProps } from "./settingsCatalog";
import { useState } from "react";
import { OcButton, OcCard, OcDialog, OcDialogFooter, OcDialogHeader, OcGroup, OcSettingsRow, OcStatus } from "../../design-system/components";
import { SettingsHeading } from "./SettingsHeading";
import type { ClientSurfaceAdmissionVerdict } from "../../client-surfaces/types";
import type { OctopusClientAccessProfile } from "../../electron";
import { replaceClientProfileStorageWithRecovery } from "../../clientLocalDataRecovery";

import { FactRow } from "./SettingsProviderState";
export function TestLabSection({
  accessProfile,
  documentRoot,
  documentVerdicts,
  surfaceErrors,
}: {
  accessProfile: OctopusClientAccessProfile;
  documentRoot: string;
  documentVerdicts: ClientSurfaceAdmissionVerdict[];
  surfaceErrors: string[];
}) {
  const [resetState, setResetState] = useState<"idle" | "resetting" | "error">("idle");
  const [confirmReset, setConfirmReset] = useState(false);
  const admitted = documentVerdicts.filter((verdict) => verdict.passed);
  const refused = documentVerdicts.filter((verdict) => !verdict.passed);

  function resetTestState() {
    setResetState("resetting");
    setConfirmReset(false);
    try {
      replaceClientProfileStorageWithRecovery(
        accessProfile,
        [],
        "replace",
        "Test state reset",
      );
    } catch {
      setResetState("error");
    }
  }

  return (
    <>
    <section className="settings-section">
      <SettingsHeading
        description="Fixtures and experimental surfaces for the isolated Test profile."
        title="Test Lab"
      />

      <OcGroup title="Fixtures" description="Deterministic local data used to exercise surfaces without a runtime.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("test.fixtures")}
            control={<OcStatus label="Enabled" tone="success" />}
            description="Test reads its isolated projection store instead of Administrator runtime evidence."
            label="Projection fixtures"
          />
          <OcSettingsRow {...settingsTargetProps("test.widgets")}
            control={<OcStatus label="Enabled" tone="success" />}
            description="Populate voice with admitted Test-only client surface documents."
            label="Built-in widget documents"
          />
          <OcSettingsRow {...settingsTargetProps("test.voice")}
            control={<OcStatus label="Enabled" tone="success" />}
            description="Drive the voice sphere with synthetic speaking and thinking levels."
            label="Synthetic voice activity"
          />
        </OcCard>
      </OcGroup>

      <OcGroup title="Experiments" description="Surfaces and simulators that are not part of the stable client.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("test.channel")}
            control={<OcStatus label="Test only" tone="warning" />}
            description="Admit documents marked for the lab channel while stable admission rules remain enforced."
            label="Lab release channel"
          />
          <OcSettingsRow {...settingsTargetProps("test.worker-environments")}
            control={<OcStatus label="Enabled" tone="success" />}
            description="Expose deterministic browser, terminal, worktree, container, and desktop preview records."
            label="Worker environment fixtures"
          />
          <OcSettingsRow {...settingsTargetProps("test.reset")}
            control={(
              <div className="settings-inline-actions">
                {resetState === "error" ? <OcStatus label="Could not reset" tone="danger" /> : null}
                <OcButton disabled={resetState === "resetting"} onClick={() => setConfirmReset(true)} variant="secondary">
                  {resetState === "resetting" ? "Resetting…" : "Reset"}
                </OcButton>
              </div>
            )}
            description="Clears Test-only layouts, preferences, scheduled fixtures, and gesture mappings. Administrator state is untouched."
            label="Reset test state"
          />
        </OcCard>
      </OcGroup>

      <OcGroup title="Surface admission" description="Validation evidence for client surface documents loaded by this process.">
        <OcCard>
          <FactRow label="Document root" mono value={documentRoot || "Not reported"} />
          <FactRow label="Admitted" value={String(admitted.length)} />
          <FactRow label="Refused" value={String(refused.length)} />
          <FactRow label="Transport errors" value={String(surfaceErrors.length)} />
          {documentVerdicts.slice(0, 8).map((verdict) => (
            <OcSettingsRow
              control={<OcStatus label={verdict.passed ? "Admitted" : "Refused"} tone={verdict.passed ? "success" : "danger"} />}
              description={verdict.passed
                ? `${verdict.details.node_count} nodes · depth ${verdict.details.max_depth} · ${verdict.details.source_digest.slice(0, 12)}`
                : verdict.details.issues[0]?.message || "The document did not pass admission."}
              key={`${verdict.details.source_digest}:${verdict.details.document_id || "unknown"}`}
              label={verdict.details.document_id || "Unreadable document"}
            />
          ))}
          {surfaceErrors.slice(0, 3).map((error) => (
            <OcSettingsRow
              control={<OcStatus label="Transport error" tone="danger" />}
              description={error}
              key={error}
              label="Client surface transport"
            />
          ))}
        </OcCard>
      </OcGroup>
    </section>
    <OcDialog
      ariaLabelledBy="test-lab-reset-title"
      busy={resetState === "resetting"}
      onClose={() => setConfirmReset(false)}
      open={confirmReset}
      panelClassName="max-w-md"
    >
      <OcDialogHeader
        closeDisabled={resetState === "resetting"}
        description="This clears all Test-profile preferences, layouts, drafts, fixtures, and gesture mappings. Administrator data and runtime evidence are not changed."
        onClose={() => setConfirmReset(false)}
        title="Reset all Test data?"
        titleId="test-lab-reset-title"
      />
      <OcDialogFooter>
        <OcButton disabled={resetState === "resetting"} onClick={() => setConfirmReset(false)} variant="ghost">Cancel</OcButton>
        <OcButton disabled={resetState === "resetting"} onClick={resetTestState} tone="danger" variant="primary">
          {resetState === "resetting" ? "Resetting…" : "Reset Test data"}
        </OcButton>
      </OcDialogFooter>
    </OcDialog>
    </>
  );
}
