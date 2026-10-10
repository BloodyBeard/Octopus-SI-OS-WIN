import type { OctopusClientAccessProfile } from "./electron";
import { appToast } from "./toasts";

const RECOVERY_KEY = "__octopus.client-settings-recovery.v1";
const RECOVERY_LIFETIME_MS = 60_000;

type ClientSettingsRecovery = {
  createdAt: number;
  entries: Array<[string, string]>;
  message: string;
  profile: OctopusClientAccessProfile;
  version: 1;
};

export function clientProfileStorageEntries(accessProfile: OctopusClientAccessProfile): Array<[string, string]> {
  const entries: Array<[string, string]> = [];
  for (let index = 0; index < window.localStorage.length; index += 1) {
    const key = window.localStorage.key(index);
    if (!key || !clientProfileOwnsStorageKey(key, accessProfile)) continue;
    const value = window.localStorage.getItem(key);
    if (value != null) entries.push([key, value]);
  }
  return entries.sort(([left], [right]) => left.localeCompare(right));
}

export function clientProfileOwnsStorageKey(key: string, accessProfile: OctopusClientAccessProfile): boolean {
  if (!key.startsWith("octopus.")) return false;
  const testOwned = key.endsWith(".test") || key.endsWith(".lab") || key.startsWith("octopus.test.");
  return (accessProfile === "lab") === testOwned;
}

export function clearClientProfileStorageWithRecovery(accessProfile: OctopusClientAccessProfile): void {
  replaceClientProfileStorageWithRecovery(
    accessProfile,
    [],
    "replace",
    "Local client data cleared",
  );
}

export function replaceClientProfileStorageWithRecovery(
  accessProfile: OctopusClientAccessProfile,
  entries: ReadonlyArray<readonly [string, string]>,
  mode: "merge" | "replace",
  message: string,
): void {
  const before = clientProfileStorageEntries(accessProfile);
  const desired = new Map<string, string>(mode === "merge" ? before : []);
  for (const [key, value] of entries) {
    if (!clientProfileOwnsStorageKey(key, accessProfile)) {
      throw new Error(`Client storage key is outside the ${accessProfile} profile: ${key}`);
    }
    desired.set(key, value);
  }
  const recovery: ClientSettingsRecovery = {
    createdAt: Date.now(),
    entries: before,
    message: boundedRecoveryMessage(message),
    profile: accessProfile,
    version: 1,
  };
  const serialized = JSON.stringify(recovery);
  window.sessionStorage.setItem(RECOVERY_KEY, serialized);
  if (window.sessionStorage.getItem(RECOVERY_KEY) !== serialized) {
    throw new Error("Could not preserve client data for recovery.");
  }
  try {
    replaceClientProfileStorageExact(accessProfile, [...desired.entries()]);
  } catch (failure) {
    try {
      replaceClientProfileStorageExact(accessProfile, before);
      discardRecovery();
    } catch (rollbackFailure) {
      throw new Error(`Client data update failed and exact rollback also failed: ${failureMessage(rollbackFailure)}`);
    }
    throw failure;
  }
  window.location.reload();
}

export function offerClientProfileStorageRecovery(accessProfile: OctopusClientAccessProfile): void {
  const recovery = readRecovery(accessProfile);
  if (!recovery) return;
  const expiresIn = Math.max(RECOVERY_LIFETIME_MS - (Date.now() - recovery.createdAt), 0);
  if (expiresIn === 0) {
    try {
      discardRecovery();
    } catch (failure) {
      appToast.danger(`Could not discard expired client recovery data: ${failureMessage(failure)}`);
    }
    return;
  }
  appToast.info(recovery.message, Math.ceil(expiresIn / 1_000), {
    action: {
      label: "Undo",
      onSelect: () => {
        try {
          replaceClientProfileStorageExact(accessProfile, recovery.entries);
          discardRecovery();
          window.location.reload();
        } catch (failure) {
          appToast.danger(`Could not restore local client data: ${failureMessage(failure)}`);
        }
      },
    },
    dedupeKey: `client-data-clear:${accessProfile}`,
  });
  window.setTimeout(() => {
    const current = readRecovery(accessProfile);
    if (current?.createdAt !== recovery.createdAt) return;
    try {
      discardRecovery();
    } catch (failure) {
      appToast.danger(`Could not discard expired client recovery data: ${failureMessage(failure)}`);
    }
  }, expiresIn);
}

function readRecovery(accessProfile: OctopusClientAccessProfile): ClientSettingsRecovery | null {
  try {
    const raw = window.sessionStorage.getItem(RECOVERY_KEY);
    if (!raw || raw.length > 5_000_000) return null;
    const parsed = JSON.parse(raw) as Partial<ClientSettingsRecovery>;
    if (parsed.version !== 1 || parsed.profile !== accessProfile || typeof parsed.createdAt !== "number" || !Number.isFinite(parsed.createdAt) || !Array.isArray(parsed.entries)) return null;
    if (typeof parsed.message !== "string" || parsed.message.length === 0 || parsed.message.length > 160) return null;
    const entries = parsed.entries.flatMap((entry) => (
      Array.isArray(entry)
      && entry.length === 2
      && typeof entry[0] === "string"
      && typeof entry[1] === "string"
      && clientProfileOwnsStorageKey(entry[0], accessProfile)
        ? [[entry[0], entry[1]] as [string, string]]
        : []
    ));
    if (entries.length !== parsed.entries.length || new Set(entries.map(([key]) => key)).size !== entries.length) return null;
    return { createdAt: parsed.createdAt, entries, message: parsed.message, profile: accessProfile, version: 1 };
  } catch {
    return null;
  }
}

function replaceClientProfileStorageExact(
  accessProfile: OctopusClientAccessProfile,
  entries: ReadonlyArray<readonly [string, string]>,
): void {
  const desired = [...entries]
    .map(([key, value]) => {
      if (!clientProfileOwnsStorageKey(key, accessProfile)) {
        throw new Error(`Client storage key is outside the ${accessProfile} profile: ${key}`);
      }
      return [key, value] as [string, string];
    })
    .sort(([left], [right]) => left.localeCompare(right));
  if (new Set(desired.map(([key]) => key)).size !== desired.length) {
    throw new Error("Client storage snapshot contains duplicate keys.");
  }
  for (const [key] of clientProfileStorageEntries(accessProfile)) window.localStorage.removeItem(key);
  for (const [key, value] of desired) window.localStorage.setItem(key, value);
  const actual = clientProfileStorageEntries(accessProfile);
  if (
    actual.length !== desired.length
    || actual.some(([key, value], index) => key !== desired[index]?.[0] || value !== desired[index]?.[1])
  ) {
    throw new Error("Client storage did not match the exact requested snapshot.");
  }
}

function discardRecovery(): void {
  window.sessionStorage.removeItem(RECOVERY_KEY);
  if (window.sessionStorage.getItem(RECOVERY_KEY) !== null) {
    throw new Error("Could not discard the client recovery snapshot.");
  }
}

function boundedRecoveryMessage(value: string): string {
  const message = value.trim();
  if (!message || message.length > 160) throw new Error("Client recovery message must contain 1–160 characters.");
  return message;
}

function failureMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}
