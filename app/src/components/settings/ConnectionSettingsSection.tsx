import { useEffect, useState } from "react";
import { OcButton, OcCard, OcEmptyState, OcGroup, OcStatus, OcTextField } from "../../design-system/components";
import type { OctopusAppInfo, OctopusTransportDefaults } from "../../electron";
import { loadRuntimePrefs, saveRuntimePrefs, validateRuntimeEndpoint } from "../../runtimeClient";
import { useAppErrorToast } from "../../toasts";
import { SettingsHeading } from "./SettingsHeading";

import { FactRow, ProviderCard, useDeclaredProviders } from "./SettingsProviderState";
import { settingsTargetProps } from "./settingsCatalog";
export function ConnectionsSection() {
  const providerState = useDeclaredProviders();
  const [info, setInfo] = useState<OctopusAppInfo | null>(null);
  const [defaults, setDefaults] = useState<OctopusTransportDefaults | null>(null);
  const [endpoint, setEndpoint] = useState(() => loadRuntimePrefs().baseUrl);
  const [savedEndpoint, setSavedEndpoint] = useState(() => loadRuntimePrefs().baseUrl);
  const [saveState, setSaveState] = useState<"idle" | "checking" | "saved" | "error">("idle");
  const [endpointError, setEndpointError] = useState("");
  useAppErrorToast(endpointError);
  useAppErrorToast(providerState.error);

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

  async function persistEndpoint() {
    setSaveState("checking");
    setEndpointError("");
    try {
      await validateRuntimeEndpoint(endpoint);
      saveRuntimePrefs({ baseUrl: endpoint });
      const stored = loadRuntimePrefs().baseUrl;
      setEndpoint(stored);
      setSavedEndpoint(stored);
      setSaveState("saved");
    } catch (error) {
      setSaveState("error");
      setEndpointError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <section className="settings-section">
      <SettingsHeading
        description="Connection preferences and facts projected from the desktop and registry."
        title="Connections"
      />

      <OcGroup {...settingsTargetProps("connections.endpoint")} title="Connection" description="The endpoint used by chat artifact submission from this client.">
        <OcCard>
          <div className="settings-endpoint-editor">
            <OcTextField
              error={endpointError || undefined}
              label="Runtime endpoint"
              onChange={(event) => {
                setEndpoint(event.currentTarget.value);
                setSaveState("idle");
                setEndpointError("");
              }}
              placeholder="http://127.0.0.1:9800"
              showLabel
              value={endpoint}
            />
            <div className="settings-endpoint-actions">
              {saveState === "saved" ? <OcStatus label="Saved" tone="success" /> : null}
              {saveState === "checking" ? <OcStatus label="Validating" tone="muted" /> : null}
              <OcButton
                disabled={saveState === "checking" || endpoint.trim() === savedEndpoint}
                onClick={() => void persistEndpoint()}
                variant="primary"
              >
                Save endpoint
              </OcButton>
            </div>
          </div>
        </OcCard>
      </OcGroup>

      <OcGroup {...settingsTargetProps("connections.facts")} title="Application facts" description="Read-only facts reported by the current desktop process.">
        <OcCard>
          <FactRow label="Application" value={info?.name ?? "Not reported in this environment"} />
          <FactRow label="Version" value={info ? `${info.version}${info.isPackaged ? "" : " · development"}` : "Not reported"} />
          <FactRow label="Platform" value={info?.platform ?? "Not reported"} />
          <FactRow label="Octopus home" mono value={defaults?.octopusHome || "Not resolved"} />
          <FactRow label="Active registry" mono value={defaults?.registryRoot || providerState.registryRoot || "Not resolved"} />
          <FactRow label="Runtime state" mono value={defaults?.stateRoot || "Not resolved"} />
          <FactRow label="Movement ledger" mono value={defaults?.ledgerPath || "Not resolved"} />
          <FactRow label="Source checkout" mono value={defaults?.repoRoot || providerState.repoRoot || "Not resolved"} />
          <FactRow label="Desktop default endpoint" mono value={defaults?.endpoint || "Not reported"} />
        </OcCard>
      </OcGroup>

      <OcGroup
        {...settingsTargetProps("connections.providers")}
        title="Declared provider interfaces"
        description="Registry descriptors exposed to this client. These values are not editable overrides."
      >
        <div className="settings-provider-grid">
          {providerState.providers.map((provider) => <ProviderCard key={provider.id} provider={provider} />)}
          {providerState.loading ? <OcEmptyState className="min-h-0 py-6" size="compact" title="Reading provider descriptors…" /> : null}
          {!providerState.loading && providerState.providers.length === 0 ? (
            <OcEmptyState
              className="min-h-0 py-6"
              detail={providerState.error || undefined}
              glyph={providerState.error ? "warning" : undefined}
              size="compact"
              title={providerState.error ? "Provider interfaces unavailable" : "No provider interfaces are declared."}
              tone={providerState.error ? "danger" : "neutral"}
            />
          ) : null}
        </div>
      </OcGroup>
    </section>
  );
}
