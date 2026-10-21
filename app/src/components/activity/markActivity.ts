import { AGENT_PRESET_ICONS, type ClientAgentPresetIcon } from "../../agentPresets";
import type { MarkActivityMode } from "../../clientPreferences";

export type ActivityKind = "message" | "approval" | "user_input" | "elicitation" | "mark";
export type MarkActivityItem = Readonly<{
  id: string;
  kind: ActivityKind;
  title: string;
  agentId: string | null;
  agentName: string | null;
  agentIcon: ClientAgentPresetIcon | null;
  agentColor: string | null;
  movementId: string | null;
  requestId: string | null;
  clientSurfaceRole: "home" | "task" | null;
  navigation: { kind: "run"; runId: string; clientSurfaceRole: "home" | "task" } | { kind: "mark"; movementId: string };
  name: string;
  worker: string;
  workerOriginal: string;
  sourceTitle: string | null;
  excerpt: string | null;
  runId: string;
  artifactId: string;
  createdAt: number | null;
  sequence: number | null;
}>;

export type MarkActivityGroup = { key: string; label: string; items: MarkActivityItem[] };

export function decodeMarkActivity(value: unknown): MarkActivityItem[] {
  if (!value || typeof value !== "object" || Array.isArray(value) || !("marks" in value) || !Array.isArray(value.marks)) {
    throw new Error("The mark projection returned an invalid row set");
  }
  const identities = new Set<string>();
  return value.marks.map((row: unknown) => {
    if (!row || typeof row !== "object" || Array.isArray(row)) throw new Error("The mark projection returned an invalid row");
    const item = row as Record<string, unknown>;
    for (const key of ["movement_id", "mark_name", "worker", "run_id", "artifact_id", "created_at"]) {
      if (typeof item[key] !== "string") throw new Error(`The mark projection has an invalid ${key}`);
    }
    const id = item.movement_id as string;
    const name = item.mark_name as string;
    if (!id || !name || identities.has(id)) throw new Error("The mark projection has a missing or duplicate identity");
    identities.add(id);
    const time = Date.parse(item.created_at as string);
    if (item.created_at && !Number.isFinite(time)) throw new Error(`Invalid mark timestamp: ${id}`);
    if (item.seq !== null && (typeof item.seq !== "number" || !Number.isSafeInteger(item.seq) || item.seq < 0)) {
      throw new Error(`Invalid mark sequence: ${id}`);
    }
    return {
      id, name,
      kind: "mark" as const, title: name, agentId: null, agentName: null, agentIcon: null, agentColor: null,
      movementId: id, requestId: null, clientSurfaceRole: null,
      navigation: { kind: "mark" as const, movementId: id },
      worker: item.worker as string,
      workerOriginal: typeof item.worker_original === "string" && item.worker_original ? item.worker_original : item.worker as string,
      sourceTitle: null,
      excerpt: null,
      runId: item.run_id as string,
      artifactId: item.artifact_id as string,
      createdAt: Number.isFinite(time) ? time : null,
      sequence: item.seq as number | null,
    };
  }).sort((a, b) => (b.createdAt ?? -1) - (a.createdAt ?? -1) || (b.sequence ?? -1) - (a.sequence ?? -1) || a.id.localeCompare(b.id));
}

function activityIdentity(row: Record<string, unknown>) {
  for (const key of ["agent_id", "agent_name", "agent_icon", "agent_color"]) {
    if (row[key] !== null && typeof row[key] !== "string") throw new Error(`Invalid Activity ${key}`);
  }
  const icon = row.agent_icon;
  if (icon !== null && !(AGENT_PRESET_ICONS as readonly string[]).includes(icon as string)) throw new Error("Activity Agent icon is not declared");
  if ([row.agent_id, row.agent_name, icon].some((v) => v !== null) && [row.agent_id, row.agent_name, icon].some((v) => !v)) throw new Error("Activity Agent identity is incomplete");
  if (row.agent_color !== null && (!row.agent_color || !row.agent_id)) throw new Error("Activity Agent color has no authored identity");
  return { agentId: row.agent_id as string | null, agentName: row.agent_name as string | null, agentIcon: icon as ClientAgentPresetIcon | null, agentColor: row.agent_color as string | null };
}

