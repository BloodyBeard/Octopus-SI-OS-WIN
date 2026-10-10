import type { ChatRunMessage, ChatRunStatus } from "../../chatRuns";
import { operatorTurnResumeAvailable } from "./chatTurnControl";
import type { ChatDraftValue } from "./useChatDraftState";
import type { ChatAttachment, ChatQueuedTurnStatus, ChatRenderMessage } from "./types";

export type QueuedChatTurn = {
  id: string;
  messageId: string;
  runKey: string;
  text: string;
  attachments: ChatAttachment[];
  hadAttachments: boolean;
  eeId: string;
  workerInterfaceId: string;
  providerInterfaceId: string;
  providerBoundInterfaceId?: string;
  providerModel: string;
  reasoningEffort: string;
  endpoint: string;
  createdAtMs: number;
  queuedAfterSeq?: number;
  resumeAfterInterrupt?: boolean;
  status: ChatQueuedTurnStatus;
  intent?: "steer" | "turn";
  owner?: string;
  editOwner?: string;
  draftBeforeEdit?: ChatDraftValue;
  beforeEdit?: { status: "queued" | "failed"; error?: string };
  applicationIds?: string[];
  driverNames?: string[];
  accessMode?: ChatDraftValue["accessMode"];
  continuation?: ChatDraftValue["continuation"];
  clientSurfaceRole?: "home" | "task";
  agentPresetId?: string;
  admittedSeq?: number;
  error?: string;
};
export type QueueState = { revision?: number; turns: QueuedChatTurn[]; pausedRunKeys: string[] };
export type QueuePersistence = {
  read: (key: string) => Promise<QueueState>;
  update: (key: string, change: (state: QueueState) => QueueState) => Promise<QueueState>;
};
const emptyState = (): QueueState => ({ turns: [], pausedRunKeys: [] });

export function queueSubmissionFailureStatus(failure: unknown, bridgeAttempted: boolean): "failed" | "unknown" {
  const notSent = failure !== null && typeof failure === "object"
    && "deliveryState" in failure && failure.deliveryState === "not_sent";
  return !bridgeAttempted || notSent ? "failed" : "unknown";
}

export function queueNamespace(profile: string, home: string): string {
  if (!profile || !home.trim()) throw new Error("The runtime Home is unavailable. Messages have not been sent or queued.");
  return JSON.stringify([profile, home]);
}
export function queueHead(state: QueueState, runKey: string): QueuedChatTurn | null {
  if (state.pausedRunKeys.includes(runKey)) return null;
  const peers = state.turns.filter((item) => item.runKey === runKey);
  if (peers.some((item) => ["sending", "accepted", "unknown"].includes(item.status))) return null;
  const first = peers[0];
  return first?.status === "queued" && first.intent !== "steer"
    && (!first.intent || Number.isFinite(first.queuedAfterSeq)) ? first : null;
}
export function visibleQueueTurns(turns: readonly QueuedChatTurn[], runKey: string): QueuedChatTurn[] {
  return turns.filter((item) => item.runKey === runKey && (item.status === "failed" || item.status === "unknown"
    || item.status === "editing" || (item.status === "queued" && (!item.intent || !item.owner
      || (item.intent !== "steer" && Number.isFinite(item.queuedAfterSeq))))
    || (item.status === "sending" && item.intent !== "steer" && Number.isFinite(item.queuedAfterSeq))));
}
export function pendingQueueSteering(turns: readonly QueuedChatTurn[], runKey: string): ChatRenderMessage[] {
  return turns.filter((item) => item.runKey === runKey && item.intent === "steer"
    && (item.status === "sending" || item.status === "accepted")).map((item) => ({
      id: item.messageId, runId: runKey, role: "user", text: item.text, createdAtMs: item.createdAtMs,
      direction: "in", eventKind: "steering", eventData: {
        steering_status: item.status === "sending" ? "pending" : "submitted",
        steering_provider_interface: item.providerBoundInterfaceId,
      },
      attachments: item.attachments.map(({ file: _file, stage: _stage, ...attachment }) => attachment),
      pending: item.status === "sending", terminal: item.status === "accepted",
    }));
}
export function queueDispatchCandidates(state: QueueState, runs: readonly { runId: string; writable: boolean; turnOpen: boolean; turnState: string }[]): QueuedChatTurn[] {
  return runs.flatMap((run) => {
    const head = queueHead(state, run.runId);
    if (!head || !run.writable || run.turnOpen || (run.turnState !== "idle" && !(head.resumeAfterInterrupt && run.turnState === "failed"))) return [];
    return [head];
  });
}

