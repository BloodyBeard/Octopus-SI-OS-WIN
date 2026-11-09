import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react";
import type { FingerCurls } from "./airHandEngine";
import { clientCommands, dispatchClientCommand, type ClientCommandId } from "./clientActions";
import type { OctopusClientAccessProfile } from "./electron";
import { subscribeGestureState } from "./gestureRecognition";
import { clientDialogOwnsCommands } from "./keyboardShortcuts";

export type CustomGesture = Readonly<{
  id: string;
  name: string;
  definition: string;
  command: ClientCommandId | "none";
  curls: FingerCurls;
}>;

const STORAGE_KEY = "octopus.custom-gestures.v2";
const MATCH_THRESHOLD = 0.34;
const HOLD_MS = 260;
const RELEASE_DISTANCE = 0.5;
const CURL_KEYS: readonly (keyof FingerCurls)[] = ["thumb", "index", "middle", "ring", "little"];

type CustomGestureStore = {
  current: CustomGesture[];
  profile: OctopusClientAccessProfile;
  subscribers: Set<() => void>;
};

const stores = new Map<OctopusClientAccessProfile, CustomGestureStore>();

export function useCustomGestures(profile: OctopusClientAccessProfile) {
  const store = customGestureStore(profile);
  const gestures = useSyncExternalStore(
    (subscriber) => subscribeStore(store, subscriber),
    () => store.current,
    () => store.current,
  );
  const add = useCallback((gesture: Omit<CustomGesture, "id">) => {
    const created: CustomGesture = { ...gesture, id: gestureId() };
    writeStore(store, [...store.current, created]);
    return created;
  }, [store]);
  const update = useCallback((id: string, patch: Partial<Omit<CustomGesture, "id">>) => {
    writeStore(store, store.current.map((gesture) => (gesture.id === id ? { ...gesture, ...patch } : gesture)));
  }, [store]);
  const remove = useCallback((id: string) => {
    const index = store.current.findIndex((gesture) => gesture.id === id);
    if (index < 0) return null;
    const gesture = store.current[index];
    if (!gesture) return null;
    writeStore(store, store.current.filter((candidate) => candidate.id !== id));
    return { gesture, index };
  }, [store]);
  const restore = useCallback((gesture: CustomGesture, index = store.current.length) => {
    if (store.current.some((candidate) => candidate.id === gesture.id)) return;
    const next = [...store.current];
    next.splice(Math.max(0, Math.min(index, next.length)), 0, gesture);
    writeStore(store, next);
  }, [store]);
  return useMemo(() => ({ gestures, add, update, remove, restore }), [add, gestures, remove, restore, update]);
}

export function useCustomGestureDispatch(profile: OctopusClientAccessProfile, enabled: boolean): void {
  const store = customGestureStore(profile);
  useEffect(() => {
    if (!enabled) return;
    const holds = new Map<number, { candidateId: string | null; candidateSince: number; firedId: string | null }>();
    const unsubscribe = subscribeGestureState((state) => {
      const now = performance.now();
      for (const id of [...holds.keys()]) {
        if (!state.hands.some((hand) => hand.id === id)) holds.delete(id);
      }
      for (const hand of state.hands) {
        const hold = holds.get(hand.id) ?? { candidateId: null, candidateSince: 0, firedId: null };
        holds.set(hand.id, hold);
        if (hand.coasting) continue;
        if (hand.engaged || hand.pose === "point" || hand.pose === "pinch") {
          hold.candidateId = null;
          hold.firedId = null;
          continue;
        }
        const best = bestMatch(store.current, hand.curls);
        if (!best || best.distance > MATCH_THRESHOLD) {
          hold.candidateId = null;
          if (hold.firedId && (!best || best.distance > RELEASE_DISTANCE)) hold.firedId = null;
          continue;
        }
        if (hold.candidateId !== best.gesture.id) {
          hold.candidateId = best.gesture.id;
          hold.candidateSince = now;
        }
        if (hold.firedId === best.gesture.id || now - hold.candidateSince < HOLD_MS) continue;
        hold.firedId = best.gesture.id;
        if (best.gesture.command !== "none" && !clientDialogOwnsCommands()) dispatchClientCommand(best.gesture.command, "gesture");
      }
    });
    return unsubscribe;
  }, [enabled, store]);
}

export function curlDistance(left: FingerCurls, right: FingerCurls): number {
  let sum = 0;
  for (const key of CURL_KEYS) {
    const delta = left[key] - right[key];
    sum += delta * delta;
  }
  return Math.sqrt(sum);
}

export function customGestureCommandLabel(command: ClientCommandId | "none"): string {
  if (command === "none") return "No action";
  return clientCommands.find((candidate) => candidate.id === command)?.label ?? command;
}

function bestMatch(gestures: CustomGesture[], curls: FingerCurls): { gesture: CustomGesture; distance: number } | null {
  let best: { gesture: CustomGesture; distance: number } | null = null;
  for (const gesture of gestures) {
    const distance = curlDistance(gesture.curls, curls);
    if (!best || distance < best.distance) best = { gesture, distance };
  }
  return best;
}

function customGestureStore(profile: OctopusClientAccessProfile): CustomGestureStore {
  const existing = stores.get(profile);
  if (existing) return existing;
  const created: CustomGestureStore = { current: readGestures(profile), profile, subscribers: new Set() };
  stores.set(profile, created);
  return created;
}

function subscribeStore(store: CustomGestureStore, subscriber: () => void): () => void {
  store.subscribers.add(subscriber);
  return () => {
    store.subscribers.delete(subscriber);
  };
}

function writeStore(store: CustomGestureStore, gestures: CustomGesture[]): void {
  store.current = gestures;
  try {
    window.localStorage.setItem(storageKey(store.profile), JSON.stringify(gestures));
  } catch {
    void 0;
  }
  for (const subscriber of [...store.subscribers]) subscriber();
}

function readGestures(profile: OctopusClientAccessProfile): CustomGesture[] {
  try {
    const raw = window.localStorage.getItem(storageKey(profile));
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(isCustomGesture);
  } catch {
    return [];
  }
}

function isCustomGesture(value: unknown): value is CustomGesture {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  const curls = candidate.curls as Record<string, unknown> | undefined;
  return typeof candidate.id === "string"
    && typeof candidate.name === "string"
    && typeof candidate.command === "string"
    && Boolean(curls)
    && CURL_KEYS.every((key) => typeof curls?.[key] === "number");
}

function storageKey(profile: OctopusClientAccessProfile): string {
  return profile === "live" ? STORAGE_KEY : `${STORAGE_KEY}.test`;
}

function gestureId(): string {
  return `gesture-${crypto.randomUUID()}`;
}
