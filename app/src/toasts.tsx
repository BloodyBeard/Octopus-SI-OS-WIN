import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type CSSProperties } from "react";
import { Glyph } from "./components/IconGlyphs";
import { circularArcRingPath, cubicEllipsePath } from "./components/icon-glyphs/geometric-primitives";

export type AppToastLevel = "info" | "success" | "warning" | "danger";

export type AppToastAction = {
  label: string;
  onSelect: () => void;
};

export type AppToastOptions = {
  action?: AppToastAction;
  dedupeKey?: string;
};

type AppToast = {
  id: number;
  level: AppToastLevel;
  title: string;
  duration: number;
  isShown: boolean;
  action?: AppToastAction;
  count: number;
  dedupeKey: string;
  revision: number;
};

let nextToastId = 1;
let toasts: AppToast[] = [];
const listeners = new Set<() => void>();
const MAX_VISIBLE_TOASTS = 5;

function publish() {
  for (const listener of listeners) listener();
}

function showToast(level: AppToastLevel, title: string, duration = 5, options: AppToastOptions = {}) {
  const dedupeKey = options.dedupeKey?.trim() || `${level}:${title}`;
  const duplicate = toasts.find((toast) => toast.isShown && toast.dedupeKey === dedupeKey);
  if (duplicate) {
    const updated = {
      ...duplicate,
      action: options.action ?? duplicate.action,
      count: duplicate.count + 1,
      duration,
      revision: duplicate.revision + 1,
      title,
    };
    toasts = [updated, ...toasts.filter((toast) => toast.id !== duplicate.id)].slice(0, MAX_VISIBLE_TOASTS);
    publish();
    return { close: () => closeToast(updated.id) };
  }
  const id = nextToastId;
  nextToastId += 1;
  toasts = [{
    id,
    level,
    title,
    duration,
    isShown: true,
    action: options.action,
    count: 1,
    dedupeKey,
    revision: 0,
  }, ...toasts].slice(0, MAX_VISIBLE_TOASTS);
  publish();
  if ((level === "warning" || level === "danger") && typeof window !== "undefined") {
    void window.octopusTransport?.logClientEvent({
      level: level === "danger" ? "error" : "warning",
      event: "renderer.toast",
      message: title,
    }).catch(() => undefined);
  }
  return { close: () => closeToast(id) };
}

function closeToast(id: number) {
  if (!toasts.some((toast) => toast.id === id && toast.isShown)) return;
  toasts = toasts.map((toast) => toast.id === id ? { ...toast, isShown: false } : toast);
  publish();
}

function removeToast(id: number) {
  if (!toasts.some((toast) => toast.id === id)) return;
  toasts = toasts.filter((toast) => toast.id !== id);
  publish();
}

export const appToast = {
  info: (title: string, duration?: number, options?: AppToastOptions) => showToast("info", title, duration, options),
  success: (title: string, duration?: number, options?: AppToastOptions) => showToast("success", title, duration, options),
  warning: (title: string, duration?: number, options?: AppToastOptions) => showToast("warning", title, duration, options),
  danger: (title: string, duration?: number, options?: AppToastOptions) => showToast("danger", title, duration, options),
  closeAll: () => {
    if (!toasts.some((toast) => toast.isShown)) return;
    toasts = toasts.map((toast) => ({ ...toast, isShown: false }));
    publish();
  },
};

export function useAppErrorToast(error: string | null | undefined) {
  const previous = useRef("");
  useEffect(() => {
    if (!error) {
      previous.current = "";
      return;
    }
    if (previous.current === error) return;
    previous.current = error;
    appToast.danger(error);
  }, [error]);
}

