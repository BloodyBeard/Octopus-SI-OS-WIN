import type { OctopusScheduledDraft } from "../../electron";
import type { ScheduledEditorDraft, ScheduledInterfaceProjection } from "./types";

export function normalizeScheduledInterface(value: unknown): ScheduledInterfaceProjection | null {
  const row = objectValue(value);
  if (!row) return null;
  const interfaceId = stringValue(row.interface_id);
  const status = row.status === "enabled" || row.status === "disabled" || row.status === "invalid" ? row.status : "invalid";
  if (!interfaceId) return null;
  const rawSchedule = objectValue(row.schedule);
  const schedule = rawSchedule?.type === "interval" && positiveNumber(rawSchedule.interval_seconds)
    ? { type: "interval" as const, intervalSeconds: rawSchedule.interval_seconds as number }
    : rawSchedule?.type === "cron" && stringValue(rawSchedule.cron)
      ? { type: "cron" as const, cron: stringValue(rawSchedule.cron) }
      : rawSchedule?.type === "once" && Number.isFinite(Date.parse(stringValue(rawSchedule.due_at)))
        ? { type: "once" as const, dueAt: stringValue(rawSchedule.due_at) }
        : null;
  const rawTemplate = objectValue(row.run_template);
  const runTemplate = rawTemplate ? {
    runId: stringValue(rawTemplate.run_id),
    eeId: stringValue(rawTemplate.ee_id),
    workerInterfaceId: stringValue(rawTemplate.worker_interface_id),
    checkpointCount: nonNegativeNumber(rawTemplate.checkpoint_count),
    outboundInterfaceIds: stringArray(rawTemplate.outbound_interface_ids),
    artifactBindingCount: nonNegativeNumber(rawTemplate.artifact_binding_count),
  } : null;
  const valid = row.valid === true
    && status !== "invalid"
    && Boolean(schedule)
    && Boolean(runTemplate?.runId && runTemplate.eeId && runTemplate.workerInterfaceId)
    && /^[0-9a-f]{64}$/.test(stringValue(row.activation_digest));
  return {
    interfaceId,
    displayName: stringValue(row.display_name) || interfaceId,
    status: valid ? status : "invalid",
    valid,
    activationDigest: stringValue(row.activation_digest),
    activationPath: stringValue(row.activation_path),
    descriptorPath: stringValue(row.descriptor_path),
    schedule,
    timezone: stringValue(row.timezone),
    fireOnStart: row.fire_on_start === true,
    instructions: typeof row.instructions === "string" ? row.instructions : "",
    reminderEnv: stringValue(row.reminder_env),
    runTemplate,
    cloneEligible: valid && row.clone_eligible === true,
    lastRunAtMs: nullableTimestamp(row.last_run_at_ms),
    lastMovementSeq: nullableInteger(row.last_movement_seq),
    nextRunAtMs: nullableTimestamp(row.next_run_at_ms),
    nextRunSource: row.next_run_source === "driver_state" || row.next_run_source === "schedule" ? row.next_run_source : "",
    error: stringValue(row.error) || (valid ? "" : "The activation does not satisfy the cron_life contract."),
  };
}

export function draftFromScheduledInterface(task: ScheduledInterfaceProjection): ScheduledEditorDraft {
  return {
    interfaceId: task.interfaceId,
    sourceInterfaceId: task.interfaceId,
    type: task.schedule?.type ?? "interval",
    intervalSeconds: task.schedule?.type === "interval" ? task.schedule.intervalSeconds : 3600,
    cron: task.schedule?.type === "cron" ? task.schedule.cron : "0 9 * * 1-5",
    dueAt: task.schedule?.type === "once" ? task.schedule.dueAt : "",
    timezone: task.timezone || "local",
    fireOnStart: task.fireOnStart,
    instructions: task.instructions,
  };
}

export function draftFromTemplate(source: ScheduledInterfaceProjection): ScheduledEditorDraft {
  const base = draftFromScheduledInterface(source);
  return {
    ...base,
    interfaceId: nextScheduledInterfaceId(source.interfaceId),
    sourceInterfaceId: source.interfaceId,
    fireOnStart: false,
  };
}

export function mutationDraft(draft: ScheduledEditorDraft): OctopusScheduledDraft {
  return {
    type: draft.type,
    ...(draft.type === "interval" ? { intervalSeconds: draft.intervalSeconds } : draft.type === "once" ? { dueAt: draft.dueAt } : { cron: draft.cron }),
    timezone: draft.timezone.trim(),
    fireOnStart: draft.fireOnStart,
    instructions: draft.instructions,
  };
}

