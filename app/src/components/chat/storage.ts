import type { ChatSettings } from "./types";
import { CHAT_SETTINGS_KEY } from "./constants";
import type { OctopusClientAccessProfile } from "../../electron";
import { normalizeTaskAccessMode, type TaskAccessMode } from "./taskAccess";

export type PersistedChatDraft = {
  key: string;
  messageId: string;
  messageHadAttachments: boolean;
  text: string;
  agentPresetId: string;
  eeId: string;
  workerInterfaceId: string;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  applicationIds: string[];
  driverNames: string[];
  accessMode: TaskAccessMode;
  continuation: {
    sourceRunId: string;
    sourceArtifactId: string;
  } | null;
  updatedAtMs: number;
};

export type PersistedQueuedChatTurn = {
  id: string;
  messageId: string;
  runKey: string;
  text: string;
  eeId: string;
  workerInterfaceId: string;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  endpoint: string;
  createdAtMs: number;
  status: "queued" | "sending" | "accepted" | "unknown" | "failed";
  error?: string;
  hadAttachments: boolean;
};

export type PersistedChatQueueState = {
  pausedRunKeys: string[];
};

const CHAT_DRAFTS_KEY = "octopus.client.chatDrafts.v1";
const CHAT_QUEUE_KEY = "octopus.client.chatQueue.v1";
const CHAT_QUEUE_STATE_KEY = "octopus.client.chatQueueState.v1";

export function chatSettingsChangedEvent(profile: OctopusClientAccessProfile): string {
  return `octopus:chat-settings-changed:${profile}`;
}

function chatSettingsKey(profile: OctopusClientAccessProfile): string {
  return profile === "live" ? CHAT_SETTINGS_KEY : `${CHAT_SETTINGS_KEY}.test`;
}

export function loadChatSettings(profile: OctopusClientAccessProfile): ChatSettings {
  try {
    const raw = localStorage.getItem(chatSettingsKey(profile));
    if (!raw) throw new Error("missing settings");
    const parsed = JSON.parse(raw) as Partial<ChatSettings>;
    return {
      providerInterfaceId: typeof parsed.providerInterfaceId === "string"
        ? parsed.providerInterfaceId.trim()
        : "",
      providerModel: typeof parsed.providerModel === "string"
        ? parsed.providerModel.trim()
        : "",
      reasoningEffort: typeof parsed.reasoningEffort === "string"
        ? parsed.reasoningEffort.trim()
        : "",
    };
  } catch {
    return { providerInterfaceId: "", providerModel: "", reasoningEffort: "" };
  }
}

export function saveChatSettings(
  settings: Partial<ChatSettings>,
  profile: OctopusClientAccessProfile,
): void {
  const current = loadChatSettings(profile);
  localStorage.setItem(chatSettingsKey(profile), JSON.stringify({
    providerInterfaceId: settings.providerInterfaceId?.trim() ?? current.providerInterfaceId,
    providerModel: settings.providerModel?.trim() ?? current.providerModel,
    reasoningEffort: settings.reasoningEffort?.trim() ?? current.reasoningEffort,
  }));
  window.dispatchEvent(new CustomEvent(chatSettingsChangedEvent(profile)));
}

