import type {
  RunProjection,
  TopologySnapshot,
  WorkerEnvironmentProjection,
  WorkerEnvironmentSurfaceProjection,
} from "../../data/topology";
import type { AdmittedClientSurfaceDocument } from "../../client-surfaces/types";
import {
  primaryWorkerEnvironmentSurface,
  workerEnvironmentProductSurfaces,
  workerEnvironmentPanelKind,
  workerEnvironmentSurfaceGlyph,
  workerEnvironmentSurfaceIsPreview,
  type WorkerEnvironmentPanelKind,
} from "./surfaceRegistry";

export const OPEN_WORKER_ENVIRONMENT_PANE_EVENT = "octopus:open-worker-environment-pane";

export type WorkerEnvironmentPaneTarget = {
  schemaVersion: 1;
  workerEnvironmentId: string;
  surfaceId?: string;
  panelKind?: WorkerEnvironmentPanelKind;
  filePath?: string;
  clientSurface?: {
    documentId: string;
    revision: number;
    sourceDigest: string;
  };
};

export type ResolvedWorkerEnvironment = {
  environmentId: string;
  environmentName: string;
  run: RunProjection | null;
  workerEnvironment: WorkerEnvironmentProjection;
};

export function isControllableNativeBrowserSurface(surface: WorkerEnvironmentSurfaceProjection | null | undefined): surface is WorkerEnvironmentSurfaceProjection {
  return surface?.kind === "browser" && surface.protocol === "browser.native.v1" && surface.mode === "direct" && !surface.readOnly;
}

export function workerEnvironmentClientSurfaces(
  workerEnvironmentId: string,
  surfaces: AdmittedClientSurfaceDocument[],
): AdmittedClientSurfaceDocument[] {
  return surfaces
    .filter((admitted) => {
      const target = admitted.document.target;
      return target.kind === "worker_environment.surface"
        && target.worker_environment_id === workerEnvironmentId;
    })
    .sort((left, right) => {
      const leftTarget = left.document.target;
      const rightTarget = right.document.target;
      const leftOrder = leftTarget.kind === "worker_environment.surface" ? leftTarget.order : 0;
      const rightOrder = rightTarget.kind === "worker_environment.surface" ? rightTarget.order : 0;
      return leftOrder - rightOrder
        || left.document.title.localeCompare(right.document.title)
        || left.sourceDigest.localeCompare(right.sourceDigest);
    });
}

export function primaryWorkerEnvironmentClientSurface(
  workerEnvironmentId: string,
  surfaces: AdmittedClientSurfaceDocument[],
): AdmittedClientSurfaceDocument | null {
  return workerEnvironmentClientSurfaces(workerEnvironmentId, surfaces)[0] ?? null;
}

export function defaultWorkerEnvironmentPaneTarget(
  workerEnvironment: WorkerEnvironmentProjection,
  surfaces: AdmittedClientSurfaceDocument[],
): Omit<WorkerEnvironmentPaneTarget, "schemaVersion"> {
  const projectedSurface = primaryWorkerEnvironmentSurface(workerEnvironment);
  const clientSurface = primaryWorkerEnvironmentClientSurface(workerEnvironment.id, surfaces);
  if (clientSurface && (!projectedSurface || workerEnvironmentPanelKind(projectedSurface) === "connection")) {
    const target = clientSurface.document.target;
    if (target.kind === "worker_environment.surface") {
      return {
        workerEnvironmentId: workerEnvironment.id,
        surfaceId: target.surface_id,
        panelKind: "custom",
        clientSurface: {
          documentId: clientSurface.document.id,
          revision: clientSurface.document.revision,
          sourceDigest: clientSurface.sourceDigest,
        },
      };
    }
  }
  if (projectedSurface) {
    return {
      workerEnvironmentId: workerEnvironment.id,
      surfaceId: projectedSurface.id,
      panelKind: workerEnvironmentPanelKind(projectedSurface),
    };
  }
  if (clientSurface) {
    const target = clientSurface.document.target;
    if (target.kind === "worker_environment.surface") {
      return {
        workerEnvironmentId: workerEnvironment.id,
        surfaceId: target.surface_id,
        panelKind: "custom",
        clientSurface: {
          documentId: clientSurface.document.id,
          revision: clientSurface.document.revision,
          sourceDigest: clientSurface.sourceDigest,
        },
      };
    }
  }
  return {
    workerEnvironmentId: workerEnvironment.id,
    panelKind: "overview",
  };
}

