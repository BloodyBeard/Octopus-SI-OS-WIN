import { readProjection } from "../../projectionClient";
import type { ArtifactReference, ResolvedArtifactPayload } from "./types";

type InterfaceArtifactRow = {
  artifact_id?: unknown;
  created_at?: unknown;
  error?: unknown;
  interface_id?: unknown;
  label?: unknown;
  movement_kind?: unknown;
  payload?: unknown;
  run_id?: unknown;
  seq?: unknown;
};

type ArtifactBytesResponse = {
  artifact_id?: unknown;
  base64?: unknown;
  content_hash?: unknown;
  filename?: unknown;
  interface_id?: unknown;
  mime_type?: unknown;
  run_id?: unknown;
  size?: unknown;
};

export async function resolveArtifactPayload(reference: ArtifactReference): Promise<ResolvedArtifactPayload> {
  if (reference.inlinePayload !== undefined) {
    return {
      artifactId: reference.artifactId,
      createdAtMs: reference.createdAtMs ?? 0,
      interfaceId: reference.interfaceId ?? "",
      label: reference.label,
      movementKind: "",
      payload: reference.inlinePayload,
      runId: reference.runId ?? "",
      sequence: -1,
    };
  }
  if (reference.source === "registry") {
    throw new Error("This registry descriptor has no admitted payload movement to preview.");
  }
  const response = await readProjection<{ artifacts?: InterfaceArtifactRow[] }>(
    "projection.interface_artifacts.list",
    {
      labels: reference.label ? [reference.label] : [],
      interface_id: reference.interfaceId ?? "",
      limit: 5000,
      run_id: reference.runId ?? "",
    },
  );
  const row = (response.artifacts ?? []).find((candidate) => text(candidate.artifact_id) === reference.artifactId);
  if (!row) {
    throw new Error("The admitted artifact is not materialized by the safe payload resolver for this client.");
  }
  const failure = text(row.error);
  if (failure) throw new Error(failure);
  const payload = await withAdmittedBytes(reference.artifactId, text(row.run_id), text(row.interface_id), row.payload);
  return {
    artifactId: text(row.artifact_id),
    createdAtMs: timestamp(row.created_at),
    interfaceId: text(row.interface_id),
    label: text(row.label),
    movementKind: text(row.movement_kind),
    payload,
    runId: text(row.run_id),
    sequence: number(row.seq, -1),
  };
}

async function withAdmittedBytes(artifactId: string, runId: string, interfaceId: string, payload: unknown): Promise<unknown> {
  if (!runId || !interfaceId) throw new Error("The admitted artifact has no complete run/interface coordinates.");
  const bytes = await readProjection<ArtifactBytesResponse>("projection.artifact.bytes.read", {
    artifact_id: artifactId,
    interface_id: interfaceId,
    run_id: runId,
  });
  const base64 = text(bytes.base64);
  const filename = text(bytes.filename);
  const mimeType = text(bytes.mime_type);
  const contentHash = text(bytes.content_hash);
  const size = number(bytes.size, -1);
  if (!base64 || !filename || size < 0) throw new Error("The admitted artifact byte projection is incomplete.");
  const record = object(payload);
  const media = object(record.media);
  return {
    ...record,
    ...(typeof payload === "string" ? { __raw: payload } : {}),
    media: {
      ...media,
      base64,
      content_hash: contentHash,
      mime_type: mimeType,
      name: filename,
      size,
    },
  };
}

function timestamp(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const parsed = new Date(value).valueOf();
    if (Number.isFinite(parsed)) return parsed;
  }
  return 0;
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function number(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function object(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
