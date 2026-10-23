import { useShouldReduceMotion } from "../appearance";
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import type { InterfaceSummary, ProjectionStatus } from "../data/topology";
import { hasProjectionBridge, readProjection } from "../projectionClient";
import { subscribeProjectionChanges } from "../projectionLive";

export const ARTIFACT_TRANSFER_DURATION_MS = 500;
export const ARTIFACT_TRANSFER_GAP_MS = 300;
export const ARTIFACT_TRANSFER_PATH_LIMIT = 48;
const TRANSFER_KINDS = ["ArtifactMoved", "InterfaceArrival", "InterfaceOutbound"];
const MAX_TRANSFER_FRAME_MS = ARTIFACT_TRANSFER_DURATION_MS / 2;

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
type TransferPresentation = { startedAt: number; updatedAt: number; completedAt?: number };

export function scheduleArtifactTransfers(current: ArtifactTransfer[], batch: Omit<ArtifactTransfer, "startedAt">[], now: number, presented: ReadonlyMap<number, TransferPresentation>): ArtifactTransfer[] {
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
    const presentation = presented.get(transfer.sequence);
    if (!presentation) coalesce(transfer);
    else if (presentation.completedAt === undefined || now < presentation.completedAt + ARTIFACT_TRANSFER_GAP_MS) {
      pathFor(transfer).anchor = { ...transfer, startedAt: presentation.completedAt === undefined
        ? presentation.startedAt : presentation.completedAt - ARTIFACT_TRANSFER_DURATION_MS };
    }
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
  presentation: (sequence: number) => TransferPresentation | undefined;
  present: (sequence: number, presentation: TransferPresentation) => void;
};

type ArtifactTransferStore = ArtifactTransferFeed & {
  update: (next: (current: ArtifactTransfer[]) => ArtifactTransfer[]) => void;
  append: (batch: Omit<ArtifactTransfer, "startedAt">[], now: number) => void;
};

const NO_TRANSFERS: ArtifactTransfer[] = [];

