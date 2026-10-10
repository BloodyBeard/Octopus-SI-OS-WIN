import type { ArtifactContextPreparedDetail, ArtifactPreviewModel, ArtifactReference } from "./types";

export const ARTIFACT_CONTEXT_PREPARED_EVENT = "octopus:artifact-context-prepared";

export function prepareArtifactContext(input: {
  model: ArtifactPreviewModel;
  reference: ArtifactReference;
  selectionText?: string;
}): ArtifactContextPreparedDetail {
  const selectionText = input.selectionText?.trim() || "";
  const promptParts = [
    `Review artifact ${input.reference.label || input.reference.artifactId}.`,
    selectionText ? `Selected content:\n${selectionText}` : "",
  ].filter(Boolean);
  const detail: ArtifactContextPreparedDetail = {
    artifact: {
      artifactId: input.reference.artifactId,
      interfaceId: input.reference.interfaceId ?? "",
      label: input.reference.label,
      renderer: input.model.kind,
      runId: input.reference.runId ?? "",
    },
    contextRef: `artifact-context:${input.reference.artifactId}:${crypto.randomUUID()}`,
    createdAtMs: Date.now(),
    prompt: promptParts.join("\n\n"),
    schema: "octopus.client.artifact_context",
    schemaVersion: 1,
    ...(selectionText ? { selection: { kind: "text" as const, text: selectionText } } : {}),
  };
  window.dispatchEvent(new CustomEvent<ArtifactContextPreparedDetail>(ARTIFACT_CONTEXT_PREPARED_EVENT, { detail }));
  return detail;
}