export function scheduledDraftError(draft: ScheduledEditorDraft, creating: boolean): string {
  if (creating && !/^[a-z0-9][a-z0-9_-]*$/.test(draft.interfaceId)) return "Interface id must use lowercase letters, digits, underscores, or hyphens.";
  if (!draft.instructions.trim()) return "Instructions are required.";
  if (!draft.timezone.trim()) return "Timezone is required.";
  if (draft.type === "interval") {
    if (!positiveNumber(draft.intervalSeconds)) return "Interval must be greater than zero.";
  } else if (draft.type === "once") {
    if (!draft.dueAt || !Number.isFinite(Date.parse(draft.dueAt))) return "Choose a date and time.";
    if (draft.fireOnStart) return "A one-time task runs at its chosen time.";
  } else if (!validCronExpression(draft.cron ?? "")) {
    return "Cron must use five numeric fields: minute hour day-of-month month day-of-week.";
  }
  return "";
}

export function formatSchedule(task: Pick<ScheduledInterfaceProjection, "schedule" | "timezone">): string {
  if (task.schedule?.type === "once") return describeSchedule(task);
  const timezone = task.timezone?.trim();
  return timezone ? `${describeSchedule(task)} · ${timezone}` : describeSchedule(task);
}

export function describeSchedule(task: Pick<ScheduledInterfaceProjection, "schedule">): string {
  if (!task.schedule) return "Invalid schedule";
  if (task.schedule.type === "interval") return formatInterval(task.schedule.intervalSeconds);
  if (task.schedule.type === "once") {
    const at = Date.parse(task.schedule.dueAt);
    return Number.isFinite(at) ? `Once · ${formatEvidenceTime(at)}` : "Choose a date and time";
  }
  return describeCron(task.schedule.cron);
}

export function scheduleExpression(task: Pick<ScheduledInterfaceProjection, "schedule">): string {
  if (!task.schedule) return "None";
  if (task.schedule.type === "once") return task.schedule.dueAt;
  return task.schedule.type === "cron" ? task.schedule.cron : `${task.schedule.intervalSeconds}s interval`;
}

export function localTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "";
}

export function nextScheduledRunAtMs(
  task: Pick<ScheduledInterfaceProjection, "schedule" | "status" | "timezone" | "lastRunAtMs" | "nextRunAtMs" | "valid">,
  fromMs: number = Date.now(),
): number | null {
  if (!task.valid || task.status !== "enabled" || !task.schedule) return null;
  if (task.schedule.type === "once") return task.nextRunAtMs;
  if (task.nextRunAtMs && task.nextRunAtMs > fromMs) return task.nextRunAtMs;
  if (task.schedule.type === "interval") {
    const period = task.schedule.intervalSeconds * 1000;
    if (period <= 0) return null;
    let next = (task.lastRunAtMs ?? fromMs) + period;
    while (next <= fromMs) next += period;
    return next;
  }
  return nextCronRunAtMs(task.schedule.cron, task.timezone, fromMs);
}

type CronFields = {
  daysOfMonth: Set<number>;
  daysOfWeek: Set<number>;
  domRestricted: boolean;
  dowRestricted: boolean;
  hours: Set<number>;
  minutes: Set<number>;
  months: Set<number>;
};

const WEEKDAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const WEEKDAY_SHORT = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

export function describeCron(expression: string): string {
  const raw = expression.trim().split(/\s+/);
  const fields = parseCron(expression);
  if (!fields || raw.length !== 5) return expression;
  const [rawMinute, rawHour, rawDom, rawMonth, rawDow] = raw;
  if (rawHour === "*" && rawDom === "*" && rawMonth === "*" && rawDow === "*") {
    if (rawMinute === "*") return "Every minute";
    const step = stepOf(rawMinute);
    if (step) return `Every ${plural(step, "minute", "minutes")}`;
    if (fields.minutes.size === 1) return `Hourly at :${String([...fields.minutes][0]).padStart(2, "0")}`;
    return `${plural(fields.minutes.size, "time", "times")} an hour`;
  }
  if (rawMonth !== "*" || fields.minutes.size !== 1) return expression;
  const time = clockText(fields);
  if (!time) return expression;
  if (rawDom === "*" && rawDow === "*") {
    return time.startsWith("every ") ? `${time[0].toLocaleUpperCase()}${time.slice(1)}` : `Daily at ${time}`;
  }
  if (rawDom === "*") return joinClock(weekdayText(fields.daysOfWeek), time);
  if (rawDow === "*" && fields.daysOfMonth.size === 1) {
    return joinClock(`Monthly on the ${ordinal([...fields.daysOfMonth][0])}`, time);
  }
  return expression;
}

