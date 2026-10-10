import { useCallback, useEffect, useMemo, useState } from "react";
import { OcEmptyState } from "../design-system/components";
import { hasProjectionBridge, readProjection } from "../projectionClient";
import { useProjectionGeneration } from "../projectionLive";
import { ArtifactLibrary } from "./artifacts/ArtifactLibrary";
import type { ArtifactLibraryRow } from "./artifacts/types";
import type { FilesOpenTarget } from "./files/types";
import {
  ProjectionResourceFailure,
  ProjectionResourceIssue,
  ProjectionResourceLoading,
  useProjectionResource,
} from "./projectionResource";

type ArtifactsProjection = {
  artifacts?: unknown;
  limit?: unknown;
  previewable_total?: unknown;
  returned?: unknown;
  total?: unknown;
  truncated?: unknown;
  unavailable_total?: unknown;
};

type ArtifactCollection = {
  limit: number;
  previewableTotal: number;
  unavailableCount: number;
  rows: ArtifactLibraryRow[];
  total: number;
  truncated: boolean;
};

const ARTIFACT_PAGE_LIMIT = 1_000;

export function RegistryArtifactsColumns({ onOpenFiles, query }: {
  onOpenFiles: (target: FilesOpenTarget) => void;
  query: string;
}) {
  const projectionAvailable = hasProjectionBridge();
  const generation = useProjectionGeneration(true, projectionAvailable);
  const [selectedId, setSelectedId] = useState("");
  const loadArtifacts = useCallback(async () => {
    const response = await readProjection<ArtifactsProjection>("projection.artifacts.list", {
      limit: ARTIFACT_PAGE_LIMIT,
      previewable_only: true,
      source: "ledger",
    });
    return { data: parseArtifactCollection(response) };
  }, []);
  const artifacts = useProjectionResource<ArtifactCollection>({
    enabled: projectionAvailable,
    load: loadArtifacts,
    resourceKey: "ledger-artifacts",
    refreshKey: generation,
  });
  const rows = artifacts.data?.rows ?? [];
  const visibleRows = useMemo(() => filterArtifactRows(rows, query), [query, rows]);

  useEffect(() => {
    if (selectedId && !rows.some((row) => text(row.artifact_id) === selectedId)) setSelectedId("");
  }, [rows, selectedId]);

  const collection = artifacts.data;

  return (
    <section aria-busy={artifacts.phase === "refreshing"} className="rg-col rg-col-artifact-library">
      <header className="rg-col-head">
        <div className="rg-col-title">Artifacts</div>
      </header>
      <div className="rg-col-body flex min-h-0 flex-col">
        {!projectionAvailable ? (
          <OcEmptyState className="h-full" detail="Artifacts are read from the movement ledger through the desktop app." glyph="warning" title="Artifacts are unavailable" />
        ) : artifacts.phase === "initial-loading" && !artifacts.error ? (
          <ProjectionResourceLoading label="Reading artifacts" />
        ) : collection === null ? (
          <ProjectionResourceFailure
            className="h-full"
            detail={artifacts.error}
            onRetry={artifacts.retry}
            retrying={artifacts.phase === "initial-loading" || artifacts.phase === "refreshing"}
            title="Unable to load artifacts"
          />
        ) : (
          <>
            {artifacts.error ? (
              <ProjectionResourceIssue
                className="m-3 mb-0"
                detail={artifacts.error}
                onRetry={artifacts.retry}
                retrying={artifacts.phase === "refreshing"}
                title="Artifacts could not be refreshed"
              />
            ) : null}
            {rows.length === 0 ? (
              <OcEmptyState
                className="h-full"
                detail={collection.unavailableCount > 0
                  ? "Recorded artifact movements are still visible in Movements. Opening a payload requires exact admitted run and interface coordinates."
                  : "Artifacts appear here after they cross a declared interface in a run."}
                glyph="artifact"
                title={collection.unavailableCount > 0 ? "No available artifact payloads" : "No recorded artifacts"}
              />
            ) : visibleRows.length === 0 ? (
              <OcEmptyState className="h-full" detail="Try a different artifact, task, interface, or movement term." glyph="search" title="No matching artifacts" />
            ) : (
              <div className="min-h-0 flex-1">
                <ArtifactLibrary
                  onSelect={(artifactId) => {
                    const resolvedId = artifactId || selectedId;
                    if (!resolvedId) return;
                    const target = filesTargetFor(rows.find((row) => text(row.artifact_id) === resolvedId));
                    if (!target) return;
                    setSelectedId(resolvedId);
                    onOpenFiles(target);
                  }}
                  rows={visibleRows}
                  selectedId={selectedId}
                />
              </div>
            )}
          </>
        )}
      </div>
    </section>
  );
}

