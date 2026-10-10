import { useEffect, useRef, useState } from "react";
import type { ChatRunEventKind } from "../../chatRuns";
import type { ChatAttachmentView } from "../chat/types";
import { readProjection } from "../../projectionClient";
import { useProjectionChange } from "../../projectionLive";
import { useAppErrorToast } from "../../toasts";

type RunActivityRow = Record<string, unknown>;
type RunActivityCursor = {
  identity: string;
  mtimes: number[];
  primarySeq: number;
  sizes: number[];
};

type RunActivityResponse = {
  activity?: RunActivityRow[];
  has_newer?: boolean;
  source_cursor?: unknown;
  total?: number;
  truncated?: boolean;
};

export type HomeActivityEndpoint = {
  kind: string;
  id: string;
  eeId: string;
};

export type HomeRunActivityEntry = {
  id: string;
  runId: string;
  artifactId: string;
  contentHash: string;
  label: string;
  direction: "in" | "out" | "internal";
  eventKind: ChatRunEventKind;
  eventData: Record<string, unknown>;
  text: string;
  error: boolean;
  terminal: boolean;
  createdAtMs: number;
  movementSeq: number;
  movementKind: string;
  interfaceId: string;
  role: "user" | "assistant" | "system";
  verification: string;
  from: HomeActivityEndpoint;
  to: HomeActivityEndpoint;
  attachments: ChatAttachmentView[];
  payloadError: string;
};

export type HomeRunActivity = {
  entries: HomeRunActivityEntry[];
  error: string;
  loading: boolean;
  total: number;
  truncated: boolean;
};

const ACTIVITY_BATCH_SIZE = 250;
const ACTIVITY_WINDOW_LIMIT = 1_000;

