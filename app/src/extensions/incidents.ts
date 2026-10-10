import type {
  ClientExtensionActionOutcome,
  ClientExtensionIncident,
} from "./types";

export type ClientExtensionIncidentOperation =
  | { status: "idle" }
  | { status: "running" }
  | { status: "succeeded"; outcome: ClientExtensionActionOutcome }
  | { status: "failed"; message: string; retryable: boolean };

export type ClientExtensionIncidentRecord = ClientExtensionIncident & {
  operation: ClientExtensionIncidentOperation;
};

let snapshot: ClientExtensionIncidentRecord[] = [];
const listeners = new Set<() => void>();
const observedIds = new Set<string>();
const MAX_INCIDENTS = 64;
const MAX_OBSERVED_IDS = 512;

export function reportClientExtensionIncident(incident: ClientExtensionIncident) {
  if (!incident.id || !incident.code || !incident.interfaceId || observedIds.has(incident.id)) return;
  observedIds.add(incident.id);
  if (observedIds.size > MAX_OBSERVED_IDS) {
    const retained = [...observedIds].slice(-Math.floor(MAX_OBSERVED_IDS / 2));
    observedIds.clear();
    for (const id of retained) observedIds.add(id);
  }
  if (snapshot.some((candidate) => (
    candidate.code === incident.code && candidate.interfaceId === incident.interfaceId
  ))) return;
  const record: ClientExtensionIncidentRecord = { ...incident, operation: { status: "idle" } };
  snapshot = [...snapshot, record].slice(-MAX_INCIDENTS);
  publish();
}

export function beginClientExtensionIncidentAction(id: string) {
  updateIncident(id, (incident) => ({ ...incident, operation: { status: "running" } }));
}

export function completeClientExtensionIncidentAction(id: string, outcome: ClientExtensionActionOutcome) {
  updateIncident(id, (incident) => ({ ...incident, operation: { status: "succeeded", outcome } }));
}

export function failClientExtensionIncidentAction(id: string, error: unknown) {
  const candidate = error as { message?: unknown; retryable?: unknown } | null;
  const message = typeof candidate?.message === "string" && candidate.message.trim()
    ? candidate.message
    : "Octopus couldn't complete the repair. Try again.";
  updateIncident(id, (incident) => ({
    ...incident,
    operation: {
      status: "failed",
      message,
      retryable: candidate?.retryable !== false,
    },
  }));
}

export function dismissClientExtensionIncident(id: string) {
  const next = snapshot.filter((incident) => incident.id !== id);
  if (next.length === snapshot.length) return;
  snapshot = next;
  publish();
}

export function clientExtensionIncidentSnapshot() {
  return snapshot;
}

export function subscribeClientExtensionIncidents(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function updateIncident(
  id: string,
  update: (incident: ClientExtensionIncidentRecord) => ClientExtensionIncidentRecord,
) {
  let changed = false;
  snapshot = snapshot.map((incident) => {
    if (incident.id !== id) return incident;
    changed = true;
    return update(incident);
  });
  if (changed) publish();
}

function publish() {
  for (const listener of listeners) listener();
}
