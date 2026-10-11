import { useEffect, useRef } from "react";
import { readProjection } from "../projectionClient";
import { useProjectionGeneration } from "../projectionLive";
import type { ClientSurfaceDescriptor } from "./surfaceTypes";

type ActionScope = { home: string; runId: string; interfaceId: string };
type DeliveryIdentity = {
  artifact_id: string;
  run_id: string;
  seq: number;
  from: { Interface: string };
  to: { Interface: string };
};
type ClientActionOptions = ActionScope & {
  enabled: boolean;
  surfaces: readonly ClientSurfaceDescriptor[];
  openSurface: (surface: ClientSurfaceDescriptor) => void;
  onRefusal: (reason: string) => void;
};

const PAGE_SIZE = 128;
const CURSOR_PREFIX = "octopus.client-actions.cursor.v1:";

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Malformed client action evidence.");
  return value as Record<string, unknown>;
}

function interfaceEndpoint(value: unknown): { Interface: string } {
  const endpoint = object(value);
  if (Object.keys(endpoint).length !== 1 || typeof endpoint.Interface !== "string" || !endpoint.Interface) {
    throw new Error("Client action has an invalid interface endpoint.");
  }
  return { Interface: endpoint.Interface };
}

function deliveryIdentity(value: unknown, scope: ActionScope): DeliveryIdentity {
  const row = object(value);
  const from = interfaceEndpoint(row.from);
  const to = interfaceEndpoint(row.to);
  if (typeof row.artifact_id !== "string" || !/^client\.action:[0-9a-f]{64}$/.test(row.artifact_id)
    || row.run_id !== scope.runId || to.Interface !== scope.interfaceId
    || typeof row.seq !== "number" || !Number.isSafeInteger(row.seq) || row.seq < 0) {
    throw new Error("Client action does not match the current Home run and interface.");
  }
  return { artifact_id: row.artifact_id, run_id: scope.runId, seq: row.seq, from, to };
}

function readCursor(storage: Storage, key: string, scope: ActionScope): number {
  const stored = storage.getItem(key);
  if (stored === null) return -1;
  const cursor = object(JSON.parse(stored));
  if (cursor.schema_version !== 1 || cursor.home !== scope.home
    || cursor.run_id !== scope.runId || cursor.interface_id !== scope.interfaceId) {
    throw new Error("Client action cursor does not match the selected Home.");
  }
  return deliveryIdentity(cursor.last_consumed, scope).seq;
}

function actionSurfaceId(row: Record<string, unknown>): string {
  if (Object.hasOwn(row, "error")) {
    throw new Error(typeof row.error === "string" && row.error ? row.error : "Client action payload could not be verified.");
  }
  const action = object(row.action);
  if (Object.keys(action).length !== 4 || action.schema !== "octopus.client_action"
    || action.schema_version !== 1 || action.action !== "open_surface"
    || typeof action.surface_id !== "string" || !action.surface_id || action.surface_id !== action.surface_id.trim()) {
    throw new Error("Client action must be an exact open_surface request.");
  }
  return action.surface_id;
}

export function useAdmittedClientActions(options: ClientActionOptions): void {
  const enabled = options.enabled && Boolean(options.home && options.runId && options.interfaceId);
  const generation = useProjectionGeneration(true, enabled);
  const scopeKey = enabled ? JSON.stringify([options.home, options.runId, options.interfaceId]) : "";
  const latest = useRef(options);
  latest.current = options;
  const active = useRef({ key: scopeKey });
  if (active.current.key !== scopeKey) active.current = { key: scopeKey };
  const lastFailure = useRef("");

  useEffect(() => {
    if (!scopeKey) return;
    const scope = { home: options.home, runId: options.runId, interfaceId: options.interfaceId };
    const token = active.current;
    const key = CURSOR_PREFIX + scopeKey;
    let cancelled = false;
    const current = () => !cancelled && active.current === token && latest.current.enabled;
    const refuse = (reason: string) => latest.current.onRefusal(`Client navigation refused: ${reason}`);

    async function consume(): Promise<void> {
      try {
        const storage = window.localStorage;
        let afterSeq = readCursor(storage, key, scope);
        while (current()) {
          const response = object(await readProjection("projection.client_actions.list", {
            home: scope.home, run_id: scope.runId, interface_id: scope.interfaceId,
            after_seq: afterSeq, limit: PAGE_SIZE,
          }));
          if (!current()) return;
          if (response.home !== scope.home || response.run_id !== scope.runId
            || response.interface_id !== scope.interfaceId || !Array.isArray(response.actions)
            || response.actions.length > PAGE_SIZE) {
            throw new Error("Client action projection does not match the selected Home.");
          }
          let previousSeq = -1;
          const deliveries = response.actions.map((value: unknown) => {
            const row = object(value);
            const identity = deliveryIdentity(row, scope);
            if (row.movement_kind !== "InterfaceOutbound" || identity.seq <= previousSeq) {
              throw new Error("Client actions must be admitted outbound movements in ledger order.");
            }
            previousSeq = identity.seq;
            return { row, identity };
          });
          for (const { row, identity } of deliveries) {
            if (!current()) return;
            if (identity.seq <= readCursor(storage, key, scope)) continue;
            storage.setItem(key, JSON.stringify({
              schema_version: 1, home: scope.home, run_id: scope.runId, interface_id: scope.interfaceId,
              last_consumed: identity,
            }));
            if (readCursor(storage, key, scope) !== identity.seq) throw new Error("Client action cursor could not be saved.");
            try {
              const surfaceId = actionSurfaceId(row);
              const surface = latest.current.surfaces.find((candidate) => candidate.id === surfaceId);
              if (!surface) throw new Error(`Surface ${surfaceId} is unavailable.`);
              latest.current.openSurface(surface);
            } catch (error) {
              refuse(error instanceof Error ? error.message : String(error));
            }
          }
          const nextSeq = readCursor(storage, key, scope);
          if (deliveries.length < PAGE_SIZE || nextSeq <= afterSeq) break;
          afterSeq = nextSeq;
        }
        lastFailure.current = "";
      } catch (error) {
        if (!current()) return;
        const reason = error instanceof Error ? error.message : String(error);
        const failure = JSON.stringify([scopeKey, reason]);
        if (lastFailure.current !== failure) refuse(reason);
        lastFailure.current = failure;
      }
    }
    void consume();
    return () => { cancelled = true; };
  }, [generation, scopeKey]);
}