export function loadChatDrafts(profile: OctopusClientAccessProfile, scope = "main"): PersistedChatDraft[] {
  try {
    const parsed = JSON.parse(chatStateStorage(scope).getItem(profileScopedStateKey(CHAT_DRAFTS_KEY, profile, scope)) || "[]") as unknown;
    if (!Array.isArray(parsed)) return [];
    const seen = new Set<string>();
    return parsed.flatMap((value) => {
      if (!value || typeof value !== "object" || Array.isArray(value)) return [];
      const candidate = value as Partial<PersistedChatDraft>;
      const key = typeof candidate.key === "string" ? candidate.key.trim() : "";
      const messageId = persistedDraftMessageId(candidate.messageId);
      const messageHadAttachments = candidate.messageHadAttachments === true;
      const text = typeof candidate.text === "string" ? candidate.text.slice(0, 250_000) : "";
      const agentPresetId = boundedString(candidate.agentPresetId, 4_096);
      const eeId = boundedString(candidate.eeId, 320);
      const workerInterfaceId = boundedString(candidate.workerInterfaceId, 320);
      const providerInterfaceId = typeof candidate.providerInterfaceId === "string" ? candidate.providerInterfaceId.trim() : "";
      const providerModel = typeof candidate.providerModel === "string" ? candidate.providerModel.trim() : "";
      const reasoningEffort = typeof candidate.reasoningEffort === "string" ? candidate.reasoningEffort.trim() : "";
      const applicationIds = boundedStringList(candidate.applicationIds, 320);
      const driverNames = boundedStringList(candidate.driverNames, 320);
      const accessMode = normalizeTaskAccessMode(candidate.accessMode);
      const continuation = normalizeContinuation(candidate.continuation);
      const updatedAtMs = typeof candidate.updatedAtMs === "number" && Number.isFinite(candidate.updatedAtMs)
        ? Math.max(0, Math.round(candidate.updatedAtMs))
        : 0;
      if (!key || seen.has(key) || (!messageId && !text.trim() && !continuation && !agentPresetId)) return [];
      seen.add(key);
      return [{ key, messageId, messageHadAttachments, text, agentPresetId, eeId, workerInterfaceId, providerInterfaceId, providerModel, reasoningEffort, applicationIds, driverNames, accessMode, continuation, updatedAtMs }];
    }).sort((left, right) => right.updatedAtMs - left.updatedAtMs).slice(0, 100);
  } catch {
    return [];
  }
}

export function saveChatDrafts(profile: OctopusClientAccessProfile, drafts: readonly PersistedChatDraft[], scope = "main"): void {
  const serializable = drafts
    .filter((draft) => draft.messageId || draft.text.trim() || draft.continuation || draft.agentPresetId)
    .sort((left, right) => right.updatedAtMs - left.updatedAtMs)
    .slice(0, 100);
  chatStateStorage(scope).setItem(profileScopedStateKey(CHAT_DRAFTS_KEY, profile, scope), JSON.stringify(serializable));
}

export function legacyChatQueueNotice(profile: OctopusClientAccessProfile, scope: string): string {
  try {
    const raw = chatStateStorage(scope).getItem(profileScopedStateKey(CHAT_QUEUE_KEY, profile, scope));
    if (!raw || raw === "[]") return "";
    return "Older saved messages have no runtime Home identity. They remain in local storage and have not been migrated or sent.";
  } catch {
    return "Older saved message storage could not be read. No older messages have been imported or sent.";
  }
}

export function loadQueuedChatTurns(profile: OctopusClientAccessProfile, scope = "main"): PersistedQueuedChatTurn[] {
  try {
    const parsed = JSON.parse(chatStateStorage(scope).getItem(profileScopedStateKey(CHAT_QUEUE_KEY, profile, scope)) || "[]") as unknown;
    if (!Array.isArray(parsed)) return [];
    const seen = new Set<string>();
    return parsed.flatMap((value) => {
      if (!value || typeof value !== "object" || Array.isArray(value)) return [];
      const candidate = value as Partial<PersistedQueuedChatTurn>;
      const id = boundedString(candidate.id, 160);
      const messageId = persistedQueuedMessageId(candidate.messageId, id);
      const runKey = boundedString(candidate.runKey, 320);
      const text = typeof candidate.text === "string" ? candidate.text.slice(0, 250_000) : "";
      const eeId = boundedString(candidate.eeId, 320);
      const workerInterfaceId = boundedString(candidate.workerInterfaceId, 320);
      const providerInterfaceId = boundedString(candidate.providerInterfaceId, 320);
      const providerModel = boundedString(candidate.providerModel, 320);
      const reasoningEffort = boundedString(candidate.reasoningEffort, 80);
      const endpoint = boundedString(candidate.endpoint, 2_048);
      const createdAtMs = typeof candidate.createdAtMs === "number" && Number.isFinite(candidate.createdAtMs)
        ? Math.max(0, Math.round(candidate.createdAtMs))
        : 0;
      const status = candidate.status === "sending" ? "unknown" as const : candidate.status === "failed" || candidate.status === "unknown" || candidate.status === "accepted" ? candidate.status : "queued" as const;
      const error = boundedString(candidate.error, 2_000) || undefined;
      const hadAttachments = candidate.hadAttachments === true;
      if (!id || !messageId || !runKey || seen.has(id) || (!text.trim() && !hadAttachments)) return [];
      seen.add(id);
      return [{ id, messageId, runKey, text, eeId, workerInterfaceId, providerInterfaceId, providerModel, reasoningEffort, endpoint, createdAtMs, status, error, hadAttachments }];
    });
  } catch {
    return [];
  }
}