export function AppToastViewport({ anchorToWorkspace = false }: { anchorToWorkspace?: boolean }) {
  const items = useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => toasts,
  );
  const [workspaceBounds, setWorkspaceBounds] = useState<{ left: number; width: number } | null>(null);

  useLayoutEffect(() => {
    if (!anchorToWorkspace) {
      setWorkspaceBounds(null);
      return;
    }
    const workspace = document.querySelector<HTMLElement>(".workspace-pane-primary");
    if (!workspace) return;
    const measure = () => {
      const rect = workspace.getBoundingClientRect();
      setWorkspaceBounds((current) => (
        current?.left === rect.left && current.width === rect.width
          ? current
          : { left: rect.left, width: rect.width }
      ));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(workspace);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [anchorToWorkspace]);

  const style = workspaceBounds ? {
    "--app-toast-anchor-left": `${workspaceBounds.left}px`,
    "--app-toast-anchor-width": `${workspaceBounds.width}px`,
  } as CSSProperties : undefined;

  return (
    <span className="app-toast-viewport" aria-live="polite" style={style}>
      {items.map((toast) => <AppToastItem key={toast.id} toast={toast} />)}
    </span>
  );
}

function AppToastItem({ toast }: { toast: AppToast }) {
  const [paused, setPaused] = useState(false);
  const remainingMs = useRef(toast.duration * 1_000);
  const timerStartedAt = useRef(0);

  useEffect(() => {
    remainingMs.current = toast.duration * 1_000;
  }, [toast.duration, toast.revision]);

  useEffect(() => {
    if (!toast.isShown || !toast.duration || paused) return;
    timerStartedAt.current = window.performance.now();
    const timer = window.setTimeout(() => closeToast(toast.id), remainingMs.current);
    return () => {
      window.clearTimeout(timer);
      remainingMs.current = Math.max(0, remainingMs.current - (window.performance.now() - timerStartedAt.current));
    };
  }, [paused, toast.duration, toast.id, toast.isShown, toast.revision]);

  useEffect(() => {
    if (toast.isShown) return;
    const timer = window.setTimeout(() => removeToast(toast.id), 250);
    return () => window.clearTimeout(timer);
  }, [toast.id, toast.isShown]);

  return (
    <div className="app-toast-root no-drag" data-state={toast.isShown ? "entered" : "exiting"}>
      <div
        className={`app-toast-alert is-${toast.level}`}
        onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget)) setPaused(false);
        }}
        onFocus={() => setPaused(true)}
        onPointerEnter={() => setPaused(true)}
        onPointerLeave={() => setPaused(false)}
        role="alert"
      >
        {toast.level === "info" ? null : (
          <div className="app-toast-icon" aria-hidden="true">
            <ToastIcon level={toast.level} />
          </div>
        )}
        <div className="app-toast-content">
          <span>{toast.title}</span>
          {toast.count > 1 ? <span aria-label={`Repeated ${toast.count} times`} className="app-toast-count">{toast.count}</span> : null}
        </div>
        {toast.action ? (
          <button
            className="app-toast-action"
            onClick={() => {
              toast.action?.onSelect();
              closeToast(toast.id);
            }}
            type="button"
          >
            {toast.action.label}
          </button>
        ) : null}
        <button aria-label="Close" className="app-toast-close" onClick={() => closeToast(toast.id)} type="button">
          <Glyph name="close" size={16} />
        </button>
      </div>
    </div>
  );
}

function ToastIcon({ level }: { level: AppToastLevel }) {
  if (level === "danger") {
    return (
      <svg fill="currentColor" height="18" viewBox="0 0 20 20" width="18">
        <path d="M7.231 7.231a.665.665 0 0 1 .94 0L10 9.06l1.828-1.829.104-.085a.666.666 0 0 1 .921.922l-.084.104L10.94 10l1.829 1.828a.665.665 0 0 1-.94.94L10 10.94l-1.828 1.83a.665.665 0 0 1-.94-.94L9.06 10 7.23 8.172a.665.665 0 0 1 0-.94Z" />
        <path clipRule="evenodd" d={circularArcRingPath(10, 10, 7.915, 6.585)} fillRule="evenodd" />
      </svg>
    );
  }
  if (level === "success") {
    return (
      <svg fill="none" height="18" viewBox="0 0 20 21" width="18">
        <path d="M12.1599 7.63617C12.3713 7.33596 12.7863 7.26372 13.0866 7.47504C13.3867 7.68642 13.4589 8.10153 13.2477 8.40179L9.28876 14.0268C9.17264 14.1917 8.98808 14.2954 8.7868 14.308C8.61044 14.319 8.43764 14.2592 8.30634 14.144L8.25262 14.0912L6.16962 11.7993L6.08954 11.6918C5.93136 11.4259 5.97666 11.0761 6.21454 10.8598C6.45225 10.6439 6.80379 10.6326 7.05341 10.8149L7.15399 10.9047L8.67841 12.5815L12.1599 7.63617Z" fill="currentColor" />
        <path clipRule="evenodd" d={cubicEllipsePath(9.99506, 10.7273, 7.91504, 7.91504, 4.37134) + cubicEllipsePath(9.99506, 10.7273, 6.58496, 6.58497, 3.6368, 3.6368, true)} fill="currentColor" fillRule="evenodd" />
      </svg>
    );
  }
  return (
    <svg fill="currentColor" height="18" viewBox="0 0 16 16" width="18">
      <path d="M8 9.8a.767.767 0 1 1 0 1.533A.767.767 0 0 1 8 9.8Zm0-5.134c.368 0 .667.299.667.667V8a.667.667 0 0 1-1.334 0V5.333c0-.368.299-.667.667-.667Z" />
      <path clipRule="evenodd" d={circularArcRingPath(8, 8, 6.667, 5.333)} fillRule="evenodd" />
    </svg>
  );
}
