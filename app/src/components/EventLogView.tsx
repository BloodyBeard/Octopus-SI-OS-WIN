import { useCallback, useEffect, useMemo, useState } from "react";
import { formatProjectionError, hasProjectionBridge, readProjection } from "../projectionClient";
import { useProjectionGeneration } from "../projectionLive";
import { jsonText, shortId } from "../runtimeFormat";
import { formatBytes } from "./chat/format";
import { Glyph } from "./IconGlyphs";
import { JsonPanel } from "./JsonPanel";
import { SurfaceCopyButton } from "./SurfaceCopyButton";
import { appToast } from "../toasts";
import { TopBar } from "./TopBar";
import type { OctopusClientAccessProfile } from "../electron";
import {
  ProjectionResourceFailure,
  ProjectionResourceIssue,
  ProjectionResourceLoading,
  useProjectionResource,
} from "./projectionResource";

// Event Log = the movement ledger shown AS IT IS. One row per recorded movement:
// from -> to (each side named with its ee + run when the projection knows them), the
// artifact's self-identification (label + content address), and the movement class the
// ledger recorded. The expanded row shows the movement itself (that IS the metadata) next
// to the referenced blob. No health, no metrics, no artifact-kind axis, no fabricated
// statuses — a movement that was recorded is a fact; a movement whose class says it
// failed/was denied is shown as that.

type MovementEndpoint = {
  kind: string;
  id: string;
  ee_id?: string;
};

type MovementRow = {
  raw_run_id?: string;
  raw_ee_id?: string;
  movement_id: string;
  seq: number | null;
  lamport: number | null;
  trace: string;
  kind: string;
  recorded_at_unix_ms: number;
  artifact_id: string;
  artifact_label: string;
  content_hash: string;
  blob_path: string;
  blob_size: number | null;
  from: MovementEndpoint;
  to: MovementEndpoint;
  source_file: string;
  source_line: number;
  raw: unknown;
};

type PayloadPreview = {
  resolved: boolean;
  blob_path: string;
  size: number;
  truncated: boolean;
  text?: string;
  binary?: boolean;
  error?: string;
};

type MovementPage = {
  nextBeforeSeq: number | null;
  rows: MovementRow[];
  total: number;
};

const ALL = "All";
const MOVEMENT_PAGE_SIZE = 500;

type Props = {
  accessProfile: OctopusClientAccessProfile;
  focusedMovement?: { id: string; sequence: number | null } | null;
  onClearMovement?: () => void;
  onQueryChange: (value: string) => void;
  placeholder: string;
  query: string;
  searchFocusTick: number;
  title: string;
  titleInWindowChrome?: boolean;
};

