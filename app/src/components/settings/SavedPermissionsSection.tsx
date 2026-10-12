import { useEffect, useState } from "react";
import { OcButton, OcCard, OcDialog, OcDialogFooter, OcDialogHeader, OcGroup, OcSettingsRow } from "../../design-system/components";
import type { OctopusSavedApplicationPermissions, OctopusSavedBrowserPermissions } from "../../electron";
import { useProjectionGeneration } from "../../projectionLive";
import { settingsTargetProps } from "./settingsCatalog";

export function SavedPermissionsSection() {
  const generation = useProjectionGeneration();
  const [revision, setRevision] = useState(0);
  const [browser, setBrowser] = useState<OctopusSavedBrowserPermissions | null>(null);
  const [browserError, setBrowserError] = useState("");
  const [browserLoading, setBrowserLoading] = useState(true);
  const [browserBusy, setBrowserBusy] = useState(false);
  useEffect(() => {
    let cancelled = false;
    setBrowserLoading(true); setBrowserError("");
    const read = async () => {
      if (!window.octopusTransport?.browserSavedPermissions) throw new Error("The native browser Settings connection is unavailable.");
      return window.octopusTransport.browserSavedPermissions({ operation: "list" });
    };
    void read().then(value => { if (!cancelled) setBrowser(value); })
      .catch(error => { if (!cancelled) { setBrowser(null); setBrowserError(String(error.message ?? error)); } })
      .finally(() => { if (!cancelled) setBrowserLoading(false); });
    return () => { cancelled = true; };
  }, [generation, revision]);

  async function removeBrowser(id: string) {
    if (!browser || browserBusy) return;
    setBrowserBusy(true); setBrowserError("");
    try {
      if (!window.octopusTransport?.browserSavedPermissions) throw new Error("The native browser Settings connection is unavailable.");
      setBrowser(await window.octopusTransport.browserSavedPermissions({ operation: "remove", home: browser.home, hostGeneration: browser.hostGeneration, id }));
    } catch (error) { setBrowserError(error instanceof Error ? error.message : String(error)); }
    finally { setBrowserBusy(false); }
  }

  return <>
    <OcGroup title="Saved browser permissions" description="Remove an Always choice to ask again on a future request. Choices for an existing task remain separate.">
      <OcCard {...settingsTargetProps("access.browser-permissions")}>
        {browserLoading ? <OcSettingsRow label="Loading saved browser permissions…" /> : browserError ? <OcSettingsRow label="Browser permissions unavailable" description={browserError} />
          : browser?.entries.length === 0 ? <OcSettingsRow label="No saved browser permissions" /> : browser?.entries.map(entry => <OcSettingsRow key={entry.id}
            label={entry.subject.type === "upload" ? entry.subject.origin : entry.subject.source.id}
            description={`${entry.subject.type === "upload" ? "File uploads" : "Browsing history"} · ${entry.decision === "accept" ? "Always allowed" : "Always declined"}`}
            control={<OcButton disabled={browserBusy} onClick={() => void removeBrowser(entry.id)} variant="ghost">Remove</OcButton>} />)}
        <OcSettingsRow label="Refresh browser permissions" control={<OcButton disabled={browserBusy || browserLoading} onClick={() => setRevision(value => value + 1)}>Refresh</OcButton>} />
      </OcCard>
    </OcGroup>
  </>;
}

export function SavedApplicationPermissionsSection() {
  const generation = useProjectionGeneration();
  const [revision, setRevision] = useState(0);
  const [applications, setApplications] = useState<OctopusSavedApplicationPermissions | null>(null);
  const [applicationError, setApplicationError] = useState("");
  const [applicationLoading, setApplicationLoading] = useState(true);
  const [applicationBusy, setApplicationBusy] = useState(false);
  const [removing, setRemoving] = useState<{ bundle_id: string; display_name: string } | null>(null);
  useEffect(() => {
    let cancelled = false;
    const read = async () => {
      setApplicationLoading(true);
      try {
        if (!window.octopusTransport?.desktopSavedApplicationPermissions) throw new Error("The Computer Use Settings connection is unavailable.");
        const result = await window.octopusTransport.desktopSavedApplicationPermissions({ operation: "list" });
        if (!cancelled) { setApplications(result); setApplicationError(""); }
      } catch (error) { if (!cancelled) { setApplications(null); setApplicationError(error instanceof Error ? error.message : String(error)); } }
      finally { if (!cancelled) setApplicationLoading(false); }
    };
    const refresh = () => { void read(); };
    void read(); window.addEventListener("focus", refresh);
    return () => { cancelled = true; window.removeEventListener("focus", refresh); };
  }, [generation, revision]);
  async function removeApplication() {
    if (!applications || !removing || applicationBusy) return;
    setApplicationBusy(true); setApplicationError("");
    try {
      if (!window.octopusTransport?.desktopSavedApplicationPermissions) throw new Error("The Computer Use Settings connection is unavailable.");
      setApplications(await window.octopusTransport.desktopSavedApplicationPermissions({ sourceId: applications.sourceId, operation: "remove", bundleId: removing.bundle_id }));
      setRemoving(null); setRevision(value => value + 1);
    } catch (error) { setApplicationError(error instanceof Error ? error.message : String(error)); }
    finally { setApplicationBusy(false); }
  }
  return <>
    <OcGroup title="Always-allowed apps">
      <OcCard {...settingsTargetProps("access.application-permissions")}>
        {applicationLoading ? <OcSettingsRow label="Loading apps…" />
          : applicationError ? <OcSettingsRow label="Application permissions unavailable" description={<details><summary>Error details</summary>{applicationError}</details>} />
          : applications?.applications.length === 0 ? <OcSettingsRow label="None yet" />
          : applications?.applications.map(item => <OcSettingsRow key={item.bundle_id} label={item.display_name}
            control={<OcButton disabled={applicationBusy} onClick={() => setRemoving(item)} variant="ghost">Remove</OcButton>} />)}
        {applicationError ? <OcSettingsRow label="Check saved apps" control={<OcButton disabled={applicationBusy || applicationLoading} onClick={() => setRevision(value => value + 1)}>Refresh</OcButton>} /> : null}
      </OcCard>
    </OcGroup>
    <OcDialog open={removing !== null} onClose={() => { if (!applicationBusy) setRemoving(null); }} size="compact" busy={applicationBusy}>
      <OcDialogHeader title={`Remove “${removing?.display_name ?? ""}” from always-allowed apps?`}
        description={`Octopus will ask to use “${removing?.display_name ?? ""}” in the next computer use task.`} />
      <OcDialogFooter>
        <OcButton disabled={applicationBusy} onClick={() => setRemoving(null)} variant="ghost">Cancel</OcButton>
        <OcButton disabled={applicationBusy} onClick={() => void removeApplication()} tone="danger" variant="primary">Remove</OcButton>
      </OcDialogFooter>
    </OcDialog>
  </>;
}
