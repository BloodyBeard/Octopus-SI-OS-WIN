export const statusKeys = ["projected", "active", "idle", "settled", "done", "blocked", "stale", "offline", "unknown"] as const;
export type FilterStatus = (typeof statusKeys)[number];

export type DashboardScope = "System" | "World";
export type DashboardTopologyMode = "drilldown" | "zones";

export type DashboardFilters = {
  statuses: Record<FilterStatus, boolean>;
};

export const statusLabels: Record<FilterStatus, string> = {
  projected: "Projected",
  active: "Active",
  idle: "Idle",
  settled: "Settled",
  done: "Completed",
  blocked: "Blocked",
  stale: "Stale",
  offline: "Offline",
  unknown: "Unknown",
};

export const defaultDashboardFilters: DashboardFilters = {
  statuses: {
    projected: true,
    active: true,
    idle: true,
    settled: true,
    done: true,
    blocked: true,
    stale: true,
    offline: true,
    unknown: true,
  },
};

export function enabledFilterCount(filters: DashboardFilters): number {
  return statusKeys.map((key) => filters.statuses[key]).filter(Boolean).length;
}
