import type { WorkerEnvironmentSurfaceProjection } from "../../data/topology";

export function surfaceHasProjectedChanges(surface?: WorkerEnvironmentSurfaceProjection): boolean {
  if (!surface) return false;
  return objectList(surface.data.diffs ?? surface.data.changes).some((entry) => {
    const path = stringValue(entry.path ?? entry.file ?? entry.name);
    const before = typeof entry.before === "string" ? entry.before : typeof entry.old === "string" ? entry.old : "";
    const after = typeof entry.after === "string" ? entry.after : typeof entry.new === "string" ? entry.new : "";
    return Boolean(path) && (before !== after || entry.binary === true);
  });
}

function objectList(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((entry): entry is Record<string, unknown> => Boolean(entry) && typeof entry === "object" && !Array.isArray(entry))
    : [];
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
