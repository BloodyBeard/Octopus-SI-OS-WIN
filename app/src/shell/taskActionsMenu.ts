import type { OctopusClientContextMenuItem } from "../electron";

export const OPEN_CLIENT_TASK_ACTIONS_MENU_EVENT = "octopus:open-client-task-actions-menu";
export const RENAME_CLIENT_TASK_EVENT = "octopus:rename-client-task";
export const RESTORE_CLIENT_TASK_EVENT = "octopus:restore-client-task";

export type ClientTaskActionsMenuRequest = {
  runId: string;
  anchorRight: number;
  anchorBottom: number;
  anchorElement?: HTMLElement;
  pointer?: { x: number; y: number };
};

export function dispatchOpenClientTaskActionsMenu(
  runId: string,
  anchor: Pick<DOMRect, "right" | "bottom"> | HTMLElement,
  pointer?: { x: number; y: number },
): void {
  if (!runId) return;
  const bounds = anchor instanceof HTMLElement ? anchor.getBoundingClientRect() : anchor;
  window.dispatchEvent(new CustomEvent(OPEN_CLIENT_TASK_ACTIONS_MENU_EVENT, {
    detail: {
      runId,
      anchorRight: bounds.right,
      anchorBottom: bounds.bottom,
      ...(anchor instanceof HTMLElement ? { anchorElement: anchor } : {}),
      ...(pointer ? { pointer } : {}),
    } satisfies ClientTaskActionsMenuRequest,
  }));
}

export function subscribeOpenClientTaskActionsMenu(
  listener: (request: ClientTaskActionsMenuRequest) => void,
): () => void {
  const handle = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    const runId = typeof event.detail?.runId === "string" ? event.detail.runId.trim() : "";
    const anchorRight = Number(event.detail?.anchorRight);
    const anchorBottom = Number(event.detail?.anchorBottom);
    if (!runId || !Number.isFinite(anchorRight) || !Number.isFinite(anchorBottom)) return;
    const point = event.detail?.pointer;
    if (point !== undefined && (!point || ![point.x, point.y].every((value) => typeof value === "number" && Number.isFinite(value)))) return;
    const anchorElement = event.detail?.anchorElement;
    if (anchorElement !== undefined && !(anchorElement instanceof HTMLElement)) return;
    listener({ runId, anchorRight, anchorBottom, ...(anchorElement ? { anchorElement } : {}), ...(point ? { pointer: { x: point.x, y: point.y } } : {}) });
  };
  window.addEventListener(OPEN_CLIENT_TASK_ACTIONS_MENU_EVENT, handle);
  return () => window.removeEventListener(OPEN_CLIENT_TASK_ACTIONS_MENU_EVENT, handle);
}

export function dispatchRenameClientTask(runId: string): void {
  if (!runId) return;
  window.dispatchEvent(new CustomEvent(RENAME_CLIENT_TASK_EVENT, { detail: { runId } }));
}

export function subscribeRenameClientTask(listener: (runId: string) => void): () => void {
  return subscribeTaskAction(RENAME_CLIENT_TASK_EVENT, listener);
}

export function dispatchRestoreClientTask(runId: string): void {
  if (runId) window.dispatchEvent(new CustomEvent(RESTORE_CLIENT_TASK_EVENT, { detail: { runId } }));
}

export function subscribeRestoreClientTask(listener: (runId: string) => void): () => void {
  return subscribeTaskAction(RESTORE_CLIENT_TASK_EVENT, listener);
}

function subscribeTaskAction(eventName: string, listener: (runId: string) => void): () => void {
  const handle = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    const runId = typeof event.detail?.runId === "string" ? event.detail.runId.trim() : "";
    if (runId) listener(runId);
  };
  window.addEventListener(eventName, handle);
  return () => window.removeEventListener(eventName, handle);
}

export type ClientTaskMenuSnapshot = {
  items: OctopusClientContextMenuItem[];
  signature: string;
  select: (id: string) => void;
};

export function nativeTaskMenuLabel(label: string): string {
  const text = label.replace(/[\x00-\x1f\x7f\s]+/g, " ").trim();
  if (text.length <= 180) return text;
  let shortened = "";
  for (const { segment } of new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text)) {
    if (shortened.length + segment.length > 179) break;
    shortened += segment;
  }
  return `${shortened}…`;
}

type NativeMenuTransport = Pick<NonNullable<Window["octopusTransport"]>, "showClientContextMenu" | "cancelClientContextMenu">;

export function createNativeTaskMenuSession(
  readCurrent: (runId: string) => ClientTaskMenuSnapshot | null,
  onError: (error: unknown) => void,
) {
  let pending: { requestId: string; runId: string; signature: string; transport: NativeMenuTransport } | null = null;
  let disposed = false;
  const cancel = () => {
    const previous = pending;
    pending = null;
    if (!previous) return;
    void Promise.resolve().then(() => previous.transport.cancelClientContextMenu({ requestId: previous.requestId })).catch(onError);
  };
  return {
    cancel,
    reconcile() {
      if (pending && readCurrent(pending.runId)?.signature !== pending.signature) cancel();
    },
    dispose() { disposed = true; cancel(); },
    open(request: ClientTaskActionsMenuRequest, transport: NativeMenuTransport) {
      cancel();
      if (disposed) return;
      const current = readCurrent(request.runId);
      if (!current) return;
      if (typeof transport.showClientContextMenu !== "function" || typeof transport.cancelClientContextMenu !== "function") {
        onError(new Error("The desktop native task menu is unavailable."));
        return;
      }
      const fence = { requestId: crypto.randomUUID(), runId: request.runId, signature: current.signature, transport };
      pending = fence;
      const point = request.pointer ?? { x: request.anchorRight, y: request.anchorBottom };
      void (async () => {
        let result;
        try {
          result = await transport.showClientContextMenu({
            requestId: fence.requestId, items: current.items,
            position: { x: Math.max(0, point.x), y: Math.max(0, point.y) },
          });
        } catch (error) {
          if (pending === fence && !disposed) { pending = null; onError(error); }
          return;
        }
        if (disposed || pending !== fence) return;
        pending = null;
        try {
          if (result?.requestId !== fence.requestId || !(result.id === null || typeof result.id === "string")) throw new Error("Invalid native task-menu response.");
          const latest = readCurrent(fence.runId);
          if (latest?.signature === fence.signature && result.id !== null) latest.select(result.id);
        } catch (error) { onError(error); }
      })();
    },
  };
}
