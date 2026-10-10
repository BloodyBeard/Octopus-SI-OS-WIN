import type { TopologySnapshot, WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../data/topology";
import type { WorkerEnvironmentPaneTarget } from "../components/worker-environment/workerEnvironmentPane";

export function terminalLaunchCwd(snapshot: TopologySnapshot, runId: string, target?: WorkerEnvironmentPaneTarget): string | undefined {
  if (!runId && !target) return undefined;
  if (snapshot.projection.mode !== "live") throw new Error("The task filesystem projection is not ready. Open its Files surface before opening a terminal.");
  const rooms = new Map<string, WorkerEnvironmentProjection>();
  for (const environment of snapshot.environments) {
    for (const run of environment.runs) {
      if (!target && run.id !== runId) continue;
      for (const room of run.workerEnvironments) {
        if (!target || room.id === target.workerEnvironmentId) rooms.set(room.id, room);
      }
    }
    for (const room of environment.workerEnvironments) {
      if (target ? room.id === target.workerEnvironmentId : room.runId === runId) rooms.set(room.id, room);
    }
  }
  if (target?.surfaceId) {
    const selected = rooms.get(target.workerEnvironmentId)?.surfaces.find((surface) => surface.id === target.surfaceId);
    if (!selected) throw new Error("The selected worker environment surface is no longer projected.");
    if (directFilesystemSurface(selected)) return selected.root;
  }
  const roots = new Set([...rooms.values()].flatMap((room) => room.surfaces.filter(directFilesystemSurface).map((surface) => surface.root)));
  if (roots.size === 0) throw new Error("The selected task has no projected filesystem root for a local terminal.");
  if (roots.size > 1) throw new Error("The selected task has multiple filesystem roots. Open the Files surface for the intended root before opening a terminal.");
  return roots.values().next().value;
}

function directFilesystemSurface(surface: WorkerEnvironmentSurfaceProjection): boolean {
  return surface.mode === "direct"
    && Boolean(surface.root)
    && (surface.protocol.toLowerCase() === "filesystem.readonly" && surface.readOnly
      || surface.protocol.toLowerCase() === "filesystem.readwrite" && !surface.readOnly);
}