export function saveQueuedChatTurns(profile: OctopusClientAccessProfile, turns: readonly PersistedQueuedChatTurn[], scope = "main"): void {
  const serializable = turns
    .filter((turn) => turn.id && turn.runKey && (turn.text.trim() || turn.hadAttachments))
;
  chatStateStorage(scope).setItem(profileScopedStateKey(CHAT_QUEUE_KEY, profile, scope), JSON.stringify(serializable));
}

export function loadChatQueueState(profile: OctopusClientAccessProfile, scope = "main"): PersistedChatQueueState {
  try {
    const value = JSON.parse(chatStateStorage(scope).getItem(profileScopedStateKey(CHAT_QUEUE_STATE_KEY, profile, scope)) || "{}") as unknown;
    if (!value || typeof value !== "object" || Array.isArray(value)) return { pausedRunKeys: [] };
    const pausedRunKeys = Array.isArray((value as Record<string, unknown>).pausedRunKeys)
      ? (value as Record<string, unknown>).pausedRunKeys as unknown[]
      : [];
    return {
      pausedRunKeys: [...new Set(pausedRunKeys.map((entry) => boundedString(entry, 320)).filter(Boolean))].slice(0, 100),
    };
  } catch {
    return { pausedRunKeys: [] };
  }
}

export function saveChatQueueState(
  profile: OctopusClientAccessProfile,
  state: PersistedChatQueueState,
  scope = "main",
): void {
  const pausedRunKeys = [...new Set(state.pausedRunKeys.map((entry) => boundedString(entry, 320)).filter(Boolean))].slice(0, 100);
  chatStateStorage(scope).setItem(profileScopedStateKey(CHAT_QUEUE_STATE_KEY, profile, scope), JSON.stringify({ pausedRunKeys }));
}

function persistedQueuedMessageId(value: unknown, queueId: string): string {
  const current = boundedString(value, 160);
  if (/^msg_[A-Za-z0-9_-]{1,156}$/.test(current)) return current;
  const legacy = /^queued_([0-9a-f]{16})$/.exec(queueId);
  return legacy ? `msg_${legacy[1]}` : "";
}

function persistedDraftMessageId(value: unknown): string {
  const current = boundedString(value, 160);
  return /^msg_[A-Za-z0-9_-]{1,156}$/.test(current) ? current : "";
}

function normalizeContinuation(value: unknown): PersistedChatDraft["continuation"] {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const candidate = value as Record<string, unknown>;
  const sourceRunId = typeof candidate.sourceRunId === "string" ? candidate.sourceRunId.trim() : "";
  const sourceArtifactId = typeof candidate.sourceArtifactId === "string" ? candidate.sourceArtifactId.trim() : "";
  if (!sourceRunId || !sourceArtifactId) return null;
  return { sourceRunId, sourceArtifactId };
}

function profileKey(base: string, profile: OctopusClientAccessProfile): string {
  return profile === "live" ? base : `${base}.test`;
}

function profileScopedStateKey(base: string, profile: OctopusClientAccessProfile, scope: string): string {
  const boundedScope = normalizedStateScope(scope);
  return profileKey(boundedScope === "main" ? base : `${base}.${boundedScope}`, profile);
}

function chatStateStorage(_scope: string): Storage {
  return window.localStorage;
}

function normalizedStateScope(scope: string): string {
  if (!scope) throw new Error("Chat state requires an exact client scope.");
  return scope;
}

function boundedString(value: unknown, maximum: number): string {
  return typeof value === "string" ? value.trim().slice(0, maximum) : "";
}

function boundedStringList(value: unknown, maximum: number): string[] {
  return [...new Set((Array.isArray(value) ? value : [])
    .map((entry) => boundedString(entry, maximum))
    .filter(Boolean))];
}
