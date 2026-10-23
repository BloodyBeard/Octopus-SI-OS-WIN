import {
  animate,
  useMotionValue,
  useMotionValueEvent,
  useReducedMotion,
  useTransform,
  type MotionValue,
} from "motion/react";
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import "./panelResizeHandle.css";

type PanelResizeEdge = "left" | "right";
const CLOSE_THRESHOLD_FACTOR = 0.5;
const PANEL_MOTION_TRANSITION = { type: "spring", duration: 0.5, bounce: 0.1 } as const;

export function PanelContent({ active, mounted, children }: { active: boolean; mounted: boolean; children: ReactNode }) {
  const retained = useRef<ReactNode>(null);
  useLayoutEffect(() => {
    if (active) retained.current = children;
    else if (!mounted) retained.current = null;
  }, [active, mounted, children]);
  return <>{active ? children : mounted ? retained.current : null}</>;
}

export function useAnimatedPanelSize(
  open: boolean,
  size: number,
  mirroredSize?: MotionValue<number>,
  immediate = false,
): { animatedSize: MotionValue<number>; mounted: boolean; opacity: MotionValue<number> } {
  const reduceMotion = useReducedMotion();
  const sizeValue = useMotionValue(size);
  const opacity = useMotionValue(open ? 1 : 0);
  const animatedSize = useTransform([opacity, sizeValue], ([progress, currentSize]) => (
    Math.max(0, Math.min(1, Number(progress))) * Number(currentSize)
  ));
  const [mounted, setMounted] = useState(open);

  useMotionValueEvent(animatedSize, "change", (value) => mirroredSize?.set(value));

  useLayoutEffect(() => {
    mirroredSize?.set(animatedSize.get());
  }, [animatedSize, mirroredSize]);

  useLayoutEffect(() => {
    if (open) sizeValue.set(size);
  }, [open, size, sizeValue]);

  useLayoutEffect(() => {
    if (reduceMotion || immediate) {
      opacity.set(open ? 1 : 0);
      animatedSize.set(open ? sizeValue.get() : 0);
      mirroredSize?.set(animatedSize.get());
      setMounted(open);
      return;
    }
    if (open) setMounted(true);
    let active = true;
    const controls = animate(
      opacity,
      open ? 1 : 0,
      PANEL_MOTION_TRANSITION,
    );
    void controls.then(() => {
      if (active && !open) setMounted(false);
    });
    return () => {
      active = false;
      controls.stop();
    };
  }, [animatedSize, immediate, mirroredSize, open, opacity, reduceMotion, sizeValue]);

  return { animatedSize, mounted, opacity };
}

type Props = {
  ariaLabel: string;
  className?: string;
  defaultSize: number;
  edge: PanelResizeEdge;
  maximum: number;
  minimum: number;
  onChange: (size: number) => void;
  onCommit: (size: number) => void;
  onOpenChange: (open: boolean) => void;
  onResizingChange: (resizing: boolean) => void;
  title?: string;
  value: number;
};

