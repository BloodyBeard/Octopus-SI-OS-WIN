import { useCallback, useEffect, useRef, type PointerEvent as ReactPointerEvent } from "react";
import type { ClientPaneDragBeginRequest, ClientPaneDragPayload, ClientPaneRect } from "../../shell/clientWindowModel";

type Options = {
  begin: (tabId: string) => ClientPaneDragPayload;
  onError: (error: unknown) => void;
  onReorder?: (sourceId: string, overId: string) => void;
};

type Session = {
  cancel: () => void;
  phase: (phase: string) => void;
  transferId?: string;
};

type StripItem = {
  element: HTMLElement;
  slot: DOMRect;
  transition: string;
  translate: string;
  zIndex: string;
};

type Strip = {
  index: number;
  items: StripItem[];
  list: HTMLElement;
};

const DRAG_THRESHOLD = 6;
const SLIDE_MS = 160;
const SLIDE = `translate ${SLIDE_MS}ms cubic-bezier(0.2, 0, 0, 1)`;

export function useClientPaneWindowDrag(options: Options) {
  const latest = useRef(options);
  latest.current = options;
  const active = useRef<Session | null>(null);

  useEffect(() => {
    const unsubscribe = window.octopusTransport?.onClientPaneDragState((state) => {
      const session = active.current;
      if (session?.transferId === state.transferId) session.phase(state.phase);
    });
    return () => { unsubscribe?.(); active.current?.cancel(); };
  }, []);

  return useCallback((tabId: string, event: ReactPointerEvent<HTMLElement>) => {
    if (event.button !== 0 || event.ctrlKey && /Mac/.test(navigator.platform)) return;
    const bridge = window.octopusTransport;
    if (!bridge?.beginClientPaneDrag) { latest.current.onError(new Error("Native pane preview is unavailable.")); return; }
    active.current?.cancel();
    const activator = event.currentTarget;
    const chrome = activator.closest<HTMLElement>(".pane-tab") ?? activator;
    const row = activator.closest<HTMLElement>("header") ?? chrome;
    const item = activator.closest<HTMLElement>("[data-pane-tab-id]");
    const start = { x: event.clientX, y: event.clientY };
    const pressed = chrome.getBoundingClientRect();
    const grab = { x: start.x - pressed.x, y: start.y - pressed.y };
    const pointerId = event.pointerId;
    const controller = new AbortController();
    const opacity = chrome.style.opacity;
    let reference: ClientPaneDragPayload | null = null;
    let started = false;
    let ending = false;
    let floating = false;
    let strip: Strip | null = null;
    let target = -1;
    let pointerX = start.x;
    let captureFrame = 0;
    let beginning: Promise<unknown> | null = null;
    const session: Session = { cancel, phase };
    active.current = session;
    activator.setPointerCapture(pointerId);

    function releasePointer() {
      controller.abort();
      window.cancelAnimationFrame(captureFrame);
      if (activator.hasPointerCapture(pointerId)) activator.releasePointerCapture(pointerId);
      if (active.current === session) active.current = null;
    }
    function cancel() {
      if (ending) return;
      ending = true;
      releasePointer();
      settleStrip(false);
      chrome.style.opacity = opacity;
      if (reference) void bridge!.cancelClientPaneDrag({ transferId: reference.transferId }).catch(latest.current.onError);
    }
    function phase(next: string) {
      if (ending) return;
      if (next === "floating") {
        floating = true;
        chrome.style.opacity = "0";
        if (strip) {
          strip.items[strip.index].element.style.translate = strip.items[strip.index].translate;
          shift(strip, strip.index);
        }
      } else if (next === "prepared") {
        floating = false;
        chrome.style.opacity = opacity;
        follow();
      }
    }
    function measureStrip(): Strip | null {
      const list = item?.parentElement;
      if (!item || !list || item.dataset.paneTabId !== tabId || !latest.current.onReorder) return null;
      const elements = Array.from(list.children).filter((child): child is HTMLElement => child instanceof HTMLElement
        && child.dataset.paneTabId != null && child.dataset.paneTabDraggable !== "false" && !child.inert);
      const index = elements.indexOf(item);
      if (index < 0) return null;
      const items = elements.map((element) => ({
        element,
        slot: element.getBoundingClientRect(),
        transition: element.style.transition,
        translate: element.style.translate,
        zIndex: element.style.zIndex,
      }));
      items.forEach((candidate, position) => { candidate.element.style.transition = position === index ? "none" : SLIDE; });
      items[index].element.style.zIndex = "10";
      return { index, items, list };
    }
    function follow() {
      const current = strip;
      if (!current || floating || ending) return;
      const own = current.items[current.index];
      const first = current.items[0].slot;
      const last = current.items[current.items.length - 1].slot;
      const left = Math.max(first.left, Math.min(pointerX - grab.x, Math.max(first.left, last.right - own.slot.width)));
      own.element.style.translate = `${left - own.slot.left}px 0`;
      const center = left + own.slot.width / 2;
      let next = current.index;
      current.items.forEach((candidate, index) => {
        const middle = candidate.slot.left + candidate.slot.width / 2;
        if (index > current.index && center > middle) next = Math.max(next, index);
        if (index < current.index && center < middle) next = Math.min(next, index);
      });
      shift(current, next);
    }
    function shift(current: Strip, next: number) {
      target = next;
      const width = current.items[current.index].slot.width;
      current.items.forEach((candidate, index) => {
        if (index === current.index) return;
        const offset = index > current.index && index <= next ? -width : index < current.index && index >= next ? width : 0;
        candidate.element.style.translate = offset ? `${offset}px 0` : candidate.translate;
      });
    }
    function settleStrip(commit: boolean) {
      const current = strip;
      strip = null;
      if (!current) return;
      const own = current.items[current.index];
      const overId = commit && target !== current.index ? current.items[target]?.element.dataset.paneTabId : undefined;
      if (!overId) {
        for (const candidate of current.items) {
          candidate.element.style.transition = SLIDE;
          candidate.element.style.translate = candidate.translate;
        }
        window.setTimeout(() => restoreStrip(current), SLIDE_MS + 40);
        return;
      }
      const visual = own.element.getBoundingClientRect().left;
      let landed = false;
      const land = () => {
        if (landed) return;
        landed = true;
        observer.disconnect();
        window.clearTimeout(fallback);
        for (const candidate of current.items) {
          candidate.element.style.transition = "none";
          candidate.element.style.translate = candidate.translate;
        }
        const slot = own.element.getBoundingClientRect().left;
        own.element.style.translate = `${visual - slot}px 0`;
        void own.element.offsetWidth;
        own.element.style.transition = SLIDE;
        own.element.style.translate = own.translate;
        window.setTimeout(() => restoreStrip(current), SLIDE_MS + 40);
      };
      const observer = new MutationObserver(land);
      observer.observe(current.list, { childList: true });
      const fallback = window.setTimeout(land, 250);
      latest.current.onReorder?.(tabId, overId);
    }
    function capture() {
      window.cancelAnimationFrame(captureFrame);
      if (beginning || !reference) return beginning;
      const captureRect = toRect(chrome.getBoundingClientRect());
      const request: ClientPaneDragBeginRequest = {
        transferId: reference.transferId,
        captureRect,
        rowRect: toRect(row.getBoundingClientRect()),
        offset: { x: clamp(grab.x, 0, captureRect.width), y: clamp(grab.y, 0, captureRect.height) },
        appearance: previewAppearance(chrome),
      };
      beginning = bridge!.beginClientPaneDrag(request).then((result) => {
        if (result?.phase === "cancelled" && !ending) { reference = null; cancel(); }
      }).catch((error) => {
        if (!ending) { cancel(); latest.current.onError(error); }
      });
      return beginning;
    }
    const move = (input: PointerEvent) => {
      if (input.pointerId !== pointerId || ending) return;
      pointerX = input.clientX;
      if (!started && Math.hypot(input.clientX - start.x, input.clientY - start.y) >= DRAG_THRESHOLD) {
        started = true;
        try {
          reference = latest.current.begin(tabId);
          session.transferId = reference.transferId;
          strip = measureStrip();
          captureFrame = window.requestAnimationFrame(() => { captureFrame = window.requestAnimationFrame(() => { void capture(); }); });
        } catch (error) { cancel(); latest.current.onError(error); return; }
      }
      if (!started) return;
      input.preventDefault();
      follow();
    };
    const finish = (input: PointerEvent) => {
      if (input.pointerId !== pointerId || ending) return;
      ending = true;
      releasePointer();
      if (!reference) { settleStrip(false); chrome.style.opacity = opacity; return; }
      const preventClick = (click: MouseEvent) => { click.preventDefault(); click.stopImmediatePropagation(); };
      window.addEventListener("click", preventClick, { capture: true, once: true });
      window.setTimeout(() => window.removeEventListener("click", preventClick, true), 0);
      const released = !floating;
      const transferId = reference.transferId;
      void Promise.resolve(capture()).then(() => bridge.finishClientPaneDrag({ transferId })).then((outcome) => {
        if (outcome?.status === "transferred") {
          settleStrip(false);
          window.setTimeout(() => { if (chrome.isConnected) chrome.style.opacity = opacity; }, 2000);
          return;
        }
        settleStrip(released);
        chrome.style.opacity = opacity;
      }).catch((error) => {
        settleStrip(false);
        chrome.style.opacity = opacity;
        latest.current.onError(error);
      });
    };
    window.addEventListener("pointermove", move, { signal: controller.signal });
    window.addEventListener("pointerup", finish, { signal: controller.signal });
    window.addEventListener("pointercancel", cancel, { signal: controller.signal });
    window.addEventListener("blur", cancel, { signal: controller.signal });
    window.addEventListener("keydown", (input) => { if (input.key === "Escape") { input.preventDefault(); cancel(); } }, { signal: controller.signal });
  }, []);
}