function joinClock(prefix: string, time: string): string {
  return time.startsWith("every ") ? `${prefix} ${time}` : `${prefix} at ${time}`;
}

function clockText(fields: CronFields): string {
  const minute = [...fields.minutes][0];
  const hours = [...fields.hours].sort((left, right) => left - right);
  if (hours.length === 1) return formatClock(hours[0], minute);
  if (hours.length > 3) {
    const step = hours[1] - hours[0];
    const even = hours.every((hour, index) => index === 0 || hour - hours[index - 1] === step);
    if (even) return `every ${plural(step, "hour", "hours")}`;
  }
  return hours.map((hour) => formatClock(hour, minute)).join(", ");
}

function formatClock(hour: number, minute: number): string {
  const date = new Date(Date.UTC(2024, 0, 1, hour, minute));
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", timeZone: "UTC" }).format(date);
}

function weekdayText(days: Set<number>): string {
  const sorted = [...days].sort((left, right) => left - right);
  const key = sorted.join(",");
  if (key === "1,2,3,4,5") return "Weekdays";
  if (key === "0,6") return "Weekends";
  if (sorted.length === 1) return `Every ${WEEKDAY_NAMES[sorted[0]]}`;
  return sorted.map((day) => WEEKDAY_SHORT[day]).join(", ");
}

function ordinal(day: number): string {
  const rest = day % 100;
  if (rest >= 11 && rest <= 13) return `${day}th`;
  const last = day % 10;
  return `${day}${last === 1 ? "st" : last === 2 ? "nd" : last === 3 ? "rd" : "th"}`;
}

function stepOf(field: string): number | null {
  const match = /^\*\/(\d+)$/.exec(field);
  if (!match) return null;
  const step = Number(match[1]);
  return step > 1 ? step : null;
}

function parseCron(expression: string): CronFields | null {
  const parts = expression.trim().split(/\s+/);
  if (parts.length !== 5) return null;
  const minutes = parseCronField(parts[0], 0, 59);
  const hours = parseCronField(parts[1], 0, 23);
  const daysOfMonth = parseCronField(parts[2], 1, 31);
  const months = parseCronField(parts[3], 1, 12);
  const daysOfWeek = parseCronField(parts[4], 0, 7);
  if (!minutes || !hours || !daysOfMonth || !months || !daysOfWeek) return null;
  if (daysOfWeek.delete(7)) daysOfWeek.add(0);
  return {
    daysOfMonth,
    daysOfWeek,
    domRestricted: parts[2] !== "*",
    dowRestricted: parts[4] !== "*",
    hours,
    minutes,
    months,
  };
}

function parseCronField(field: string, min: number, max: number): Set<number> | null {
  const values = new Set<number>();
  for (const chunk of field.split(",")) {
    const [range, stepText] = chunk.split("/");
    const step = stepText === undefined ? 1 : Number(stepText);
    if (!Number.isInteger(step) || step < 1) return null;
    let start = min;
    let end = max;
    if (range !== "*" && range !== "") {
      const bounds = range.split("-");
      if (bounds.length > 2) return null;
      start = Number(bounds[0]);
      end = bounds.length === 2 ? Number(bounds[1]) : stepText === undefined ? start : max;
      if (!Number.isInteger(start) || !Number.isInteger(end) || start < min || end > max || start > end) return null;
    }
    for (let value = start; value <= end; value += step) values.add(value);
  }
  return values.size ? values : null;
}

