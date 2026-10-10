export type ArtifactReference = {
  artifactId: string;
  label: string;
  runId?: string;
  interfaceId?: string;
  createdAtMs?: number;
  source?: "ledger" | "registry" | "message";
  manifest?: unknown;
  inlinePayload?: unknown;
};

export type ResolvedArtifactPayload = {
  artifactId: string;
  createdAtMs: number;
  interfaceId: string;
  label: string;
  movementKind: string;
  payload: unknown;
  runId: string;
  sequence: number;
};

export type ArtifactRendererKind =
  | "code"
  | "document"
  | "image"
  | "json"
  | "media"
  | "notebook"
  | "pdf"
  | "text"
  | "unsupported";

export type ArtifactPreviewModel = {
  bytes?: Uint8Array;
  dataUrl?: string;
  filename: string;
  kind: ArtifactRendererKind;
  language: string;
  mediaType: string;
  payload: unknown;
  searchableText: string;
  title: string;
};

export type ArtifactContextPreparedDetail = {
  schema: "octopus.client.artifact_context";
  schemaVersion: 1;
  contextRef: string;
  createdAtMs: number;
  artifact: {
    artifactId: string;
    interfaceId: string;
    label: string;
    renderer: ArtifactRendererKind;
    runId: string;
  };
  selection?: {
    kind: "text";
    text: string;
  };
  prompt: string;
};

export type ArtifactLibraryRow = {
  artifact_id?: unknown;
  created_at?: unknown;
  ee_id?: unknown;
  interface_id?: unknown;
  label?: unknown;
  movement_kind?: unknown;
  raw?: unknown;
  route_label?: unknown;
  run_id?: unknown;
  source?: unknown;
  status?: unknown;
  [key: string]: unknown;
};