export async function anchorClientPaneDrop(transferId: string, tabId: string) {
  const anchor = window.octopusTransport?.anchorClientPaneDrop;
  if (!anchor) return;
  const find = () => Array.from(document.querySelectorAll<HTMLElement>('[data-pane-role="secondary"] [data-pane-tab-id]'))
    .find((element) => element.dataset.paneTabId === tabId)?.querySelector<HTMLElement>(".pane-tab") ?? null;
  let previous: ClientPaneRect | null = null;
  let stable = 0;
  for (let frame = 0; frame < 40 && stable < 2; frame += 1) {
    await nextFrame();
    const element = find();
    const next = element ? toRect(element.getBoundingClientRect()) : null;
    stable = next && previous && next.width > 0 && next.x === previous.x && next.y === previous.y && next.width === previous.width ? stable + 1 : 0;
    previous = next;
  }
  if (!previous || previous.width <= 0 || previous.height <= 0) return;
  await anchor({ transferId, rect: previous }).catch(() => undefined);
}

function restoreStrip(strip: Strip) {
  for (const candidate of strip.items) {
    candidate.element.style.transition = candidate.transition;
    candidate.element.style.zIndex = candidate.zIndex;
  }
}

function previewAppearance(chrome: HTMLElement): NonNullable<ClientPaneDragBeginRequest["appearance"]> {
  const surface = chrome.querySelector<HTMLElement>(".pane-tab-background") ?? chrome;
  const radius = Number.parseFloat(getComputedStyle(surface).borderTopLeftRadius);
  const probe = document.createElement("span");
  probe.style.cssText = "position:fixed;width:0;height:0;visibility:hidden;pointer-events:none;background-color:var(--color-surface)";
  document.body.append(probe);
  const background = getComputedStyle(probe).backgroundColor;
  probe.remove();
  return {
    radius: Number.isFinite(radius) ? clamp(radius, 0, 64) : 0,
    ...(background && background !== "transparent" && background !== "rgba(0, 0, 0, 0)" ? { background } : {}),
  };
}

function nextFrame() {
  return new Promise<void>((resolve) => {
    const timer = window.setTimeout(resolve, 48);
    window.requestAnimationFrame(() => { window.clearTimeout(timer); resolve(); });
  });
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.max(minimum, Math.min(maximum, value));
}

function toRect(value: DOMRect): ClientPaneRect {
  return { x: value.x, y: value.y, width: value.width, height: value.height };
}