export function PanelResizeHandle({
  ariaLabel,
  className = "",
  defaultSize,
  edge,
  maximum,
  minimum,
  onChange,
  onCommit,
  onOpenChange,
  onResizingChange,
  title,
  value,
}: Props) {
  const [resizing, setResizing] = useState(false);
  const drag = useRef<{
    didMove: boolean;
    element: HTMLDivElement;
    pointerId: number;
    startPosition: number;
    startSize: number;
    lastSize: number;
  } | null>(null);
  const current = useRef({ maximum, minimum, onChange, onCommit, onOpenChange, onResizingChange, value });
  current.current = { maximum, minimum, onChange, onCommit, onOpenChange, onResizingChange, value };

  useEffect(() => {
    if (!resizing) return;

    const proposedSize = (clientX: number) => {
      const activeDrag = drag.current;
      if (!activeDrag) return current.current.value;
      const delta = clientX - activeDrag.startPosition;
      return activeDrag.startSize + (edge === "right" ? delta : -delta);
    };

    const applySize = (proposed: number) => {
      const callbacks = current.current;
      const open = proposed >= callbacks.minimum * CLOSE_THRESHOLD_FACTOR;
      callbacks.onOpenChange(open);
      if (open) callbacks.onChange(clampPanelSize(proposed, callbacks.minimum, callbacks.maximum));
    };

    const onPointerMove = (event: globalThis.PointerEvent) => {
      const activeDrag = drag.current;
      if (!activeDrag || event.pointerId !== activeDrag.pointerId) return;
      event.preventDefault();
      const proposed = proposedSize(event.clientX);
      if (proposed !== activeDrag.startSize) activeDrag.didMove = true;
      activeDrag.lastSize = proposed;
      applySize(proposed);
    };

    const finish = (event: globalThis.PointerEvent) => {
      const activeDrag = drag.current;
      if (!activeDrag || event.pointerId !== activeDrag.pointerId) return;
      event.preventDefault();
      if (activeDrag.didMove) {
        const proposed = event.type === "pointerup" ? proposedSize(event.clientX) : activeDrag.lastSize;
        applySize(proposed);
        if (proposed >= current.current.minimum * CLOSE_THRESHOLD_FACTOR) {
          current.current.onCommit(clampPanelSize(proposed, current.current.minimum, current.current.maximum));
        }
      }
      drag.current = null;
      if (activeDrag.element.hasPointerCapture(activeDrag.pointerId)) {
        activeDrag.element.releasePointerCapture(activeDrag.pointerId);
      }
      setResizing(false);
      current.current.onResizingChange(false);
    };

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
    window.addEventListener("lostpointercapture", finish);
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      window.removeEventListener("lostpointercapture", finish);
      const activeDrag = drag.current;
      if (activeDrag?.element.hasPointerCapture(activeDrag.pointerId)) {
        activeDrag.element.releasePointerCapture(activeDrag.pointerId);
      }
      if (activeDrag) {
        drag.current = null;
        current.current.onResizingChange(false);
      }
    };
  }, [edge, resizing]);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const direction = edge === "right" ? 1 : -1;
    let next = value;
    if (event.key === "ArrowLeft") next -= 10 * direction;
    else if (event.key === "ArrowRight") next += 10 * direction;
    else if (event.key === "Home") next = minimum;
    else if (event.key === "End") next = maximum;
    else return;
    event.preventDefault();
    event.stopPropagation();
    const size = clampPanelSize(next, minimum, maximum);
    onChange(size);
    onCommit(size);
  };

  return (
    <div
      aria-label={ariaLabel}
      aria-orientation="vertical"
      aria-valuemax={maximum}
      aria-valuemin={minimum}
      aria-valuenow={Math.round(value)}
      className={`panel-resize-handle pane-resize-handle is-edge-${edge}${resizing ? " is-resizing" : ""}${className ? ` ${className}` : ""}`}
      onDoubleClick={(event) => {
        event.preventDefault();
        const activeDrag = drag.current;
        if (activeDrag?.element.hasPointerCapture(activeDrag.pointerId)) {
          activeDrag.element.releasePointerCapture(activeDrag.pointerId);
        }
        drag.current = null;
        setResizing(false);
        onResizingChange(false);
        const size = clampPanelSize(defaultSize, minimum, maximum);
        onOpenChange(true);
        onChange(size);
        onCommit(size);
      }}
      onKeyDown={onKeyDown}
      onPointerDown={(event) => {
        if (event.button !== 0 || !event.isPrimary || drag.current) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture(event.pointerId);
        drag.current = {
          didMove: false,
          element: event.currentTarget,
          pointerId: event.pointerId,
          startPosition: event.clientX,
          startSize: value,
          lastSize: value,
        };
        setResizing(true);
        onResizingChange(true);
      }}
      role="separator"
      tabIndex={0}
      title={title}
    >
      <span aria-hidden="true" className="panel-resize-handle-line" />
    </div>
  );
}

function clampPanelSize(size: number, minimum: number, maximum: number): number {
  return Math.max(minimum, Math.min(maximum, size));
}
