import type {
  OctopusCodexUsageReadRequest,
  OctopusCodexUsageSnapshot,
} from "./electron";

export function hasCodexUsageBridge(): boolean {
  return typeof window.octopusTransport?.readCodexUsage === "function";
}

export async function readCodexUsage(
  request: OctopusCodexUsageReadRequest,
): Promise<OctopusCodexUsageSnapshot> {
  const read = window.octopusTransport?.readCodexUsage;
  if (!read) throw new Error("Codex usage is available in the Octopus desktop app");
  return read(request);
}
