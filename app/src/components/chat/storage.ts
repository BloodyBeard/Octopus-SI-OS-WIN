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
  serviceTier?: string;
  applicationIds: string[];
  driverNames: string[];
  accessMode: TaskAccessMode;
  continuation: {
    sourceRunId: string;
    sourceArtifactId: string;
  } | null;
  updatedAtMs: number;
};

const CHAT_DRAFTS_KEY = "octopus.client.chatDrafts.v1";

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
      serviceTier: typeof parsed.serviceTier === "string" ? parsed.serviceTier : "",
      reasoningEffort: typeof parsed.reasoningEffort === "string"
        ? parsed.reasoningEffort.trim()
        : "",
    };
  } catch {
    return { providerInterfaceId: "", providerModel: "", reasoningEffort: "", serviceTier: "" };
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
    serviceTier: settings.serviceTier ?? current.serviceTier,
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
      const serviceTier = typeof candidate.serviceTier === "string" ? candidate.serviceTier : "";
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
      return [{ key, messageId, messageHadAttachments, text, agentPresetId, eeId, workerInterfaceId, providerInterfaceId, providerModel, reasoningEffort, serviceTier, applicationIds, driverNames, accessMode, continuation, updatedAtMs }];
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
