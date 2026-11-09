import type { ArtifactReference } from "../artifacts/types";
import type { FilesOpenTarget } from "./types";

type ArtifactFilesTarget = Extract<FilesOpenTarget, { kind: "artifact" }>;
const previewTextLimit = 2 * 1024 * 1024;

export function normalizeArtifactFilesTarget(value: unknown): ArtifactFilesTarget | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (row.kind !== "artifact" || Object.keys(row).some((key) => !["kind", "artifactId", "interfaceId", "label", "runId", "createdAtMs", "previewText"].includes(key))) return null;
  if (!token(row.artifactId, 1024) || !token(row.label, 1024) || !token(row.runId, 512)) return null;
  if (typeof row.interfaceId !== "string" || row.interfaceId.length > 512 || /[\u0000-\u001f\u007f]/.test(row.interfaceId)) return null;
  if (row.createdAtMs !== undefined && (!Number.isSafeInteger(row.createdAtMs) || (row.createdAtMs as number) < 0)) return null;
  if (row.previewText !== undefined && (typeof row.previewText !== "string" || new TextEncoder().encode(row.previewText).byteLength > previewTextLimit)) return null;
  return { kind: "artifact", artifactId: row.artifactId, interfaceId: row.interfaceId, label: row.label, runId: row.runId,
    ...(typeof row.createdAtMs === "number" ? { createdAtMs: row.createdAtMs } : {}),
    ...(typeof row.previewText === "string" ? { previewText: row.previewText } : {}) };
}

export function artifactFilesTarget(reference: ArtifactReference): ArtifactFilesTarget {
  if (reference.source === "registry" && reference.inlinePayload === undefined) throw new Error("This registry descriptor has no admitted payload movement to preview.");
  const previewText = reference.inlinePayload === undefined ? undefined
    : typeof reference.inlinePayload === "string" ? reference.inlinePayload : JSON.stringify(reference.inlinePayload, null, 2);
  const target = normalizeArtifactFilesTarget({ kind: "artifact", artifactId: reference.artifactId,
    interfaceId: reference.interfaceId ?? "", label: reference.label, runId: reference.runId ?? "",
    ...(reference.createdAtMs === undefined ? {} : { createdAtMs: reference.createdAtMs }),
    ...(previewText === undefined ? {} : { previewText }) });
  if (!target) throw new Error("This artifact needs valid run coordinates and a preview of at most 2 MiB to open in Files.");
  return target;
}

function token(value: unknown, maximum: number): value is string {
  return typeof value === "string" && value.trim().length > 0 && value.length <= maximum && !/[\u0000-\u001f\u007f]/.test(value);
}
