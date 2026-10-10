import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  compactMetricCount,
  computeActivityMetrics,
  formatMetricBytes,
  type ActivityMetrics,
  type MovementRow,
} from "./activityMetrics";
import { Glyph, type GlyphName } from "./IconGlyphs";
import { OcDropdown } from "../design-system/OcDropdown";
import { formatProjectionError, hasProjectionBridge, readProjection } from "../projectionClient";
import type { ProjectionReadMethod } from "../generated/projectionReadContract";
import { useProjectionGeneration } from "../projectionLive";
import { array, countBy, formatCell, formatTime, jsonText, readPath, shortId, stringValue, timeOf } from "../runtimeFormat";
import {
  ProjectionResourceFailure,
  ProjectionResourceIssue,
  ProjectionResourceLoading,
  useProjectionResource,
} from "./projectionResource";

type MetricSource = {
  id: string;
  method: ProjectionReadMethod;
  params: Record<string, unknown>;
  path: string;
};

type MetricsResourceData = {
  payloads: Record<string, unknown>;
  errors: Record<string, string>;
};

type MetricsState = MetricsResourceData & {
  loading: boolean;
};

type MetricPeriod = "1h" | "6h" | "24h" | "7d" | "30d" | "90d" | "custom";

type MetricWindow = {
  start: number;
  end: number;
  buckets: number;
  label: string;
  bucketLabel: string;
};

type MetricEvent = ReturnType<typeof eventRow>;

type MetricTrend = {
  direction: "up" | "down" | "flat";
  label: string;
};

type InterfaceMetric = {
  name: string;
  count: number;
  series: number[];
};

const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

const PERIODS: Record<Exclude<MetricPeriod, "custom">, { duration: number; buckets: number; bucketLabel: string }> = {
  "1h": { duration: HOUR_MS, buckets: 30, bucketLabel: "2 min" },
  "6h": { duration: 6 * HOUR_MS, buckets: 36, bucketLabel: "10 min" },
  "24h": { duration: DAY_MS, buckets: 48, bucketLabel: "30 min" },
  "7d": { duration: 7 * DAY_MS, buckets: 42, bucketLabel: "4 hour" },
  "30d": { duration: 30 * DAY_MS, buckets: 30, bucketLabel: "Daily" },
  "90d": { duration: 90 * DAY_MS, buckets: 90, bucketLabel: "Daily" },
};

const SOURCES: MetricSource[] = [
  { id: "runs", method: "projection.runs.list", params: { include_ledger: false, limit: 500 }, path: "runs" },
  { id: "workerEnvironments", method: "projection.worker_environments.list", params: { all_scopes: true, include_archived: false, limit: 500 }, path: "worker_environments" },
  { id: "interfaces", method: "projection.interfaces.list", params: {}, path: "interfaces" },
  { id: "artifacts", method: "projection.descriptors.artifact_labels.list", params: { limit: 500 }, path: "descriptors" },
  { id: "movements", method: "projection.movements.list", params: {}, path: "movements" },
];

