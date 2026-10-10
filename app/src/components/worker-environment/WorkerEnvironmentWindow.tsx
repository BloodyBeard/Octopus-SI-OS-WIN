import { useEffect, useMemo, useState } from "react";
import "../icon-glyphs/worker";
import { useClientSurfaces } from "../../shell/surfaceRegistry";
import { useRuntimeTopology } from "../../runtimeTopology";
import type {
  OctopusClientAccessProfile,
  OctopusWorkerEnvironmentPanelKind,
  OctopusWorkerEnvironmentWindowTarget,
} from "../../electron";
import type { RunProjection, WorkerEnvironmentProjection } from "../../data/topology";
import { appToast } from "../../toasts";
import { Glyph } from "../IconGlyphs";
import { RunEnvironmentView } from "./RunEnvironmentView";
import { workerEnvironmentPanelKind, workerEnvironmentSurfaceIsLaunchable } from "./surfaceRegistry";

const PANEL_KINDS = new Set<OctopusWorkerEnvironmentPanelKind>([
  "browser",
  "connection",
  "container",
  "runtime",
  "worktree",
  "changes",
  "desktop",
  "custom",
  "overview",
]);

type ResolvedEnvironment = {
  environmentName: string;
  run: RunProjection | null;
  workerEnvironment: WorkerEnvironmentProjection;
};

export function WorkerEnvironmentWindow({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  const [target, setTarget] = useState<OctopusWorkerEnvironmentWindowTarget>(readInitialTarget);
  const [pinned, setPinned] = useState(false);
  const topology = useRuntimeTopology(true, target.runId ? [target.runId] : []);
  const surfaceRegistry = useClientSurfaces(accessProfile);
  const resolved = useMemo(
    () => resolveWorkerEnvironment(topology.environments, target),
    [target, topology.environments],
  );

  useEffect(() => window.octopusTransport?.onWorkerEnvironmentWindowTarget?.((next) => {
    const parsed = parseTarget(next);
    if (parsed) setTarget(parsed);
  }), []);

  useEffect(() => {
    document.title = `${resolved?.workerEnvironment.label || "Worker Environment"} — Octopus`;
  }, [resolved?.workerEnvironment.label]);

  const openInMain = () => {
    void window.octopusTransport?.openWorkerEnvironmentInMain?.(target).catch((error) => {
      appToast.danger(formatFailure(error));
    });
  };

  const togglePinned = () => {
    void window.octopusTransport?.setWorkerEnvironmentWindowPinned?.(!pinned)
      .then((outcome) => setPinned(outcome.pinned))
      .catch((error) => appToast.danger(formatFailure(error)));
  };

  return (
    <main className="worker-environment-window">
      <header className="worker-environment-window-titlebar">
        <div className="worker-environment-window-title-left">
          <button aria-label="Dismiss Worker Environment window" onClick={closeWindow} title="Dismiss" type="button">
            <Glyph name="close" size={15} />
          </button>
        </div>
        <div className="worker-environment-window-title">
          <span>{resolved?.workerEnvironment.label || "Worker Environment"}</span>
          {resolved ? <small>{resolved.environmentName}</small> : null}
        </div>
        <div className="worker-environment-window-title-actions">
          <button aria-pressed={pinned} className={pinned ? "is-active" : ""} onClick={togglePinned} title={pinned ? "Stop keeping on top" : "Keep on top"} type="button">
            <Glyph name="pin" size={15} />
          </button>
          <button onClick={openInMain} title="Open in main window" type="button">
            <Glyph name="openExternal" size={15} />
          </button>
        </div>
      </header>

      <section className="worker-environment-window-content">
        {resolved && workerEnvironmentWindowTargetAvailable(resolved.workerEnvironment, surfaceRegistry.workerEnvironmentSurfaces, target) ? (
          <RunEnvironmentView
            accessProfile={accessProfile}
            maximized
            onClose={closeWindow}
            onMaximizedChange={() => {}}
            openPanelKind={target.panelKind}
            openSurfaceId={target.surfaceId}
            openFilePath={target.filePath}
            openClientSurface={target.clientSurface}
            openRequestId={1}
            run={resolved.run}
            surfaces={surfaceRegistry.workerEnvironmentSurfaces}
            windowed
            workerEnvironment={resolved.workerEnvironment}
          />
        ) : (
          <WorkerEnvironmentWindowUnavailable
            detail={topology.projection.mode === "connecting"
              ? "Reading the active serialized projection."
              : resolved
                ? "The exact projected surface behind this window is no longer available. Reopen it from the current Worker Environment projection."
                : `The projected Worker Environment ${target.workerEnvironmentId || "target"} is unavailable.`}
            title={topology.projection.mode === "connecting" ? "Reading Worker Environment" : resolved ? "Worker Environment surface changed" : "Worker Environment unavailable"}
          />
        )}
      </section>
    </main>
  );
}

export function WorkerEnvironmentWindowAccessUnavailable() {
  return (
    <main className="worker-environment-window">
      <header className="worker-environment-window-titlebar">
        <div className="worker-environment-window-title-left">
          <button aria-label="Dismiss Worker Environment window" onClick={closeWindow} title="Dismiss" type="button"><Glyph name="close" size={15} /></button>
        </div>
        <div className="worker-environment-window-title"><span>Worker Environment</span></div>
      </header>
      <section className="worker-environment-window-content">
        <WorkerEnvironmentWindowUnavailable detail="Unlock the main application before opening a projected Worker Environment." title="Octopus is locked" />
      </section>
    </main>
  );
}

function WorkerEnvironmentWindowUnavailable({ detail, title }: { detail: string; title: string }) {
  return (
    <div className="worker-environment-window-unavailable" role="status">
      <Glyph name="ee" size={23} />
      <strong>{title}</strong>
      <span>{detail}</span>
    </div>
  );
}

function resolveWorkerEnvironment(
  environments: ReturnType<typeof useRuntimeTopology>["environments"],
  target: OctopusWorkerEnvironmentWindowTarget,
): ResolvedEnvironment | null {
  for (const environment of environments) {
    for (const run of environment.runs) {
      const projected = run.workerEnvironments.find((candidate) => candidate.id === target.workerEnvironmentId)
        ?? (run.workerEnvironment.id === target.workerEnvironmentId ? run.workerEnvironment : null);
      if (projected) return { environmentName: environment.name, run, workerEnvironment: projected };
    }
    const projected = environment.workerEnvironments.find((candidate) => candidate.id === target.workerEnvironmentId);
    if (projected) {
      const run = projected.runId
        ? environment.runs.find((candidate) => candidate.id === projected.runId) ?? null
        : null;
      return { environmentName: environment.name, run, workerEnvironment: projected };
    }
  }
  return null;
}

function readInitialTarget(): OctopusWorkerEnvironmentWindowTarget {
  const params = new URLSearchParams(window.location.search);
  return parseTarget({
    runId: params.get("runId") || "",
    workerEnvironmentId: params.get("environmentId") || "",
    panelKind: params.get("panel") || "overview",
    surfaceId: params.get("surfaceId") || "",
    filePath: params.get("filePath") || "",
    clientSurface: params.get("documentId") ? {
      documentId: params.get("documentId") || "",
      revision: Number(params.get("documentRevision")),
      sourceDigest: params.get("sourceDigest") || "",
    } : undefined,
  }) ?? { runId: "", workerEnvironmentId: "", panelKind: "overview" };
}

function parseTarget(value: unknown): OctopusWorkerEnvironmentWindowTarget | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const runId = boundedText(record.runId, 512);
  const workerEnvironmentId = boundedText(record.workerEnvironmentId, 512);
  const panelKind = boundedText(record.panelKind, 32) as OctopusWorkerEnvironmentPanelKind;
  if (!workerEnvironmentId || !PANEL_KINDS.has(panelKind)) return null;
  const surfaceId = boundedText(record.surfaceId, 512);
  const filePath = boundedText(record.filePath, 4_096);
  if (filePath && !surfaceId) return null;
  const clientSurfaceRecord = record.clientSurface && typeof record.clientSurface === "object" && !Array.isArray(record.clientSurface)
    ? record.clientSurface as Record<string, unknown>
    : null;
  const documentId = boundedText(clientSurfaceRecord?.documentId, 512);
  const sourceDigest = boundedText(clientSurfaceRecord?.sourceDigest, 128);
  const revision = Number(clientSurfaceRecord?.revision);
  const hasClientSurface = Boolean(clientSurfaceRecord);
  if (hasClientSurface && (!surfaceId || !documentId || !sourceDigest || !Number.isSafeInteger(revision) || revision < 0)) return null;
  return {
    runId,
    workerEnvironmentId,
    panelKind,
    ...(surfaceId ? { surfaceId } : {}),
    ...(filePath ? { filePath } : {}),
    ...(hasClientSurface ? { clientSurface: { documentId, revision, sourceDigest } } : {}),
  };
}

