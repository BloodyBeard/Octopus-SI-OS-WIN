import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  OctopusCodexUsageBucket,
  OctopusCodexUsageSnapshot,
  OctopusCodexUsageWindowDays,
} from "../electron";
import { OcIconButton, OcSegmented } from "../design-system/components";
import { hasCodexUsageBridge, readCodexUsage } from "../usageClient";
import { OctopusLoadingState } from "./OctopusLoadingLogo";
import "./UsageView.css";

type UsageMetric = "cost" | "tokens";
type UsageBreakdown = "model" | "day";
type UsageWindowValue = "7" | "30" | "90";

type UsageState = {
  loading: boolean;
  snapshot: OctopusCodexUsageSnapshot | null;
  error: string;
};

type UsageTotals = {
  totalTokens: number;
  cachedInputTokens: number;
  uncachedInputTokens: number;
  cacheCreationTokens: number;
  outputTokens: number;
  reasoningTokens: number;
  costUsd: number;
  cacheSavingsUsd: number;
  pricedTokens: number;
  unpricedTokens: number;
  records: number;
};

type UsageRow = UsageTotals & {
  id: string;
  label: string;
  priced: boolean;
};

type DailyPoint = UsageTotals & {
  day: string;
};

type Point = { x: number; y: number };
type CurveSegment = { from: Point; c1: Point; c2: Point; to: Point };

const WINDOW_OPTIONS: Array<{ value: UsageWindowValue; label: string }> = [
  { value: "7", label: "7 days" },
  { value: "30", label: "30 days" },
  { value: "90", label: "90 days" },
];

const EMPTY_TOTALS: UsageTotals = {
  totalTokens: 0,
  cachedInputTokens: 0,
  uncachedInputTokens: 0,
  cacheCreationTokens: 0,
  outputTokens: 0,
  reasoningTokens: 0,
  costUsd: 0,
  cacheSavingsUsd: 0,
  pricedTokens: 0,
  unpricedTokens: 0,
  records: 0,
};

const VIEW_WIDTH = 920;
const VIEW_HEIGHT = 236;
const PLOT_TOP = 8;