export function MetricsView({ query }: { query: string }) {
  const [period, setPeriod] = useState<MetricPeriod>("30d");
  const [customOpen, setCustomOpen] = useState(false);
  const [customRange, setCustomRange] = useState(() => ({
    start: toDateInput(Date.now() - 30 * DAY_MS),
    end: toDateInput(Date.now()),
  }));
  const [appliedCustomRange, setAppliedCustomRange] = useState(customRange);
  const customAnchorRef = useRef<HTMLButtonElement>(null);
  const generation = useProjectionGeneration();
  const projectionAvailable = hasProjectionBridge();
  const metricWindow = useMemo(
    () => resolveMetricWindow(period, appliedCustomRange),
    [appliedCustomRange, generation, period],
  );
  const metricSources = useMemo(() => {
    const previous = previousMetricWindow(metricWindow);
    return SOURCES.map((source) => source.id === "movements" ? {
      ...source,
      params: {
        limit: 5_000,
        recorded_after_unix_ms: previous.start,
        recorded_before_unix_ms: metricWindow.end,
      },
    } : source);
  }, [metricWindow]);

  const loadMetrics = useCallback(async (lastGood: MetricsResourceData | null) => {
    const results = await Promise.all(metricSources.map(async (source) => {
      try {
        const payload = await readProjection(source.method, source.params);
        if (!Array.isArray(readPath(payload, source.path))) {
          throw new Error(`${source.method} returned an invalid row set`);
        }
        return [source.id, payload, ""] as const;
      } catch (error) {
        return [source.id, undefined, formatProjectionError(error) || "Projection read failed"] as const;
      }
    }));
    const payloads: Record<string, unknown> = { ...(lastGood?.payloads ?? {}) };
    const errors: Record<string, string> = {};
    let successfulSources = 0;
    for (const [sourceId, payload, error] of results) {
      if (error) errors[sourceId] = error;
      else {
        payloads[sourceId] = payload;
        successfulSources += 1;
      }
    }
    const error = Object.entries(errors)
      .map(([sourceId, detail]) => `${formatCell(sourceId)}: ${detail}`)
      .join("\n");
    if (successfulSources === 0) {
      if (lastGood) return { data: { payloads, errors }, error };
      throw new Error(error || "No metrics projection could be read");
    }
    return { data: { payloads, errors }, error: error || undefined };
  }, [metricSources]);
  const metrics = useProjectionResource<MetricsResourceData>({
    enabled: projectionAvailable,
    load: loadMetrics,
    resourceKey: period === "custom"
      ? `custom\0${appliedCustomRange.start}\0${appliedCustomRange.end}`
      : period,
    refreshKey: generation,
  });
  const state = useMemo<MetricsState>(() => ({
    errors: metrics.data?.errors ?? {},
    loading: metrics.phase === "refreshing",
    payloads: metrics.data?.payloads ?? {},
  }), [metrics.data, metrics.phase]);
  const refreshing = metrics.phase === "refreshing";



  const model = useMemo(() => buildMetricsModel(state, metricWindow), [state, metricWindow]);
  const filteredEvents = useMemo(() => filterEvents(model.events, query), [model.events, query]);
  const environmentRows = useMemo(() => filterNamedRows(model.workerEnvironmentTypes, query), [model.workerEnvironmentTypes, query]);
  const artifactRows = useMemo(() => filterNamedRows(model.artifactLabels, query), [model.artifactLabels, query]);
  const interfaceRows = useMemo(() => filterNamedRows(model.interfaceActivity, query), [model.interfaceActivity, query]);
  const customValid = customRange.start.length > 0
    && customRange.end.length > 0
    && parseDateInput(customRange.start, false) <= parseDateInput(customRange.end, true);

  if (!projectionAvailable) {
    return <RuntimeEmpty title="Projection bridge unavailable" detail="Metrics reads ledger projections through Electron" />;
  }
  if (metrics.phase === "initial-loading" && !metrics.error) {
    return <ProjectionResourceLoading label="Loading metrics" />;
  }
  if (metrics.data === null) {
    return (
      <ProjectionResourceFailure
        detail={metrics.error}
        onRetry={metrics.retry}
        retrying={metrics.phase === "initial-loading" || metrics.phase === "refreshing"}
        title="Unable to load metrics"
      />
    );
  }

  return (
    <section aria-busy={state.loading} className="observability-view">
      <div className="observability-overview">
        {metrics.error ? (
          <ProjectionResourceIssue
            className="mb-2"
            detail={metrics.error}
            onRetry={metrics.retry}
            retrying={refreshing}
            title={Object.keys(state.errors).length > 0 && Object.keys(state.errors).length < SOURCES.length
              ? "Some metrics use the last successful projection"
              : "Metrics could not be refreshed"}
          />
        ) : null}
        <div className="observability-commandbar" aria-label="Metrics time range">
          {refreshing ? <span className="mr-auto text-[10px] text-token-text-secondary" role="status">Refreshing metrics…</span> : null}
          <div className="observability-periods">
            {(Object.keys(PERIODS) as Array<Exclude<MetricPeriod, "custom">>).map((value) => (
              <button
                aria-pressed={period === value}
                className={period === value ? "is-active" : ""}
                key={value}
                onClick={() => { setPeriod(value); setCustomOpen(false); }}
                type="button"
              >
                {value}
              </button>
            ))}
            <div className="observability-custom-anchor">
              <button
                ref={customAnchorRef}
                aria-expanded={customOpen}
                aria-pressed={period === "custom"}
                className={period === "custom" ? "is-active" : ""}
                onClick={() => setCustomOpen((open) => !open)}
                type="button"
              >
                Custom
              </button>
              <OcDropdown anchorRef={customAnchorRef} align="end" className="observability-custom-popover" role="dialog" aria-label="Custom metrics range" onOpenChange={setCustomOpen} open={customOpen} style={{ position: "fixed", width: 260, minWidth: 0 }} sideOffset={6}>
                  <label>
                    <span>From</span>
                    <input type="date" value={customRange.start} onChange={(event) => setCustomRange((current) => ({ ...current, start: event.target.value }))} />
                  </label>
                  <label>
                    <span>To</span>
                    <input type="date" value={customRange.end} onChange={(event) => setCustomRange((current) => ({ ...current, end: event.target.value }))} />
                  </label>
                  <button
                    disabled={!customValid}
                    onClick={() => {
                      if (!customValid) return;
                      setAppliedCustomRange(customRange);
                      setPeriod("custom");
                      setCustomOpen(false);
                    }}
                    type="button"
                  >
                    Apply range
                  </button>
              </OcDropdown>
            </div>
          </div>
        </div>

        <div className="observability-signal-strip">
          <MetricSignal detail={model.runTrend.label} glyph="pulse" label="Runs" loading={state.loading} series={model.runSeries} trend={model.runTrend} value={sourceCount(state, "runs", model.windowRuns.length)} />
          <MetricSignal detail={model.activity.ledgerWindowAtLimit ? "Indexed page limit; refine range" : model.movementTrend.label} glyph="worktree" label="Movements" loading={state.loading} series={model.movementSeries} trend={model.movementTrend} value={sourceCount(state, "movements", model.activity.movements)} />
          <MetricSignal detail={model.activity.ledgerWindowAtLimit ? "Indexed page limit; refine range" : model.transferTrend.label} glyph="sites" label="Transferred" loading={state.loading} series={model.transferSeries} trend={model.transferTrend} value={sourceValue(state, "movements", formatMetricBytes(model.activity.bytesMoved))} />
          <MetricSignal detail={`${model.workerEnvironments.length} projected`} glyph="dashboard" label="Environments" loading={state.loading} series={model.environmentSeries} value={sourceCount(state, "workerEnvironments", model.workerEnvironments.length)} />
          <MetricSignal detail={`${model.interfaceActivity.reduce((sum, row) => sum + row.count, 0)} endpoints`} glyph="interfaces" label="Interfaces" loading={state.loading} series={model.interfaceSeries} value={sourceCount(state, "interfaces", model.interfaces.length)} />
          <MetricSignal detail={model.markTrend.label} glyph="registry" label="Marks" loading={state.loading} series={model.markSeries} trend={model.markTrend} value={sourceCount(state, "movements", model.windowMarks.length)} />
        </div>
      </div>

      <div className="observability-primary-grid">
        <Panel className="observability-throughput-panel" detail={metricWindow.bucketLabel} title="Movement throughput">
          <ActivityChart activity={model.activity} loading={state.loading} unavailable={sourceUnavailable(state, "movements")} />
        </Panel>
        <Panel className="observability-status-panel" title="Execution status">
          {sourceUnavailable(state, "runs") ? <div className="observability-empty">Run projection unavailable</div> : (
            <>
              <RunStateChart rows={model.runStatus} total={model.windowRuns.length} />
              <span className="observability-status-footnote">
                {model.windowRuns.length
                  ? `${model.windowRuns.length} runs in ${metricWindow.label}`
                  : state.loading ? "Refreshing run projection…" : "No runs in selected range"}
              </span>
            </>
          )}
        </Panel>
      </div>

      <div className="observability-detail-grid">
        <Panel title="Top worker environments">
          <EnvironmentTable loading={state.loading} rows={environmentRows} total={model.workerEnvironments.length} unavailable={sourceUnavailable(state, "workerEnvironments")} />
        </Panel>
        <Panel className="observability-artifact-panel" title="Artifact types">
          <ArtifactTypes loading={state.loading} rows={artifactRows} total={model.artifacts.length} unavailable={sourceUnavailable(state, "artifacts")} />
        </Panel>
        <Panel title="Interfaces activity">
          <InterfaceActivity loading={state.loading} rows={interfaceRows} unavailable={sourceUnavailable(state, "movements")} />
        </Panel>
      </div>

      <Panel className="observability-events-panel" title="Recent events">
        <EventTable events={filteredEvents} filtered={Boolean(query.trim())} loading={state.loading} unavailable={sourceUnavailable(state, "movements")} />
      </Panel>
    </section>
  );
}

