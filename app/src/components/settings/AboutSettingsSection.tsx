import { settingsTargetProps } from "./settingsCatalog";
import { useEffect, useState } from "react";
import { OcButton, OcCard, OcGroup, OcSettingsRow, OcStatus } from "../../design-system/components";
import type { OctopusAppInfo, OctopusClientDoctorReport, OctopusTransportDefaults } from "../../electron";
import { useProjectionGeneration } from "../../projectionLive";
import { useAppErrorToast } from "../../toasts";
import { resetOnboardingState } from "../../onboarding/onboardingStorage";
import { SettingsHeading } from "./SettingsHeading";
import { OpenSourceLicensesDialog } from "./OpenSourceLicensesDialog";

import { FactRow, useDeclaredProviders } from "./SettingsProviderState";
export function AboutSection() {
  const providerState = useDeclaredProviders();
  const generation = useProjectionGeneration();
  const [info, setInfo] = useState<OctopusAppInfo | null>(null);
  const [defaults, setDefaults] = useState<OctopusTransportDefaults | null>(null);
  const [reportError, setReportError] = useState("");
  const [licensesOpen, setLicensesOpen] = useState(false);
  const [logsState, setLogsState] = useState<"idle" | "opening" | "opened" | "error">("idle");
  const [doctorReport, setDoctorReport] = useState<OctopusClientDoctorReport | null>(null);
  const [doctorState, setDoctorState] = useState<"idle" | "running" | "complete" | "error">("idle");
  const [welcomeState, setWelcomeState] = useState<"idle" | "ready" | "error">("idle");
  useAppErrorToast(welcomeState === "error" ? "Could not reset the welcome screen." : "");
  useAppErrorToast(logsState === "error" ? "Could not open the client logs directory." : "");
  useAppErrorToast(doctorState === "error" ? "Doctor could not inspect the current Octopus Home." : "");
  useAppErrorToast(reportError);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      window.octopusTransport?.appInfo?.() ?? Promise.resolve(null),
      window.octopusTransport?.defaults?.() ?? Promise.resolve(null),
    ])
      .then(([nextInfo, nextDefaults]) => {
        if (cancelled) return;
        setInfo(nextInfo);
        setDefaults(nextDefaults);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  async function openReport() {
    setReportError("");
    try {
      if (!window.octopusTransport?.openProblemReport) throw new Error("Problem reports are available in the desktop app.");
      await window.octopusTransport.openProblemReport();
    } catch (error) { setReportError(error instanceof Error ? error.message : String(error)); }
  }

  async function runDoctor() {
    if (!window.octopusTransport?.runClientDoctor) {
      setDoctorState("error");
      return;
    }
    setDoctorState("running");
    try {
      setDoctorReport(await window.octopusTransport.runClientDoctor());
      setDoctorState("complete");
    } catch {
      setDoctorState("error");
    }
  }

  async function openLogs() {
    if (!window.octopusTransport?.openClientLogs) {
      setLogsState("error");
      return;
    }
    setLogsState("opening");
    try {
      await window.octopusTransport.openClientLogs();
      setLogsState("opened");
    } catch {
      setLogsState("error");
    }
  }

  function resetWelcome() {
    try {
      resetOnboardingState();
      setWelcomeState("ready");
    } catch {
      setWelcomeState("error");
    }
  }

  return (
    <section className="settings-section">
      <SettingsHeading
        description="Application facts, diagnostics, and local client state."
        title="About & Diagnostics"
      />

      <OcGroup title="Application" description="Read-only facts reported by the current desktop process.">
        <OcCard>
          <FactRow label="Application" value={info?.name ?? "Not reported in this environment"} />
          <FactRow label="Version" value={info ? `${info.version}${info.isPackaged ? "" : " · development"}` : "Not reported"} />
          <FactRow label="Platform" value={info?.platform ?? "Not reported"} />
          <OcSettingsRow {...settingsTargetProps("about.update-channel")}
            control={<OcStatus label={info?.isPackaged ? "Packaged build" : "Development build"} tone="neutral" />}
            description="No updater or release-channel contract is installed, so the client reports its build mode instead of exposing a false selector."
            label="Update channel"
          />
          <OcSettingsRow {...settingsTargetProps("about.licenses")}
            control={<OcButton onClick={() => setLicensesOpen(true)} size="sm" variant="secondary">View</OcButton>}
            description="Production dependency licenses and notices generated from the packaged renderer graph."
            label="Open-source licenses"
          />
        </OcCard>
      </OcGroup>

      <OcGroup title="Diagnostics" description="Snapshot facts for support and debugging.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("about.diagnostics")}
            control={<OcButton onClick={() => void openReport()}>Report a problem…</OcButton>}
            description="Review, save or copy diagnostics, then open a GitHub issue draft."
            label="Diagnostics report"
          />
          <OcSettingsRow
            control={(
              <div className="settings-inline-actions">
                {doctorState === "complete" ? <OcStatus label={doctorReport?.findings.length ? `${doctorReport.findings.length} finding${doctorReport.findings.length === 1 ? "" : "s"}` : "Passed"} tone={doctorReport?.findings.length ? "warning" : "success"} /> : null}
                <OcButton disabled={doctorState === "running"} onClick={() => void runDoctor()}>
                  {doctorState === "running" ? "Inspecting…" : "Run doctor"}
                </OcButton>
              </div>
            )}
            description="Reads run, ledger, package ownership, receipts, and bound context digests without changing Octopus Home."
            label="Doctor"
          />
          {doctorReport?.findings.map((finding) => (
            <div className="mx-4 my-2 rounded-xl border border-token-border-default bg-token-main-surface-secondary px-4 py-3 text-sm" key={`${finding.code}:${finding.subject}`}>
              <div className="font-medium text-token-foreground">{finding.code} · {finding.subject}</div>
              <div className="mt-1 text-token-text-secondary">{finding.reason}</div>
              <div className="mt-2 text-token-text-secondary"><span className="font-medium text-token-foreground">Next manual step:</span> {finding.next_step}</div>
            </div>
          ))}
          <OcSettingsRow {...settingsTargetProps("about.logs")}
            control={(
              <div className="settings-inline-actions">
                {logsState === "opened" ? <OcStatus label="Opened" tone="success" /> : null}
                <OcButton disabled={logsState === "opening"} onClick={() => void openLogs()}>
                  {logsState === "opening" ? "Opening…" : "Open logs"}
                </OcButton>
              </div>
            )}
            description="Opens the local rotating JSONL log written by the desktop client."
            label="Logs"
          />
          <FactRow label="Projection generation" value={String(generation)} />
        </OcCard>
      </OcGroup>

      <OcGroup title="Runtime facts" description="Read-only facts projected from the desktop and registry.">
        <OcCard>
          <FactRow label="Octopus home" mono value={defaults?.octopusHome || "Not resolved"} />
          <FactRow label="Active registry" mono value={defaults?.registryRoot || providerState.registryRoot || "Not resolved"} />
          <FactRow label="Runtime state" mono value={defaults?.stateRoot || "Not resolved"} />
          <FactRow label="Movement ledger" mono value={defaults?.ledgerPath || "Not resolved"} />
          <FactRow label="Source checkout" mono value={defaults?.repoRoot || providerState.repoRoot || "Not resolved"} />
          <FactRow label="Desktop default endpoint" mono value={defaults?.endpoint || "Not reported"} />
        </OcCard>
      </OcGroup>

      <OcGroup title="Welcome" description="The welcome flow is client state, not runtime state.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("about.welcome")}
            control={(
              <div className="settings-inline-actions">
                {welcomeState === "ready" ? <OcStatus label="Ready for next launch" tone="success" /> : null}
                {welcomeState === "error" ? <OcStatus label="Could not reset" tone="danger" /> : null}
                <OcButton onClick={resetWelcome} variant="secondary">Show on next launch</OcButton>
              </div>
            )}
            description="Clears the local completion marker. The welcome window will appear the next time the app launches."
            label="Welcome window"
          />
        </OcCard>
      </OcGroup>
      <OpenSourceLicensesDialog onClose={() => setLicensesOpen(false)} open={licensesOpen} />
    </section>
  );
}