export function dispatchOpenWorkerEnvironmentPane(target: Omit<WorkerEnvironmentPaneTarget, "schemaVersion">): void {
  window.dispatchEvent(new CustomEvent(OPEN_WORKER_ENVIRONMENT_PANE_EVENT, {
    detail: { schemaVersion: 1, ...target } satisfies WorkerEnvironmentPaneTarget,
  }));
}

export function resolveWorkerEnvironment(snapshot: TopologySnapshot, workerEnvironmentId: string): ResolvedWorkerEnvironment | null {
  if (!workerEnvironmentId) return null;
  for (const environment of snapshot.environments) {
    for (const run of environment.runs) {
      for (const workerEnvironment of run.workerEnvironments) {
        if (workerEnvironment.id === workerEnvironmentId) {
          return { environmentId: environment.id, environmentName: environment.name, run, workerEnvironment };
        }
      }
    }
    for (const workerEnvironment of environment.workerEnvironments) {
      if (workerEnvironment.id !== workerEnvironmentId) continue;
      const ownerRun = workerEnvironment.runId
        ? environment.runs.find((candidate) => candidate.id === workerEnvironment.runId) ?? null
        : null;
      return { environmentId: environment.id, environmentName: environment.name, run: ownerRun, workerEnvironment };
    }
  }
  return null;
}

export function resolvePaneSurface(
  workerEnvironment: WorkerEnvironmentProjection,
  target: Pick<WorkerEnvironmentPaneTarget, "surfaceId" | "panelKind">,
) {
  if (target.panelKind === "file" && target.surfaceId) {
    return workerEnvironment.surfaces.find((surface) => (
      surface.id === target.surfaceId && workerEnvironmentPanelKind(surface) === "files"
    )) ?? null;
  }
  const surfaces = workerEnvironmentProductSurfaces(workerEnvironment.surfaces);
  if (target.surfaceId) {
    // A tab carrying an exact projected surface identity must never drift to a
    // same-kind replacement when that identity is withdrawn.
    return surfaces.find((surface) => surface.id === target.surfaceId) ?? null;
  }
  if (target.panelKind && target.panelKind !== "overview") {
    const byKind = surfaces.find((surface) => workerEnvironmentPanelKind(surface) === target.panelKind);
    if (byKind) return byKind;
  }
  return primaryWorkerEnvironmentSurface(workerEnvironment);
}

export function resolvePaneClientSurface(
  surfaces: AdmittedClientSurfaceDocument[],
  target: WorkerEnvironmentPaneTarget,
): AdmittedClientSurfaceDocument | null {
  if (!target.clientSurface || !target.surfaceId) return null;
  return surfaces.find((admitted) => {
    const documentTarget = admitted.document.target;
    return documentTarget.kind === "worker_environment.surface"
      && documentTarget.worker_environment_id === target.workerEnvironmentId
      && documentTarget.surface_id === target.surfaceId
      && admitted.document.id === target.clientSurface?.documentId
      && admitted.document.revision === target.clientSurface.revision
      && admitted.sourceDigest === target.clientSurface.sourceDigest;
  }) ?? null;
}