export function useHomeRunActivity(runId: string, enabled: boolean): HomeRunActivity {
  const projectionChange = useProjectionChange();
  const [entries, setEntries] = useState<HomeRunActivityEntry[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [total, setTotal] = useState(0);
  const [truncated, setTruncated] = useState(false);
  const entriesRef = useRef<HomeRunActivityEntry[]>([]);
  const totalRef = useRef(0);
  const truncatedRef = useRef(false);
  const sourceCursorRef = useRef<RunActivityCursor | null>(null);
  const loadedKeyRef = useRef("");
  useAppErrorToast(error);

  useEffect(() => {
    entriesRef.current = [];
    totalRef.current = 0;
    truncatedRef.current = false;
    sourceCursorRef.current = null;
    loadedKeyRef.current = "";
  }, [runId]);

  useEffect(() => {
    if (!enabled || !runId) {
      entriesRef.current = [];
      totalRef.current = 0;
      truncatedRef.current = false;
      sourceCursorRef.current = null;
      loadedKeyRef.current = "";
      setEntries([]);
      setError("");
      setLoading(false);
      setTotal(0);
      setTruncated(false);
      return;
    }
    let cancelled = false;
    const loadedKey = runId;
    const incremental = projectionChange.kind === "ledger"
      && loadedKeyRef.current === loadedKey
      && sourceCursorRef.current !== null;
    if (!incremental) setLoading(true);
    void (async () => {
      const readCatchup = async (
        afterSeq: number,
        previousCursor: RunActivityCursor | null,
        fromEnd: boolean,
      ): Promise<{
        cursor: RunActivityCursor | null;
        entries: HomeRunActivityEntry[];
        reportedTotal: number;
      } | null> => {
        let pageAfterSeq = afterSeq;
        let caughtUp: HomeRunActivityEntry[] = [];
        let reportedTotal = 0;
        let firstPage = true;
        for (;;) {
          const response = await readProjection<RunActivityResponse>(
            "projection.operator.run.activity",
            {
              ...(fromEnd ? { from_end: true } : { after_seq: pageAfterSeq }),
              limit: ACTIVITY_BATCH_SIZE,
              primary_only: true,
              run_id: runId,
            },
          );
          if (cancelled) return null;
          const nextCursor = activityCursor(response.source_cursor);
          if (firstPage) {
            reportedTotal = finiteNumber(response.total);
            if (previousCursor && !canContinueActivityCursor(previousCursor, nextCursor)) return null;
          }
          const projected = (response.activity ?? [])
            .map((row) => activityEntry(row, runId))
            .filter((entry): entry is HomeRunActivityEntry => entry !== null);
          caughtUp = mergeActivityEntries(caughtUp, projected);
          if (response.has_newer !== true) {
            return { cursor: nextCursor, entries: caughtUp, reportedTotal };
          }
          const lastReturnedSeq = (response.activity ?? []).reduce(
            (latest, row) => Math.max(latest, finiteNumber(row.movement_seq)),
            pageAfterSeq,
          );
          if (lastReturnedSeq <= pageAfterSeq) {
            throw new Error("run activity catch-up was truncated without advancing movement_seq");
          }
          pageAfterSeq = lastReturnedSeq;
          firstPage = false;
        }
      };

      const previousCursor = incremental ? sourceCursorRef.current : null;
      const catchup = await readCatchup(previousCursor?.primarySeq ?? -1, previousCursor, !incremental);
      if (cancelled) return;
      if (!catchup) throw new Error("run activity cursor changed while reading the indexed page");
      if (incremental) {
        const nextEntries = mergeActivityEntries(entriesRef.current, catchup.entries).slice(-ACTIVITY_WINDOW_LIMIT);
        const nextTotal = Math.max(totalRef.current, catchup.reportedTotal, nextEntries.length);
        const nextTruncated = nextTotal > nextEntries.length;
        entriesRef.current = nextEntries;
        totalRef.current = nextTotal;
        truncatedRef.current = nextTruncated;
        if (catchup.entries.length > 0) setEntries(nextEntries);
        setTotal(nextTotal);
        setTruncated(nextTruncated);
      } else {
        const nextTotal = catchup.reportedTotal || catchup.entries.length;
        const nextEntries = catchup.entries.slice(-ACTIVITY_WINDOW_LIMIT);
        const nextTruncated = nextTotal > nextEntries.length;
        entriesRef.current = nextEntries;
        totalRef.current = nextTotal;
        truncatedRef.current = nextTruncated;
        setEntries(nextEntries);
        setTotal(nextTotal);
        setTruncated(nextTruncated);
      }
      sourceCursorRef.current = catchup.cursor;
      loadedKeyRef.current = loadedKey;
      setError("");
      setLoading(false);
    })().catch((failure) => {
      if (cancelled) return;
      if (!incremental) {
        entriesRef.current = [];
        totalRef.current = 0;
        truncatedRef.current = false;
        sourceCursorRef.current = null;
        loadedKeyRef.current = "";
        setEntries([]);
        setTotal(0);
        setTruncated(false);
      }
      setError(`Home could not read run activity: ${failureText(failure)}`);
      setLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [
    enabled,
    projectionChange.generation,
    projectionChange.kind,
    runId,
  ]);

  return { entries, error, loading, total, truncated };
}

function activityCursor(value: unknown): RunActivityCursor | null {
  const row = record(value);
  const identity = text(row.identity);
  const primarySeq = row.primary_seq;
  if (
    !identity
    || !Array.isArray(row.mtimes)
    || !Array.isArray(row.sizes)
    || typeof primarySeq !== "number"
    || !Number.isFinite(primarySeq)
    || primarySeq < -1
  ) return null;
  if (
    row.mtimes.some((mtime) => typeof mtime !== "number" || !Number.isFinite(mtime) || mtime < 0)
    || row.sizes.some((size) => typeof size !== "number" || !Number.isFinite(size) || size < 0)
  ) {
    return null;
  }
  const mtimes = row.mtimes as number[];
  const sizes = row.sizes as number[];
  if (mtimes.length !== sizes.length) return null;
  return { identity, mtimes, primarySeq, sizes };
}

function canContinueActivityCursor(
  previous: RunActivityCursor | null,
  next: RunActivityCursor | null,
): boolean {
  if (!previous || !next || previous.identity !== next.identity) return false;
  if (
    previous.sizes.length !== next.sizes.length
    || previous.mtimes.length !== next.mtimes.length
    || next.primarySeq < previous.primarySeq
  ) return false;
  return next.sizes.every((size, index) => {
    const previousSize = previous.sizes[index];
    const appendOnly = size >= previousSize;
    const unchanged = size === previousSize && next.mtimes[index] === previous.mtimes[index];
    return index === 0 ? appendOnly && (size > previousSize || unchanged) : unchanged;
  });
}

function mergeActivityEntries(
  previous: HomeRunActivityEntry[],
  incoming: HomeRunActivityEntry[],
): HomeRunActivityEntry[] {
  const byId = new Map(previous.map((entry) => [entry.id, entry]));
  for (const entry of incoming) byId.set(entry.id, entry);
  return [...byId.values()].sort((left, right) => (
    left.movementSeq - right.movementSeq
    || left.createdAtMs - right.createdAtMs
    || left.id.localeCompare(right.id)
  ));
}

function activityEntry(row: RunActivityRow, runId: string): HomeRunActivityEntry | null {
  const id = text(row.id) || text(row.artifact_id);
  if (!id) return null;
  const direction = row.direction === "in" || row.direction === "out" || row.direction === "internal"
    ? row.direction
    : "internal";
  return {
    artifactId: text(row.artifact_id),
    attachments: activityAttachments(row.attachments),
    contentHash: text(row.content_hash),
    createdAtMs: finiteNumber(row.created_at_ms),
    direction,
    error: row.error === true,
    eventData: record(row.event_data),
    eventKind: activityEventKind(row.event_kind),
    from: activityEndpoint(row.from),
    id,
    interfaceId: text(row.interface_id),
    label: text(row.label) || "Artifact",
    movementKind: text(row.movement_kind) || "Movement",
    movementSeq: finiteNumber(row.movement_seq),
    payloadError: text(row.payload_error),
    role: row.role === "user" || row.role === "system" ? row.role : "assistant",
    runId,
    terminal: row.terminal !== false,
    text: text(row.text),
    to: activityEndpoint(row.to),
    verification: text(row.verification),
  };
}

function activityEndpoint(value: unknown): HomeActivityEndpoint {
  const row = record(value);
  return {
    eeId: text(row.ee_id),
    id: text(row.id),
    kind: text(row.kind) || "unknown",
  };
}

function activityAttachments(value: unknown): ChatAttachmentView[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((candidate) => {
    const row = record(candidate);
    const id = text(row.id);
    if (!id) return [];
    return [{
      id,
      label: text(row.label) || "artifact",
      lastModified: finiteNumber(row.last_modified),
      name: text(row.name) || "Artifact",
      size: finiteNumber(row.size),
      type: text(row.type) || "application/octet-stream",
    }];
  });
}

function activityEventKind(value: unknown): ChatRunEventKind {
  const kind = text(value) as ChatRunEventKind;
  const supported: readonly ChatRunEventKind[] = [
    "message", "steering", "message_delta", "commentary", "commentary_delta", "reasoning", "reasoning_delta", "plan", "plan_delta", "usage",
    "tool", "tool_output_delta", "terminal_interaction", "file_change", "file_change_delta", "diff", "web_search",
    "hook", "hook_prompt", "mcp_tool", "mcp_progress", "mcp_status", "dynamic_tool", "collaboration",
    "subagent_activity", "image_view", "image_generation", "sleep", "review_entered", "review_exited", "compaction",
    "item_started", "turn_started", "turn_completed", "model_rerouted", "model_verification", "auth_recovery_started", "auth_recovery_completed", "warning",
    "auto_review_started", "auto_review_completed", "operator_request_resolved", "turn_control", "rate_limits", "goal",
    "approval", "elicitation", "user_input", "error", "generic_activity", "event",
  ];
  return supported.includes(kind) ? kind : "generic_activity";
}

function failureText(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function finiteNumber(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}
