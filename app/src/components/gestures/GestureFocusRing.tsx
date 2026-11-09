import { useLayoutEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { useGestureFocusView } from "../../gestureFocus";
import "./gestureFocusRing.css";

const RING_GAP = 3;
const HINT_ROOM = 34;

export function GestureFocusRing() {
  const view = useGestureFocusView();
  const ring = useRef<HTMLDivElement>(null);
  const element = view.element;

  useLayoutEffect(() => {
    const node = ring.current;
    if (!element || !node) return;
    const radius = Number.parseFloat(getComputedStyle(element).borderTopLeftRadius) || 0;
    node.style.setProperty("--gesture-ring-radius", `${radius + RING_GAP}px`);
    let frame = 0;
    let settle = 0;
    let previous = "";
    const place = () => {
      frame = window.requestAnimationFrame(place);
      const rect = element.getBoundingClientRect();
      const visible = element.isConnected && rect.width > 0 && rect.height > 0;
      const key = visible ? `${rect.left}|${rect.top}|${rect.width}|${rect.height}` : "hidden";
      if (key === previous) return;
      previous = key;
      node.dataset.visible = String(visible);
      if (!visible) return;
      node.dataset.hintAbove = String(rect.bottom + HINT_ROOM > window.innerHeight);
      node.style.setProperty("--gesture-ring-x", `${rect.left - RING_GAP}px`);
      node.style.setProperty("--gesture-ring-y", `${rect.top - RING_GAP}px`);
      node.style.setProperty("--gesture-ring-width", `${rect.width + RING_GAP * 2}px`);
      node.style.setProperty("--gesture-ring-height", `${rect.height + RING_GAP * 2}px`);
    };
    place();
    if (!node.dataset.settled) {
      node.dataset.settled = "false";
      settle = window.requestAnimationFrame(() => {
        node.dataset.settled = "true";
      });
    }
    return () => {
      window.cancelAnimationFrame(frame);
      window.cancelAnimationFrame(settle);
    };
  }, [element]);

  if (!element) return null;
  return createPortal(
    <div
      aria-hidden="true"
      className="gesture-focus-ring"
      data-bump={view.bump ?? undefined}
      data-gesture-ring=""
      data-state={view.state}
      ref={ring}
    >
      {view.hint ? <span className="gesture-focus-ring__hint">{view.hint}</span> : null}
    </div>,
    document.body,
  );
}