export function EventLogView({ accessProfile, focusedMovement, onClearMovement, onQueryChange, placeholder, query, searchFocusTick, title, titleInWindowChrome = false }: Props) {
  const generation = useProjectionGeneration();
  const [interfaceFilter, setInterfaceFilter] = useState(ALL);
  const [environmentFilter, setEnvironmentFilter] = useState(ALL);
  const [typeFilter, setTypeFilter] = useState(ALL);
  const [expandedId, setExpandedId] = useState("");
  const [payloads, setPayloads] = useState<Record<string, PayloadPreview>>({});
  const [beforeSeq, setBeforeSeq] = useState<number | null>(null);

  const searchQuery = focusedMovement ? "" : query.trim();
  const projectionAvailable = hasProjectionBridge();

  useEffect(() => {
    setBeforeSeq(null);
    if (focusedMovement) { setInterfaceFilter(ALL); setEnvironmentFilter(ALL); setTypeFilter(ALL); }
  }, [searchQuery, focusedMovement]);

  const loadMovements = useCallback(async () => {
    if (focusedMovement && (focusedMovement.sequence === null || !Number.isSafeInteger(focusedMovement.sequence)
      || focusedMovement.sequence < 0 || focusedMovement.sequence >= Number.MAX_SAFE_INTEGER)) {
      throw new Error("This mark has no addressable movement sequence to open.");
    }
    const response = await readProjection<{ movements?: MovementRow[]; total?: number; next_before_seq?: number }>("projection.movements.list", {
      limit: MOVEMENT_PAGE_SIZE,
      query: searchQuery,
      ...(beforeSeq === null ? {} : { before_seq: beforeSeq }),
      ...(focusedMovement?.sequence != null ? { before_seq: focusedMovement.sequence + 1, ...(focusedMovement.sequence > 0 ? { after_seq: focusedMovement.sequence - 1 } : {}) } : {}),
    });
    if (!response || !Array.isArray(response.movements)) {
      throw new Error("The movement projection returned an invalid row set");
    }
    const target = focusedMovement ? response.movements.find((row) => row?.movement_id === focusedMovement.id && row.seq === focusedMovement.sequence) : null;
    if (focusedMovement && !target) throw new Error("The selected mark movement is unavailable.");
    const rows = target ? [target] : response.movements;
    return {
      data: {
        nextBeforeSeq: !focusedMovement && typeof response?.next_before_seq === "number" ? response.next_before_seq : null,
        rows,
        total: focusedMovement ? rows.length : typeof response?.total === "number" ? response.total : rows.length,
      },
    };
  }, [beforeSeq, searchQuery, focusedMovement]);
  const movements = useProjectionResource<MovementPage>({
    enabled: projectionAvailable,
    load: loadMovements,
    resourceKey: `${searchQuery}\0${beforeSeq ?? "latest"}\0${focusedMovement?.id ?? ""}\0${focusedMovement?.sequence ?? ""}`,
    refreshKey: generation,
  });
  const rows = movements.data?.rows ?? [];
  const total = movements.data?.total ?? 0;
  const nextBeforeSeq = movements.data?.nextBeforeSeq ?? null;
  const loaded = movements.data !== null;

  useEffect(() => {
    if (!movements.data) return;
    const retained = new Set(movements.data.rows.map(movementRowKey));
    setPayloads((current) => Object.fromEntries(Object.entries(current).filter(([key]) => retained.has(key))));
    const target = focusedMovement ? movements.data.rows.find((row) => row.movement_id === focusedMovement.id && row.seq === focusedMovement.sequence) : null;
    setExpandedId((current) => target ? movementRowKey(target) : retained.has(current) ? current : "");
  }, [movements.data, focusedMovement?.id, focusedMovement?.sequence]);

  // Resolve the expanded movement's blob lazily — one small read per expansion.
  const indexedRows = useMemo(
    () => rows.map((row) => ({ row, key: movementRowKey(row) })),
    [rows],
  );

  const expandedBlobPath = indexedRows.find((item) => item.key === expandedId)?.row.blob_path;
  const expandedPayload = payloads[expandedId];
  useEffect(() => {
    if (!expandedId) return;
    if (!expandedBlobPath || expandedPayload) return;
    let cancelled = false;
    readProjection<PayloadPreview>("projection.movements.payload", { blob_path: expandedBlobPath })
      .then((preview) => {
        if (!cancelled) setPayloads((current) => ({ ...current, [expandedId]: preview }));
      })
      .catch((failure) => {
        if (!cancelled) {
          const message = formatProjectionError(failure);
          appToast.danger(message);
          setPayloads((current) => ({
            ...current,
            [expandedId]: {
              resolved: false,
              blob_path: expandedBlobPath,
              size: 0,
              truncated: false,
              error: message,
            },
          }));
        }
      });
    return () => { cancelled = true; };
  }, [expandedId, expandedBlobPath, expandedPayload]);

  const interfaceOptions = useMemo(() => optionList(rows.flatMap((row) => [interfaceName(row.from), interfaceName(row.to)])), [rows]);
  const environmentOptions = useMemo(() => optionList(rows.map((row) => row.raw_ee_id ?? "")), [rows]);
  const typeOptions = useMemo(() => optionList(rows.map((row) => row.kind)), [rows]);

  const visibleRows = useMemo(() => {
    const needle = searchQuery.toLowerCase();
    return indexedRows.filter(({ row }) => {
      if (interfaceFilter !== ALL && interfaceName(row.from) !== interfaceFilter && interfaceName(row.to) !== interfaceFilter) return false;
      if (environmentFilter !== ALL && row.raw_ee_id !== environmentFilter) return false;
      if (typeFilter !== ALL && row.kind !== typeFilter) return false;
      if (!needle) return true;
      const haystack = [
        row.artifact_id,
        row.artifact_label,
        row.kind,
        row.trace,
        row.raw_run_id,
        row.raw_ee_id,
        row.content_hash,
        endpointText(row.from),
        endpointText(row.to),
      ].join(" ").toLowerCase();
      return haystack.includes(needle);
    });
  }, [indexedRows, searchQuery, interfaceFilter, environmentFilter, typeFilter]);

  const clearFilters = () => {
    setBeforeSeq(null);
    setInterfaceFilter(ALL);
    setEnvironmentFilter(ALL);
    setTypeFilter(ALL);
    onClearMovement?.();
    onQueryChange("");
  };

  return (
    <>
      <TopBar
        accessProfile={accessProfile}
        mode="wide"
        onQueryChange={onQueryChange}
        placeholder={placeholder}
        query={query}
        searchFocusTick={searchFocusTick}
        title={title}
        titleInWindowChrome={titleInWindowChrome}
        toolbar={(
          <>
            <div className="eventlog-filters">
              {focusedMovement ? (
                <button className="eventlog-filter" onClick={clearFilters} title="Clear movement filter and show all movements" type="button">
                  <span>Movement #{focusedMovement.sequence}</span><Glyph name="close" size={14} />
                </button>
              ) : null}
              <FilterSelect label="Interface" value={interfaceFilter} options={interfaceOptions} onChange={setInterfaceFilter} />
              <FilterSelect label="Environment" value={environmentFilter} options={environmentOptions} onChange={setEnvironmentFilter} />
              <FilterSelect label="Type" value={typeFilter} options={typeOptions} onChange={setTypeFilter} />
              {focusedMovement || searchQuery || interfaceFilter !== ALL || environmentFilter !== ALL || typeFilter !== ALL ? (
                <button className="eventlog-filter" onClick={clearFilters} type="button">Show all movements</button>
              ) : null}
            </div>
            <span className="eventlog-count">
              {movements.phase === "refreshing" ? "Refreshing movements…" : visibleRows.length === rows.length
                ? `${rows.length} / ${total} movements`
                : `${visibleRows.length} filtered / ${rows.length} loaded`}
            </span>
          </>
        )}
      />
      <section className="workspace-panel is-event-log">
        {!projectionAvailable ? (
          <section className="workspace-empty">
            <Glyph name="metrics" size={28} />
            <strong>Projection bridge unavailable</strong>
            <span>The event log reads the movement ledger through the desktop app</span>
          </section>
        ) : movements.phase === "initial-loading" && !movements.error ? (
          <ProjectionResourceLoading label="Reading the movement ledger" />
        ) : (
          <div className="eventlog-view">
            <div className="flex min-h-0 flex-col">
              {movements.error && loaded ? (
                <ProjectionResourceIssue
                  className="m-3 mb-0"
                  detail={movements.error}
                  onRetry={movements.retry}
                  retrying={movements.phase === "refreshing"}
                  title="The movement ledger could not be refreshed"
                />
              ) : null}
              <div className="eventlog-table min-h-0 flex-1" role="table" aria-label="Movement ledger">
                <div className="eventlog-head" role="row">
                  <span>Time</span>
                  <span>Artifact</span>
                  <span>From → To</span>
                  <span>Environment</span>
                  <span>Type</span>
                </div>
                <div className="eventlog-body">
                  {movements.data === null ? (
                    <ProjectionResourceFailure
                      className="min-h-40"
                      detail={movements.error}
                      onRetry={movements.retry}
                      retrying={movements.phase === "initial-loading"}
                      title="Unable to load the movement ledger"
                    />
                  ) : visibleRows.length === 0 ? (
                    <div className="eventlog-empty">
                      {rows.length === 0
                        ? "No movements are recorded on the ledger yet. The system is alive when artifacts move."
                        : "No movements match the current filters."}
                    </div>
                  ) : visibleRows.map(({ row, key }) => (
                    <MovementRowView
                      row={row}
                      expanded={expandedId === key}
                      payload={payloads[key]}
                      onToggle={() => setExpandedId((current) => (current === key ? "" : key))}
                      key={key}
                    />
                  ))}
                  {loaded && nextBeforeSeq !== null ? (
                    <button className="eventlog-load-more" onClick={() => setBeforeSeq(nextBeforeSeq)} type="button">
                      Show older movements
                    </button>
                  ) : null}
                  {loaded && beforeSeq !== null ? (
                    <button className="eventlog-load-more" onClick={() => setBeforeSeq(null)} type="button">
                      Back to latest movements
                    </button>
                  ) : null}
                </div>
              </div>
            </div>
          </div>
        )}
      </section>
    </>
  );
}

