import type { OctopusChatgptLimit, OctopusChatgptUsage } from "../electron";

export type AccountUsageWindow = {
  key: string;
  label: string;
  compactLabel: string;
  durationMins: number | null;
  reported: boolean;
  remaining: number | null;
  resetsAt: number | null;
};
export type AccountUsageGroup = { key: string; label: string; limit: OctopusChatgptLimit; windows: AccountUsageWindow[] };

export function accountUsageGroups(usage: OctopusChatgptUsage): AccountUsageGroup[] {
  const entries: Array<[string, OctopusChatgptLimit]> = usage.rateLimitsByLimitId === null
    ? [[usage.rateLimits.limitId ?? "account", usage.rateLimits]]
    : Object.entries(usage.rateLimitsByLimitId);
  return entries.map(([key, limit]) => ({
    key, limit, label: limit.limitName || limit.limitId || "Account usage",
    windows: (["primary", "secondary"] as const).map((slot) => {
      const window = limit[slot];
      const duration = window?.windowDurationMins ?? null;
      const label = duration === 300 ? "5 hour usage limit" : duration === 10_080 ? "Weekly usage limit"
        : duration === 1440 ? "Daily usage limit" : "Usage limit";
      const compactLabel = duration === 300 ? "5h" : duration === 10_080 ? "Weekly"
        : duration === 1440 ? "Daily" : duration && duration > 0 ? `${duration}m` : "Usage";
      return { key: slot, label, compactLabel, durationMins: duration, reported: window != null,
        remaining: window && Number.isFinite(window.usedPercent) ? Math.max(0, Math.min(100, 100 - window.usedPercent)) : null,
        resetsAt: window?.resetsAt != null && Number.isFinite(window.resetsAt) ? window.resetsAt : null };
    }),
  }));
}

export function accountResetTime(resetsAt: number | null, durationMins: number | null = null, nowMs = Date.now()): string {
  if (resetsAt === null) return "Reset time unavailable";
  const date = new Date(resetsAt * 1000);
  if (Number.isNaN(date.getTime())) return "Reset time unavailable";
  const seconds = Math.floor((date.getTime() - nowMs) / 1000);
  const time = durationMins === 10_080
    ? new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date)
    : seconds <= 0 ? new Intl.RelativeTimeFormat(undefined, { numeric: "auto" }).format(0, "second")
      : seconds < 86_400 ? new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(date)
        : new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(date);
  return `Resets ${time}`;
}

export function accountPlanLabel(plan: string | null | undefined): string {
  return plan?.trim() ? plan.replace(/_/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase()) : "Plan unavailable";
}

export function accountRemainingLabel(remaining: number | null, suffix = "left"): string {
  return remaining === null ? "Unavailable" : `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 }).format(remaining)}% ${suffix}`;
}