function parseArtifactCollection(value: unknown): ArtifactCollection {
  const response = record(value);
  if (!Array.isArray(response.artifacts)) throw new Error("The artifact projection returned an invalid row set");
  const total = unsignedInteger(response.total, "total");
  const previewableTotal = unsignedInteger(response.previewable_total, "previewable_total");
  const unavailableCount = unsignedInteger(response.unavailable_total, "unavailable_total");
  const returned = unsignedInteger(response.returned, "returned");
  const limit = positiveInteger(response.limit, "limit");
  if (typeof response.truncated !== "boolean") throw new Error("The artifact projection returned an invalid truncated fact");
  if (total !== previewableTotal + unavailableCount) throw new Error("The artifact projection returned inconsistent totals");
  if (returned !== response.artifacts.length || returned > limit || returned > previewableTotal) {
    throw new Error("The artifact projection returned inconsistent page facts");
  }
  if (response.truncated !== (previewableTotal > returned)) {
    throw new Error("The artifact projection returned an inconsistent truncation fact");
  }
  const rows: ArtifactLibraryRow[] = [];
  for (const candidate of response.artifacts) {
    const row = recordOrNull(candidate);
    if (!row) throw new Error("The artifact projection returned a malformed row");
    if (text(row.source) !== "ledger" || row.previewable !== true) {
      throw new Error("The artifact projection returned a non-previewable page row");
    }
    const artifactId = text(row.artifact_id);
    const label = text(row.label);
    const runId = text(row.run_id);
    const interfaceId = text(row.interface_id);
    const movementKind = text(row.movement_kind);
    if (!artifactId || !label || !runId || !interfaceId || !["InterfaceArrival", "InterfaceOutbound"].includes(movementKind)) {
      throw new Error("The artifact projection returned incomplete admitted coordinates");
    }
    rows.push(row);
  }
  return { limit, previewableTotal, rows, total, truncated: response.truncated, unavailableCount };
}

function filterArtifactRows(rows: ArtifactLibraryRow[], query: string): ArtifactLibraryRow[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return rows;
  return rows.filter((row) => [
    row.artifact_id,
    row.label,
    row.run_id,
    row.interface_id,
    row.ee_id,
    row.worker,
    row.movement_kind,
  ].some((value) => text(value).toLocaleLowerCase().includes(needle)));
}

function filesTargetFor(row: ArtifactLibraryRow | undefined): FilesOpenTarget | null {
  if (!row) return null;
  const artifactId = text(row.artifact_id);
  const label = text(row.label);
  const runId = text(row.run_id);
  const interfaceId = text(row.interface_id);
  if (!artifactId || !label || !runId || !interfaceId) return null;
  return {
    kind: "artifact",
    artifactId,
    createdAtMs: timestamp(row.created_at),
    interfaceId,
    label,
    runId,
  };
}

function record(value: unknown): Record<string, unknown> {
  return recordOrNull(value) ?? {};
}

function recordOrNull(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function timestamp(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const parsed = new Date(value).valueOf();
    if (Number.isFinite(parsed)) return parsed;
  }
  return 0;
}

function unsignedInteger(value: unknown, field: string): number {
  if (!Number.isSafeInteger(value) || Number(value) < 0) throw new Error(`The artifact projection returned an invalid ${field} fact`);
  return Number(value);
}

function positiveInteger(value: unknown, field: string): number {
  const parsed = unsignedInteger(value, field);
  if (parsed < 1) throw new Error(`The artifact projection returned an invalid ${field} fact`);
  return parsed;
}
