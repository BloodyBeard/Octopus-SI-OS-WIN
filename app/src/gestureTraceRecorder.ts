import { useSyncExternalStore } from "react";
import { recordGestureTrace } from "./gestureRecognition";

export type GestureTraceStatus = Readonly<
  | { phase: "idle" }
  | { phase: "countdown"; startsAt: number; durationMs: number }
  | { phase: "recording"; endsAt: number }
  | { phase: "saving" }
  | { phase: "saved"; path: string; filename: string; frames: number }
  | { phase: "error"; message: string }
>;

const TRACE_COUNTDOWN_MS = 3_000;

let status: GestureTraceStatus = { phase: "idle" };
let cancelCurrent: (() => void) | null = null;
let generation = 0;
const listeners = new Set<() => void>();

function setStatus(next: GestureTraceStatus): void {
  status = next;
  for (const listener of [...listeners]) listener();
}

export function useGestureTraceStatus(): GestureTraceStatus {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    () => status,
    () => status,
  );
}

export function startGestureTrace(durationMs: number, label: string): void {
  if (status.phase === "countdown" || status.phase === "recording" || status.phase === "saving") return;
  const transport = window.octopusTransport;
  if (!transport?.saveGestureTrace) {
    setStatus({ phase: "error", message: "Saving gesture traces needs the desktop client." });
    return;
  }
  const run = ++generation;
  setStatus({ phase: "countdown", startsAt: Date.now() + TRACE_COUNTDOWN_MS, durationMs });
  const timer = window.setTimeout(() => {
    if (run !== generation) return;
    const recording = recordGestureTrace(durationMs, label);
    cancelCurrent = recording.cancel;
    setStatus({ phase: "recording", endsAt: Date.now() + durationMs });
    recording.done
      .then(async (trace) => {
        if (run !== generation) return;
        cancelCurrent = null;
        setStatus({ phase: "saving" });
        const saved = await transport.saveGestureTrace(JSON.stringify(trace));
        if (run === generation) setStatus({ phase: "saved", path: saved.path, filename: saved.filename, frames: trace.frames.length });
      })
      .catch((error: unknown) => {
        if (run !== generation) return;
        cancelCurrent = null;
        setStatus({ phase: "error", message: error instanceof Error ? error.message : String(error) });
      });
  }, TRACE_COUNTDOWN_MS);
  cancelCurrent = () => window.clearTimeout(timer);
}

export function cancelGestureTrace(): void {
  generation += 1;
  cancelCurrent?.();
  cancelCurrent = null;
  setStatus({ phase: "idle" });
}

export async function revealGestureTrace(path: string): Promise<void> {
  const transport = window.octopusTransport;
  if (!transport?.revealGestureTrace) throw new Error("Revealing gesture traces needs the desktop client.");
  await transport.revealGestureTrace(path);
}