function buildMetricsModel(state: MetricsState, window: MetricWindow) {
  const runs = rows(state, "runs");
  const workerEnvironments = rows(state, "workerEnvironments");
  const interfaces = rows(state, "interfaces");
  const artifacts = rows(state, "artifacts");
  const movements = rows(state, "movements") as MovementRow[];
  const projectedMovementTotal = Number(readPath(state.payloads.movements, "total"));
  const movementTotal = Number.isFinite(projectedMovementTotal) ? projectedMovementTotal : movements.length;
  const marks = movements.filter((movement) => stringValue(movement?.kind).startsWith("MarkOutcomeAdmitted("));
  const envelopes = movements;
  const windowRuns = rowsInWindow(runs, window);
  const windowMarks = rowsInWindow(marks, window);
  const previousWindow = previousMetricWindow(window);
  const previousRuns = rowsInWindow(runs, previousWindow);
  const previousMarks = rowsInWindow(marks, previousWindow);
  const activity = computeActivityMetrics({
    movements,
    environments: workerEnvironments.length,
    runs: windowRuns.length,
    interfaces: interfaces.length,
    marks: windowMarks.length,
    ledgerWindowAtLimit: movementTotal > movements.length,
    rangeStartMs: window.start,
    rangeEndMs: window.end,
    bucketCount: window.buckets,
  });
  const previousActivity = computeActivityMetrics({
    movements,
    environments: workerEnvironments.length,
    runs: previousRuns.length,
    interfaces: interfaces.length,
    marks: previousMarks.length,
    rangeStartMs: previousWindow.start,
    rangeEndMs: previousWindow.end,
    bucketCount: window.buckets,
  });
  const events = [
    ...envelopes.map((row, index) => eventRow(row, index, "envelope")),
  ]
    .filter((event) => event.timestamp >= window.start && event.timestamp <= window.end)
    .sort((left, right) => right.timestamp - left.timestamp);
  const movementSeries = activity.daily.map((point) => point.count);
  const transferSeries = activity.daily.map((point) => point.bytes);
  const interfaceActivity = buildInterfaceActivity(movements, window);

  return {
    runs,
    windowRuns,
    workerEnvironments,
    interfaces,
    artifacts,
    windowMarks,
    activity,
    movementSeries,
    transferSeries,
    runSeries: bucketRows(runs, window),
    environmentSeries: bucketMovementEndpoints(movements, window, "environment"),
    interfaceSeries: bucketMovementEndpoints(movements, window, "interface"),
    markSeries: bucketRows(marks, window),
    runTrend: compareMetric(windowRuns.length, previousRuns.length),
    movementTrend: compareMetric(activity.movements, previousActivity.movements),
    transferTrend: compareMetric(activity.bytesMoved, previousActivity.bytesMoved),
    markTrend: compareMetric(windowMarks.length, previousMarks.length),
    runStatus: buildRunStatus(windowRuns),
    workerEnvironmentTypes: countBy(workerEnvironments, (row) => stringValue(row?.worker_environment_kind ?? row?.kind)),
    artifactLabels: countBy(artifacts, (row) => stringValue(row?.label ?? row?.artifact_label ?? row?.kind)),
    interfaceActivity,
    events,
  };
}