export function paneTargetTitle(
  workerEnvironment: WorkerEnvironmentProjection,
  target: Pick<WorkerEnvironmentPaneTarget, "surfaceId" | "panelKind" | "filePath">,
  clientSurface?: AdmittedClientSurfaceDocument | null,
): { title: string; glyph: ReturnType<typeof workerEnvironmentSurfaceGlyph> } {
  if (clientSurface) return { title: clientSurface.document.title, glyph: clientSurface.document.glyph };
  if (target.panelKind === "terminal") return { title: "Terminal", glyph: "terminal" };
  if (target.filePath) {
    const name = target.filePath.replaceAll("\\", "/").split("/").filter(Boolean).pop() ?? target.filePath;
    return { title: name, glyph: "file" };
  }
  const surface = resolvePaneSurface(workerEnvironment, target);
  if (!surface) {
    if (target.panelKind === "browser") return { title: "Browser", glyph: "browser" };
    return target.panelKind === "files"
      ? { title: "Files", glyph: "folder" }
      : { title: workerEnvironment.label || "Worker Environment", glyph: "ee" };
  }
  return { title: paneWorkerEnvironmentSurfaceLabel(workerEnvironment, surface), glyph: workerEnvironmentSurfaceGlyph(surface) };
}

export function paneSurfaceLabel(kind: string, label: string, panelKind: WorkerEnvironmentPanelKind): string {
  if (panelKind === "changes") return "Review";
  if (panelKind === "browser") {
    return label || (workerEnvironmentSurfaceIsPreview({ kind }) ? "Preview" : "Browser");
  }
  if (panelKind === "terminal") return "Terminal";
  if (panelKind === "files") return "Files";
  if (panelKind === "desktop") return "Desktop";
  return label || kind;
}

export function paneWorkerEnvironmentSurfaceLabel(
  workerEnvironment: Pick<WorkerEnvironmentProjection, "surfaces">,
  surface: WorkerEnvironmentSurfaceProjection,
): string {
  const panelKind = workerEnvironmentPanelKind(surface);
  if (panelKind === "changes") {
    const reviewSurfaces = workerEnvironmentProductSurfaces(workerEnvironment.surfaces)
      .filter((candidate) => workerEnvironmentPanelKind(candidate) === "changes");
    const rootLabel = surfaceSlotLabel(surface.data.slot) || filesystemRootLabel(surface.root);
    return reviewSurfaces.length > 1 && rootLabel ? `Review · ${rootLabel}` : "Review";
  }
  if (panelKind !== "files") return paneSurfaceLabel(surface.kind, surface.label, panelKind);
  const label = surface.label.trim();
  const fileSurfaces = workerEnvironmentProductSurfaces(workerEnvironment.surfaces)
    .filter((candidate) => workerEnvironmentPanelKind(candidate) === "files");
  if (fileSurfaces.length <= 1) return label || filesystemRootLabel(surface.root) || "Files";
  if (label && label.toLowerCase() !== "files") return label;
  const genericLabel = label || "Files";
  const rootLabel = surfaceSlotLabel(surface.data.slot) || filesystemRootLabel(surface.root);
  return rootLabel && rootLabel.toLowerCase() !== genericLabel.toLowerCase()
    ? `${genericLabel} · ${rootLabel}`
    : genericLabel;
}

export type WorkerEnvironmentPaneAction = {
  glyph: ReturnType<typeof workerEnvironmentSurfaceGlyph>;
  id: string;
  panelKind: WorkerEnvironmentPanelKind;
  target: Omit<WorkerEnvironmentPaneTarget, "schemaVersion">;
  title: string;
};

