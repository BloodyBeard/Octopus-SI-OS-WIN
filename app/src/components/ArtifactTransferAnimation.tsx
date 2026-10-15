import { useReducedMotion } from "motion/react";
import { useCallback, useEffect, useId, useState, useSyncExternalStore } from "react";
import type { InterfaceSummary, ProjectionStatus } from "../data/topology";
import { hasProjectionBridge, readProjection } from "../projectionClient";
import { subscribeProjectionChanges } from "../projectionLive";

export const ARTIFACT_TRANSFER_DURATION_MS = 500;
export const ARTIFACT_TRANSFER_GAP_MS = 300;
export const ARTIFACT_TRANSFER_PATH_LIMIT = 48;
const TRANSFER_KINDS = ["ArtifactMoved", "InterfaceArrival", "InterfaceOutbound"];

export type ArtifactTransfer = {
  id: string;
  sequence: number;
  from: string;
  to: string;
  runId: string;
  checkpointId: string;
  traceId?: string;
  movementCount?: number;
  firstSequence?: number;
  lastSequence?: number;
  startedAt: number;
};

export type TransferPoint = { x: number; y: number };
export type TransferTrajectory = { transfer: ArtifactTransfer; points: TransferPoint[] };

export function scheduleArtifactTransfers(current: ArtifactTransfer[], batch: Omit<ArtifactTransfer, "startedAt">[], now: number): ArtifactTransfer[] {
  type PendingTransfer = Omit<ArtifactTransfer, "startedAt">;
  type Path = { anchor?: ArtifactTransfer; pending: Map<string, PendingTransfer>; lastSequence: number };
  const paths = new Map<string, Path>();
  const interval = ARTIFACT_TRANSFER_DURATION_MS + ARTIFACT_TRANSFER_GAP_MS;
  const pathFor = (transfer: PendingTransfer) => {
    const key = JSON.stringify([transfer.runId, [transfer.from, transfer.to].sort()]);
    let path = paths.get(key);
    if (!path) {
      path = { pending: new Map(), lastSequence: transfer.lastSequence ?? transfer.sequence };
      paths.set(key, path);
    }
    path.lastSequence = Math.max(path.lastSequence, transfer.lastSequence ?? transfer.sequence);
    return path;
  };
  const coalesce = (transfer: PendingTransfer) => {
    const path = pathFor(transfer);
    const direction = JSON.stringify([transfer.from, transfer.to]);
    const previous = path.pending.get(direction);
    const first = !previous || transfer.sequence < previous.sequence ? transfer : previous;
    path.pending.set(direction, {
      ...first,
      movementCount: (previous?.movementCount ?? (previous ? 1 : 0)) + (transfer.movementCount ?? 1),
      firstSequence: Math.min(previous?.firstSequence ?? previous?.sequence ?? transfer.sequence,
        transfer.firstSequence ?? transfer.sequence),
      lastSequence: Math.max(previous?.lastSequence ?? previous?.sequence ?? transfer.sequence,
        transfer.lastSequence ?? transfer.sequence),
    });
  };
  for (const transfer of current) {
    if (now >= transfer.startedAt + interval) continue;
    if (transfer.startedAt <= now) pathFor(transfer).anchor = transfer;
    else coalesce(transfer);
  }
  for (const transfer of batch) coalesce(transfer);
  return [...paths.values()]
    .sort((left, right) => right.lastSequence - left.lastSequence)
    .slice(0, ARTIFACT_TRANSFER_PATH_LIMIT)
    .flatMap((path) => {
      let startedAt = Math.max(now, path.anchor ? path.anchor.startedAt + interval : now);
      const pending = [...path.pending.values()].sort((left, right) => left.sequence - right.sequence);
      return [...(path.anchor ? [path.anchor] : []), ...pending.map((transfer) => {
        const scheduled = { ...transfer, startedAt };
        startedAt += interval;
        return scheduled;
      })];
    }).sort((left, right) => left.sequence - right.sequence);
}

type MovementPage = {
  movements: Array<Record<string, unknown>>;
  next_seq: number;
  next_before_seq?: number;
};