function nextCronRunAtMs(cron: string, timezone: string, fromMs: number): number | null {
  const fields = parseCron(cron);
  if (!fields) return null;
  const zone = timezone.trim() || localTimezone() || "UTC";
  let offset = 0;
  try {
    offset = timezoneOffsetMs(zone, fromMs);
  } catch {
    return null;
  }
  const cursor = new Date(fromMs + offset);
  cursor.setUTCSeconds(0, 0);
  cursor.setUTCMinutes(cursor.getUTCMinutes() + 1);
  const limit = fromMs + offset + 366 * 86_400_000;
  while (cursor.getTime() <= limit) {
    if (!fields.months.has(cursor.getUTCMonth() + 1)) {
      cursor.setUTCHours(0, 0, 0, 0);
      cursor.setUTCDate(1);
      cursor.setUTCMonth(cursor.getUTCMonth() + 1);
      continue;
    }
    if (!cronDayMatches(fields, cursor)) {
      cursor.setUTCHours(0, 0, 0, 0);
      cursor.setUTCDate(cursor.getUTCDate() + 1);
      continue;
    }
    if (!fields.hours.has(cursor.getUTCHours())) {
      cursor.setUTCMinutes(0, 0, 0);
      cursor.setUTCHours(cursor.getUTCHours() + 1);
      continue;
    }
    if (!fields.minutes.has(cursor.getUTCMinutes())) {
      cursor.setUTCMinutes(cursor.getUTCMinutes() + 1);
      continue;
    }
    const wall = cursor.getTime();
    try {
      return wall - timezoneOffsetMs(zone, wall - offset);
    } catch {
      return wall - offset;
    }
  }
  return null;
}

function cronDayMatches(fields: CronFields, cursor: Date): boolean {
  const dayOfMonth = fields.daysOfMonth.has(cursor.getUTCDate());
  const dayOfWeek = fields.daysOfWeek.has(cursor.getUTCDay());
  if (fields.domRestricted && fields.dowRestricted) return dayOfMonth || dayOfWeek;
  if (fields.domRestricted) return dayOfMonth;
  if (fields.dowRestricted) return dayOfWeek;
  return true;
}

function timezoneOffsetMs(timeZone: string, at: number): number {
  const parts = new Intl.DateTimeFormat("en-US", {
    day: "2-digit",
    hour: "2-digit",
    hour12: false,
    minute: "2-digit",
    month: "2-digit",
    second: "2-digit",
    timeZone,
    year: "numeric",
  }).formatToParts(new Date(at));
  const values: Record<string, number> = {};
  for (const part of parts) {
    if (part.type !== "literal") values[part.type] = Number(part.value);
  }
  const hour = values.hour === 24 ? 0 : values.hour;
  const asUtc = Date.UTC(values.year, values.month - 1, values.day, hour, values.minute, values.second);
  return asUtc - (at - (at % 1000));
}

export function formatInterval(seconds: number): string {
  if (seconds % 86_400 === 0) return `Every ${plural(seconds / 86_400, "day", "days")}`;
  if (seconds % 3_600 === 0) return `Every ${plural(seconds / 3_600, "hour", "hours")}`;
  if (seconds % 60 === 0) return `Every ${plural(seconds / 60, "minute", "minutes")}`;
  return `Every ${plural(seconds, "second", "seconds")}`;
}

export function formatEvidenceTime(timestamp: number | null): string {
  if (!timestamp) return "Not recorded";
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(timestamp));
}

export function relativeEvidenceTime(timestamp: number | null): string {
  if (!timestamp) return "";
  const delta = timestamp - Date.now();
  const future = delta > 0;
  const magnitude = Math.abs(delta);
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (magnitude < 90_000) return formatter.format(future ? 1 : -1, "minute");
  if (magnitude < 5_400_000) return formatter.format(Math.round(delta / 60_000), "minute");
  if (magnitude < 129_600_000) return formatter.format(Math.round(delta / 3_600_000), "hour");
  return formatter.format(Math.round(delta / 86_400_000), "day");
}

function nextScheduledInterfaceId(source: string): string {
  const stem = source.replace(/^(world|system)_/, "").replace(/_cron$/, "").replace(/[^a-z0-9_-]/g, "_");
  return `world_${stem || "scheduled"}_${new Date().toISOString().slice(0, 10).replaceAll("-", "")}`;
}

function validCronExpression(expression: string): boolean {
  const fields = expression.trim().split(/\s+/);
  return fields.length === 5 && fields.every((field) => /^[0-9*/,-]+$/.test(field));
}

function plural(value: number, singular: string, pluralForm: string): string {
  return `${value} ${value === 1 ? singular : pluralForm}`;
}

function objectValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.map(stringValue).filter(Boolean) : [];
}

function positiveNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function nonNegativeNumber(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

function nullableTimestamp(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
}

function nullableInteger(value: unknown): number | null {
  return Number.isSafeInteger(value) && (value as number) >= 0 ? value as number : null;
}
