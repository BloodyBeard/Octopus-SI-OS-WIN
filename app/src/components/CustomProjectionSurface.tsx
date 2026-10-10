import { useCallback, useMemo } from "react";
import { Glyph } from "./IconGlyphs";
import { JsonPanel } from "./JsonPanel";
import { hasProjectionBridge, readProjection } from "../projectionClient";
import { array, formatCell, jsonText, readPath, stringValue } from "../runtimeFormat";
import type { ClientSurfaceDescriptor, SurfaceProjectionColumn } from "../shell/surfaceTypes";
import { useAppErrorToast } from "../toasts";
import { useFixedVirtualRange } from "./useFixedVirtualRange";
import { useProjectionGeneration } from "../projectionLive";
import { ProjectionResourceFailure, ProjectionResourceIssue, ProjectionResourceLoading, useProjectionResource } from "./projectionResource";

type Props = {
  surface: ClientSurfaceDescriptor;
  query: string;
};

export function CustomProjectionSurface({ surface, query }: Props) {
  const projection = surface.projection;
  const generation = useProjectionGeneration();
  const load = useCallback(async () => {
    if (!projection) throw new Error("The surface has no projection descriptor.");
    return { data: { payload: await readProjection(projection.method, projection.params) } };
  }, [projection]);
  const state = useProjectionResource({
    enabled: Boolean(projection) && hasProjectionBridge(),
    load,
    resourceKey: JSON.stringify([surface.id, projection?.method, projection?.params]),
    refreshKey: generation,
  });
  useAppErrorToast(state.error);
  const payload = state.data?.payload;
  const projected = useMemo(() => projection?.path ? readPath(payload, projection.path) : payload, [projection?.path, payload]);
  const rows = useMemo(() => array(projected), [projected]);
  const columns = useMemo(() => projection?.columns ?? inferColumns(rows), [projection?.columns, rows]);
  const filteredRows = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return rows;
    return rows.filter((row) => jsonText(row).toLowerCase().includes(needle));
  }, [query, rows]);
  const virtualRows = useFixedVirtualRange({ count: filteredRows.length, itemSize: 29 });

  if (!projection) {
    return <SurfaceEmpty glyph="descriptor" title="Surface descriptor incomplete" detail="custom.projection requires projection.method" />;
  }

  if (!hasProjectionBridge()) {
    return <SurfaceEmpty glyph={surface.glyph} title="Projection bridge unavailable" detail="Open this surface through the desktop app" />;
  }

  if (state.phase === "initial-loading") {
    return <ProjectionResourceLoading label="Reading projection" />;
  }

  if (state.phase === "failed") {
    return <ProjectionResourceFailure detail={state.error} onRetry={state.retry} retrying={false} title="Projection unavailable" />;
  }

  return (
    <section className="custom-surface">
      {state.error ? <ProjectionResourceIssue detail={state.error} onRetry={state.retry} retrying={state.phase === "refreshing"} title="Could not refresh this projection" /> : null}
      <header className="custom-surface-head">
        <div>
          <span>{projection.title || surface.label}</span>
          <strong>{projection.method}</strong>
        </div>
      </header>

      {rows.length > 0 ? (
        <div className="custom-table-wrap" onScroll={virtualRows.onScroll} ref={virtualRows.setScrollRef}>
          <table aria-rowcount={filteredRows.length + 1} className="custom-table">
            <thead>
              <tr>
                {columns.map((column) => <th key={column.key}>{column.label}</th>)}
              </tr>
            </thead>
            <tbody>
              {virtualRows.start > 0 ? <tr aria-hidden="true" className="custom-table-spacer"><td colSpan={columns.length} style={{ height: virtualRows.start * 29 }} /></tr> : null}
              {filteredRows.slice(virtualRows.start, virtualRows.end).map((row, offset) => {
                const index = virtualRows.start + offset;
                return (
                <tr aria-rowindex={index + 2} key={rowKey(row, index)} style={{ height: 29 }}>
                  {columns.map((column) => (
                    <td className={column.mono ? "is-mono" : ""} key={column.key}>
                      {formatCell(readPath(row, column.key))}
                    </td>
                  ))}
                </tr>
                );
              })}
              {virtualRows.end < filteredRows.length ? <tr aria-hidden="true" className="custom-table-spacer"><td colSpan={columns.length} style={{ height: (filteredRows.length - virtualRows.end) * 29 }} /></tr> : null}
            </tbody>
          </table>
          {filteredRows.length === 0 ? <div className="panel-empty table-empty">no projection rows match search</div> : null}
        </div>
      ) : (
        <JsonPanel value={projected} preClassName="registry-json custom-json" copyLabel="Copy projection payload" />
      )}
    </section>
  );
}

export function StaticSurface({ surface }: { surface: ClientSurfaceDescriptor }) {
  return (
    <section className="workspace-empty">
      <Glyph name={surface.glyph} size={28} />
      <strong>{surface.staticContent?.title || surface.label}</strong>
      <span>{surface.staticContent?.body || "Static client surface descriptor is registered."}</span>
    </section>
  );
}

function SurfaceEmpty({ glyph, title, detail }: { glyph: ClientSurfaceDescriptor["glyph"]; title: string; detail: string }) {
  return (
    <section className="workspace-empty">
      <Glyph name={glyph} size={28} />
      <strong>{title}</strong>
      <span>{detail}</span>
    </section>
  );
}

function inferColumns(rows: any[]): SurfaceProjectionColumn[] {
  const first = rows.find((row) => row && typeof row === "object" && !Array.isArray(row));
  if (!first) return [{ key: "value", label: "value" }];
  return Object.keys(first).slice(0, 6).map((key) => ({
    key,
    label: key,
    mono: key.endsWith("_id") || key === "id" || key.includes("path"),
  }));
}

function rowKey(row: any, index: number): string {
  return stringValue(row?.id ?? row?.artifact_id ?? row?.run_id ?? row?.worker_environment_id) || `row_${index}`;
}