function rows(state: MetricsState, sourceId: string): any[] {
  const source = SOURCES.find((item) => item.id === sourceId);
  if (!source) return [];
  return array(readPath(state.payloads[sourceId], source.path));
}

function resolveMetricWindow(period: MetricPeriod, customRange: { start: string; end: string }): MetricWindow {
  if (period === "custom") {
    const start = parseDateInput(customRange.start, false);
    const end = parseDateInput(customRange.end, true);
    const duration = Math.max(1, end - start);
    return {
      start,
      end,
      buckets: Math.min(90, Math.max(24, Math.ceil(duration / DAY_MS))),
      label: `${customRange.start} – ${customRange.end}`,
      bucketLabel: duration <= DAY_MS ? "Hourly" : "Daily",
    };
  }
  const preset = PERIODS[period];
  const end = Date.now();
  return { start: end - preset.duration, end, buckets: preset.buckets, label: period, bucketLabel: preset.bucketLabel };
}

function previousMetricWindow(window: MetricWindow): MetricWindow {
  const duration = window.end - window.start;
  return { ...window, start: window.start - duration, end: window.start };
}

function toDateInput(timestamp: number): string {
  const date = new Date(timestamp);
  const offset = date.getTimezoneOffset() * 60_000;
  return new Date(timestamp - offset).toISOString().slice(0, 10);
}

function parseDateInput(value: string, endOfDay: boolean): number {
  const suffix = endOfDay ? "T23:59:59.999" : "T00:00:00.000";
  const parsed = new Date(`${value}${suffix}`).getTime();
  return Number.isFinite(parsed) ? parsed : 0;
}

function rowsInWindow(input: any[], window: MetricWindow): any[] {
  return input.filter((row) => {
    const timestamp = timeOf(row);
    return timestamp >= window.start && timestamp <= window.end;
  });
}

function bucketRows(input: any[], window: MetricWindow): number[] {
  const values = Array.from({ length: window.buckets }, () => 0);
  const duration = Math.max(1, window.end - window.start);
  for (const row of input) {
    const timestamp = timeOf(row);
    if (timestamp < window.start || timestamp > window.end) continue;
    const index = Math.min(window.buckets - 1, Math.max(0, Math.floor(((timestamp - window.start) / duration) * window.buckets)));
    values[index] += 1;
  }
  return values;
}

