export type MovementEndpoint = {
  kind?: string;
  id?: string;
  ee_id?: string;
};

export type MovementRow = {
  raw_run_id?: string;
  raw_ee_id?: string;
  kind?: string;
  recorded_at_unix_ms?: number;
  artifact_id?: string;
  artifact_label?: string;
  content_hash?: string;
  blob_size?: number | null;
  from?: MovementEndpoint;
  to?: MovementEndpoint;
};

export type ActivityHeatCell = {
  count: number;
  level: number;
  label: string;
};

export type ActivityPoint = {
  bytes: number;
  count: number;
  label: string;
  shortLabel: string;
  timestamp: number;
};

export type ActivityMetrics = {
  movements: number;
  environments: number;
  runs: number;
  interfaces: number;
  marks: number;
  bytesMoved: number;
  activeDays: number;
  currentStreak: number;
  longestStreak: number;
  peakDay: number;
  peakBucket: number;
  daily: ActivityPoint[];
  weeks: ActivityHeatCell[][];
  monthLabels: string[];
  topInterfaces: Array<{ name: string; count: number }>;
  busiestEnvironment: string;
  repoRoot: string;
  ledgerWindowAtLimit: boolean;
};

export const ACTIVITY_HEAT_WEEKS = 30;
export const MOVEMENT_READ_LIMIT = 20_000;

const DAY_MS = 86_400_000;

export function computeActivityMetrics({
  movements,
  environments,
  runs,
  interfaces,
  marks,
  repoRoot = "",
  ledgerWindowAtLimit = false,
  rangeDays,
  rangeStartMs,
  rangeEndMs,
  bucketCount,
}: {
  movements: MovementRow[];
  environments: number;
  runs: number;
  interfaces: number;
  marks: number;
  repoRoot?: string;
  ledgerWindowAtLimit?: boolean;
  rangeDays?: number;
  rangeStartMs?: number;
  rangeEndMs?: number;
  bucketCount?: number;
}): ActivityMetrics {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const normalizedRangeDays = rangeDays == null ? null : Math.max(1, Math.floor(rangeDays));
  const windowEnd = Number.isFinite(rangeEndMs) ? Number(rangeEndMs) : Date.now();
  const windowStart = Number.isFinite(rangeStartMs)
    ? Number(rangeStartMs)
    : normalizedRangeDays == null
      ? Number.NEGATIVE_INFINITY
      : today.getTime() - (normalizedRangeDays - 1) * DAY_MS;
  const firstIncludedDay = normalizedRangeDays == null
    ? Number.NEGATIVE_INFINITY
    : localDayOrdinal(today.getTime()) - normalizedRangeDays + 1;
  const artifactMovements = movements
    .filter(isArtifactMovement)
    .filter((movement) => {
      const timestamp = Number(movement.recorded_at_unix_ms) || 0;
      if (Number.isFinite(windowStart)) return timestamp >= windowStart && timestamp <= windowEnd;
      if (normalizedRangeDays == null) return true;
      return timestamp > 0 && localDayOrdinal(timestamp) >= firstIncludedDay;
    });
  const perDay = new Map<number, number>();
  const bytesPerDay = new Map<number, number>();
  const interfaceCounts = new Map<string, number>();
  const environmentCounts = new Map<string, number>();
  let bytesMoved = 0;

  for (const movement of artifactMovements) {
    const timestamp = Number(movement.recorded_at_unix_ms) || 0;
    if (timestamp > 0) {
      const day = localDayOrdinal(timestamp);
      perDay.set(day, (perDay.get(day) ?? 0) + 1);
      bytesPerDay.set(day, (bytesPerDay.get(day) ?? 0) + (Number(movement.blob_size) || 0));
    }
    bytesMoved += Number(movement.blob_size) || 0;
    for (const endpoint of [movement.from, movement.to]) {
      if (!endpoint) continue;
      if (endpoint.kind === "interface") {
        const name = interfaceNameOf(endpoint.id);
        if (name) interfaceCounts.set(name, (interfaceCounts.get(name) ?? 0) + 1);
      }
    }
    if (movement.raw_ee_id) {
      environmentCounts.set(movement.raw_ee_id, (environmentCounts.get(movement.raw_ee_id) ?? 0) + 1);
    }
  }

  const heatWeeks = normalizedRangeDays == null
    ? ACTIVITY_HEAT_WEEKS
    : Math.max(1, Math.ceil((normalizedRangeDays + today.getDay()) / 7));
  const weekStart = new Date(today);
  weekStart.setDate(today.getDate() - today.getDay());
  const gridStart = new Date(weekStart);
  gridStart.setDate(weekStart.getDate() - (heatWeeks - 1) * 7);
  const weeks: ActivityHeatCell[][] = [];
  const monthLabels: string[] = [];
  let peakDay = 0;
  let lastMonth = -1;

  for (let week = 0; week < heatWeeks; week += 1) {
    const column: ActivityHeatCell[] = [];
    for (let day = 0; day < 7; day += 1) {
      const date = new Date(gridStart);
      date.setDate(gridStart.getDate() + week * 7 + day);
      const dayKey = localDayOrdinal(date.getTime());
      const count = date.getTime() <= today.getTime() && dayKey >= firstIncludedDay ? perDay.get(dayKey) ?? 0 : 0;
      peakDay = Math.max(peakDay, count);
      column.push({
        count,
        level: 0,
        label: date.toLocaleDateString(undefined, { month: "short", day: "numeric" }),
      });
    }
    weeks.push(column);
    const firstDay = new Date(gridStart);
    firstDay.setDate(gridStart.getDate() + week * 7);
    const month = firstDay.getMonth();
    monthLabels.push(month === lastMonth ? "" : firstDay.toLocaleDateString(undefined, { month: "short" }));
    lastMonth = month;
  }

  for (const column of weeks) {
    for (const cell of column) {
      cell.level = cell.count === 0
        ? 0
        : Math.min(4, Math.max(1, Math.ceil((cell.count / Math.max(peakDay, 1)) * 4)));
    }
  }

  const seriesEnd = Number.isFinite(windowEnd) ? windowEnd : Date.now();
  const seriesStart = Number.isFinite(windowStart) ? windowStart : seriesEnd - ACTIVITY_HEAT_WEEKS * 7 * DAY_MS;
  const seriesDuration = Math.max(1, seriesEnd - seriesStart);
  const seriesBuckets = Math.max(1, Math.floor(bucketCount ?? Math.min(90, Math.max(1, Math.ceil(seriesDuration / DAY_MS)))));
  const bucketDuration = seriesDuration / seriesBuckets;
  const daily: ActivityPoint[] = Array.from({ length: seriesBuckets }, (_, index) => {
    const timestamp = seriesStart + (index + 0.5) * bucketDuration;
    const date = new Date(timestamp);
    return {
      bytes: 0,
      count: 0,
      label: seriesDuration <= DAY_MS
        ? date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
        : date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: seriesDuration > 180 * DAY_MS ? "numeric" : undefined }),
      shortLabel: seriesDuration <= DAY_MS
        ? date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
        : date.toLocaleDateString(undefined, { month: "short", day: "numeric" }),
      timestamp,
    };
  });
  for (const movement of artifactMovements) {
    const timestamp = Number(movement.recorded_at_unix_ms) || 0;
    if (timestamp < seriesStart || timestamp > seriesEnd) continue;
    const index = Math.min(seriesBuckets - 1, Math.max(0, Math.floor((timestamp - seriesStart) / bucketDuration)));
    daily[index].count += 1;
    daily[index].bytes += Number(movement.blob_size) || 0;
  }
  const peakBucket = daily.reduce((maximum, point) => Math.max(maximum, point.count), 0);

  return {
    movements: artifactMovements.length,
    environments,
    runs,
    interfaces,
    marks,
    bytesMoved,
    activeDays: perDay.size,
    currentStreak: currentStreak(perDay),
    longestStreak: longestStreak(perDay),
    peakDay,
    peakBucket,
    daily,
    weeks,
    monthLabels,
    topInterfaces: [...interfaceCounts.entries()]
      .sort((left, right) => right[1] - left[1])
      .slice(0, 5)
      .map(([name, count]) => ({ name, count })),
    busiestEnvironment: cleanEnvironmentName(
      [...environmentCounts.entries()].sort((left, right) => right[1] - left[1])[0]?.[0] ?? "",
    ),
    repoRoot,
    ledgerWindowAtLimit,
  };
}