export async function readArtifactTransferBatch(cursor: number | null) {
  let before: number | undefined;
  let nextSeq: number | undefined;
  const transfers: Omit<ArtifactTransfer, "startedAt">[] = [];
  const sequences = new Set<number>();
  do {
    const page = await readProjection<MovementPage>("projection.movements.list", {
      limit: cursor === null ? 1 : 500,
      ...(cursor === null ? {} : { kinds: TRANSFER_KINDS }),
      ...(cursor !== null && cursor > 0 ? { after_seq: cursor - 1 } : {}),
      ...(before === undefined ? {} : { before_seq: before }),
    });
    if (!Array.isArray(page?.movements) || !Number.isSafeInteger(page.next_seq)
      || page.next_seq < (nextSeq ?? cursor ?? 0)) {
      throw new Error("Transfer animation received an invalid or regressed movement cursor.");
    }
    nextSeq ??= page.next_seq;
    if (cursor === null) return { nextSeq, transfers };
    for (const row of page.movements) {
      const seq = row.seq;
      if (typeof seq !== "number" || !Number.isSafeInteger(seq) || seq < cursor
        || seq >= nextSeq || before !== undefined && seq >= before || sequences.has(seq)) {
        throw new Error("Transfer animation received movements outside its sequence window.");
      }
      sequences.add(seq);
      if (!TRANSFER_KINDS.includes(String(row.kind))) continue;
      const from = record(row.from);
      const to = record(row.to);
      if (from.kind !== "interface" || to.kind !== "interface"
        || !text(from.id) || !text(to.id) || !text(row.artifact_id) || !text(row.movement_id)) {
        throw new Error("Transfer animation received an incomplete interface movement.");
      }
      const runtime = record(record(record(row.raw).invocation_metadata).runtime);
      transfers.push({
        id: text(row.movement_id),
        sequence: seq,
        from: text(from.id),
        to: text(to.id),
        runId: text(row.raw_run_id),
        checkpointId: text(runtime.checkpoint),
        traceId: text(row.trace),
      });
    }
    const older = page.next_before_seq;
    if (older !== undefined && (!Number.isSafeInteger(older) || older <= cursor
      || older >= (before ?? nextSeq) || !sequences.has(older))) {
      throw new Error("Transfer animation received an invalid movement page boundary.");
    }
    before = older;
  } while (before !== undefined);
  return { nextSeq: nextSeq!, transfers: transfers.sort((a, b) => a.sequence - b.sequence) };
}

export type ArtifactTransferFeed = {
  snapshot: () => ArtifactTransfer[];
  subscribe: (listener: () => void) => () => void;
};

type ArtifactTransferStore = ArtifactTransferFeed & {
  update: (next: (current: ArtifactTransfer[]) => ArtifactTransfer[]) => void;
};

const NO_TRANSFERS: ArtifactTransfer[] = [];

function createArtifactTransferStore(): ArtifactTransferStore {
  let transfers = NO_TRANSFERS;
  const listeners = new Set<() => void>();
  return {
    snapshot: () => transfers,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    update: (next) => {
      const value = next(transfers);
      if (value === transfers) return;
      transfers = value;
      for (const listener of [...listeners]) listener();
    },
  };
}

export function useArtifactTransferList(feed: ArtifactTransferFeed): ArtifactTransfer[] {
  return useSyncExternalStore(feed.subscribe, feed.snapshot, feed.snapshot);
}

export function useArtifactTransfers(projection: ProjectionStatus) {
  const { feed, error } = useArtifactTransferFeed(projection);
  return { transfers: useArtifactTransferList(feed), error };
}

export function useArtifactTransferFeed(projection: ProjectionStatus): { feed: ArtifactTransferFeed; error: string } {
  const reducedMotion = useReducedMotion();
  const enabled = projection.mode === "live" && reducedMotion === false && hasProjectionBridge();
  const [store] = useState(createArtifactTransferStore);
  const [error, setError] = useState("");
  useEffect(() => {
    store.update(() => NO_TRANSFERS);
    setError("");
    if (!enabled) return;
    let cancelled = false;
    let cursor: number | null = null;
    let reading = false;
    let pending = false;
    const refresh = async () => {
      reading = true;
      while (pending && !cancelled) {
        pending = false;
        try {
          const batch = await readArtifactTransferBatch(cursor);
          if (cancelled) break;
          cursor = batch.nextSeq;
          setError("");
          if (batch.transfers.length) {
            const now = window.performance.now();
            store.update((current) => scheduleArtifactTransfers(current, batch.transfers, now));
          }
        } catch (failure) {
          if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure));
        }
      }
      reading = false;
    };
    const request = () => {
      pending = true;
      if (!reading) void refresh();
    };
    const unsubscribe = subscribeProjectionChanges(request);
    request();
    return () => { cancelled = true; unsubscribe(); store.update(() => NO_TRANSFERS); };
  }, [enabled, projection.endpoint, store]);
  return { feed: store, error: enabled ? error : "" };
}

export function transferSegment(points: TransferPoint[], progress: number) {
  const lengths = points.slice(1).map((point, index) => Math.hypot(point.x - points[index].x, point.y - points[index].y));
  const length = lengths.reduce((sum, value) => sum + value, 0);
  if (!length || progress <= 0 || progress >= 1) return null;
  const segmentLength = Math.min(42, length * .16);
  const head = progress * (length + segmentLength);
  const start = Math.max(0, head - segmentLength);
  const end = Math.min(length, head);
  const segment: TransferPoint[] = [];
  let offset = 0;
  for (let index = 0; index < lengths.length; index += 1) {
    const edgeLength = lengths[index];
    const edgeStart = Math.max(start, offset);
    const edgeEnd = Math.min(end, offset + edgeLength);
    if (edgeLength && edgeEnd > edgeStart) {
      const from = points[index];
      const to = points[index + 1];
      const pointAt = (distance: number) => ({
        x: from.x + (to.x - from.x) * (distance - offset) / edgeLength,
        y: from.y + (to.y - from.y) * (distance - offset) / edgeLength,
      });
      if (!segment.length) segment.push(pointAt(edgeStart));
      segment.push(pointAt(edgeEnd));
    }
    offset += edgeLength;
  }
  return segment.length > 1 ? segment : null;
}