function bucketMovementEndpoints(movements: MovementRow[], window: MetricWindow, kind: "interface" | "environment"): number[] {
  const values = Array.from({ length: window.buckets }, () => 0);
  const duration = Math.max(1, window.end - window.start);
  for (const movement of movements) {
    const timestamp = Number(movement.recorded_at_unix_ms) || 0;
    if (timestamp < window.start || timestamp > window.end) continue;
    const count = kind === "interface"
      ? [movement.from, movement.to].filter((endpoint) => endpoint?.kind === "interface").length
      : Number(Boolean(movement.raw_ee_id));
    if (!count) continue;
    const index = Math.min(window.buckets - 1, Math.max(0, Math.floor(((timestamp - window.start) / duration) * window.buckets)));
    values[index] += count;
  }
  return values;
}

function buildInterfaceActivity(movements: MovementRow[], window: MetricWindow): InterfaceMetric[] {
  const byInterface = new Map<string, { count: number; timestamps: number[] }>();
  for (const movement of movements) {
    const timestamp = Number(movement.recorded_at_unix_ms) || 0;
    if (timestamp < window.start || timestamp > window.end) continue;
    for (const endpoint of [movement.from, movement.to]) {
      if (endpoint?.kind?.toLowerCase() !== "interface") continue;
      const name = stringValue(endpoint.id) || "unknown";
      const current = byInterface.get(name) ?? { count: 0, timestamps: [] };
      current.count += 1;
      current.timestamps.push(timestamp);
      byInterface.set(name, current);
    }
  }
  const duration = Math.max(1, window.end - window.start);
  return [...byInterface.entries()]
    .map(([name, metric]) => {
      const series = Array.from({ length: window.buckets }, () => 0);
      for (const timestamp of metric.timestamps) {
        const index = Math.min(window.buckets - 1, Math.max(0, Math.floor(((timestamp - window.start) / duration) * window.buckets)));
        series[index] += 1;
      }
      return { name, count: metric.count, series };
    })
    .sort((left, right) => right.count - left.count || left.name.localeCompare(right.name))
    .slice(0, 8);
}