function movementRowKey(row: MovementRow): string {
  return [
    row.source_file,
    row.source_line,
    row.movement_id,
    row.seq,
    row.lamport,
    row.recorded_at_unix_ms,
    row.artifact_id,
    row.from.kind,
    row.from.id,
    row.to.kind,
    row.to.id,
  ].join(":");
}

function MovementRowView({
  row,
  expanded,
  payload,
  onToggle,
}: {
  row: MovementRow;
  expanded: boolean;
  payload?: PayloadPreview;
  onToggle: () => void;
}) {
  const failed = movementStatus(row) === "failed";
  return (
    <article className={`eventlog-row ${expanded ? "is-expanded" : ""} ${failed ? "is-failed" : ""}`}>
      <button className="eventlog-row-line" type="button" onClick={onToggle} aria-expanded={expanded}>
        <span className="eventlog-time">
          <strong>{formatClock(row.recorded_at_unix_ms)}</strong>
          <small>{formatDay(row.recorded_at_unix_ms)}</small>
        </span>
        <span className="eventlog-artifact">
          <Glyph className="eventlog-artifact-glyph" name="sites" size={15} />
          <span>
            <strong>{row.artifact_label || row.artifact_id || "movement"}</strong>
            <small>{row.blob_size !== null ? formatBytes(row.blob_size) : row.content_hash ? shortId(row.content_hash, 8, 4) : "no payload reference"}</small>
          </span>
        </span>
        <span className="eventlog-route">
          <EndpointCell endpoint={row.from} />
          <i className="eventlog-arrow">→</i>
          <EndpointCell endpoint={row.to} />
        </span>
        <span className="eventlog-environment">{row.raw_ee_id || "—"}</span>
        <span className="eventlog-kind"><em>{row.kind || "movement"}</em></span>
      </button>

      {expanded ? (
        <div className="eventlog-detail">
          <div className="eventlog-pane">
            <div className="eventlog-pane-head">
              <span>Movement</span>
            </div>
            <JsonPanel value={row.raw} preClassName="eventlog-json" copyLabel="Copy movement JSON" />
          </div>
          <div className="eventlog-pane">
            <div className="eventlog-pane-head">
              <span>File</span>
            </div>
            {row.blob_path ? (
              <SurfaceCopyButton className="eventlog-file-copy" label="Copy blob path" value={row.blob_path} />
            ) : null}
            <FilePane row={row} payload={payload} />
          </div>
        </div>
      ) : null}
    </article>
  );
}

