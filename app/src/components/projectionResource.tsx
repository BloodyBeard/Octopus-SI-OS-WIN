import { useCallback, useEffect, useRef, useState } from "react";
import { OcButton, OcEmptyState, cx } from "../design-system/components";
import { formatProjectionError } from "../projectionClient";
import { OctopusLoadingState } from "./OctopusLoadingLogo";

export type ProjectionResourcePhase = "initial-loading" | "refreshing" | "ready" | "failed" | "degraded";

export type ProjectionResourceLoadResult<T> = {
  data: T;
  error?: string;
};

export type ProjectionResourceState<T> = {
  data: T | null;
  error: string;
  phase: ProjectionResourcePhase;
  retry: () => void;
};

export function useProjectionResource<T>({
  enabled,
  load,
  resourceKey,
  refreshKey,
}: {
  enabled: boolean;
  load: (lastGood: T | null) => Promise<ProjectionResourceLoadResult<T>>;
  resourceKey: string;
  refreshKey: unknown;
}): ProjectionResourceState<T> {
  const dataRef = useRef<T | null>(null);
  const resourceKeyRef = useRef(resourceKey);
  const refreshRef = useRef<((next: typeof load) => void) | null>(null);
  const [retryTick, setRetryTick] = useState(0);
  const [state, setState] = useState<Omit<ProjectionResourceState<T>, "retry"> & { resourceKey: string }>({
    data: null,
    error: "",
    phase: "initial-loading",
    resourceKey,
  });

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    if (resourceKeyRef.current !== resourceKey) {
      resourceKeyRef.current = resourceKey;
      dataRef.current = null;
    }
    let reading = false;
    let pending: typeof load | null = null;
    const read = async () => {
      reading = true;
      while (pending && !cancelled) {
        const next = pending;
        pending = null;
        const lastGood = dataRef.current;
        setState((current) => ({
          data: lastGood,
          error: current.resourceKey === resourceKey ? current.error : "",
          phase: lastGood === null ? "initial-loading" : "refreshing",
          resourceKey,
        }));
        try {
          const result = await next(lastGood);
          if (cancelled) break;
          const error = result.error?.trim() ?? "";
          dataRef.current = result.data;
          setState({ data: result.data, error, phase: error ? "degraded" : "ready", resourceKey });
        } catch (failure) {
          if (cancelled) break;
          const error = formatProjectionError(failure) || "Projection read failed";
          const data = dataRef.current;
          setState({ data, error, phase: data === null ? "failed" : "degraded", resourceKey });
        }
      }
      reading = false;
    };
    refreshRef.current = (next) => {
      pending = next;
      if (!reading) void read();
    };
    return () => { cancelled = true; refreshRef.current = null; };
  }, [enabled, resourceKey]);

  useEffect(() => {
    refreshRef.current?.(load);
  }, [enabled, load, refreshKey, resourceKey, retryTick]);

  const retry = useCallback(() => setRetryTick((value) => value + 1), []);
  if (state.resourceKey !== resourceKey) {
    return { data: null, error: "", phase: "initial-loading", retry };
  }
  return { data: state.data, error: state.error, phase: state.phase, retry };
}

export function ProjectionResourceLoading({ label, className }: { label: string; className?: string }) {
  return (
    <OctopusLoadingState className={cx("flex h-full min-h-64 w-full flex-1 items-center justify-center", className)} label={label} />
  );
}

export function ProjectionResourceFailure({
  detail,
  onRetry,
  retrying,
  title,
  className,
}: {
  detail: string;
  onRetry: () => void;
  retrying: boolean;
  title: string;
  className?: string;
}) {
  return (
    <OcEmptyState
      action={(
        <OcButton disabled={retrying} leadingIcon="retry" onClick={onRetry} variant="secondary">
          {retrying ? "Retrying…" : "Try again"}
        </OcButton>
      )}
      className={className}
      detail={detail}
      glyph="warning"
      role="alert"
      title={title}
      tone="danger"
    />
  );
}

export function ProjectionResourceIssue({
  detail,
  onRetry,
  retrying,
  title,
  className,
}: {
  detail: string;
  onRetry: () => void;
  retrying: boolean;
  title: string;
  className?: string;
}) {
  return (
    <div className={cx("flex flex-wrap items-start justify-between gap-3 rounded-lg border border-token-border bg-token-main-surface-secondary p-3", className)} role="alert">
      <div className="min-w-0 flex-1">
        <div className="text-xs font-medium text-token-foreground">{title}</div>
        <div className="mt-1 whitespace-pre-wrap break-words text-[10px] leading-4 text-token-text-secondary">{detail}</div>
      </div>
      <OcButton disabled={retrying} leadingIcon="retry" onClick={onRetry} size="xs" variant="secondary">
        {retrying ? "Retrying…" : "Retry"}
      </OcButton>
    </div>
  );
}