export function UsageView({ query }: { query: string }) {
  const [windowValue, setWindowValue] = useState<UsageWindowValue>("30");
  const windowDays = Number(windowValue) as OctopusCodexUsageWindowDays;
  const [metric, setMetric] = useState<UsageMetric>("cost");
  const [breakdown, setBreakdown] = useState<UsageBreakdown>("model");
  const [refreshGeneration, setRefreshGeneration] = useState(0);
  const [state, setState] = useState<UsageState>({ loading: hasCodexUsageBridge(), snapshot: null, error: "" });

  useEffect(() => {
    let cancelled = false;
    if (!hasCodexUsageBridge()) {
      setState({ loading: false, snapshot: null, error: "Usage reads local Codex transcripts in the Octopus desktop app." });
      return () => { cancelled = true; };
    }
    setState((previous) => ({ ...previous, loading: true, error: "" }));
    void readCodexUsage({
      windowDays,
      timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    })
      .then((snapshot) => {
        if (!cancelled) setState({ loading: false, snapshot, error: "" });
      })
      .catch((error) => {
        if (!cancelled) {
          setState((previous) => ({
            loading: false,
            snapshot: previous.snapshot,
            error: error instanceof Error ? error.message : String(error),
          }));
        }
      });
    return () => { cancelled = true; };
  }, [refreshGeneration, windowDays]);

  const model = useMemo(() => buildUsageModel(state.snapshot), [state.snapshot]);
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const modelRows = useMemo(
    () => model.models.filter((row) => row.label.toLocaleLowerCase().includes(normalizedQuery)),
    [model.models, normalizedQuery],
  );
  const dayRows = useMemo(
    () => model.daily.toReversed().filter((row) => row.day.toLocaleLowerCase().includes(normalizedQuery)),
    [model.daily, normalizedQuery],
  );
  const activeDays = model.daily.filter((day) => day.totalTokens > 0).length;
  const observedInput = model.totals.cachedInputTokens + model.totals.uncachedInputTokens;
  const cachedShare = observedInput === 0 ? 0 : model.totals.cachedInputTokens / observedInput;
  const pricedShare = model.totals.totalTokens === 0 ? 0 : model.totals.pricedTokens / model.totals.totalTokens;
  const unpricedShare = model.totals.totalTokens === 0 ? 0 : model.totals.unpricedTokens / model.totals.totalTokens;
  const rawInputCost = model.totals.costUsd + model.totals.cacheSavingsUsd;

  return (
    <section className="usage-view">
      <header className="usage-header">
        <div>
          <h1>Usage</h1>
          <p>{formatWindow(state.snapshot, windowDays)}</p>
        </div>
        <div className="usage-header-actions">
          <OcSegmented
            className="usage-control-group"
            label="Usage time range"
            onChange={setWindowValue}
            options={WINDOW_OPTIONS}
            size="sm"
            value={windowValue}
          />
          <OcIconButton
            className={`usage-refresh${state.loading ? " is-loading" : ""}`}
            disabled={state.loading || !hasCodexUsageBridge()}
            glyph="refresh"
            label="Refresh usage"
            onClick={() => setRefreshGeneration((generation) => generation + 1)}
            size="sm"
            variant="ghost"
          />
        </div>
      </header>

      {state.error ? <UsageNotice tone="error">{state.error}</UsageNotice> : null}
      {state.snapshot?.source.status === "partial" ? (
        <UsageNotice tone="warning">
          Some transcript records could not be read. Totals include every valid record found in this scan.
        </UsageNotice>
      ) : null}
      {state.snapshot?.source.status === "missing" ? (
        <UsageNotice tone="neutral">No local Codex session directory was found.</UsageNotice>
      ) : null}
      {state.snapshot?.source.status === "failed" ? (
        <UsageNotice tone="error">{state.snapshot.source.message || "The local Codex session directory could not be scanned."}</UsageNotice>
      ) : null}

      {state.loading && !state.snapshot ? (
        <OctopusLoadingState className="usage-loading" label="Scanning local Codex transcripts" />
      ) : (
        <>
          <section className="usage-feature-grid">
            <div className="usage-summary">
              <div className="usage-kicker">{metric === "cost" ? "Raw API-equivalent cost" : "Processed tokens"}</div>
              <strong className="usage-headline">
                {metric === "cost"
                  ? model.hasPricedUsage ? `${formatUsd(model.totals.costUsd)}*` : "—"
                  : formatTokens(model.totals.totalTokens)}
              </strong>
              <p>
                {metric === "cost"
                  ? "* estimated from public API rates, not your subscription bill"
                  : `${formatCount(state.snapshot?.source.distinctSessions ?? 0)} local Codex sessions`}
              </p>
              <div className="usage-provider-row">
                <div className="usage-provider-heading">
                  <span><OpenAIMark />Codex</span>
                  <b>{metric === "cost" ? formatUsd(model.totals.costUsd) : formatTokens(model.totals.totalTokens)}</b>
                </div>
                <div className="usage-provider-track"><span style={{ width: `${Math.max(0, Math.min(100, pricedShare * 100))}%` }} /></div>
                <small>
                  {metric === "cost"
                    ? `${formatPercent(pricedShare)} model-priced · ${formatTokens(model.totals.totalTokens)} tokens`
                    : `${formatPercent(cachedShare)} of observed input read from cache`}
                </small>
              </div>
            </div>

            <div className="usage-chart-section">
              <div className="usage-section-heading">
                <h2>Daily {metric === "cost" ? "cost" : "processed tokens"}</h2>
                <div className="usage-chart-controls">
                  <OcSegmented
                    className="usage-control-group is-compact"
                    label="Chart metric"
                    onChange={setMetric}
                    options={[{ value: "cost", label: "Cost" }, { value: "tokens", label: "Tokens" }]}
                    value={metric}
                  />
                  <span className="usage-legend"><OpenAIMark />Codex</span>
                </div>
              </div>
              <UsageChart daily={model.daily} metric={metric} />
            </div>
          </section>

          <section className="usage-metric-strip">
            <UsageMetricCell
              detail={`${formatTokens(activeDays === 0 ? 0 : model.totals.totalTokens / activeDays)} per active day`}
              label="Processed tokens"
              value={formatTokens(model.totals.totalTokens)}
            />
            <UsageMetricCell
              detail={`${formatPercent(cachedShare)} of observed input`}
              label="Cached input"
              value={formatTokens(model.totals.cachedInputTokens)}
            />
            <UsageMetricCell
              detail={`${formatTokens(model.totals.cacheCreationTokens)} cache writes`}
              label="Uncached input"
              value={formatTokens(model.totals.uncachedInputTokens)}
            />
            <UsageMetricCell
              detail={`includes ${formatTokens(model.totals.reasoningTokens)} reasoning`}
              label="Output"
              value={formatTokens(model.totals.outputTokens)}
            />
            <UsageMetricCell
              detail={rawInputCost > 0 ? `${(model.totals.cacheSavingsUsd / rawInputCost * 100).toFixed(1)}% of full input cost` : "vs full input rates"}
              label="Cache savings"
              value={model.hasPricedUsage ? formatUsd(model.totals.cacheSavingsUsd) : "—"}
            />
          </section>

          <section className="usage-detail-grid">
            <div className="usage-breakdown">
              <div className="usage-section-heading">
                <h2>Breakdown</h2>
                <OcSegmented
                  className="usage-control-group is-compact"
                  label="Usage breakdown"
                  onChange={setBreakdown}
                  options={[{ value: "model", label: "Model" }, { value: "day", label: "Day" }]}
                  value={breakdown}
                />
              </div>
              <div className="usage-table-scroll">
                {breakdown === "model"
                  ? <ModelTable rows={modelRows} totalCost={model.totals.costUsd} />
                  : <DayTable rows={dayRows} />}
              </div>
            </div>

            <aside className="usage-quality">
              <h2>Cost quality</h2>
              <dl>
                <QualityRow label="Model priced" value={formatPercent(pricedShare)} />
                <QualityRow label="Unpriced" value={formatPercent(unpricedShare)} />
                <QualityRow label="Cache savings" value={model.hasPricedUsage ? formatUsd(model.totals.cacheSavingsUsd) : "—"} />
                <QualityRow label="Pricing" value={formatPricingStatus(state.snapshot)} />
              </dl>
              <div className="usage-source-detail">
                <span>Local source</span>
                <b>{formatCount((state.snapshot?.source.scannedFiles ?? 0) + (state.snapshot?.source.cachedFiles ?? 0))} files</b>
                <small>{state.snapshot ? `${state.snapshot.scanDurationMs.toLocaleString()} ms · ${state.snapshot.source.path}` : "Waiting for desktop source"}</small>
              </div>
            </aside>
          </section>
        </>
      )}
    </section>
  );
}