function FilePane({ row, payload }: { row: MovementRow; payload?: PayloadPreview }) {
  if (!row.blob_path) {
    return <div className="eventlog-pane-empty">The movement carries no payload reference.</div>;
  }
  const reference = jsonText({
    artifact_id: row.artifact_id,
    content_hash: row.content_hash,
    blob_path: row.blob_path,
    size: payload?.resolved ? payload.size : row.blob_size,
  });
  if (!payload) {
    return <pre>{reference}</pre>;
  }
  if (payload.error) {
    return (
      <>
        <pre>{reference}</pre>
        <div className="eventlog-pane-empty">{payload.error}</div>
      </>
    );
  }
  if (!payload.resolved) {
    return (
      <>
        <pre>{reference}</pre>
        <div className="eventlog-pane-empty">The blob is not resolvable from this client (movement recorded elsewhere).</div>
      </>
    );
  }
  return (
    <>
      <pre>{reference}</pre>
      {payload.binary ? (
        <div className="eventlog-pane-empty">Binary payload · {formatBytes(payload.size)}</div>
      ) : (
        <pre className="eventlog-payload">{payload.text}{payload.truncated ? "\n… (truncated preview)" : ""}</pre>
      )}
    </>
  );
}

function EndpointCell({ endpoint }: { endpoint: MovementEndpoint }) {
  if (!endpoint.id) return <span className="eventlog-endpoint"><strong>unknown</strong></span>;

  return (
    <span className="eventlog-endpoint">
      <strong>{interfaceName(endpoint) || endpoint.id}</strong>
      <small>{endpoint.kind}</small>
    </span>
  );
}

function FilterSelect({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: string[];
  onChange: (value: string) => void;
}) {
  return (
    <label className="eventlog-filter">
      <span>{label}</span>
      <select value={value} onChange={(event) => onChange(event.target.value)}>
        <option value={ALL}>{ALL}</option>
        {options.map((option) => (
          <option value={option} key={option}>{option}</option>
        ))}
      </select>
    </label>
  );
}

function interfaceName(endpoint: MovementEndpoint): string {
  return endpoint.kind === "interface" ? endpoint.id : "";
}

function endpointText(endpoint: MovementEndpoint): string {
  return [endpoint.kind, endpoint.id, endpoint.ee_id ?? "", interfaceName(endpoint)].join(" ");
}

// The ledger records movements; it does not stamp verdict fields. The only honest status
// axis is the movement class itself: classes that say failure/denial are failures.
function movementStatus(row: MovementRow): "recorded" | "failed" {
  const kind = row.kind.toLowerCase();
  return kind.includes("failed") || kind.includes("denied") || kind.includes("rejected") ? "failed" : "recorded";
}

function optionList(values: string[]): string[] {
  return Array.from(new Set(values.filter(Boolean))).sort((a, b) => a.localeCompare(b));
}

function formatClock(ms: number): string {
  if (!ms) return "--:--:--";
  return new Date(ms).toLocaleTimeString([], { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function formatDay(ms: number): string {
  if (!ms) return "";
  return new Date(ms).toLocaleDateString([], { month: "short", day: "2-digit", year: "numeric" });
}
