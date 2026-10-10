import { DEFAULT_RUNTIME_ENDPOINT } from "./gateway";
import type { OctopusRuntimeEndpointValidation } from "./electron";

export type ConnectionPrefs = {
  baseUrl: string;
};

const PREFS_KEY = "octopus.runtimeUi.prefs.v1";
export const RUNTIME_PREFS_CHANGED_EVENT = "octopus:runtime-prefs-changed";
const DEV_VALIDATE_ENDPOINT_URL = "/__octopus/validate-endpoint";

export function loadRuntimePrefs(): ConnectionPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) throw new Error("missing prefs");
    const parsed = JSON.parse(raw) as Partial<ConnectionPrefs>;
    return {
      baseUrl: typeof parsed.baseUrl === "string" && parsed.baseUrl.trim()
        ? parsed.baseUrl.trim()
        : DEFAULT_RUNTIME_ENDPOINT,
    };
  } catch {
    return { baseUrl: DEFAULT_RUNTIME_ENDPOINT };
  }
}

export function saveRuntimePrefs(next: ConnectionPrefs): void {
  localStorage.setItem(PREFS_KEY, JSON.stringify({
    baseUrl: next.baseUrl.trim() || DEFAULT_RUNTIME_ENDPOINT,
  }));
  window.dispatchEvent(new CustomEvent(RUNTIME_PREFS_CHANGED_EVENT));
}

export async function validateRuntimeEndpoint(endpoint: string): Promise<OctopusRuntimeEndpointValidation> {
  const bridge = window.octopusTransport;
  if (bridge?.validateRuntimeEndpoint) {
    const result = await bridge.validateRuntimeEndpoint({ endpoint });
    if (!result.valid) throw new Error(result.error || "The endpoint is invalid.");
    return result;
  }
  if (import.meta.env.DEV) {
    const response = await fetch(DEV_VALIDATE_ENDPOINT_URL, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ endpoint }),
    });
    const result = await response.json() as OctopusRuntimeEndpointValidation;
    if (!response.ok || !result.valid) throw new Error(result.error || `Endpoint validation failed: ${response.status}`);
    return result;
  }
  throw new Error("Endpoint validation is unavailable outside the desktop app or development server.");
}

export function displayEndpoint(baseUrl: string): string {
  return baseUrl.replace(/^https?:\/\//, "");
}

export function formatRuntimeError(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return String(error);
}