function UsageNotice({ children, tone }: { children: string; tone: "error" | "warning" | "neutral" }) {
  return <div className={`usage-notice is-${tone}`} role={tone === "error" ? "alert" : "status"}>{children}</div>;
}

function UsageMetricCell({ detail, label, value }: { detail: string; label: string; value: string }) {
  return (
    <div className="usage-metric-cell">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{detail}</small>
    </div>
  );
}

function QualityRow({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd>{value}</dd></div>;
}

function ModelTable({ rows, totalCost }: { rows: UsageRow[]; totalCost: number }) {
  return (
    <table className="usage-table">
      <thead><tr><th scope="col">Model</th><th scope="col">Cost</th><th scope="col">Share</th><th scope="col">Tokens</th></tr></thead>
      <tbody>
        {rows.length === 0 ? <EmptyTableRow /> : rows.map((row) => (
          <tr key={row.id}>
            <td><span className="usage-model-name"><OpenAIMark />{row.label}</span></td>
            <td>{row.priced ? formatUsd(row.costUsd) : "—"}</td>
            <td>{row.priced && totalCost > 0 ? formatPercent(row.costUsd / totalCost) : "Unpriced"}</td>
            <td>{formatTokens(row.totalTokens)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function DayTable({ rows }: { rows: DailyPoint[] }) {
  return (
    <table className="usage-table">
      <thead><tr><th scope="col">Day</th><th scope="col">Cost</th><th scope="col">Cached input</th><th scope="col">Tokens</th></tr></thead>
      <tbody>
        {rows.length === 0 ? <EmptyTableRow /> : rows.map((row) => (
          <tr key={row.day}>
            <td>{formatDayShort(row.day)}</td>
            <td>{row.pricedTokens > 0 ? formatUsd(row.costUsd) : "—"}</td>
            <td>{formatTokens(row.cachedInputTokens)}</td>
            <td>{formatTokens(row.totalTokens)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function EmptyTableRow() {
  return <tr><td className="usage-empty-row" colSpan={4}>No matching activity in this window.</td></tr>;
}

function UsageChart({ daily, metric }: { daily: DailyPoint[]; metric: UsageMetric }) {
  const plotRef = useRef<HTMLDivElement>(null);
  const [activeIndex, setActiveIndex] = useState<number | null>(null);
  const chart = useMemo(() => buildChart(daily, metric), [daily, metric]);
  const selectFromClientX = useCallback((clientX: number) => {
    const bounds = plotRef.current?.getBoundingClientRect();
    if (!bounds || bounds.width <= 0 || daily.length === 0) return;
    const fraction = Math.max(0, Math.min(1, (clientX - bounds.left) / bounds.width));
    setActiveIndex(Math.round(fraction * (daily.length - 1)));
  }, [daily.length]);
  const active = activeIndex === null ? null : daily[activeIndex] ?? null;
  const activeLeft = daily.length <= 1 ? 0 : ((activeIndex ?? 0) / (daily.length - 1)) * 100;
  const format = metric === "cost" ? formatUsd : formatTokens;

  return (
    <div className="usage-chart">
      <div className="usage-chart-y-axis" aria-hidden="true">
        {chart.ticks.map((tick) => <span key={tick} style={{ top: `${chart.toY(tick) / VIEW_HEIGHT * 100}%` }}>{tick === 0 ? "0" : format(tick)}</span>)}
      </div>
      <div
        aria-label={`Daily Codex ${metric === "cost" ? "API-equivalent cost" : "processed tokens"}`}
        className="usage-chart-plot"
        onBlur={() => setActiveIndex(null)}
        onKeyDown={(event) => {
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          event.preventDefault();
          const direction = event.key === "ArrowRight" ? 1 : -1;
          setActiveIndex((index) => Math.max(0, Math.min(daily.length - 1, (index ?? (direction > 0 ? -1 : daily.length)) + direction)));
        }}
        onPointerLeave={() => setActiveIndex(null)}
        onPointerMove={(event) => selectFromClientX(event.clientX)}
        ref={plotRef}
        role="img"
        tabIndex={0}
      >
        <svg aria-hidden="true" preserveAspectRatio="none" viewBox={`0 0 ${VIEW_WIDTH} ${VIEW_HEIGHT}`}>
          <defs>
            <linearGradient id="usage-area-fill" x1="0" x2="0" y1="0" y2="1">
              <stop offset="0%" stopColor="var(--usage-series)" stopOpacity=".2" />
              <stop offset="100%" stopColor="var(--usage-series)" stopOpacity=".015" />
            </linearGradient>
          </defs>
          {chart.ticks.map((tick) => <line className="usage-chart-gridline" key={tick} x1="0" x2={VIEW_WIDTH} y1={chart.toY(tick)} y2={chart.toY(tick)} />)}
          {chart.area ? <path className="usage-chart-area" d={chart.area} /> : null}
          {chart.line ? <path className="usage-chart-line" d={chart.line} /> : null}
          {activeIndex === null ? null : <line className="usage-chart-cursor" x1={activeIndex * chart.stepX} x2={activeIndex * chart.stepX} y1={PLOT_TOP} y2={VIEW_HEIGHT} />}
        </svg>
        {active ? (
          <div className="usage-chart-tooltip" style={{ left: `${activeLeft}%`, transform: activeLeft > 62 ? "translateX(-100%)" : undefined }}>
            <span>{formatDayShort(active.day)}</span>
            <div><span><OpenAIMark />Codex</span><b>{format(metric === "cost" ? active.costUsd : active.totalTokens)}</b></div>
          </div>
        ) : null}
      </div>
      <div className="usage-chart-x-axis" aria-hidden="true">
        <span>{formatDayShort(daily[0]?.day ?? "")}</span>
        <span>{formatDayShort(daily[Math.floor(daily.length / 2)]?.day ?? "")}</span>
        <span>{formatDayShort(daily.at(-1)?.day ?? "")}</span>
      </div>
    </div>
  );
}

function OpenAIMark() {
  return (
    <svg aria-hidden="true" className="usage-openai-mark" preserveAspectRatio="xMidYMid" viewBox="0 0 256 260">
      <path d="M239.184 106.203a64.716 64.716 0 0 0-5.576-53.103C219.452 28.459 191 15.784 163.213 21.74A65.586 65.586 0 0 0 52.096 45.22a64.716 64.716 0 0 0-43.23 31.36c-14.31 24.602-11.061 55.634 8.033 76.74a64.665 64.665 0 0 0 5.525 53.102c14.174 24.65 42.644 37.324 70.446 31.36a64.72 64.72 0 0 0 48.754 21.744c28.481.025 53.714-18.361 62.414-45.481a64.767 64.767 0 0 0 43.229-31.36c14.137-24.558 10.875-55.423-8.083-76.483Zm-97.56 136.338a48.397 48.397 0 0 1-31.105-11.255l1.535-.87 51.67-29.825a8.595 8.595 0 0 0 4.247-7.367v-72.85l21.845 12.636c.218.111.37.32.409.563v60.367c-.056 26.818-21.783 48.545-48.601 48.601Zm-104.466-44.61a48.345 48.345 0 0 1-5.781-32.589l1.534.921 51.722 29.826a8.339 8.339 0 0 0 8.441 0l63.181-36.425v25.221a.87.87 0 0 1-.358.665l-52.335 30.184c-23.257 13.398-52.97 5.431-66.404-17.803ZM23.549 85.38a48.499 48.499 0 0 1 25.58-21.333v61.39a8.288 8.288 0 0 0 4.195 7.316l62.874 36.272-21.845 12.636a.819.819 0 0 1-.767 0L41.353 151.53c-23.211-13.454-31.171-43.144-17.804-66.405v.256Zm179.466 41.695-63.08-36.63L161.73 77.86a.819.819 0 0 1 .768 0l52.233 30.184a48.6 48.6 0 0 1-7.316 87.635v-61.391a8.544 8.544 0 0 0-4.4-7.213Zm21.742-32.69-1.535-.922-51.619-30.081a8.39 8.39 0 0 0-8.492 0L99.98 99.808V74.587a.716.716 0 0 1 .307-.665l52.233-30.133a48.652 48.652 0 0 1 72.236 50.391v.205ZM88.061 139.097l-21.845-12.585a.87.87 0 0 1-.41-.614V65.685a48.652 48.652 0 0 1 79.757-37.346l-1.535.87-51.67 29.825a8.595 8.595 0 0 0-4.246 7.367l-.051 72.697Zm11.868-25.58 28.138-16.217 28.188 16.218v32.434l-28.086 16.218-28.188-16.218-.052-32.434Z" />
    </svg>
  );
}

function buildUsageModel(snapshot: OctopusCodexUsageSnapshot | null) {
  const buckets = snapshot?.buckets ?? [];
  const totals = buckets.reduce((sum, bucket) => addBucket(sum, bucket), { ...EMPTY_TOTALS });
  const models = groupRows(buckets, (bucket) => bucket.model || "Unknown model");
  const days = snapshot ? enumerateDays(snapshot.sinceDay, snapshot.untilDay) : [];
  const byDay = new Map<string, UsageTotals>();
  for (const bucket of buckets) byDay.set(bucket.day, addBucket(byDay.get(bucket.day) ?? { ...EMPTY_TOTALS }, bucket));
  const daily = days.map((day) => ({ day, ...(byDay.get(day) ?? { ...EMPTY_TOTALS }) }));
  return {
    totals,
    models,
    daily,
    hasPricedUsage: totals.pricedTokens > 0,
  };
}

function groupRows(buckets: OctopusCodexUsageBucket[], keyOf: (bucket: OctopusCodexUsageBucket) => string): UsageRow[] {
  const grouped = new Map<string, UsageRow>();
  for (const bucket of buckets) {
    const key = keyOf(bucket);
    const current = grouped.get(key) ?? { id: key, label: key, priced: true, ...EMPTY_TOTALS };
    grouped.set(key, { ...current, ...addBucket(current, bucket), priced: current.priced && bucket.priced });
  }
  return Array.from(grouped.values()).sort((left, right) => right.costUsd - left.costUsd || right.totalTokens - left.totalTokens || left.label.localeCompare(right.label));
}

function addBucket(total: UsageTotals, bucket: OctopusCodexUsageBucket): UsageTotals {
  return {
    totalTokens: total.totalTokens + bucket.totalTokens,
    cachedInputTokens: total.cachedInputTokens + bucket.cachedInputTokens,
    uncachedInputTokens: total.uncachedInputTokens + bucket.uncachedInputTokens,
    cacheCreationTokens: total.cacheCreationTokens + bucket.cacheCreationTokens,
    outputTokens: total.outputTokens + bucket.outputTokens,
    reasoningTokens: total.reasoningTokens + bucket.reasoningTokens,
    costUsd: total.costUsd + bucket.costUsd,
    cacheSavingsUsd: total.cacheSavingsUsd + bucket.cacheSavingsUsd,
    pricedTokens: total.pricedTokens + (bucket.priced ? bucket.totalTokens : 0),
    unpricedTokens: total.unpricedTokens + (bucket.priced ? 0 : bucket.totalTokens),
    records: total.records + bucket.records,
  };
}

function buildChart(daily: DailyPoint[], metric: UsageMetric) {
  const values = daily.map((day) => metric === "cost" ? day.costUsd : day.totalTokens);
  const scale = niceScale(Math.max(0, ...values), 4);
  const stepX = daily.length <= 1 ? 0 : VIEW_WIDTH / (daily.length - 1);
  const toY = (value: number) => scale.max === 0 ? VIEW_HEIGHT : VIEW_HEIGHT - value / scale.max * (VIEW_HEIGHT - PLOT_TOP);
  const points = values.map((value, index) => ({ x: index * stepX, y: toY(value) }));
  const segments = smoothCurve(points);
  const line = curvePath(segments);
  const area = line ? `${line} L${VIEW_WIDTH},${VIEW_HEIGHT} L0,${VIEW_HEIGHT} Z` : "";
  return { area, line, stepX, ticks: scale.ticks, toY };
}

function niceScale(peak: number, count: number) {
  if (peak <= 0) return { max: 0, ticks: [0] };
  const raw = peak / count;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const normalized = raw / magnitude;
  const step = (normalized > 5 ? 10 : normalized > 2 ? 5 : normalized > 1 ? 2 : 1) * magnitude;
  const max = Math.ceil(peak / step) * step;
  const ticks: number[] = [];
  for (let value = 0; value <= max + step * 1e-6; value += step) ticks.push(value);
  return { max, ticks };
}

function smoothCurve(points: Point[]): CurveSegment[] {
  if (points.length < 2) return [];
  const slopes = points.slice(0, -1).map((point, index) => {
    const next = points[index + 1] ?? point;
    return next.x === point.x ? 0 : (next.y - point.y) / (next.x - point.x);
  });
  const tangents = points.map((_, index) => {
    if (index === 0) return slopes[0] ?? 0;
    if (index === points.length - 1) return slopes.at(-1) ?? 0;
    const previous = slopes[index - 1] ?? 0;
    const next = slopes[index] ?? 0;
    return previous * next <= 0 ? 0 : (previous + next) / 2;
  });
  for (let index = 0; index < slopes.length; index += 1) {
    const slope = slopes[index] ?? 0;
    if (slope === 0) {
      tangents[index] = 0;
      tangents[index + 1] = 0;
      continue;
    }
    const a = (tangents[index] ?? 0) / slope;
    const b = (tangents[index + 1] ?? 0) / slope;
    const magnitude = a * a + b * b;
    if (magnitude > 9) {
      const scale = 3 / Math.sqrt(magnitude);
      tangents[index] = scale * a * slope;
      tangents[index + 1] = scale * b * slope;
    }
  }
  return points.slice(0, -1).flatMap((from, index) => {
    const to = points[index + 1];
    if (!to) return [];
    const dx = to.x - from.x;
    return [{
      from,
      c1: { x: from.x + dx / 3, y: from.y + (tangents[index] ?? 0) * dx / 3 },
      c2: { x: to.x - dx / 3, y: to.y - (tangents[index + 1] ?? 0) * dx / 3 },
      to,
    }];
  });
}

function curvePath(segments: CurveSegment[]): string {
  const first = segments[0];
  if (!first) return "";
  return segments.reduce((path, segment, index) => `${path}${index === 0 ? `M${segment.from.x.toFixed(2)},${segment.from.y.toFixed(2)}` : ""} C${segment.c1.x.toFixed(2)},${segment.c1.y.toFixed(2)} ${segment.c2.x.toFixed(2)},${segment.c2.y.toFixed(2)} ${segment.to.x.toFixed(2)},${segment.to.y.toFixed(2)}`, "");
}

function enumerateDays(sinceDay: string, untilDay: string): string[] {
  const start = Date.parse(`${sinceDay}T00:00:00Z`);
  const end = Date.parse(`${untilDay}T00:00:00Z`);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return [];
  const days: string[] = [];
  for (let cursor = start; cursor <= end; cursor += 86_400_000) days.push(new Date(cursor).toISOString().slice(0, 10));
  return days;
}

function formatWindow(snapshot: OctopusCodexUsageSnapshot | null, windowDays: number): string {
  if (!snapshot) return `Last ${windowDays} days`;
  return `${formatDayShort(snapshot.sinceDay)} to ${formatDayShort(snapshot.untilDay)}`;
}

function formatPricingStatus(snapshot: OctopusCodexUsageSnapshot | null): string {
  if (!snapshot) return "Unavailable";
  if (snapshot.pricing.status === "unavailable") return "Unavailable";
  return snapshot.pricing.status === "fresh" ? "Current" : "Cached";
}

function formatDayShort(day: string): string {
  const parsed = Date.parse(`${day}T00:00:00Z`);
  return Number.isFinite(parsed) ? new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", timeZone: "UTC" }).format(parsed) : day;
}

function formatTokens(value: number): string {
  const absolute = Math.abs(value);
  const divisor = absolute >= 1e12 ? 1e12 : absolute >= 1e9 ? 1e9 : absolute >= 1e6 ? 1e6 : absolute >= 1e3 ? 1e3 : 1;
  const suffix = divisor === 1e12 ? "T" : divisor === 1e9 ? "B" : divisor === 1e6 ? "M" : divisor === 1e3 ? "K" : "";
  if (divisor === 1) return formatCount(value);
  const scaled = value / divisor;
  const digits = Math.abs(scaled) >= 100 ? 0 : Math.abs(scaled) >= 10 ? 1 : 2;
  return `${scaled.toFixed(digits).replace(/\.0+$/, "")}${suffix}`;
}

function formatUsd(value: number): string {
  return new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(value);
}

function formatCount(value: number): string {
  return new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 }).format(Math.round(value));
}

function formatPercent(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}
