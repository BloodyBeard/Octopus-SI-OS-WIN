import { useSyncExternalStore } from "react";
import type { OctopusProjectionChange } from "./electron";

let generation = 0;
let structureGeneration = 0;
const initialProjectionChange: OctopusProjectionChange = { generation: 0, kind: "structure" };
let projectionChange = initialProjectionChange;
let attached = false;
let detachProjectionChange: (() => void) | null = null;
let eventSource: EventSource | null = null;
const listeners = new Set<() => void>();

function bump(next: unknown): void {
  const row = next && typeof next === "object" && !Array.isArray(next)
    ? next as Record<string, unknown>
    : {};
  generation = typeof row.generation === "number" && Number.isFinite(row.generation)
    ? row.generation
    : generation + 1;
  projectionChange = {
    generation,
    kind: row.kind === "ledger" ? "ledger" : "structure",
  };
  if (projectionChange.kind === "structure") structureGeneration = generation;
  for (const listener of [...listeners]) listener();
}

function attach(): void {
  if (attached) return;
  attached = true;
  const bridge = window.octopusTransport;
  if (bridge?.onProjectionChanged) {
    detachProjectionChange = bridge.onProjectionChanged((change) => bump(change));
  } else if (import.meta.env.DEV && typeof EventSource === "function") {
    eventSource = new EventSource("/__octopus/events");
    eventSource.onmessage = (event) => {
      try {
        bump(JSON.parse(event.data));
      } catch {
        bump(undefined);
      }
    };
  }
}

function detach(): void {
  if (!attached) return;
  attached = false;
  detachProjectionChange?.();
  detachProjectionChange = null;
  eventSource?.close();
  eventSource = null;
}

export function subscribeProjectionChanges(listener: () => void): () => void {
  attach();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) detach();
  };
}

function subscribeInactive(): () => void {
  return () => {};
}

export function currentProjectionGeneration(): number {
  return generation;
}

export function currentProjectionStructureGeneration(): number {
  return structureGeneration;
}

export function currentProjectionChange(): OctopusProjectionChange {
  return projectionChange;
}

export function useProjectionGeneration(includeLedger = true, active = true): number {
  return useSyncExternalStore(
    active ? subscribeProjectionChanges : subscribeInactive,
    includeLedger ? currentProjectionGeneration : currentProjectionStructureGeneration,
    () => 0,
  );
}

export function useProjectionStructureGeneration(): number {
  return useSyncExternalStore(
    subscribeProjectionChanges,
    currentProjectionStructureGeneration,
    () => 0,
  );
}

export function useProjectionChange(): OctopusProjectionChange {
  return useSyncExternalStore(
    subscribeProjectionChanges,
    currentProjectionChange,
    () => initialProjectionChange,
  );
}

export function nextProjectionChange(timeoutMs: number, afterGeneration = generation): Promise<boolean> {
  return new Promise((resolve) => {
    let done = false;
    const unsubscribe = subscribeProjectionChanges(() => finish(true));
    const timer = setTimeout(() => finish(false), Math.max(0, timeoutMs));
    function finish(moved: boolean) {
      if (done) return;
      done = true;
      clearTimeout(timer);
      unsubscribe();
      resolve(moved);
    }
    if (generation !== afterGeneration) finish(true);
  });
}