export function ArtifactTransferSegments({ transfers, resolve }: {
  transfers: ArtifactTransfer[];
  resolve: () => TransferTrajectory[];
}) {
  const prefix = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const [frame, setFrame] = useState<{ now: number; trajectories: TransferTrajectory[] }>(() => ({
    now: window.performance.now(),
    trajectories: [],
  }));
  useEffect(() => {
    if (!transfers.length) return;
    const trajectories = resolve();
    let request = 0;
    const draw = () => {
      const time = window.performance.now();
      setFrame({ now: time, trajectories });
      if (transfers.some((transfer) => time - transfer.startedAt < ARTIFACT_TRANSFER_DURATION_MS)) {
        request = window.requestAnimationFrame(draw);
      }
    };
    draw();
    return () => window.cancelAnimationFrame(request);
  }, [resolve, transfers]);
  const segments = frame.trajectories.flatMap(({ transfer, points }) => {
    const segment = transferSegment(points, (frame.now - transfer.startedAt) / ARTIFACT_TRANSFER_DURATION_MS);
    return segment ? [{ transfer, segment }] : [];
  });
  if (!segments.length) return null;
  return (
    <g className="artifact-transfer-animations" aria-hidden="true">
      <defs>
        {segments.map(({ transfer, segment }) => (
          <linearGradient id={`${prefix}-${transfer.sequence}`} gradientUnits="userSpaceOnUse"
            x1={segment[0].x} y1={segment[0].y} x2={segment.at(-1)!.x} y2={segment.at(-1)!.y} key={transfer.sequence}>
            <stop offset="0" stopColor="var(--artifact-transfer-color)" stopOpacity="0" />
            <stop offset=".3" stopColor="var(--artifact-transfer-color)" />
            <stop offset=".7" stopColor="var(--artifact-transfer-color)" />
            <stop offset="1" stopColor="var(--artifact-transfer-color)" stopOpacity="0" />
          </linearGradient>
        ))}
      </defs>
      {segments.map(({ transfer, segment }) => (
        <path className="artifact-transfer-segment" stroke={`url(#${prefix}-${transfer.sequence})`}
          d={segment.map((point, index) => `${index ? "L" : "M"}${point.x},${point.y}`).join(" ")}
          data-movement-id={transfer.id} data-movement-count={transfer.movementCount ?? 1}
          data-first-sequence={transfer.firstSequence ?? transfer.sequence} data-last-sequence={transfer.lastSequence ?? transfer.sequence}
          key={transfer.sequence} />
      ))}
    </g>
  );
}

export function SystemArtifactTransfers({ container, feed, interfaces }: {
  container: HTMLElement | null;
  feed: ArtifactTransferFeed;
  interfaces: Record<string, InterfaceSummary>;
}) {
  const transfers = useArtifactTransferList(feed);
  const resolve = useCallback(() => {
    if (!container) return [];
    const origin = container.getBoundingClientRect();
    const anchors = [...container.querySelectorAll<HTMLElement>("[data-projection-interface]")].flatMap((element) => {
      const bounds = element.getBoundingClientRect();
      if (!bounds.width || !bounds.height) return [];
      return [{
        id: element.dataset.projectionInterface,
        runId: element.closest<HTMLElement>("[data-run-id]")?.dataset.runId,
        checkpointId: element.dataset.projectionCheckpoint,
        worker: element.dataset.projectionAnchor === "worker",
        point: { x: bounds.x + bounds.width / 2 - origin.x, y: bounds.y + bounds.height / 2 - origin.y },
      }];
    });
    const endpoint = (id: string, transfer: ArtifactTransfer) => {
      const candidates = anchors.filter((anchor) => anchor.id === id);
      const worker = interfaces[id]?.descriptor.role === "worker";
      const matching = worker
        ? candidates.filter((anchor) => anchor.worker && anchor.runId === transfer.runId)
        : candidates.filter((anchor) => anchor.runId === transfer.runId
          && (!transfer.checkpointId || anchor.checkpointId === transfer.checkpointId));
      const represented = worker && matching.length === 0 ? candidates.filter((anchor) => anchor.worker) : matching;
      return represented.length === 1 ? represented[0].point : null;
    };
    return transfers.flatMap((transfer) => {
      if (interfaces[transfer.from]?.type !== "system" || interfaces[transfer.to]?.type !== "system") return [];
      const from = endpoint(transfer.from, transfer);
      const to = endpoint(transfer.to, transfer);
      return from && to ? [{ transfer, points: [from, to] }] : [];
    });
  }, [container, interfaces, transfers]);
  return transfers.length ? (
    <svg className="system-transfer-layer" aria-hidden="true">
      <ArtifactTransferSegments transfers={transfers} resolve={resolve} />
    </svg>
  ) : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
