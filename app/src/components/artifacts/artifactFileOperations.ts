import { resolveArtifactPayload } from "./artifactResolver";
import { createArtifactPreviewModel, previewDownload } from "./rendererRegistry";
import type { ArtifactPreviewModel, ArtifactReference } from "./types";

export type ArtifactFile = {
  bytes: Uint8Array;
  filename: string;
  mediaType: string;
};

export type ResolvedArtifactFilePreview = {
  file: ArtifactFile;
  model: ArtifactPreviewModel;
};

export async function resolveArtifactFilePreview(reference: ArtifactReference): Promise<ResolvedArtifactFilePreview> {
  const resolved = await resolveArtifactPayload(reference);
  const model = createArtifactPreviewModel({
    ...reference,
    createdAtMs: resolved.createdAtMs || reference.createdAtMs,
    interfaceId: resolved.interfaceId || reference.interfaceId,
    runId: resolved.runId || reference.runId,
  }, resolved.payload);
  const file = previewDownload(model);
  if (!file) throw new Error("This artifact does not expose admitted file bytes.");
  return { file, model };
}

export async function resolveArtifactFile(reference: ArtifactReference): Promise<ArtifactFile> {
  return (await resolveArtifactFilePreview(reference)).file;
}

export async function openArtifactFile(file: ArtifactFile, action: "open" | "reveal") {
  const transport = window.octopusTransport;
  if (!transport?.openClientFile) throw new Error("Native file opening is unavailable.");
  await transport.openClientFile({
    action,
    bytes: file.bytes,
    filename: safeArtifactFilename(file.filename),
  });
}

export async function listArtifactFileOpenTargets(file: ArtifactFile) {
  const transport = window.octopusTransport;
  if (!transport?.listClientFileOpenTargets) throw new Error("Native application discovery is unavailable.");
  return transport.listClientFileOpenTargets({
    bytes: file.bytes,
    filename: safeArtifactFilename(file.filename),
  });
}

export async function openArtifactFileWithApplication(file: ArtifactFile, applicationId: string) {
  const transport = window.octopusTransport;
  if (!transport?.openClientFileWithApplication) throw new Error("Native application opening is unavailable.");
  await transport.openClientFileWithApplication({
    applicationId,
    bytes: file.bytes,
    filename: safeArtifactFilename(file.filename),
  });
}

export async function downloadArtifactFileCopy(file: ArtifactFile) {
  const transport = window.octopusTransport;
  if (!transport?.downloadClientFileCopy) throw new Error("The Downloads folder is unavailable.");
  return transport.downloadClientFileCopy({
    bytes: file.bytes,
    filename: safeArtifactFilename(file.filename),
  });
}

export function startArtifactFileDrag(file: ArtifactFile) {
  const transport = window.octopusTransport;
  if (!transport?.startClientFileDrag) return false;
  transport.startClientFileDrag({
    bytes: file.bytes,
    filename: safeArtifactFilename(file.filename),
  });
  return true;
}

export function safeArtifactFilename(value: string): string {
  return value.replaceAll(/[\\/:*?"<>|]/g, "-").trim() || "artifact";
}
