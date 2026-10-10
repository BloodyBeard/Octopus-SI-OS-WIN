import type { ArtifactReference } from "../artifacts/types";
import type { GlyphName } from "../IconGlyphs";
import type { ChatRenderMessage } from "./types";

export const OPEN_TASK_SOURCES_PANE_EVENT = "octopus:open-task-sources-pane";

export type TaskSourceRow = {
  activity: string;
  details?: string[];
  glyph: GlyphName;
  id: string;
  label: string;
  meta: string;
  reference?: ArtifactReference;
};

export type TaskSourcesPaneTarget = {
  schemaVersion: 1;
  runId: string;
  sources: TaskSourceRow[];
};

export function dispatchOpenTaskSourcesPane(runId: string, sources: TaskSourceRow[]): void {
  window.dispatchEvent(new CustomEvent(OPEN_TASK_SOURCES_PANE_EVENT, {
    detail: {
      runId,
      schemaVersion: 1,
      sources,
    } satisfies TaskSourcesPaneTarget,
  }));
}

export function collectTaskSources(messages: ChatRenderMessage[]): TaskSourceRow[] {
  const attachments: TaskSourceRow[] = [];
  const artifacts: TaskSourceRow[] = [];
  const seen = new Set<string>();
  const webQueries: string[] = [];
  for (const message of messages) {
    if (message.eventKind === "web_search") {
      const value = message.text.trim() || stringValue(message.eventData?.query) || stringValue(message.eventData?.action);
      if (value && !webQueries.includes(value)) webQueries.push(value);
    }
    for (const attachment of message.attachments ?? []) {
      const id = `attachment:${attachment.id || attachment.name}`;
      if (seen.has(id)) continue;
      seen.add(id);
      attachments.push({
        activity: "Attached to the conversation",
        id,
        glyph: attachment.type.startsWith("image/") ? "image" : "file",
        label: attachment.name,
        meta: attachment.label || attachment.type,
      });
    }
    for (const reference of eventArtifactReferences(message)) {
      const id = `artifact:${reference.artifactId}`;
      if (seen.has(id)) continue;
      seen.add(id);
      artifacts.push({
        activity: "Read during the chat",
        glyph: "artifact",
        id,
        label: reference.label,
        meta: "Artifact",
        reference,
      });
    }
  }
  const web = webQueries.length ? [{
    activity: webQueries.length === 1 ? "Searched once" : `Searched ${webQueries.length} times`,
    details: webQueries,
    glyph: "search" as const,
    id: "web-search",
    label: "Web search",
    meta: "",
  }] : [];
  return [...attachments, ...artifacts, ...web];
}

function eventArtifactReferences(message: ChatRenderMessage): ArtifactReference[] {
  const data = record(message.eventData);
  const values: unknown[] = [
    data.source_artifact_id,
    data.input_artifact_id,
    data.context_artifact_id,
    ...arrayValue(data.source_artifact_ids),
    ...arrayValue(data.input_artifact_ids),
    ...arrayValue(data.source_artifacts),
  ];
  const references = new Map<string, ArtifactReference>();
  for (const value of values) {
    const row = record(value);
    const artifactId = typeof value === "string"
      ? value.trim()
      : stringValue(row.artifact_id) || stringValue(row.id);
    if (!artifactId || artifactId === message.artifactId || references.has(artifactId)) continue;
    const label = artifactLabel(artifactId) || stringValue(row.label) || "Artifact source";
    references.set(artifactId, {
      artifactId,
      createdAtMs: numberValue(row.created_at_ms) || message.createdAtMs,
      interfaceId: stringValue(row.interface_id),
      label,
      manifest: Object.keys(row).length ? row : undefined,
      runId: stringValue(row.run_id) || message.runId,
      source: "ledger",
    });
  }
  return [...references.values()];
}

function artifactLabel(artifactId: string): string {
  const separator = artifactId.indexOf(":");
  return separator > 0 ? artifactId.slice(0, separator) : "";
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function arrayValue(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function numberValue(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