export function emptyActivityWeeks(): ActivityHeatCell[][] {
  return Array.from(
    { length: ACTIVITY_HEAT_WEEKS },
    () => Array.from({ length: 7 }, () => ({ count: 0, level: 0, label: "" })),
  );
}

export function compactMetricCount(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`;
  return String(value);
}

export function formatMetricBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const exponent = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
  return `${(bytes / 1024 ** exponent).toFixed(exponent === 0 ? 0 : 1)} ${units[exponent]}`;
}

export function formatMetricDays(days: number): string {
  return `${days} ${days === 1 ? "day" : "days"}`;
}

function isArtifactMovement(movement: MovementRow): boolean {
  return [movement.artifact_id, movement.artifact_label, movement.content_hash]
    .some((value) => typeof value === "string" && value.trim().length > 0);
}

function localDayOrdinal(timestamp: number): number {
  const date = new Date(timestamp);
  return Math.floor(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / DAY_MS);
}

function longestStreak(perDay: Map<number, number>): number {
  const days = [...perDay.keys()].sort((left, right) => left - right);
  let best = 0;
  let current = 0;
  let previous = Number.NaN;
  for (const day of days) {
    current = day === previous + 1 ? current + 1 : 1;
    best = Math.max(best, current);
    previous = day;
  }
  return best;
}

function currentStreak(perDay: Map<number, number>): number {
  const today = localDayOrdinal(Date.now());
  let cursor = perDay.has(today) ? today : today - 1;
  let streak = 0;
  while (perDay.has(cursor)) {
    streak += 1;
    cursor -= 1;
  }
  return streak;
}

function interfaceNameOf(id: string | undefined): string {
  return id ?? "";
}

function cleanEnvironmentName(id: string): string {
  if (!id) return "";
  return id.split(":").pop() || id;
}
