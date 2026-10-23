import type { ClientSurfaceDocumentRegistryState } from "./useClientSurfaceDocuments";
import type { AdmittedClientSurfaceDocument, ClientSurfaceDocumentSnapshot, ClientSurfaceDocumentSource } from "./types";
import { admitClientSurfaceSource } from "./validation";

export function materializeRegistry(
  snapshot: ClientSurfaceDocumentSnapshot,
  readError: string,
  loading: boolean,
  accessProfile: "live" | "lab",
): Omit<ClientSurfaceDocumentRegistryState, "hydrated"> {
  const errors = [...snapshot.errors, ...(readError ? [readError] : [])];
  const candidates = snapshot.documents.map((source, index) => ({ source, index, result: admitClientSurfaceSource(source) }));
  const verdicts = candidates.map(({ result }) => result.verdict);
  const groups = new Map<string, typeof candidates>();
  const activeRefusals = new Set<number>();
  const blockedScopes = new Set<string>();
  const unreadableThrough = new Map<string, number>();
  let unscopedDeliveryError = snapshot.errors.length > 0;
  for (const candidate of candidates) {
    const { source, result } = candidate;
    const scope = sourceScope(source);
    if (result.verdict.details.issues.some((issue) => issue.code === "document.delivery.invalid")) {
      activeRefusals.add(candidate.index);
      if (source.runId && source.interfaceId) blockedScopes.add(scope);
      else unscopedDeliveryError = true;
    }
    const id = result.verdict.details.document_id;
    if (!id) {
      activeRefusals.add(candidate.index);
      if (source.artifactId && Number.isSafeInteger(source.sequence) && source.sequence! >= 0) {
        unreadableThrough.set(scope, Math.max(unreadableThrough.get(scope) ?? -1, source.sequence!));
      } else blockedScopes.add(scope);
      continue;
    }
    const target = result.admitted?.document.target;
    const identity = !source.artifactId && target?.kind === "worker_environment.surface"
      ? [target.worker_environment_id, target.surface_id] : [id];
    const key = JSON.stringify([scope, ...identity]);
    const group = groups.get(key) ?? [];
    group.push(candidate);
    groups.set(key, group);
  }

  const selected: AdmittedClientSurfaceDocument[] = [];
  for (const [key, group] of groups) {
    if (blockedScopes.has(sourceScope(group[0].source))) continue;
    const revision = group.reduce((latest, { result }) => Math.max(latest, result.verdict.details.revision ?? -1), -1);
    const current = group.filter(({ result }) => result.verdict.details.revision === revision);
    const latestSequence = current.reduce((latest, { source }) => Math.max(latest, source.sequence ?? -1), -1);
    const unreadable = unreadableThrough.get(sourceScope(group[0].source));
    if (unreadable !== undefined && latestSequence <= unreadable) continue;
    const unknownRevision = group.filter(({ source, result }) => result.verdict.details.revision === null
      && (source.sequence === undefined || source.sequence >= latestSequence));
    if (unknownRevision.length || !current.length || current.some(({ result }) => !result.admitted)) {
      for (const candidate of [...unknownRevision, ...current]) {
        if (!candidate.result.admitted) activeRefusals.add(candidate.index);
      }
      errors.push(`client surface ${key} has an invalid current revision; display is unavailable`);
      continue;
    }
    if (new Set(current.map(({ source }) => source.digest)).size !== 1) {
      const message = `client surface ${key} revision ${revision} conflicts across ${current.map(({ source }) => source.path).join(", ")}`;
      errors.push(message);
      for (const candidate of current) {
        verdicts[candidate.index] = {
          passed: false,
          details: { ...candidate.result.verdict.details, issues: [{ code: "registry.revision.conflict", path: "/id", message }] },
        };
      }
      continue;
    }
    const earliest = [...current].sort((left, right) => (left.source.sequence ?? 0) - (right.source.sequence ?? 0))[0];
    selected.push(earliest.result.admitted!);
  }

  for (const { source, index, result } of candidates) {
    if (!activeRefusals.has(index)) continue;
    const issue = result.verdict.details.issues[0];
    errors.push(`${source.path}: ${issue?.path || "/"}: ${issue?.message || "surface document was refused"}`);
  }

  const documents = (readError ? [] : selected)
    .filter((item) => !unscopedDeliveryError || !item.sourceArtifactId)
    .filter((item) => accessProfile === "lab" || item.document.release_channel === "stable")
    .sort((left, right) => left.document.target.order - right.document.target.order || left.document.title.localeCompare(right.document.title));
  return {
    generation: snapshot.generation,
    root: snapshot.root,
    documents,
    voiceSurfaces: documents.filter((item) => item.document.target.kind === "voice.widget" || item.document.target.kind === "voice.window"),
    voiceWidgets: documents.filter((item) => item.document.target.kind === "voice.widget"),
    voiceWindows: documents.filter((item) => item.document.target.kind === "voice.window"),
    workspaceSurfaces: documents.filter((item) => item.document.target.kind === "workspace.surface"),
    workerEnvironmentSurfaces: documents.filter((item) => item.document.target.kind === "worker_environment.surface"),
    verdicts,
    errors,
    loading,
  };
}

export function uniqueExtensionSurfaces(
  surfaces: AdmittedClientSurfaceDocument[],
  scope: string,
  errors: string[],
  keepEachDelivery = false,
): AdmittedClientSurfaceDocument[] {
  const byId = new Map<string, AdmittedClientSurfaceDocument[]>();
  const delivered: AdmittedClientSurfaceDocument[] = [];
  for (const surface of surfaces) {
    if (keepEachDelivery && surface.sourceArtifactId) {
      delivered.push(surface);
      continue;
    }
    const rows = byId.get(surface.document.id) ?? [];
    rows.push(surface);
    byId.set(surface.document.id, rows);
  }
  return [...delivered, ...[...byId.entries()].flatMap(([id, rows]) => {
    const digests = new Set(rows.map((row) => row.sourceDigest));
    if (digests.size === 1) return [rows[0]];
    errors.push(`${scope} client surface "${id}" conflicts across ${rows.map((row) => row.sourcePath).join(", ")}`);
    return [];
  })];
}

function sourceScope(source: ClientSurfaceDocumentSource): string {
  return source.artifactId || source.path.startsWith("artifact:")
    ? JSON.stringify(["artifact", source.runId ?? "", source.interfaceId ?? ""])
    : "local";
}
