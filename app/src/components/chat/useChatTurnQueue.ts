import { useEffect, useMemo, useSyncExternalStore } from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import { ClientTurnQueue, indexedQueuePersistence, queueHead, queueNamespace, type QueuedChatTurn } from "./clientTurnQueue";
export type { QueuedChatTurn } from "./clientTurnQueue";

const queues = new Map<string, ClientTurnQueue>();
function sharedQueue(profile: string, home: string): ClientTurnQueue {
  const key = home ? queueNamespace(profile, home) : "";
  const existing = queues.get(key);
  if (existing) return existing;
  const unavailable = async (): Promise<never> => { throw new Error("The runtime Home is unavailable. Messages have not been sent or queued."); };
  const storageUnavailable = async (): Promise<never> => { throw new Error("Durable message storage is unavailable in this client."); };
  const persistence = !key ? { read: unavailable, update: unavailable } : globalThis.indexedDB
    ? indexedQueuePersistence(globalThis.indexedDB) : { read: storageUnavailable, update: storageUnavailable };
  const channel = key && typeof BroadcastChannel !== "undefined" ? new BroadcastChannel(`octopus:queue:${key}`) : null;
  const queue = new ClientTurnQueue(key, persistence, () => channel?.postMessage("changed"));
  if (channel) channel.onmessage = () => { void queue.refresh(); };
  queues.set(key, queue);
  return queue;
}

export function useChatTurnQueue(accessProfile: OctopusClientAccessProfile, runtimeHome: string) {
  const queueStore = useMemo(() => sharedQueue(accessProfile, runtimeHome), [accessProfile, runtimeHome]);
  const snapshot = useSyncExternalStore(queueStore.subscribe, queueStore.getSnapshot);
  const queueOwner = useMemo(() => ({ id: crypto.randomUUID(), active: false, ready: false }), [queueStore]);
  useEffect(() => {
    const owner = crypto.randomUUID();
    queueOwner.id = owner;
    queueOwner.active = true;
    queueOwner.ready = false;
    let release = () => {};
    const retired = new Promise<void>((resolve) => { release = resolve; });
    const locks = globalThis.navigator?.locks;
    if (!locks || !queueStore.key) {
      void queueStore.refresh();
      return () => { queueOwner.active = false; };
    }
    void locks.request(`octopus:queue-owner:${owner}`, async () => {
      while (queueOwner.active && queueOwner.id === owner) {
        try {
          await queueStore.refresh();
          if (!queueStore.snapshot.ready) throw new Error(queueStore.snapshot.error);
          const observedOwners = new Set(queueStore.snapshot.turns.flatMap((item) => [item.owner, item.editOwner].filter((owner): owner is string => Boolean(owner))));
          const held = (await locks.query()).held ?? [];
          if (!queueOwner.active || queueOwner.id !== owner) return;
          await queueStore.recoverOwners(new Set(held.flatMap((lock) => lock.name?.startsWith("octopus:queue-owner:") ? [lock.name.slice("octopus:queue-owner:".length)] : [])), observedOwners);
          if (!queueOwner.active || queueOwner.id !== owner) return;
          queueOwner.ready = true;
          await queueStore.refresh();
          await retired;
          return;
        } catch (failure) {
          if (!queueOwner.active || queueOwner.id !== owner) return;
          queueOwner.ready = false;
          queueStore.reportError(failure);
          let timer: ReturnType<typeof setTimeout> | undefined;
          await Promise.race([retired, new Promise<void>((resolve) => { timer = setTimeout(resolve, 1_000); })]);
          clearTimeout(timer);
        }
      }
    }).catch((failure) => { queueOwner.ready = false; queueStore.reportError(failure); });
    return () => {
      queueOwner.active = false;
      queueOwner.ready = false;
      void queueStore.retire(owner).finally(release).catch(() => {});
    };
  }, [queueStore, queueOwner]);
  useEffect(() => {
    if (!snapshot.error || !queueOwner.active || !queueOwner.ready) return;
    const timer = setTimeout(() => { void queueStore.refresh(); }, 1_000);
    return () => clearTimeout(timer);
  }, [queueStore, queueOwner, snapshot]);
  const refs = useMemo(() => ({
    queuedTurnsRef: { get current() { return queueStore.snapshot.turns; } },
    pausedQueueRunKeysRef: { get current() { return queueStore.snapshot.pausedRunKeys; } },
  }), [queueStore]);

  const methods = useMemo(() => {
    async function replaceQueuedTurns(update: QueuedChatTurn[] | ((current: QueuedChatTurn[]) => QueuedChatTurn[])) {
      const state = await queueStore.change((current) => ({ ...current, turns: typeof update === "function" ? update(current.turns) : update }));
      return state.turns;
    }
    async function replacePausedQueueRunKeys(update: string[] | ((current: string[]) => string[])) {
      const state = await queueStore.change((current) => ({ ...current, pausedRunKeys: typeof update === "function" ? update(current.pausedRunKeys) : update }));
      return state.pausedRunKeys;
    }
    function nextQueuedTurn(runKey: string) { return queueHead(queueStore.snapshot, runKey); }
    return { replaceQueuedTurns, replacePausedQueueRunKeys, nextQueuedTurn };
  }, [queueStore]);
  return {
    ...refs, ...methods, queueStore, queueOwner, queueReady: snapshot.ready && queueOwner.ready,
    queueError: snapshot.error || (!globalThis.navigator?.locks ? "This client cannot coordinate message delivery across windows." : ""),
    queuedTurns: snapshot.turns, pausedQueueRunKeys: snapshot.pausedRunKeys,
  };
}