export function queueTurnReady(turn: QueuedChatTurn, status: ChatRunStatus | undefined, history: readonly ChatRunMessage[]): boolean {
  if (!status || status.archived || status.runId !== turn.runKey || status.turnOpen || !Number.isFinite(turn.queuedAfterSeq)) return false;
  if (status.turnState !== "idle" && !(turn.resumeAfterInterrupt && status.turnState === "failed")) return false;
  const terminal = history.some((message) => message.runId === turn.runKey && message.direction === "out" && message.role === "assistant"
    && message.terminal && (message.eventKind === "message" || (turn.resumeAfterInterrupt && ["interrupted_result", "error"].includes(message.eventKind)))
    && message.movementSeq > turn.queuedAfterSeq!);
  return terminal && (turn.resumeAfterInterrupt === true || !operatorTurnResumeAvailable(history, false));
}

export function claimQueueTurn(state: QueueState, id: string, owner: string, steer: boolean, manual = false): QueueState {
  const turn = state.turns.find((item) => item.id === id);
  if (!turn || turn.status !== "queued" || (turn.owner && turn.owner !== owner)) return state;
  const peers = state.turns.filter((item) => item.runKey === turn.runKey && item.id !== id);
  if (peers.some((item) => item.status === "sending" || item.status === "unknown" || (!steer && item.status === "accepted"))) return state;
  if (!steer && !manual && queueHead(state, turn.runKey)?.id !== id) return state;
  return { ...state, turns: state.turns.map((item) => item.id === id
    ? { ...item, status: "sending", owner, intent: steer ? "steer" : manual ? "turn" : item.intent, error: undefined }
    : item) };
}

export function indexedQueuePersistence(factory: IDBFactory): QueuePersistence {
  let connection: Promise<IDBDatabase> | undefined;
  function open() {
    if (connection) return connection;
    let resolve!: (db: IDBDatabase) => void;
    let reject!: (error: unknown) => void;
    const opening = new Promise<IDBDatabase>((accept, refuse) => { resolve = accept; reject = refuse; });
    connection = opening;
    let failed = false;
    const release = () => { if (connection === opening) connection = undefined; };
    const fail = (error: unknown) => { failed = true; release(); reject(error); };
    try {
      const request = factory.open("octopus.client.turnQueue", 1);
      request.onupgradeneeded = () => request.result.createObjectStore("queues");
      request.onerror = () => fail(request.error);
      request.onblocked = () => fail(new Error("Message storage is blocked by another client window."));
      request.onsuccess = () => {
        const db = request.result;
        if (failed) { db.close(); return; }
        db.onversionchange = () => { db.close(); release(); };
        db.onclose = release;
        resolve(db);
      };
    } catch (error) { fail(error); }
    return opening;
  }
  async function transact(key: string, change?: (state: QueueState) => QueueState): Promise<QueueState> {
    const db = await open();
    return new Promise((resolve, reject) => {
      const transaction = db.transaction("queues", change ? "readwrite" : "readonly");
      const store = transaction.objectStore("queues");
      const request = store.get(key);
      let result = emptyState();
      let failure: unknown;
      request.onsuccess = () => {
        try {
          const state = request.result ?? emptyState();
          if (!Array.isArray(state.turns) || !Array.isArray(state.pausedRunKeys)) throw new Error("Saved message queue is invalid; it has been preserved without sending.");
          result = change ? change(state) : state;
          if (change) store.put(result, key);
        } catch (error) { failure = error; transaction.abort(); }
      };
      transaction.oncomplete = () => resolve(result);
      transaction.onabort = () => reject(failure ?? transaction.error ?? new Error("Message storage transaction aborted."));
      transaction.onerror = () => { failure ??= transaction.error; };
    });
  }
  return { read: (key) => transact(key), update: (key, change) => transact(key, change) };
}

