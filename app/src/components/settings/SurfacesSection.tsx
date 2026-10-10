import { useMemo, useState } from "react";
import {
  useClientPreferences,
  useSetClientPreferences,
} from "../../clientPreferences";
import type { AdmittedClientSurfaceDocument, ClientSurfaceDocument } from "../../client-surfaces/types";
import { OcButton, OcCard, OcGroup, OcSettingsRow, OcStatus, OcSwitch } from "../../design-system/components";
import type { OctopusClientAccessProfile } from "../../electron";
import { copyText } from "../../runtimeFormat";
import { clientNavigationDestinations } from "../../shell/navigation";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import { Glyph } from "../IconGlyphs";
import { SettingsHeading } from "./SettingsHeading";

type CopyState = "idle" | "root" | "document" | "template" | "error";

export function SurfacesSection({
  accessProfile,
  documentRoot,
  documents,
  surfaceErrors,
  surfaces,
}: {
  accessProfile: OctopusClientAccessProfile;
  documentRoot: string;
  documents: AdmittedClientSurfaceDocument[];
  surfaceErrors: string[];
  surfaces: ClientSurfaceDescriptor[];
}) {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const [selectedDigest, setSelectedDigest] = useState("");
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const navigationSurfaces = surfaces;
  const primaryDestinations = new Set(clientNavigationDestinations.map((destination) => destination.surfaceId));
  const hidden = new Set(preferences.hiddenSurfaceIds);
  const navigationSurfaceIds = new Set(navigationSurfaces.map((surface) => surface.id));
  const hiddenNavigationSurfaceIds = preferences.hiddenSurfaceIds.filter((surfaceId) => navigationSurfaceIds.has(surfaceId));
  const selected = useMemo(
    () => documents.find((document) => document.sourceDigest === selectedDigest) ?? documents[0] ?? null,
    [documents, selectedDigest],
  );

  const setSurfaceVisible = (surfaceId: string, visible: boolean) => {
    const next = new Set(preferences.hiddenSurfaceIds);
    if (visible) next.delete(surfaceId);
    else next.add(surfaceId);
    setClientPreferences({ hiddenSurfaceIds: [...next] });
  };

  async function copy(value: string, state: Exclude<CopyState, "idle" | "error">) {
    const copied = await copyText(value);
    setCopyState(copied ? state : "error");
    window.setTimeout(() => setCopyState("idle"), 1_500);
  }

  return (
    <section className="settings-section settings-surfaces-section">
      <SettingsHeading
        description="Built-in navigation and verified client documents share one renderer registry without becoming runtime truth."
        title="Surfaces"
      />

      <OcGroup
        actions={hiddenNavigationSurfaceIds.length > 0 ? (
          <OcButton onClick={() => setClientPreferences({ hiddenSurfaceIds: preferences.hiddenSurfaceIds.filter((surfaceId) => !navigationSurfaceIds.has(surfaceId)) })} variant="ghost">
            Show all
          </OcButton>
        ) : null}
        description="Choose the additional views shown in Tasks and Dashboard. Primary rail destinations remain available."
        title="Navigation"
      >
        <OcCard>
          {navigationSurfaces.map((surface) => {
            const required = primaryDestinations.has(surface.id);
            const visible = required || !hidden.has(surface.id);
            return (
              <OcSettingsRow
                control={(
                  <OcSwitch
                    checked={visible}
                    disabled={required}
                    label={`Show ${surface.label}`}
                    onCheckedChange={(checked) => setSurfaceVisible(surface.id, checked)}
                  />
                )}
                description={required ? "This destination is always available in the navigation rail." : surfaceDescription(surface)}
                key={surface.id}
                label={(
                  <span className="settings-surface-label">
                    <Glyph name={surface.glyph} size={16} />
                    <span>{surface.label}</span>
                  </span>
                )}
              />
            );
          })}
        </OcCard>
      </OcGroup>

      <OcGroup
        description="Documents are admitted by schema, revision, source digest, and target before entering any workspace, Voice, or Worker Environment renderer."
        title="Client surface registry"
      >
        <OcCard>
          {documents.length ? documents.map((admitted) => {
            const document = admitted.document;
            return (
              <OcSettingsRow
                control={(
                  <div className="settings-inline-actions">
                    <OcStatus label={document.release_channel === "stable" ? "Stable" : "Test"} tone={document.release_channel === "stable" ? "success" : "warning"} />
                    <OcButton onClick={() => setSelectedDigest(admitted.sourceDigest)} size="sm" variant="ghost">
                      Details
                    </OcButton>
                  </div>
                )}
                description={`${surfaceTargetLabel(document)} · revision ${document.revision} · ${admitted.sourcePath}`}
                key={admitted.sourceDigest}
                label={(
                  <span className="settings-surface-label">
                    <Glyph name={document.glyph} size={16} />
                    <span>{document.title}</span>
                  </span>
                )}
              />
            );
          }) : (
            <OcSettingsRow
              control={<OcStatus label="Empty" tone="muted" />}
              description={accessProfile === "live"
                ? "Administrator admits only stable documents projected from serialized evidence."
                : "Add a .surface.json document to the Test document root to exercise the renderer."}
              label="No admitted documents"
            />
          )}
        </OcCard>
      </OcGroup>

      {selected ? (
        <OcGroup
          actions={(
            <OcButton onClick={() => void copy(JSON.stringify(selected.document, null, 2), "document")} size="sm" variant="ghost">
              {copyState === "document" ? "Copied" : "Copy document"}
            </OcButton>
          )}
          description="Read-only admission and provenance for the selected renderer document."
          title={selected.document.title}
        >
          <OcCard>
            <SurfaceFact label="Document id" mono value={selected.document.id} />
            <SurfaceFact label="Target" value={surfaceTargetLabel(selected.document)} />
            <SurfaceFact label="Release" value={selected.document.release_channel} />
            <SurfaceFact label="Revision" value={String(selected.document.revision)} />
            <SurfaceFact label="Source" mono value={selected.sourcePath} />
            <SurfaceFact label="Digest" mono value={selected.sourceDigest} />
            <SurfaceFact label="Renderer" value={selected.document.render.kind} />
            <SurfaceFact label="Verified nodes" value={`${selected.verdict.details.node_count} · depth ${selected.verdict.details.max_depth}`} />
            {selected.sourceArtifactId ? <SurfaceFact label="Artifact" mono value={selected.sourceArtifactId} /> : null}
            {selected.sourceRunId ? <SurfaceFact label="Run" mono value={selected.sourceRunId} /> : null}
            {selected.sourceInterfaceId ? <SurfaceFact label="Interface" mono value={selected.sourceInterfaceId} /> : null}
          </OcCard>
        </OcGroup>
      ) : null}

      <OcGroup
        description={accessProfile === "lab"
          ? "Test watches this directory and atomically rematerializes only documents that pass the complete verifier contract."
          : "Administrator reads stable surface documents from projection evidence; local authoring is intentionally unavailable."}
        title="Document transport"
      >
        <OcCard>
          <OcSettingsRow
            control={(
              <OcButton disabled={!documentRoot} onClick={() => void copy(documentRoot, "root")} size="sm" variant="secondary">
                {copyState === "root" ? "Copied" : "Copy path"}
              </OcButton>
            )}
            description={documentRoot || "The current process did not report a document root."}
            label="Document root"
          />
          {accessProfile === "lab" ? (
            <OcSettingsRow
              control={(
                <OcButton onClick={() => void copy(JSON.stringify(testWidgetTemplate(), null, 2), "template")} size="sm" variant="secondary">
                  {copyState === "template" ? "Copied" : "Copy template"}
                </OcButton>
              )}
              description="Copies a minimal native Voice widget that can be saved as a root-level .surface.json file."
              label="Starter document"
            />
          ) : null}
          {copyState === "error" ? (
            <OcSettingsRow
              control={<OcStatus label="Copy failed" tone="danger" />}
              description="The desktop clipboard bridge refused the operation."
              label="Clipboard"
            />
          ) : null}
          {surfaceErrors.slice(0, 4).map((error) => (
            <OcSettingsRow
              control={<OcStatus label="Refused" tone="danger" />}
              description={error}
              key={error}
              label="Surface document"
            />
          ))}
        </OcCard>
      </OcGroup>
    </section>
  );
}