function createArtifactTransferStore(): ArtifactTransferStore {
  let transfers = NO_TRANSFERS;
  const listeners = new Set<() => void>();
  const presented = new Map<number, TransferPresentation>();
  const store: ArtifactTransferStore = {
    snapshot: () => transfers,
    presentation: (sequence) => presented.get(sequence),
    present: (sequence, presentation) => {
      if (presented.get(sequence)?.completedAt !== undefined) return;
      if (transfers.some((transfer) => transfer.sequence === sequence)) presented.set(sequence, presentation);
    },
    subscribe: (listener) => {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    update: (next) => {
      const value = next(transfers);
      if (value === transfers) return;
      transfers = value;
      const sequences = new Set(value.map((transfer) => transfer.sequence));
      for (const sequence of presented.keys()) if (!sequences.has(sequence)) presented.delete(sequence);
      for (const listener of [...listeners]) listener();
    },
    append: (batch, now) => store.update((current) => scheduleArtifactTransfers(current, batch, now, presented)),
  };
  return store;
}

export function useArtifactTransferList(feed: ArtifactTransferFeed): ArtifactTransfer[] {
  return useSyncExternalStore(feed.subscribe, feed.snapshot, feed.snapshot);
}

export function useArtifactTransfers(projection: ProjectionStatus) {
  const { feed, error } = useArtifactTransferFeed(projection);
  return { transfers: useArtifactTransferList(feed), error };
}

export function useArtifactTransferFeed(projection: ProjectionStatus): { feed: ArtifactTransferFeed; error: string } {
  const reducedMotion = useShouldReduceMotion();
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
            store.append(batch.transfers, now);
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

export function ArtifactTransferSegments({ transfers, resolve, presentation, onPresent, container }: {
  transfers: ArtifactTransfer[];
  resolve: () => TransferTrajectory[];
  presentation: ArtifactTransferFeed["presentation"];
  onPresent: ArtifactTransferFeed["present"];
  container?: HTMLElement | null;
}) {
  const prefix = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const layer = useRef<SVGGElement>(null);
  const playback = useRef(new Map<number, { startedAt: number; updatedAt: number; shown: boolean; completed: boolean }>());
  const [trajectories, setTrajectories] = useState<TransferTrajectory[]>([]);
  useLayoutEffect(() => {
    setTrajectories(resolve());
    if (!container) return;
    let request = 0;
    const refresh = () => {
      if (request) return;
      request = window.requestAnimationFrame(() => {
        request = 0;
        setTrajectories(resolve());
      });
    };
    const mutations = new window.MutationObserver((records) => {
      if (records.some((record) => !layer.current?.contains(record.target))) refresh();
    });
    mutations.observe(container, { childList: true, subtree: true, attributes: true,
      attributeFilter: ["class", "style", "data-projection-interface", "data-projection-checkpoint"] });
    const resize = typeof window.ResizeObserver === "function" ? new window.ResizeObserver(refresh) : null;
    resize?.observe(container);
    container.addEventListener("scroll", refresh, true);
    container.addEventListener("transitionend", refresh);
    window.addEventListener("resize", refresh);
    return () => {
      mutations.disconnect();
      resize?.disconnect();
      container.removeEventListener("scroll", refresh, true);
      container.removeEventListener("transitionend", refresh);
      window.removeEventListener("resize", refresh);
      if (request) window.cancelAnimationFrame(request);
    };
  }, [container, resolve, transfers]);
  useLayoutEffect(() => {
    const sequences = new Set(transfers.map((transfer) => transfer.sequence));
    for (const sequence of playback.current.keys()) {
      if (!sequences.has(sequence)) playback.current.delete(sequence);
    }
    const paths = layer.current!.querySelectorAll<SVGPathElement>(".artifact-transfer-segment");
    const gradients = layer.current!.querySelectorAll<SVGLinearGradientElement>("linearGradient");
    const mountedAt = window.performance.now();
    const nodes = trajectories.map(({ transfer, points }, index) => {
      let clock = playback.current.get(transfer.sequence);
      if (!clock) {
        const retained = presentation(transfer.sequence);
        clock = {
          startedAt: retained?.startedAt ?? Math.max(transfer.startedAt, mountedAt),
          updatedAt: retained?.updatedAt ?? mountedAt,
          shown: retained !== undefined,
          completed: retained?.completedAt !== undefined,
        };
        playback.current.set(transfer.sequence, clock);
      }
      return { transfer, points, clock, path: paths[index], gradient: gradients[index],
        key: JSON.stringify([transfer.runId, [transfer.from, transfer.to].sort()]) };
    }).filter(({ points }) => points.some((point, index) => index > 0
      && (point.x !== points[index - 1].x || point.y !== points[index - 1].y)))
      .sort((left, right) => left.transfer.startedAt - right.transfer.startedAt || left.transfer.sequence - right.transfer.sequence);
    let request = 0;
    const draw = (advance: boolean) => {
      const time = window.performance.now();
      const pathEnds = new Map<string, number>();
      let pending = false;
      for (const { transfer, points, clock, path, gradient, key } of nodes) {
        if (advance && !clock.completed) {
          if (!clock.shown) clock.startedAt = Math.max(clock.startedAt, pathEnds.get(key) ?? clock.startedAt);
          const elapsed = Math.max(0, time - Math.max(clock.updatedAt, clock.startedAt));
          clock.startedAt += Math.max(0, elapsed - MAX_TRANSFER_FRAME_MS);
          clock.updatedAt = time;
          clock.completed = time >= clock.startedAt + ARTIFACT_TRANSFER_DURATION_MS;
        }
        pathEnds.set(key, (clock.completed ? clock.updatedAt : clock.startedAt + ARTIFACT_TRANSFER_DURATION_MS) + ARTIFACT_TRANSFER_GAP_MS);
        const segment = transferSegment(points, (clock.updatedAt - clock.startedAt) / ARTIFACT_TRANSFER_DURATION_MS);
        path.setAttribute("d", segment ? segment.map((point, index) => `${index ? "L" : "M"}${point.x},${point.y}`).join(" ") : "");
        if (segment) {
          onPresent(transfer.sequence, { startedAt: clock.startedAt, updatedAt: clock.updatedAt });
          clock.shown = true;
          gradient.setAttribute("x1", String(segment[0].x));
          gradient.setAttribute("y1", String(segment[0].y));
          gradient.setAttribute("x2", String(segment.at(-1)!.x));
          gradient.setAttribute("y2", String(segment.at(-1)!.y));
        }
        if (clock.completed && clock.shown) onPresent(transfer.sequence, {
          startedAt: clock.startedAt, updatedAt: clock.updatedAt, completedAt: clock.updatedAt,
        });
        pending ||= !clock.completed;
      }
      if (pending) request = window.requestAnimationFrame(() => draw(true));
    };
    draw(false);
    return () => { if (request) window.cancelAnimationFrame(request); };
  }, [onPresent, presentation, trajectories, transfers]);
  return (
    <g className="artifact-transfer-animations" aria-hidden="true" ref={layer}>
      <defs>
        {trajectories.map(({ transfer }) => (
          <linearGradient id={`${prefix}-${transfer.sequence}`} gradientUnits="userSpaceOnUse"
            key={transfer.sequence}>
            <stop offset="0" stopColor="var(--artifact-transfer-color)" stopOpacity="0" />
            <stop offset=".3" stopColor="var(--artifact-transfer-color)" />
            <stop offset=".7" stopColor="var(--artifact-transfer-color)" />
            <stop offset="1" stopColor="var(--artifact-transfer-color)" stopOpacity="0" />
          </linearGradient>
        ))}
      </defs>
      {trajectories.map(({ transfer }) => (
        <path className="artifact-transfer-segment" stroke={`url(#${prefix}-${transfer.sequence})`}
          d=""
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
    const anchors = new Map<string, Array<{ runId?: string; checkpointId?: string; worker: boolean; point: TransferPoint }>>();
    for (const element of container.querySelectorAll<HTMLElement>("[data-projection-interface]")) {
      const bounds = element.getBoundingClientRect();
      if (!bounds.width || !bounds.height) continue;
      const id = element.dataset.projectionInterface!;
      const members = anchors.get(id) ?? [];
      members.push({
        runId: element.closest<HTMLElement>("[data-run-id]")?.dataset.runId,
        checkpointId: element.dataset.projectionCheckpoint,
        worker: element.dataset.projectionAnchor === "worker",
        point: { x: bounds.x + bounds.width / 2 - origin.x, y: bounds.y + bounds.height / 2 - origin.y },
      });
      anchors.set(id, members);
    }
    const endpoint = (id: string, transfer: ArtifactTransfer) => {
      const candidates = anchors.get(id) ?? [];
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
      <ArtifactTransferSegments transfers={transfers} resolve={resolve} presentation={feed.presentation} onPresent={feed.present} container={container} />
    </svg>
  ) : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