export function activeWorkerEnvironmentPaneActivities(snapshot: TopologySnapshot, runIds: string[] = snapshot.environments.flatMap((environment) => [
  ...environment.runs.map((run) => run.id),
  ...environment.workerEnvironments.map((room) => room.runId),
]).filter((id): id is string => Boolean(id))) {
  return workerEnvironmentPaneActions(snapshot, runIds).flatMap((action) => {
    if (action.panelKind !== "browser" && action.panelKind !== "desktop") return [];
    const resolved = resolveWorkerEnvironment(snapshot, action.target.workerEnvironmentId);
    if (!resolved || !(action.panelKind === "desktop" ? ["active", "failed"] : ["active"]).includes(resolved.workerEnvironment.lifecycle)) return [];
    const surface = resolvePaneSurface(resolved.workerEnvironment, action.target);
    if (!surface || surface.mode !== "direct") return [];
    if (action.panelKind === "desktop") {
      const openedAt = surface.data.opened_at_movement_seq, sequence = surface.data.last_activity_movement_seq;
      if (surface.protocol !== "desktop_session" || typeof surface.data.host_generation !== "string" || !surface.data.host_generation
        || typeof openedAt !== "number" || !Number.isSafeInteger(openedAt) || openedAt <= 0
        || typeof sequence !== "number" || !Number.isSafeInteger(sequence) || sequence < openedAt) return [];
      const identity = JSON.stringify([resolved.workerEnvironment.runId, resolved.workerEnvironment.id, openedAt, surface.data.formation_pin, surface.id, surface.data.host_generation]);
      return [{ identity, presentationId: identity, sequence, running: false, explicit: false, native: false, activate: true, target: action.target }];
    }
    const targetId = surface.data.target_id;
    const activity = surface.data.activity;
    const openedAt = surface.data.opened_at_movement_seq;
    if (typeof targetId !== "string" || !targetId.trim()) return [];
    if (typeof openedAt !== "number" || !Number.isSafeInteger(openedAt) || openedAt <= 0) return [];
    const identity = JSON.stringify([resolved.workerEnvironment.runId, resolved.workerEnvironment.id, openedAt, surface.data.formation_pin, surface.id, targetId]);
    const events: Array<{ identity: string; presentationId: string; sequence: number; running: boolean; explicit: boolean; native: boolean; activate: boolean; target: typeof action.target }> = [];
    if (activity && typeof activity === "object" && !Array.isArray(activity)) {
      const sequence = (activity as Record<string, unknown>).source_movement_seq;
      if (typeof sequence === "number" && Number.isSafeInteger(sequence) && sequence > 0) events.push({
        identity, presentationId: identity, sequence, running: (activity as Record<string, unknown>).status === "running",
        explicit: false, native: false, activate: true, target: action.target,
      });
    }
    for (const [name, native] of [["presentation_intent", false], ["native_open_intent", true]] as const) {
      const intent = surface.data[name];
      if (!intent || typeof intent !== "object" || Array.isArray(intent)) continue;
      const row = intent as Record<string, unknown>;
      if (typeof row.id !== "number" || !Number.isSafeInteger(row.id) || row.id <= 0 || typeof row.activate !== "boolean"
        || (native ? row.targetId !== targetId || !["popup", "context-menu", "pip", "tool"].includes(String(row.source)) : row.target_id !== targetId || row.source !== "tool")) continue;
      events.push({ identity, presentationId: JSON.stringify([identity, name, row.id]), sequence: native ? openedAt : row.id,
        running: false, explicit: true, native, activate: row.activate, target: action.target });
    }
    return events;
  }).sort((left, right) => Number(left.native) - Number(right.native) || left.sequence - right.sequence || left.presentationId.localeCompare(right.presentationId));
}

const PANE_ACTION_ORDER: Partial<Record<WorkerEnvironmentPanelKind, number>> = {
  changes: 1,
  browser: 2,
};