function SurfaceFact({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <OcSettingsRow
      control={<span className={`settings-fact${mono ? " is-mono" : ""}`} title={value}>{value}</span>}
      label={label}
    />
  );
}

function surfaceTargetLabel(document: ClientSurfaceDocument): string {
  const target = document.target;
  if (target.kind === "voice.widget") return `Voice widget · ${target.slot}`;
  if (target.kind === "voice.window") return `Voice window · ${target.slot}`;
  if (target.kind === "workspace.surface") return `${target.placements.join(" + ")} placement`;
  return `Worker Environment · ${target.worker_environment_id}`;
}

function testWidgetTemplate(): ClientSurfaceDocument {
  return {
    schema: "octopus.client_surface",
    schema_version: 1,
    release_channel: "lab",
    id: "voice.example",
    revision: 1,
    title: "Example widget",
    glyph: "descriptor",
    target: {
      kind: "voice.widget",
      slot: "right",
      order: 50,
      layout: {
        min_width: 180,
        preferred_width: 240,
        max_width: 360,
        min_height: 120,
        preferred_height: 160,
        max_height: 240,
        content_fit: "reflow",
      },
    },
    data: { status: "Ready" },
    render: {
      kind: "native",
      density: "compact",
      root: {
        type: "stack",
        gap: "sm",
        children: [
          { type: "status", label: "Document admitted", state: { path: "data.status" }, tone: "positive" },
          { type: "button", label: "Open settings", glyph: "settings", variant: "secondary", action: { event: "client.command", payload: { command: "open_settings" } } },
        ],
      },
    },
  };
}

function surfaceDescription(surface: ClientSurfaceDescriptor): string {
  switch (surface.rendererKind) {
    case "builtin.dashboard":
      return "Execution environments, runs, bindings, and topology.";
    case "builtin.registry":
      return "Artifact, mark, run, and descriptor projections.";
    case "builtin.worker_environments":
      return "Declared worker environment kinds and live instances.";
    case "builtin.event_log":
      return "Artifact movements and evidence from the ledger.";
    case "builtin.applications":
      return "Installed interface-first packages and their project availability.";
    case "builtin.drivers":
      return "Worker-environment kinds, project setup, doors, tools, and live rooms.";
    default:
      return surface.description ?? (surface.source === "client"
        ? "Client-defined projection surface."
        : "Dashboard projection surface.");
  }
}