export function decodeOperatorActivity(value: unknown, marks: readonly MarkActivityItem[] = []): MarkActivityItem[] {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Activity projection is invalid");
  const result = value as Record<string, unknown>;
  if (!Array.isArray(result.activity) || !Array.isArray(result.chats)) throw new Error("Activity projection is incomplete");
  const chats = new Map<string, ReturnType<typeof activityIdentity>>();
  const titles = new Map<string, string | null>();
  for (const chat of result.chats) {
    if (!chat || typeof chat !== "object" || typeof chat.run_id !== "string" || !chat.run_id || chats.has(chat.run_id)) throw new Error("Activity chat identity is invalid");
    chats.set(chat.run_id, activityIdentity(chat));
    titles.set(chat.run_id, typeof chat.title === "string" && chat.title.trim() ? chat.title.trim() : null);
  }
  const identities = new Set<string>();
  const attention = result.activity.map((row: unknown): MarkActivityItem => {
    if (!row || typeof row !== "object" || Array.isArray(row)) throw new Error("Activity row is invalid");
    const item = row as Record<string, unknown>;
    for (const field of ["id", "kind", "title", "name", "run_id", "artifact_id"]) if (typeof item[field] !== "string" || !item[field]) throw new Error(`Activity ${field} is invalid`);
    if (!["message", "approval", "user_input", "elicitation"].includes(item.kind as string) || !chats.has(item.run_id as string)) throw new Error("Activity row has no exact chat/kind");
    if (item.client_surface_role !== "home" && item.client_surface_role !== "task") throw new Error("Activity chat role is invalid");
    if (typeof item.worker !== "string" || !Number.isSafeInteger(item.seq) || (item.seq as number) < 0) throw new Error("Activity movement is invalid");
    if (item.created_at_ms !== null && (typeof item.created_at_ms !== "number" || !Number.isFinite(item.created_at_ms) || item.created_at_ms <= 0)) throw new Error("Activity timestamp is invalid");
    for (const key of ["movement_id", "request_id", "excerpt"]) if (item[key] !== null && (typeof item[key] !== "string" || !item[key])) throw new Error(`Activity ${key} is invalid`);
    if (item.kind === "message" ? !item.movement_id || item.request_id !== null : !item.request_id) throw new Error("Activity receipt identity is missing");
    if (identities.has(item.id as string)) throw new Error("Activity identity is duplicated");
    identities.add(item.id as string);
    const identity = activityIdentity(item);
    if (JSON.stringify(identity) !== JSON.stringify(chats.get(item.run_id as string))) throw new Error("Activity Agent differs from its projected chat identity");
    return {
      id: item.id as string, kind: item.kind as ActivityKind, title: item.title as string, name: item.name as string,
      ...identity, worker: item.worker, workerOriginal: item.worker, sourceTitle: titles.get(item.run_id as string) ?? null,
      excerpt: item.excerpt as string | null,
      runId: item.run_id as string, artifactId: item.artifact_id as string,
      movementId: item.movement_id as string | null, requestId: item.request_id as string | null,
      sequence: item.seq as number, createdAt: item.created_at_ms as number | null, clientSurfaceRole: item.client_surface_role,
      navigation: { kind: "run", runId: item.run_id as string, clientSurfaceRole: item.client_surface_role },
    };
  });
  return [...attention, ...marks.map((item) => ({ ...item, ...chats.get(item.runId), sourceTitle: titles.get(item.runId) ?? null }))]
    .sort((a, b) => (b.createdAt ?? -1) - (a.createdAt ?? -1) || (b.sequence ?? -1) - (a.sequence ?? -1) || a.id.localeCompare(b.id));
}

export function selectActivity(items: readonly MarkActivityItem[], mode: MarkActivityMode, types: readonly string[] | null): MarkActivityItem[] {
  const selected = new Set(types ?? []);
  return items.filter((item) => item.kind !== "mark" || mode === "all" || mode === "custom" && selected.has(item.name));
}

export function latestActivityMessages(items: readonly MarkActivityItem[]): MarkActivityItem[] {
  const latest = new Map<string, MarkActivityItem>();
  for (const item of items) {
    if (item.kind !== "message") continue;
    const previous = latest.get(item.runId);
    if (!previous || (item.sequence ?? -1) > (previous.sequence ?? -1)) latest.set(item.runId, item);
  }
  return items.filter((item) => item.kind !== "message" || latest.get(item.runId) === item);
}

export function decodeMarkKinds(value: unknown): string[] {
  if (!value || typeof value !== "object" || Array.isArray(value) || !("descriptors" in value) || !Array.isArray(value.descriptors)) {
    throw new Error("The mark-kind projection returned an invalid descriptor set");
  }
  return value.descriptors.map((row: unknown) => {
    if (!row || typeof row !== "object" || Array.isArray(row) || !("name" in row) || typeof row.name !== "string" || !row.name) {
      throw new Error("The mark-kind projection returned an invalid descriptor");
    }
    return row.name;
  });
}

export function decodeActivityReadState(raw: string | null): Set<string> {
  if (raw === null) return new Set();
  const value: unknown = JSON.parse(raw);
  if (!Array.isArray(value) || value.some((id) => typeof id !== "string" || !id)) throw new Error("Saved Activity read state is invalid");
  return new Set(value);
}