export class ClientTurnQueue {
  snapshot: QueueState & { ready: boolean; error: string } = { ...emptyState(), ready: false, error: "" };
  listeners = new Set<() => void>();
  private reads = 0;
  constructor(readonly key: string, private persistence: QueuePersistence, private notify: () => void = () => {}) {}
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  getSnapshot = () => this.snapshot;
  private publish(state: QueueState) {
    if ((state.revision ?? 0) < (this.snapshot.revision ?? 0)) return;
    this.snapshot = { ...state, ready: true, error: "" };
    this.listeners.forEach((listener) => listener());
  }
  reportError(error: unknown) {
    this.snapshot = { ...this.snapshot, ready: false, error: error instanceof Error ? error.message : String(error) };
    this.listeners.forEach((listener) => listener());
  }
  async refresh() {
    const generation = ++this.reads;
    try { const state = await this.persistence.read(this.key); if (generation === this.reads) this.publish(state); }
    catch (error) { if (generation === this.reads) this.reportError(error); }
  }
  async change(change: (state: QueueState) => QueueState) {
    ++this.reads;
    try {
      const state = await this.persistence.update(this.key, (current) => ({ ...change(current), revision: (current.revision ?? 0) + 1 }));
      this.publish(state);
      this.notify();
      return state;
    } catch (error) { this.reportError(error); throw error; }
  }
  async claim(id: string, owner: string, steer: boolean, manual = false) {
    let claimed: QueuedChatTurn | null = null;
    await this.change((state) => {
      const next = claimQueueTurn(state, id, owner, steer, manual);
      if (next !== state) claimed = next.turns.find((item) => item.id === id) ?? null;
      return next;
    });
    return claimed as QueuedChatTurn | null;
  }
  async settle(id: string, owner: string, status: "queued" | "accepted" | "failed" | "unknown", error?: string, admittedSeq?: number, runKey?: string) {
    await this.change((state) => {
      const target = state.turns.find((item) => item.id === id && item.owner === owner && (item.status === "sending" || item.status === "unknown"));
      if (!target) return state;
      return { pausedRunKeys: status === "unknown" ? [...new Set([...state.pausedRunKeys, target.runKey])] : state.pausedRunKeys,
        turns: state.turns.map((item) => item === target ? { ...item, runKey: runKey ?? item.runKey, status, error, admittedSeq, owner: status === "unknown" ? owner : undefined } : item) };
    });
  }
  async recoverOwners(liveOwners: Set<string>, observedOwners: Set<string>) {
    await this.change((state) => {
      const turns: QueuedChatTurn[] = state.turns.map((item) => item.status === "sending" && observedOwners.has(item.owner ?? "") && !liveOwners.has(item.owner ?? "")
        ? { ...item, status: "unknown", error: "Delivery is unconfirmed after the sending view closed. Check task history; this message will not be retried automatically." }
        : item.status === "queued" && item.intent && observedOwners.has(item.owner ?? "") && !liveOwners.has(item.owner ?? "")
          ? { ...item, status: "failed", owner: undefined, error: "The sending view closed before submission. This message was not sent." }
        : item.editOwner && observedOwners.has(item.editOwner) && !liveOwners.has(item.editOwner) ? { ...item, editOwner: undefined } : item);
      return { turns, pausedRunKeys: [...new Set([...state.pausedRunKeys, ...turns.filter((item) => item.status === "unknown").map((item) => item.runKey)])] };
    });
  }
  async retire(owner: string) {
    await this.change((state) => {
      const turns: QueuedChatTurn[] = state.turns.map((item) => item.owner === owner && item.status === "sending"
        ? { ...item, status: "unknown", error: "Delivery is unconfirmed. The sending view closed; check task history before removing this message. It will not be retried automatically." }
        : item.owner === owner && item.status === "queued" && item.intent
          ? { ...item, status: "failed", owner: undefined, error: "The sending view closed before submission. This message was not sent." }
        : item.editOwner === owner ? { ...item, editOwner: undefined } : item);
      return { turns, pausedRunKeys: [...new Set([...state.pausedRunKeys, ...turns.filter((item) => item.status === "unknown").map((item) => item.runKey)])] };
    });
  }
}
