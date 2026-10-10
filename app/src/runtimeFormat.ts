export function array(value: unknown): any[] {
  return Array.isArray(value) ? value : [];
}

export function record(value: unknown): Record<string, any> | undefined {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, any> : undefined;
}

export function readPath(obj: unknown, path: string): unknown {
  if (!path) return obj;
  return path.split(".").reduce<unknown>((acc, key) => (
    acc && typeof acc === "object" ? (acc as Record<string, unknown>)[key] : undefined
  ), obj);
}

export function stringValue(value: unknown): string {
  if (typeof value === "string") return value.trim();
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  if (typeof value === "boolean") return value ? "true" : "false";
  return "";
}

export function shortId(value: unknown, head = 10, tail = 5): string {
  const text = stringValue(value);
  if (!text) return "";
  if (text.length <= head + tail + 4) return text;
  return `${text.slice(0, head)}...${text.slice(-tail)}`;
}

export function formatCell(value: unknown): string {
  if (value === null || value === undefined || value === "") return "-";
  if (Array.isArray(value)) return value.length ? value.map((item) => formatCell(item)).join(", ") : "-";
  if (typeof value === "object") {
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }
  return String(value);
}

export function jsonText(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export async function copyText(value: string): Promise<boolean> {
  if (window.octopusTransport?.copyText) {
    await window.octopusTransport.copyText(value);
    return true;
  }
  if (!navigator.clipboard?.writeText) return false;
  await navigator.clipboard.writeText(value);
  return true;
}

export function timeOf(row: any): number {
  const value = row?.recorded_at_unix_ms ?? row?.created_at_unix_ms ?? row?.ts_unix_ms ?? row?.created_at ?? row?.accepted_at ?? row?.ts ?? row?.time;
  if (typeof value === "number") return value > 10_000_000_000 ? value : value * 1000;
  if (typeof value === "string") {
    const parsed = Date.parse(value);
    return Number.isNaN(parsed) ? 0 : parsed;
  }
  return 0;
}

export function formatTime(ms: number): string {
  if (!ms) return "-";
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleTimeString([], { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

export function formatDateTime(value: unknown): string {
  const text = stringValue(value);
  if (!text) return "-";
  const parsed = Date.parse(text);
  if (Number.isNaN(parsed)) return text;
  return new Date(parsed).toLocaleString([], {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function countBy<T>(items: T[], getKey: (item: T) => string): Array<{ key: string; count: number }> {
  const counts = new Map<string, number>();
  for (const item of items) {
    const key = getKey(item) || "unknown";
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return Array.from(counts.entries())
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([key, count]) => ({ key, count }));
}
