import { useEffect, useRef, useState } from "react";
import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import { OcButton, OcSelect } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { ShellTooltip } from "../shell/ShellTooltip";
import { LiveSurfaceEmpty, ManualRoomControlWarning, dataText } from "./RunEnvironmentSurfacePrimitives";
import type { DesktopEvidence, DesktopSurfaceRequest } from "./desktopSurfaceModel";
import { desktopEvidenceRevision, resolveDesktopViewEvidence } from "./desktopSurfaceModel";
import "./desktopSurface.css";

export function DesktopSurface({ active = true, environment, surface }: {
  active?: boolean;
  environment: WorkerEnvironmentProjection;
  surface?: WorkerEnvironmentSurfaceProjection;
}) {
  const [evidence, setEvidence] = useState<DesktopEvidence | null>(null);
  const [screenId, setScreenId] = useState("");
  const [error, setError] = useState("");
  const [pendingControl, setPendingControl] = useState<DesktopSurfaceRequest["operation"] | null>(null);
  const busy = pendingControl !== null;
  const [warning, setWarning] = useState(false);
  const [openingPermissions, setOpeningPermissions] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [loadedFrame, setLoadedFrame] = useState("");
  const generation = useRef(0);
  const requestGeneration = useRef(0);
  const controlPending = useRef<{ operation: DesktopSurfaceRequest["operation"] } | null>(null);
  const frame = useRef<HTMLDivElement>(null);
  const viewId = useRef("");
  const identity = `${environment.id}\0${environment.runId}\0${surface?.id}\0${surface?.viewerUrl}`;
  const activeIdentity = useRef(identity); activeIdentity.current = identity;

  useEffect(() => {
    setEvidence(null); setError(""); setScreenId(""); setWarning(false); setPendingControl(null); controlPending.current = null;
  }, [identity]);

  useEffect(() => {
    const current = ++generation.current;
    const attachment = crypto.randomUUID(); viewId.current = attachment;
    let latest: DesktopEvidence | null = null;
    let revision = "";
    setEvidence(null); setError(""); setLoadedFrame("");
    let timer: ReturnType<typeof setTimeout> | undefined;
    const transport = window.octopusTransport?.desktopSurfaceRequest;
    if (!active || !surface || surface.mode === "fixture" || !transport) return;
    const selectedSurface = surface;
    async function read() {
      if (generation.current !== current) return;
      if (controlPending.current) { timer = setTimeout(() => void read(), 67); return; }
      const readGeneration = ++requestGeneration.current;
      try {
        const result = await transport!({ workerEnvironmentId: environment.id, surfaceId: selectedSurface.id,
          runId: environment.runId, operation: "view", viewId: attachment, ...(screenId ? { screenId } : {}) });
        if (generation.current !== current || requestGeneration.current !== readGeneration) return;
        if (!("evidence" in result)) throw new Error("Desktop view returned no observation evidence");
        latest = resolveDesktopViewEvidence(latest, result.evidence);
        const nextRevision = desktopEvidenceRevision(latest);
        if (revision !== nextRevision) { setEvidence(latest); revision = nextRevision; }
        setError("");
        if (!screenId && result.evidence.screens.length >= 1) setScreenId(result.evidence.screens[0].id);
        timer = setTimeout(() => void read(), 67);
      } catch (failure) {
        if (generation.current === current && requestGeneration.current === readGeneration) { latest = null; revision = ""; setEvidence(null); setError(failure instanceof Error ? failure.message : String(failure)); }
      }
    }
    void read();
    return () => {
      ++generation.current; if (timer) clearTimeout(timer);
      void transport!({workerEnvironmentId: environment.id, surfaceId: selectedSurface.id, runId: environment.runId, operation: "pause_view", viewId: attachment}).catch((failure) => console.error("Desktop viewer detach failed", failure));
    };
  }, [active, identity, screenId, refresh]);

  async function control(operation: DesktopSurfaceRequest["operation"]) {
    const transport = window.octopusTransport?.desktopSurfaceRequest;
    if (!surface || !transport || surface.readOnly || (controlPending.current && (operation !== "stop" || controlPending.current.operation === "stop"))) return;
    if (operation !== "stop" && !evidence) return;
    const pending = { operation }; controlPending.current = pending; setPendingControl(operation); setWarning(false);
    const current = generation.current; ++requestGeneration.current;
    const targetIdentity = identity;
    const isCurrent = () => controlPending.current === pending && (operation === "stop" ? activeIdentity.current === targetIdentity : generation.current === current);
    try {
      const result = await transport({ viewId: viewId.current, workerEnvironmentId: environment.id, surfaceId: surface.id, runId: environment.runId, operation,
        ...(operation === "stop" ? {} : { controlGeneration: evidence!.control.generation }) });
      if (!isCurrent()) return;
      if (!("evidence" in result)) throw new Error("Desktop control returned no acknowledgement evidence");
      if (operation === "stop" && result.evidence.control.mode !== "stopped") throw new Error("Desktop Stop response did not confirm stopped control");
      setEvidence(result.evidence);
      if (operation !== "stop") { setError(""); setRefresh((value) => value + 1); }
    } catch (failure) {
      if (isCurrent()) setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      if (controlPending.current === pending) { controlPending.current = null; setPendingControl(null); }
    }
  }

  async function openPermissions() {
    const transport = window.octopusTransport?.openDesktopPermissions;
    if (!surface || !transport || openingPermissions) return;
    const targetIdentity = identity; setOpeningPermissions(true);
    try { await transport({ workerEnvironmentId: environment.id, surfaceId: surface.id, runId: environment.runId }); }
    catch (failure) { if (activeIdentity.current === targetIdentity) setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setOpeningPermissions(false); }
  }

  function invalidFrame() {
    ++generation.current; ++requestGeneration.current;
    setEvidence(null); setLoadedFrame(""); setError("Desktop frame could not be decoded");
    if (surface) void window.octopusTransport?.desktopSurfaceRequest({ workerEnvironmentId: environment.id, surfaceId: surface.id, runId: environment.runId, viewId: viewId.current, operation: "pause_view" }).catch((failure) => console.error("Desktop viewer detach failed", failure));
  }

  if (surface?.mode === "fixture") return (
    <div className="we-desktop-shell"><div className="we-desktop-fixture">
      <div className="we-desktop-fixture-toolbar"><span /><span /><span /><strong>{dataText(surface.data, "title") || surface.label}</strong></div>
      <div><Glyph name="desktop" size={28} /><strong>{dataText(surface.data, "detail") || "Test desktop room"}</strong><small>No host desktop is connected.</small></div>
    </div></div>
  );
  if (!surface || !window.octopusTransport?.desktopSurfaceRequest) return (
    <LiveSurfaceEmpty glyph="desktop" title="Desktop viewer unavailable" detail="The desktop room needs its declared host connection before its screen can be viewed." />
  );
  const observation = evidence?.observation;
  const image = observation?.image;
  const mode = evidence?.control.mode;
  const controlling = mode === "model" && evidence?.control.owner === environment.id;
  const state = mode === "stopped" ? "Computer use stopped" : error ? "View interrupted" : mode === "human" ? "You have control"
    : controlling ? "Worker has control" : mode === "model" ? "Another room has control"
      : evidence?.status === "permission_required" ? "Screen permission required" : evidence ? "Observing" : "Connecting to desktop";
  const screens = evidence?.screens || [];
  const missingPermissions = evidence ? [
    evidence.permissions.screen_capture ? "" : "Screen Recording",
    evidence.permissions.accessibility ? "" : "Accessibility",
  ].filter(Boolean) : [];
  const permissionLabel = missingPermissions.length ? `${missingPermissions.join(" and ")} required` : "";
  const permissionButton = permissionLabel ? (
    <OcButton disabled={openingPermissions || !window.octopusTransport?.openDesktopPermissions} onClick={() => void openPermissions()} size="sm" variant="secondary">{permissionLabel}</OcButton>
  ) : null;
  return (
    <div className="we-browser-shell" data-control={mode || "unknown"}>
      <div className="we-browser-toolbar">
        <span aria-live="polite" className="mr-auto flex items-center gap-2 text-sm text-token-description-foreground"><Glyph name="desktop" size={14} />{state}</span>
        {image ? permissionButton : null}
        {screens.length > 1 ? <OcSelect label="Display" disabled={busy} onChange={setScreenId} value={screenId} placeholder="Choose a display" options={screens.map((screen) => ({value:screen.id,label:`Display ${screen.id} · ${screen.pixel_width} × ${screen.pixel_height}`}))} /> : null}
        <div className="we-browser-history">
          <DesktopToolbarButton disabled={busy} glyph="refresh" label="Refresh" onClick={() => setRefresh((value) => value + 1)} />
          {image ? <DesktopToolbarButton glyph="expand" label="Expand" onClick={() => void frame.current?.requestFullscreen().catch((failure) => setError(String(failure)))} /> : null}
          {mode === "human" || mode === "stopped"
            ? <DesktopToolbarButton disabled={busy || surface.readOnly} glyph="resume" label="Release control" onClick={() => void control("release_control")} selected />
            : <DesktopToolbarButton disabled={busy || surface.readOnly || !evidence} glyph="hand" label="Take control" onClick={() => setWarning(true)} />}
          <DesktopToolbarButton disabled={pendingControl === "stop" || surface.readOnly || (mode === "stopped" && !busy)} glyph="stop" label="Stop computer use" onClick={() => void control("stop")} />
        </div>
      </div>
      {error ? <div className="we-browser-control-error" role="alert">{error} Refresh to check the connection again.</div> : null}
      <div className="we-desktop-native-viewport" ref={frame}>
        <div className="we-desktop-native-frame" style={image ? {width:`min(100cqw, ${100*image.width/image.height}cqh)`,height:`min(100cqh, ${100*image.height/image.width}cqw)`} : {width:"100%",height:"100%"}} data-streaming={(image && loadedFrame === image.sha256) || undefined}>
        {image ? <img alt={`Display ${observation.screen.id}, observed ${new Date(observation.captured_at_unix_ms).toLocaleTimeString()}`} onLoad={() => setLoadedFrame(image.sha256)} onError={invalidFrame} draggable={false} src={`data:${image.media_type};base64,${image.data_base64}`} />
          : permissionLabel ? <div className="we-desktop-permission">
            <LiveSurfaceEmpty glyph="desktop" title="Allow Octopus to see this Mac" detail={`${missingPermissions.join(" and ")} ${missingPermissions.length > 1 ? "are" : "is"} off in System Settings. Viewing never takes control.`} />
            {permissionButton}
          </div>
          : <LiveSurfaceEmpty glyph="desktop" title={screenId || !evidence ? "Waiting for the screen" : "Choose a display"} detail="Viewing never takes control of the Mac." />}
        {observation?.focus ? <span className="absolute inset-x-0 top-full flex h-12 items-center justify-center px-4 text-center text-sm text-token-description-foreground">{observation.focus.app_id}</span> : null}
      </div>
      </div>
      <ManualRoomControlWarning glyph="desktop" onCancel={() => setWarning(false)} onContinue={() => void control("take_control")} open={warning} title="Take control of the desktop" />
    </div>
  );
}

function DesktopToolbarButton({ disabled = false, glyph, label, onClick, selected = false }: {
  disabled?: boolean; glyph: GlyphName; label: string; onClick: () => void; selected?: boolean;
}) {
  return <ShellTooltip label={label}><button aria-label={label} aria-pressed={selected || undefined} className="we-browser-control" disabled={disabled} onClick={onClick} type="button">
    <Glyph name={glyph} size={16} />
  </button></ShellTooltip>;
}