function workerEnvironmentWindowTargetAvailable(
  environment: WorkerEnvironmentProjection,
  surfaces: ReturnType<typeof useClientSurfaces>["workerEnvironmentSurfaces"],
  target: OctopusWorkerEnvironmentWindowTarget,
): boolean {
  if (target.clientSurface) {
    return surfaces.some((admitted) => {
      const documentTarget = admitted.document.target;
      return documentTarget.kind === "worker_environment.surface"
        && documentTarget.worker_environment_id === environment.id
        && documentTarget.surface_id === target.surfaceId
        && admitted.document.id === target.clientSurface?.documentId
        && admitted.document.revision === target.clientSurface.revision
        && admitted.sourceDigest === target.clientSurface.sourceDigest;
    });
  }
  if (target.surfaceId) {
    return environment.surfaces.some((surface) => surface.id === target.surfaceId && workerEnvironmentSurfaceIsLaunchable(surface))
      || surfaces.some((admitted) => {
        const documentTarget = admitted.document.target;
        return documentTarget.kind === "worker_environment.surface"
          && documentTarget.worker_environment_id === environment.id
          && documentTarget.surface_id === target.surfaceId;
      });
  }
  if (target.panelKind === "overview") return true;
  if (target.panelKind === "custom") {
    return surfaces.some((admitted) => admitted.document.target.kind === "worker_environment.surface"
      && admitted.document.target.worker_environment_id === environment.id);
  }
  return environment.surfaces.some((surface) => (
    workerEnvironmentSurfaceIsLaunchable(surface)
    && workerEnvironmentPanelKind(surface) === target.panelKind
  ));
}

function boundedText(value: unknown, limit: number): string {
  if (typeof value !== "string") return "";
  const text = value.trim();
  return text.length <= limit && !/[\u0000-\u001f]/.test(text) ? text : "";
}

function closeWindow() {
  window.octopusTransport?.windowControl("close");
}

function formatFailure(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}
