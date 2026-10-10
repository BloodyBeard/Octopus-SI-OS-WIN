import { settingsTargetProps } from "./settingsCatalog";
import { useEffect, useRef, useState, type ChangeEvent, type RefObject } from "react";
import { OcButton, OcCard, OcGroup, OcSettingsRow, OcStatus, OcTextField } from "../../design-system/components";
import type { OctopusClientAccessDetails, OctopusClientAccessProfile, OctopusTouchIdStatus } from "../../electron";
import { readClientAccessDetails, readTouchIdStatus, setTouchIdEnabled, updateClientAdministratorAccess, updateClientLabAccess } from "../../clientAccess";
import {
  clearClientProfileStorageWithRecovery,
  clientProfileOwnsStorageKey,
  clientProfileStorageEntries,
  replaceClientProfileStorageWithRecovery,
} from "../../clientLocalDataRecovery";
import { useAppErrorToast } from "../../toasts";
import { SettingsHeading } from "./SettingsHeading";
import { SavedPermissionsSection } from "./SavedPermissionsSection";

export function AccessSection({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  const [details, setDetails] = useState<OctopusClientAccessDetails | null>(null);
  const [currentAdministratorPassword, setCurrentAdministratorPassword] = useState("");
  const [administratorPassword, setAdministratorPassword] = useState("");
  const [administratorConfirmation, setAdministratorConfirmation] = useState("");
  const [testPassword, setTestPassword] = useState("");
  const [testConfirmation, setTestConfirmation] = useState("");
  const [administratorState, setAdministratorState] = useState<"idle" | "saving" | "saved">("idle");
  const [testState, setTestState] = useState<"idle" | "saving" | "saved">("idle");
  const [touchId, setTouchId] = useState<OctopusTouchIdStatus | null>(null);
  const [touchIdBusy, setTouchIdBusy] = useState(false);
  const [error, setError] = useState("");
  const [clearLocalState, setClearLocalState] = useState<"idle" | "confirm" | "clearing" | "error">("idle");
  const [exportState, setExportState] = useState<"idle" | "exported" | "error">("idle");
  const [importState, setImportState] = useState<"idle" | "reading" | "confirm" | "importing" | "error">("idle");
  const [pendingImport, setPendingImport] = useState<Array<[string, string]>>([]);
  const importInputRef = useRef<HTMLInputElement>(null);
  useAppErrorToast(error);

  useEffect(() => {
    let cancelled = false;
    readClientAccessDetails()
      .then((value) => {
        if (!cancelled) setDetails(value);
      })
      .catch((failure) => {
        if (!cancelled) {
          setTouchId({ available: false, enrolled: false, prompted: false });
          setError(failure instanceof Error ? failure.message : String(failure));
        }
      });
    return () => { cancelled = true; };
  }, [accessProfile]);

  useEffect(() => {
    if (accessProfile !== "live") return;
    let cancelled = false;
    readTouchIdStatus()
      .then((value) => {
        if (!cancelled) setTouchId(value);
      })
      .catch((failure) => {
        if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure));
      });
    return () => { cancelled = true; };
  }, [accessProfile]);

  const minimumLength = details?.minimumLength ?? 8;
  const administratorManagedByEnvironment = details?.administrator.source === "environment";
  const testManagedByEnvironment = details?.test.source === "environment";

  async function saveAdministratorPassword() {
    setError("");
    if (administratorPassword !== administratorConfirmation) {
      setError("Administrator password confirmation does not match.");
      return;
    }
    setAdministratorState("saving");
    try {
      const next = await updateClientAdministratorAccess({
        currentPassword: currentAdministratorPassword,
        password: administratorPassword,
      });
      setDetails(next);
      setCurrentAdministratorPassword("");
      setAdministratorPassword("");
      setAdministratorConfirmation("");
      setAdministratorState("saved");
      setTouchId((current) => current ? { ...current, enrolled: false } : current);
    } catch (failure) {
      setAdministratorState("idle");
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }

  async function saveTestPassword() {
    setError("");
    if (testPassword !== testConfirmation) {
      setError("Test password confirmation does not match.");
      return;
    }
    setTestState("saving");
    try {
      await updateClientLabAccess(testPassword);
      setDetails(await readClientAccessDetails());
      setTestPassword("");
      setTestConfirmation("");
      setTestState("saved");
    } catch (failure) {
      setTestState("idle");
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }

  async function updateTouchId(enabled: boolean) {
    if (touchIdBusy) return;
    setError("");
    setTouchIdBusy(true);
    try {
      setTouchId(await setTouchIdEnabled(enabled));
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setTouchIdBusy(false);
    }
  }

  async function exportLocalSettings() {
    try {
      const settings = Object.fromEntries(clientProfileStorageEntries(accessProfile));
      const payload = JSON.stringify({
        schema: "octopus.client_settings_export",
        schema_version: 1,
        profile: accessProfile,
        exported_at: new Date().toISOString(),
        settings,
      }, null, 2);
      const result = await window.octopusTransport?.saveClientFile({
        bytes: new TextEncoder().encode(payload),
        filename: `octopus-${accessProfile === "live" ? "administrator" : "test"}-settings.json`,
      });
      if (!result) throw new Error("Native file saving is unavailable.");
      if (!result.saved) return;
      setExportState("exported");
      window.setTimeout(() => setExportState("idle"), 1_500);
    } catch {
      setExportState("error");
    }
  }

  function clearLocalSettings() {
    if (clearLocalState !== "confirm") {
      setClearLocalState("confirm");
      return;
    }
    setClearLocalState("clearing");
    try {
      clearClientProfileStorageWithRecovery(accessProfile);
    } catch {
      setClearLocalState("error");
    }
  }

  async function chooseSettingsImport(event: ChangeEvent<HTMLInputElement>) {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = "";
    if (!file) return;
    setImportState("reading");
    setPendingImport([]);
    setError("");
    try {
      if (file.size <= 0 || file.size > 4 * 1024 * 1024) throw new Error("Settings import must be a JSON file smaller than 4 MB.");
      const entries = parseClientSettingsImport(await file.text(), accessProfile);
      setPendingImport(entries);
      setImportState("confirm");
    } catch (failure) {
      const message = failure instanceof Error ? failure.message : String(failure);
      setImportState("error");
      setError(message);
    }
  }

  function applySettingsImport() {
    if (importState !== "confirm" || pendingImport.length === 0) return;
    setImportState("importing");
    try {
      replaceClientProfileStorageWithRecovery(
        accessProfile,
        pendingImport,
        "merge",
        "Local settings imported",
      );
    } catch (failure) {
      setImportState("error");
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }

  if (accessProfile === "lab") {
    return (
      <section className="settings-section">
        <SettingsHeading
          description="The Test profile is a local fixture boundary with no runtime, registry, ledger, filesystem, or live endpoint authority."
          title="Access & Privacy"
        />
        <OcGroup title="Current profile" description="Only Administrator can change either profile credential.">
          <OcCard>
            <OcSettingsRow {...settingsTargetProps("access.test")}
              control={<OcStatus label="Isolated" tone="success" />}
              description="Log out and unlock with the Administrator password to manage access."
              label="Test"
            />
          </OcCard>
        </OcGroup>
        <OcGroup title="Local data" description="Test preferences and drafts remain isolated from Administrator.">
          <OcCard>
            <LocalDataRows
              clearState={clearLocalState}
              exportState={exportState}
              importCount={pendingImport.length}
              importInputRef={importInputRef}
              importState={importState}
              onApplyImport={applySettingsImport}
              onCancelClear={() => setClearLocalState("idle")}
              onCancelImport={() => { setPendingImport([]); setImportState("idle"); }}
              onChooseImport={chooseSettingsImport}
              onClear={clearLocalSettings}
              onExport={exportLocalSettings}
            />
          </OcCard>
        </OcGroup>
      </section>
    );
  }

  return (
    <section className="settings-section">
      <SettingsHeading
        description="Credentials and privacy controls stored only on this device. Administrator controls the isolated Test profile."
        title="Access & Privacy"
      />

      <SavedPermissionsSection />

      <OcGroup
        title="Administrator"
        description={administratorManagedByEnvironment
          ? "Loaded from the production Home .env file. Change it there and restart the client."
          : "Changing this password revokes every other Administrator session on this device."}
      >
        <OcCard>
          <div {...settingsTargetProps("access.administrator")} className="settings-access-editor">
            <div className="settings-access-fields">
              <OcTextField
                autoComplete="current-password"
                disabled={administratorManagedByEnvironment}
                label="Current password"
                onChange={(event) => {
                  setCurrentAdministratorPassword(event.currentTarget.value);
                  setAdministratorState("idle");
                  setError("");
                }}
                showLabel
                type="password"
                value={currentAdministratorPassword}
              />
              <OcTextField
                autoComplete="new-password"
                disabled={administratorManagedByEnvironment}
                label="New password"
                onChange={(event) => {
                  setAdministratorPassword(event.currentTarget.value);
                  setAdministratorState("idle");
                  setError("");
                }}
                placeholder={`At least ${minimumLength} characters`}
                showLabel
                type="password"
                value={administratorPassword}
              />
              <OcTextField
                autoComplete="new-password"
                disabled={administratorManagedByEnvironment}
                error={administratorConfirmation && administratorConfirmation !== administratorPassword ? "Passwords do not match" : undefined}
                label="Confirm new password"
                onChange={(event) => {
                  setAdministratorConfirmation(event.currentTarget.value);
                  setAdministratorState("idle");
                  setError("");
                }}
                showLabel
                type="password"
                value={administratorConfirmation}
              />
            </div>
            <div className="settings-endpoint-actions">
              {administratorManagedByEnvironment ? <OcStatus label="Managed by .env" tone="neutral" /> : null}
              {details?.administrator.updatedAt ? <OcStatus label="Previously updated" tone="muted" /> : null}
              {administratorState === "saved" ? <OcStatus label="Updated" tone="success" /> : null}
              <OcButton
                disabled={
                  administratorManagedByEnvironment
                  || administratorState === "saving"
                  || !currentAdministratorPassword
                  || administratorPassword.length < minimumLength
                  || administratorPassword !== administratorConfirmation
                }
                onClick={() => void saveAdministratorPassword()}
                variant="primary"
              >
                Update Administrator password
              </OcButton>
            </div>
          </div>
          {accessProfile === "live" ? (
            <OcSettingsRow {...settingsTargetProps("access.touch-id")}
              className="border-t border-token-border"
              control={(
                <div className="settings-inline-actions">
                  <OcStatus
                    label={!touchId ? "Checking…" : touchId.enrolled ? "Enabled" : touchId.available ? "Off" : "Unavailable"}
                    tone={touchId?.enrolled ? "success" : "muted"}
                  />
                  {touchId?.available ? (
                    <OcButton
                      disabled={touchIdBusy}
                      onClick={() => void updateTouchId(!touchId.enrolled)}
                      variant="secondary"
                    >
                      {touchIdBusy ? "Updating…" : touchId.enrolled ? "Turn off" : "Enable"}
                    </OcButton>
                  ) : null}
                </div>
              )}
              description="Use this Mac's Touch ID after the Administrator password has been configured. The password remains available as fallback."
              label="Touch ID"
            />
          ) : null}
        </OcCard>
      </OcGroup>

      <OcGroup
        title="Test"
        description="Test opens fixtures and experimental UI only. It cannot read live runtime, registry, ledger, files, endpoints, or installed stable surface data."
      >
        <OcCard>
          <div {...settingsTargetProps("access.test")} className="settings-access-editor">
            <div className="settings-access-fields is-two-column">
              <OcTextField
                autoComplete="new-password"
                disabled={testManagedByEnvironment}
                label="New Test password"
                onChange={(event) => {
                  setTestPassword(event.currentTarget.value);
                  setTestState("idle");
                  setError("");
                }}
                placeholder={`At least ${minimumLength} characters`}
                showLabel
                type="password"
                value={testPassword}
              />
              <OcTextField
                autoComplete="new-password"
                disabled={testManagedByEnvironment}
                error={testConfirmation && testConfirmation !== testPassword ? "Passwords do not match" : undefined}
                label="Confirm Test password"
                onChange={(event) => {
                  setTestConfirmation(event.currentTarget.value);
                  setTestState("idle");
                  setError("");
                }}
                showLabel
                type="password"
                value={testConfirmation}
              />
            </div>
            <div className="settings-endpoint-actions">
              {details?.test.source ? <OcStatus label={`Source: ${details.test.source}`} tone="muted" /> : null}
              {testState === "saved" ? <OcStatus label="Updated" tone="success" /> : null}
              <OcButton
                disabled={testManagedByEnvironment || testState === "saving" || testPassword.length < minimumLength || testPassword !== testConfirmation}
                onClick={() => void saveTestPassword()}
                variant="primary"
              >
                Update Test password
              </OcButton>
            </div>
          </div>
        </OcCard>
      </OcGroup>

      <OcGroup title="Privacy" description="Desktop permissions and locally stored client data.">
        <OcCard>
          <PermissionRows />
          <OcSettingsRow {...settingsTargetProps("access.recordings")}
            control={<OcStatus label="Artifact governed" tone="neutral" />}
            description="Audio follows admitted artifact files, projected transcripts remain runtime evidence, and local dictation history is profile bounded."
            label="Recordings and transcripts"
          />
          <LocalDataRows
            clearState={clearLocalState}
            exportState={exportState}
            importCount={pendingImport.length}
            importInputRef={importInputRef}
            importState={importState}
            onApplyImport={applySettingsImport}
            onCancelClear={() => setClearLocalState("idle")}
            onCancelImport={() => { setPendingImport([]); setImportState("idle"); }}
            onChooseImport={chooseSettingsImport}
            onClear={clearLocalSettings}
            onExport={exportLocalSettings}
          />
        </OcCard>
      </OcGroup>
    </section>
  );
}

function LocalDataRows({
  clearState,
  exportState,
  importCount,
  importInputRef,
  importState,
  onApplyImport,
  onCancelClear,
  onCancelImport,
  onChooseImport,
  onClear,
  onExport,
}: {
  clearState: "idle" | "confirm" | "clearing" | "error";
  exportState: "idle" | "exported" | "error";
  importCount: number;
  importInputRef: RefObject<HTMLInputElement | null>;
  importState: "idle" | "reading" | "confirm" | "importing" | "error";
  onApplyImport: () => void;
  onCancelClear: () => void;
  onCancelImport: () => void;
  onChooseImport: (event: ChangeEvent<HTMLInputElement>) => void;
  onClear: () => void;
  onExport: () => void;
}) {
  return (
    <>
      <OcSettingsRow {...settingsTargetProps("access.clear")}
        control={(
          <div className="settings-inline-actions">
            {clearState === "confirm" ? <OcStatus label="Confirmation required" tone="warning" /> : null}
            {clearState === "error" ? <OcStatus label="Could not clear" tone="danger" /> : null}
            <OcButton
              disabled={clearState === "clearing"}
              onClick={onClear}
              tone={clearState === "confirm" ? "danger" : "neutral"}
              variant={clearState === "confirm" ? "primary" : "secondary"}
            >
              {clearState === "confirm" ? "Confirm clear" : clearState === "clearing" ? "Clearing…" : "Clear data"}
            </OcButton>
            {clearState === "confirm" ? <OcButton onClick={onCancelClear} variant="ghost">Cancel</OcButton> : null}
          </div>
        )}
        description="Removes local preferences, layouts, drafts, and Test fixtures for this profile. Projected runtime evidence is not changed."
        label="Clear local client data"
      />
      <OcSettingsRow {...settingsTargetProps("access.export")}
        control={(
          <div className="settings-inline-actions">
            {exportState === "exported" ? <OcStatus label="Exported" tone="success" /> : null}
            {exportState === "error" ? <OcStatus label="Export failed" tone="danger" /> : null}
            <OcButton onClick={onExport} variant="secondary">Export</OcButton>
          </div>
        )}
        description="Exports profile-owned local client settings as a versioned JSON file. Credentials and projection evidence are excluded."
        label="Export local settings"
      />
      <OcSettingsRow {...settingsTargetProps("access.import")}
        control={(
          <div className="settings-inline-actions">
            <input
              accept="application/json,.json"
              aria-label="Choose settings export"
              className="sr-only"
              onChange={onChooseImport}
              ref={importInputRef}
              type="file"
            />
            {importState === "confirm" ? <OcStatus label={`${importCount} ready`} tone="warning" /> : null}
            {importState === "error" ? <OcStatus label="Import refused" tone="danger" /> : null}
            {importState === "confirm" ? (
              <>
                <OcButton onClick={onApplyImport}>Apply and reload</OcButton>
                <OcButton onClick={onCancelImport} variant="ghost">Cancel</OcButton>
              </>
            ) : (
              <OcButton
                disabled={importState === "reading" || importState === "importing"}
                onClick={() => importInputRef.current?.click()}
                variant="secondary"
              >
                {importState === "reading" ? "Reading…" : importState === "importing" ? "Importing…" : "Choose file"}
              </OcButton>
            )}
          </div>
        )}
        description="Merges only settings exported for this exact profile, then reloads the client. Unknown namespaces and malformed values are refused."
        label="Import local settings"
      />
    </>
  );
}

function PermissionRows() {
  const [camera, setCamera] = useState<{ granted: boolean; status: string } | null>(null);
  const [microphone, setMicrophone] = useState<{ granted: boolean; status: string } | null>(null);
  const [permissionError, setPermissionError] = useState("");
  useAppErrorToast(permissionError);

  useEffect(() => {
    let cancelled = false;
    window.octopusTransport?.cameraAccessStatus?.()
      .then((status) => {
        if (!cancelled) setCamera(status);
      })
      .catch(() => undefined);
    return () => { cancelled = true; };
  }, []);

  async function requestCamera() {
    setPermissionError("");
    try {
      const status = await window.octopusTransport?.requestCameraAccess?.();
      if (status) setCamera(status);
    } catch (error) {
      setPermissionError(error instanceof Error ? error.message : String(error));
    }
  }

  async function requestMicrophone() {
    setPermissionError("");
    try {
      const status = await window.octopusTransport?.requestMicrophoneAccess?.();
      if (status) setMicrophone(status);
    } catch (error) {
      setPermissionError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <>
      <OcSettingsRow {...settingsTargetProps("access.camera")}
        control={(
          <div className="settings-inline-actions">
            <OcStatus
              label={camera ? (camera.granted ? "Allowed" : camera.status) : "Not requested"}
              tone={camera?.granted ? "success" : "muted"}
            />
            {!camera?.granted ? <OcButton onClick={() => void requestCamera()}>Request</OcButton> : null}
          </div>
        )}
        description="Used by gesture control and camera-based surfaces."
        label="Camera"
      />
      <OcSettingsRow {...settingsTargetProps("access.microphone")}
        control={(
          <div className="settings-inline-actions">
            <OcStatus
              label={microphone ? (microphone.granted ? "Allowed" : microphone.status) : "Not requested"}
              tone={microphone?.granted ? "success" : "muted"}
            />
            {!microphone?.granted ? <OcButton onClick={() => void requestMicrophone()}>Request</OcButton> : null}
          </div>
        )}
        description="Used by dictation and Voice."
        label="Microphone"
      />
    </>
  );
}

function parseClientSettingsImport(source: string, accessProfile: OctopusClientAccessProfile): Array<[string, string]> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(source);
  } catch {
    throw new Error("Settings import is not valid JSON.");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("Settings import must be a JSON object.");
  const document = parsed as Record<string, unknown>;
  if (document.schema !== "octopus.client_settings_export" || document.schema_version !== 1) {
    throw new Error("Settings import does not satisfy octopus.client_settings_export@1.");
  }
  if (document.profile !== accessProfile) {
    throw new Error(`This export belongs to the ${document.profile === "lab" ? "Test" : "Administrator"} profile.`);
  }
  const settings = document.settings;
  if (!settings || typeof settings !== "object" || Array.isArray(settings)) throw new Error("Settings import has no settings object.");
  const entries = Object.entries(settings);
  if (entries.length === 0 || entries.length > 500) throw new Error("Settings import must contain between 1 and 500 entries.");
  return entries.map(([key, value]) => {
    if (key.length > 256 || !clientProfileOwnsStorageKey(key, accessProfile)) {
      throw new Error(`Settings key is outside the ${accessProfile === "lab" ? "Test" : "Administrator"} profile: ${key.slice(0, 80)}`);
    }
    if (typeof value !== "string" || value.length > 1_000_000) throw new Error(`Settings value is invalid for ${key}.`);
    return [key, value];
  });
}