function buildRunStatus(runs: any[]): Array<{ key: string; count: number }> {
  const canonical = ["Success", "Failed", "Cancelled", "Timeout"];
  const counts = new Map<string, number>(canonical.map((key) => [key, 0]));
  for (const run of runs) {
    const raw = stringValue(run?.status).toLowerCase();
    const key = ["done", "complete", "completed", "success", "succeeded"].includes(raw)
      ? "Success"
      : ["failed", "failure", "blocked"].includes(raw)
        ? "Failed"
        : ["canceled", "cancelled"].includes(raw)
          ? "Cancelled"
          : ["timeout", "timed_out"].includes(raw)
            ? "Timeout"
            : formatCell(raw || "Unknown");
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return [...counts.entries()].map(([key, count]) => ({ key, count }));
}

function compareMetric(current: number, previous: number): MetricTrend {
  if (current === previous) return { direction: "flat", label: "—" };
  if (previous === 0) return { direction: "up", label: current > 0 ? "New" : "—" };
  const change = Math.round(((current - previous) / previous) * 100);
  return { direction: change > 0 ? "up" : "down", label: `${Math.abs(change)}%` };
}

function eventRow(row: any, index: number, family: "envelope" | "artifact" | "mark") {
  const id = stringValue(row?.envelope_id ?? row?.artifact_id ?? row?.mark_id ?? row?.id) || `${family}_${index}`;
  const kind = stringValue(row?.kind ?? row?.mark_name ?? row?.name ?? row?.label) || family;
  const timestamp = timeOf(row);
  const source = stringValue(row?.source ?? row?.from?.id ?? row?.run_id ?? row?.trace_id);
  const environment = stringValue(
    row?.worker_environment_kind
      ?? row?.environment_kind
      ?? row?.ee_id
      ?? row?.raw_ee_id,
  );
  return {
    key: `${family}:${id}`,
    timestamp,
    time: formatTime(timestamp),
    label: family === "envelope" ? kind.split(".").join(" ") : `${family} ${kind}`,
    source: source ? shortId(source, 20, 8) : "—",
    environment: environment ? shortId(environment, 18, 6) : "—",
    tone: family === "artifact" ? "amber" : family === "mark" ? "purple" : "green",
    raw: row,
  };
}

function filterEvents(events: MetricEvent[], query: string): MetricEvent[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return events;
  return events.filter((row) => jsonText(row.raw).toLowerCase().includes(needle) || row.label.toLowerCase().includes(needle));
}

function filterNamedRows<T extends { key?: string; name?: string }>(rows: T[], query: string): T[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return rows;
  return rows.filter((row) => stringValue(row.key ?? row.name).toLowerCase().includes(needle));
}

function MetricSignal({
  label,
  value,
  detail,
  glyph,
  series,
  trend,
  loading,
}: {
  label: string;
  value: string;
  detail: string;
  glyph: GlyphName;
  series: number[];
  trend?: MetricTrend;
  loading: boolean;
}) {
  return (
    <div className={`observability-signal${loading ? " is-loading" : ""}`}>
      <span className="observability-signal-label"><Glyph name={glyph} size={14} />{label}</span>
      <div className="observability-signal-body">
        <div>
          <strong>{value}</strong>
          <em className={trend ? `is-${trend.direction}` : ""} title={detail}>
            {trend?.direction === "up" ? "↑" : trend?.direction === "down" ? "↓" : ""} {detail}
          </em>
        </div>
        <Sparkline series={series} />
      </div>
    </div>
  );
}

function Panel({ title, detail, children, className = "" }: { title: string; detail?: string; children: ReactNode; className?: string }) {
  return (
    <section className={`observability-panel ${className}`.trim()}>
      <header><h2>{title}</h2>{detail ? <span>{detail}</span> : null}</header>
      <div className="observability-panel-content">{children}</div>
    </section>
  );
}

function Sparkline({ series, compact = false }: { series: number[]; compact?: boolean }) {
  const width = compact ? 78 : 94;
  const height = compact ? 22 : 28;
  const max = Math.max(...series, 1);
  const min = Math.min(...series, 0);
  const span = Math.max(1, max - min);
  const points = series.map((value, index) => ({
    x: (index / Math.max(series.length - 1, 1)) * width,
    y: height - 2 - ((value - min) / span) * (height - 4),
  }));
  const path = straightMetricPath(points);
  return (
    <svg className="observability-sparkline" preserveAspectRatio="none" viewBox={`0 0 ${width} ${height}`} aria-hidden="true">
      <path className="observability-sparkline-baseline" d={`M 0 ${height - 2} L ${width} ${height - 2}`} />
      {path ? <path className="observability-sparkline-line" d={path} /> : null}
    </svg>
  );
}

function ActivityChart({ activity, loading, unavailable }: { activity: ActivityMetrics; loading: boolean; unavailable: boolean }) {
  const chartRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 820, height: 226 });
  useEffect(() => {
    const element = chartRef.current;
    if (!element) return;
    const applySize = (width: number, height: number) => {
      const next = {
        width: Math.max(180, Math.round(width)),
        height: Math.max(120, Math.round(height)),
      };
      setSize((current) => current.width === next.width && current.height === next.height ? current : next);
    };
    const bounds = element.getBoundingClientRect();
    applySize(bounds.width, bounds.height);
    const observer = new ResizeObserver(([entry]) => {
      if (entry) applySize(entry.contentRect.width, entry.contentRect.height);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const { width, height } = size;
  const inset = { top: 14, right: 14, bottom: 27, left: 42 };
  const plotWidth = width - inset.left - inset.right;
  const plotHeight = height - inset.top - inset.bottom;
  const peak = Math.max(...activity.daily.map((point) => point.count), 1);
  const points = activity.daily.map((point, index) => ({
    ...point,
    x: inset.left + (index / Math.max(activity.daily.length - 1, 1)) * plotWidth,
    y: inset.top + plotHeight - (point.count / peak) * plotHeight,
  }));
  const line = straightMetricPath(points);
  const base = inset.top + plotHeight;
  const area = points.length ? `${line} L ${points.at(-1)?.x ?? inset.left} ${base} L ${points[0].x} ${base} Z` : "";
  const labelIndexes = Array.from(new Set([0, .2, .4, .6, .8, 1].map((ratio) => Math.round((points.length - 1) * ratio))));
  if (unavailable) {
    return <div className="observability-throughput-chart"><span className="observability-chart-empty">Movement projection unavailable</span></div>;
  }
  return (
    <div className={`observability-throughput-chart${loading ? " is-loading" : ""}`} aria-label="Movement throughput" ref={chartRef} role="img">
      <svg viewBox={`0 0 ${width} ${height}`}>
        <defs>
          <pattern height="7" id="observability-throughput-dots" patternUnits="userSpaceOnUse" width="7">
            <circle className="observability-chart-area-dot" cx="1.6" cy="1.6" r="1.05" />
          </pattern>
        </defs>
        {[0, 1, 2, 3, 4].map((step) => {
          const y = inset.top + (step / 4) * plotHeight;
          return (
            <g className="observability-chart-gridline" key={step}>
              <line x1={inset.left} x2={width - inset.right} y1={y} y2={y} />
              <text x={inset.left - 9} y={y + 3}>{Math.round(peak * (1 - step / 4))}</text>
            </g>
          );
        })}
        {area ? <path className="observability-chart-area" d={area} /> : null}
        {line ? <path className="observability-chart-line" d={line} /> : null}
        {points.filter((point) => point.count > 0).map((point) => (
          <circle className="observability-chart-point" cx={point.x} cy={point.y} key={point.timestamp} r="6">
            <title>{`${point.label}: ${point.count} movements · ${formatMetricBytes(point.bytes)}`}</title>
          </circle>
        ))}
        {labelIndexes.map((index, position) => {
          const point = points[index];
          if (!point) return null;
          return <text className="observability-chart-date" key={point.timestamp} textAnchor={position === 0 ? "start" : position === labelIndexes.length - 1 ? "end" : "middle"} x={point.x} y={height - 4}>{point.shortLabel}</text>;
        })}
      </svg>
      {activity.movements === 0 && !loading ? <span className="observability-chart-empty">No movements in this range</span> : null}
    </div>
  );
}

function straightMetricPath(points: Array<{ x: number; y: number }>): string {
  if (!points.length) return "";
  return points
    .map((point, index) => `${index === 0 ? "M" : "L"} ${point.x} ${point.y}`)
    .join(" ");
}

function RunStateChart({ rows, total }: { rows: Array<{ key: string; count: number }>; total: number }) {
  const radius = 40;
  const circumference = Math.PI * 2 * radius;
  let cursor = 0;
  const segments = rows.map((row, index) => {
    const ratio = total > 0 ? row.count / total : 0;
    const segment = { ...row, offset: cursor, ratio, tone: metricStatusTone(row.key, index) };
    cursor += ratio;
    return segment;
  });
  return (
    <div className="observability-run-state">
      <svg aria-label={`${total} projected runs`} role="img" viewBox="0 0 112 112">
        <circle className="observability-orbit-track" cx="56" cy="56" r={radius} transform="rotate(-90 56 56)" />
        {segments.filter((segment) => segment.count > 0).map((segment) => (
          <circle
            className={`observability-orbit-segment is-${segment.tone}`}
            cx="56"
            cy="56"
            key={segment.key}
            r={radius}
            strokeDasharray={`${Math.max(0, segment.ratio * circumference - 2.5)} ${circumference}`}
            strokeDashoffset={-segment.offset * circumference}
            transform="rotate(-90 56 56)"
          />
        ))}
      </svg>
      <div className="observability-run-legend">
        {segments.map((segment) => (
          <div key={segment.key}>
            <i className={`is-${segment.tone}`} />
            <span>{segment.key}</span>
            <strong>{segment.count}</strong>
            <em>{total > 0 ? `${Math.round(segment.ratio * 100)}%` : "0%"}</em>
          </div>
        ))}
      </div>
    </div>
  );
}

function metricStatusTone(status: string, index: number): "green" | "blue" | "amber" | "red" | "purple" | "neutral" {
  const normalized = status.toLowerCase();
  if (["success", "active", "running"].includes(normalized)) return "green";
  if (["failed", "blocked"].includes(normalized)) return "red";
  if (["cancelled", "canceled"].includes(normalized)) return "amber";
  if (["timeout", "timed out"].includes(normalized)) return "purple";
  return (["blue", "neutral", "amber"] as const)[index % 3];
}

function EnvironmentTable({ loading, rows, total, unavailable }: { loading: boolean; rows: Array<{ key: string; count: number }>; total: number; unavailable: boolean }) {
  if (!rows.length) return <div className="observability-empty">{loading ? "Refreshing worker environments…" : unavailable ? "Worker environment projection unavailable" : "No worker environments"}</div>;
  return (
    <div className="observability-table observability-environment-table">
      <div className="observability-table-head"><span>Environment</span><span>Instances</span><span>Share</span></div>
      {rows.slice(0, 8).map((row) => {
        const share = total > 0 ? (row.count / total) * 100 : 0;
        return (
          <div className="observability-table-row" key={row.key}>
            <strong title={row.key}>{formatCell(row.key)}</strong>
            <span>{row.count}</span>
            <span className="observability-share"><i style={{ width: `${share}%` }} /><em>{Math.round(share)}%</em></span>
          </div>
        );
      })}
    </div>
  );
}

function ArtifactTypes({ loading, rows, total, unavailable }: { loading: boolean; rows: Array<{ key: string; count: number }>; total: number; unavailable: boolean }) {
  return (
    <div className="observability-artifact-layout">
      {rows.length ? (
        <div className="observability-table observability-artifact-table">
          <div className="observability-table-head"><span>Type</span><span>Count</span></div>
          {rows.slice(0, 8).map((row) => (
            <div className="observability-table-row" key={row.key}><strong title={row.key}>{formatCell(row.key)}</strong><span>{row.count}</span></div>
          ))}
        </div>
      ) : <div className="observability-empty">{loading ? "Refreshing artifacts…" : unavailable ? "Artifact projection unavailable" : "No artifact rows"}</div>}
      {!unavailable && (!loading || rows.length > 0) ? <SimpleDonut rows={rows} total={total} /> : null}
    </div>
  );
}

function SimpleDonut({ rows, total }: { rows: Array<{ key: string; count: number }>; total: number }) {
  const radius = 34;
  const circumference = Math.PI * 2 * radius;
  let cursor = 0;
  const segments = rows.slice(0, 10).map((row, index) => {
    const ratio = total > 0 ? row.count / total : 0;
    const segment = { ...row, ratio, offset: cursor, index };
    cursor += ratio;
    return segment;
  });
  return (
    <svg className="observability-artifact-donut" viewBox="0 0 96 96" aria-label={`${total} artifacts`} role="img">
      <circle className="observability-artifact-track" cx="48" cy="48" r={radius} />
      {segments.map((segment) => (
        <circle
          className={`observability-artifact-segment is-${segment.index % 5}`}
          cx="48"
          cy="48"
          key={segment.key}
          r={radius}
          strokeDasharray={`${Math.max(0, segment.ratio * circumference - 2.5)} ${circumference}`}
          strokeDashoffset={-segment.offset * circumference}
          transform="rotate(-90 48 48)"
        />
      ))}
    </svg>
  );
}

function InterfaceActivity({ loading, rows, unavailable }: { loading: boolean; rows: InterfaceMetric[]; unavailable: boolean }) {
  if (!rows.length) return <div className="observability-empty">{loading ? "Refreshing interface activity…" : unavailable ? "Movement projection unavailable" : "No interface movements"}</div>;
  return (
    <div className="observability-table observability-interface-table">
      <div className="observability-table-head"><span>Interface</span><span>Endpoints</span><span>Trend</span></div>
      {rows.map((row) => (
        <div className="observability-table-row" key={row.name}>
          <strong title={row.name}>{row.name}</strong>
          <span>{row.count}</span>
          <Sparkline compact series={row.series} />
        </div>
      ))}
    </div>
  );
}

function EventTable({ events, filtered, loading, unavailable }: { events: MetricEvent[]; filtered: boolean; loading: boolean; unavailable: boolean }) {
  if (!events.length) return <div className="observability-empty">{loading ? "Refreshing events…" : unavailable ? "Movement projection unavailable" : filtered ? "No events match search" : "No events in selected range"}</div>;
  return (
    <div className="observability-table observability-event-table">
      <div className="observability-table-head"><span>Time</span><span>Event</span><span>Source</span><span>Environment</span></div>
      {events.slice(0, 200).map((event) => (
        <div className="observability-table-row" key={event.key}>
          <time><i className={`is-${event.tone}`} />{event.time}</time>
          <strong title={event.label}>{event.label}</strong>
          <span title={event.source}>{event.source}</span>
          <span title={event.environment}>{event.environment}</span>
        </div>
      ))}
    </div>
  );
}

function RuntimeEmpty({ title, detail }: { title: string; detail: string }) {
  return (
    <section className="workspace-empty">
      <Glyph name="metrics" size={28} />
      <strong>{title}</strong>
      <span>{detail}</span>
    </section>
  );
}

function sourceCount(state: MetricsState, sourceId: string, count: number): string {
  return sourceValue(state, sourceId, compactMetricCount(count));
}

function sourceValue(state: MetricsState, sourceId: string, value: string): string {
  if (sourceUnavailable(state, sourceId)) return "—";
  if (state.loading && !sourceHasPayload(state, sourceId)) return "…";
  return value;
}

function sourceUnavailable(state: MetricsState, sourceId: string): boolean {
  return Boolean(state.errors[sourceId]) && !sourceHasPayload(state, sourceId);
}

function sourceHasPayload(state: MetricsState, sourceId: string): boolean {
  return Object.prototype.hasOwnProperty.call(state.payloads, sourceId);
}