export function workerEnvironmentPaneActions(
  snapshot: TopologySnapshot,
  preferredRunIds: string[],
  clientSurfaces: AdmittedClientSurfaceDocument[] = [],
): WorkerEnvironmentPaneAction[] {
  const preferred = new Set(preferredRunIds.filter(Boolean));
  const candidates = new Map<string, { action: WorkerEnvironmentPaneAction; environmentId: string; environmentLabel: string; preferredRun: boolean }>();
  for (const environment of snapshot.environments) {
    const rows: Array<{ run: RunProjection | null; workerEnvironment: WorkerEnvironmentProjection }> = [
      ...environment.runs.flatMap((run) => run.workerEnvironments.map((workerEnvironment) => ({ run, workerEnvironment }))),
      ...environment.workerEnvironments.map((workerEnvironment) => ({ run: null, workerEnvironment })),
    ];
    for (const { run, workerEnvironment } of rows) {
      if (!workerEnvironment.id) continue;
      for (const surface of workerEnvironmentProductSurfaces(workerEnvironment.surfaces)) {
        const panelKind = workerEnvironmentPanelKind(surface);
        const key = `${workerEnvironment.id}\0projection:${surface.id}`;
        const candidate = {
          action: {
            glyph: workerEnvironmentSurfaceGlyph(surface),
            id: `${workerEnvironment.id}:projection:${surface.id}`,
            panelKind,
            target: { workerEnvironmentId: workerEnvironment.id, surfaceId: surface.id, panelKind },
            title: paneWorkerEnvironmentSurfaceLabel(workerEnvironment, surface),
          },
          environmentId: workerEnvironment.id,
          environmentLabel: workerEnvironment.label || workerEnvironment.kind || workerEnvironment.id,
          preferredRun: Boolean(run && preferred.has(run.id)) || Boolean(workerEnvironment.runId && preferred.has(workerEnvironment.runId)),
        };
        const current = candidates.get(key);
        if (!current || candidate.preferredRun) candidates.set(key, candidate);
      }
    }
  }
  for (const admitted of clientSurfaces) {
    const target = admitted.document.target;
    if (target.kind !== "worker_environment.surface") continue;
    const resolved = resolveWorkerEnvironment(snapshot, target.worker_environment_id);
    if (!resolved) continue;
    const key = `${resolved.workerEnvironment.id}\0document:${target.surface_id}`;
    candidates.set(key, {
      action: {
        glyph: admitted.document.glyph,
        id: `${resolved.workerEnvironment.id}:document:${target.surface_id}:${admitted.sourceDigest}`,
        panelKind: "custom",
        target: {
          workerEnvironmentId: resolved.workerEnvironment.id,
          surfaceId: target.surface_id,
          panelKind: "custom",
          clientSurface: {
            documentId: admitted.document.id,
            revision: admitted.document.revision,
            sourceDigest: admitted.sourceDigest,
          },
        },
        title: admitted.document.title,
      },
      environmentId: resolved.workerEnvironment.id,
      environmentLabel: resolved.workerEnvironment.label || resolved.workerEnvironment.kind || resolved.workerEnvironment.id,
      preferredRun: Boolean(resolved.run && preferred.has(resolved.run.id))
        || Boolean(resolved.workerEnvironment.runId && preferred.has(resolved.workerEnvironment.runId)),
    });
  }
  const rows = [...candidates.values()];
  const scoped = preferred.size ? rows.filter((candidate) => candidate.preferredRun) : [];
  const titleCounts = new Map<string, number>();
  for (const candidate of scoped) titleCounts.set(candidate.action.title, (titleCounts.get(candidate.action.title) ?? 0) + 1);
  return scoped.map((candidate) => ({
    ...candidate.action,
    title: (titleCounts.get(candidate.action.title) ?? 0) > 1
      ? `${candidate.action.title} · ${paneEnvironmentQualifier(candidate.action.title, candidate.environmentLabel, candidate.environmentId)}`
      : candidate.action.title,
  })).sort((left, right) => (
    (PANE_ACTION_ORDER[left.panelKind] ?? 9) - (PANE_ACTION_ORDER[right.panelKind] ?? 9)
      || left.title.localeCompare(right.title)
  ));
}

function filesystemRootLabel(value: string): string {
  const normalized = value.trim().replace(/[\\/]+$/, "");
  return normalized.split(/[\\/]/).filter(Boolean).at(-1) ?? "";
}

function surfaceSlotLabel(value: unknown): string {
  if (typeof value !== "string") return "";
  const normalized = value.trim().replace(/[_-]+/g, " ");
  return normalized ? normalized.charAt(0).toUpperCase() + normalized.slice(1) : "";
}

function paneEnvironmentQualifier(title: string, environmentLabel: string, environmentId: string): string {
  const label = environmentLabel.trim();
  if (label && label.toLowerCase() !== title.trim().toLowerCase()) return label;
  if (environmentId.length <= 16) return environmentId;
  return `${environmentId.slice(0, 9)}…${environmentId.slice(-5)}`;
}
