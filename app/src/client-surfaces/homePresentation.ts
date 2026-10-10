import type { AdmittedClientSurfaceDocument, ClientSurfaceAdmissionIssue, ClientSurfaceDocument } from "./types";

export type HomePresentation = { lifetime_ms?: number; expires_at?: string };
export type HomeSurfaceScope = { runId: string; interfaceId: string };

export const MAX_HOME_SURFACES = 32;
export const MAX_HOME_PRESENTATION_LIFETIME_MS = 86_400_000;

export function inspectHomePresentation(document: ClientSurfaceDocument): ClientSurfaceAdmissionIssue[] {
  const value = document.data?.home_presentation;
  if (value === undefined) return [];
  const fail = (message: string): ClientSurfaceAdmissionIssue[] => [{
    code: "home.presentation.invalid", path: "/data/home_presentation", message,
  }];
  if (document.target.kind !== "voice.widget" && document.target.kind !== "voice.window") {
    return fail("Home presentation data requires a voice.widget or voice.window target");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return fail("Home presentation must be an object");
  const fields = value as Record<string, unknown>;
  if (Object.keys(fields).some((key) => key !== "lifetime_ms" && key !== "expires_at")) {
    return fail("Home presentation accepts only lifetime_ms or expires_at");
  }
  if (fields.lifetime_ms !== undefined && fields.expires_at !== undefined) {
    return fail("Home presentation must not combine lifetime_ms and expires_at");
  }
  if (fields.lifetime_ms !== undefined && (
    typeof fields.lifetime_ms !== "number" || !Number.isSafeInteger(fields.lifetime_ms)
    || fields.lifetime_ms < 1 || fields.lifetime_ms > MAX_HOME_PRESENTATION_LIFETIME_MS
  )) return fail(`lifetime_ms must be an integer between 1 and ${MAX_HOME_PRESENTATION_LIFETIME_MS}`);
  if (fields.expires_at !== undefined && !isPresentationTimestamp(fields.expires_at)) {
    return fail("expires_at must be a valid UTC timestamp such as 2026-10-04T12:00:00.000Z");
  }
  return [];
}

export function isPresentationTimestamp(value: unknown): value is string {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/.test(value)) return false;
  const time = Date.parse(value);
  return Number.isFinite(time) && new Date(time).toISOString().slice(0, 19) === value.slice(0, 19);
}

export function homeSurfaceIdentity(item: AdmittedClientSurfaceDocument): string {
  return JSON.stringify([homeSurfaceLayoutIdentity(item), item.sourceDigest]);
}

export function homeSurfaceLayoutIdentity(item: AdmittedClientSurfaceDocument): string {
  return JSON.stringify([item.sourceRunId ?? "", item.sourceInterfaceId ?? "", item.sourceExtensionId ?? "", item.document.id]);
}

export function homeSurfaceExpiry(item: AdmittedClientSurfaceDocument): number | null {
  const issues = inspectHomePresentation(item.document);
  if (issues.length) throw new Error(issues[0].message);
  const presentation = item.document.data?.home_presentation as HomePresentation | undefined;
  if (presentation?.expires_at) return Date.parse(presentation.expires_at);
  if (presentation?.lifetime_ms !== undefined) {
    if (!isPresentationTimestamp(item.sourceDeliveredAt)) throw new Error("A presentation lifetime requires its artifact delivery timestamp");
    return Date.parse(item.sourceDeliveredAt) + presentation.lifetime_ms;
  }
  return null;
}

export function selectHomeSurfaceDocuments(
  surfaces: AdmittedClientSurfaceDocument[],
  scope: HomeSurfaceScope,
  now: number,
  dismissed: ReadonlySet<string> = new Set(),
) {
  const documents: AdmittedClientSurfaceDocument[] = [];
  const errors: string[] = [];
  let nextExpiry: number | null = null;
  for (const item of surfaces) {
    if (item.document.target.kind !== "voice.widget" && item.document.target.kind !== "voice.window") continue;
    if (!item.sourceExtensionId) {
      if (!scope.runId || !scope.interfaceId) continue;
      if (item.sourceRunId !== scope.runId || item.sourceInterfaceId !== scope.interfaceId) continue;
      if (!item.sourceArtifactId || !Number.isSafeInteger(item.sourceSequence) || item.sourceSequence! < 0) {
        errors.push(`${item.document.title}: Home display requires exact artifact delivery provenance`);
        continue;
      }
    }
    try {
      const expiry = homeSurfaceExpiry(item);
      if (expiry !== null && expiry <= now) continue;
      if (expiry !== null) nextExpiry = nextExpiry === null ? expiry : Math.min(nextExpiry, expiry);
      documents.push(item);
    } catch (error) {
      errors.push(`${item.document.title}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  if (documents.filter((item) => !item.sourceExtensionId).length > MAX_HOME_SURFACES) {
    errors.push(`Home has more than ${MAX_HOME_SURFACES} active delivered surfaces; custom display is unavailable`);
    documents.splice(0, documents.length, ...documents.filter((item) => item.sourceExtensionId));
  }
  return { documents, visibleDocuments: documents.filter((item) => !dismissed.has(homeSurfaceIdentity(item))), nextExpiry, errors };
}
