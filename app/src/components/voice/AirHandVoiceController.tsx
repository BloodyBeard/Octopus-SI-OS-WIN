import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type RefObject } from "react";
import { emitVoiceSpatialInteraction, type VoiceSpatialCorner } from "../../client-surfaces/voiceSpatialControl";
import type { OctopusClientAccessProfile } from "../../electron";
import { useGestureRecognitionState } from "../../gestureRecognition";
import { useGestureShortcutPreferences } from "../../gestureShortcuts";
import { clientDialogOwnsCommands } from "../../keyboardShortcuts";

type Props = {
  accessProfile: OctopusClientAccessProfile;
  stageRef: RefObject<HTMLDivElement | null>;
};

type CursorState = {
  label: string;
  mode: "aim" | "press" | "drag" | "resize";
  target: boolean;
  visible: boolean;
  x: number;
  y: number;
};

type SpatialTarget = {
  action: HTMLElement | null;
  corner?: VoiceSpatialCorner;
  documentId: string;
  element: HTMLElement;
  rect: DOMRect;
};

type PressState = {
  action: HTMLElement | null;
  element: HTMLElement;
};

type GrabState = {
  begun: boolean;
  documentId: string;
  element: HTMLElement;
  lastX: number;
  lastY: number;
};

type PinchState = {
  centerX: number;
  centerY: number;
  cornerX: number;
  cornerY: number;
  documentId: string;
  element: HTMLElement;
};

const EMPTY_CURSOR: CursorState = { label: "", mode: "aim", target: false, visible: false, x: 0, y: 0 };
const AIRHAND_CAPTURE_RADIUS = 64;

export function AirHandVoiceController({ accessProfile, stageRef }: Props) {
  const preferences = useGestureShortcutPreferences(accessProfile);
  const recognition = useGestureRecognitionState();
  const [cursor, setCursor] = useState<CursorState>(EMPTY_CURSOR);
  const press = useRef<PressState | null>(null);
  const grab = useRef<GrabState | null>(null);
  const pinch = useRef<PinchState | null>(null);
  const targetElement = useRef<HTMLElement | null>(null);
  const lastWidget = useRef<HTMLElement | null>(null);
  const enabled = preferences.enabled
    && preferences.spatialControl
    && recognition.phase === "running";

  useLayoutEffect(() => {
    const stage = stageRef.current;
    const pose = recognition.pose;
    const pointing = pose === "aim" || pose === "left_click";
    const handled = pointing || pose === "fist" || pose === "pinch";
    if (!enabled || !stage || !handled || clientDialogOwnsCommands() || stage.closest("[inert], [aria-hidden='true']")) {
      releaseAll(press, grab, pinch, false);
      updateTarget(targetElement, null, "");
      setCursor((current) => current.visible ? EMPTY_CURSOR : current);
      return;
    }
    const stageRect = stage.getBoundingClientRect();
    if (stageRect.width <= 0 || stageRect.height <= 0) return;

    if (pose === "fist") {
      finishPress(press, false);
      finishPinch(pinch);
      const palm = recognition.palmPoint;
      if (!palm) return;
      const raw = stagePoint(stageRect, palm);
      let active = grab.current;
      if (!active) {
        const resolved = resolveSpatialTarget(stage, raw.x, raw.y);
        if (!resolved) {
          updateTarget(targetElement, null, "");
          setCursor({ label: "", mode: "aim", target: false, visible: true, x: raw.x, y: raw.y });
          return;
        }
        active = {
          begun: false,
          documentId: resolved.documentId,
          element: resolved.element,
          lastX: raw.x,
          lastY: raw.y,
        };
        grab.current = active;
        lastWidget.current = resolved.element;
        emitVoiceSpatialInteraction({
          clientX: raw.x,
          clientY: raw.y,
          documentId: active.documentId,
          kind: "drag",
          phase: "begin",
        });
        active.begun = true;
      }
      active.lastX = raw.x;
      active.lastY = raw.y;
      emitVoiceSpatialInteraction({
        clientX: raw.x,
        clientY: raw.y,
        documentId: active.documentId,
        kind: "drag",
        phase: "move",
      });
      updateTarget(targetElement, active.element, "drag");
      setCursor({ label: "Move", mode: "drag", target: true, visible: true, x: raw.x, y: raw.y });
      return;
    }

    if (pose === "pinch") {
      finishPress(press, false);
      finishGrab(grab);
      const ratio = recognition.pinchRatio;
      if (ratio == null) return;
      let active = pinch.current;
      if (!active) {
        const palm = recognition.palmPoint;
        const raw = palm ? stagePoint(stageRect, palm) : null;
        const resolved = raw ? resolveSpatialTarget(stage, raw.x, raw.y) : null;
        const element = resolved?.element ?? (lastWidget.current?.isConnected ? lastWidget.current : null);
        const documentId = resolved?.documentId ?? element?.dataset.surfaceDocument;
        if (!element || !documentId) return;
        const rect = element.getBoundingClientRect();
        active = {
          centerX: rect.left + rect.width / 2,
          centerY: rect.top + rect.height / 2,
          cornerX: rect.right,
          cornerY: rect.bottom,
          documentId,
          element,
        };
        pinch.current = active;
        lastWidget.current = element;
        emitVoiceSpatialInteraction({
          clientX: active.cornerX,
          clientY: active.cornerY,
          corner: "south-east",
          documentId,
          kind: "resize",
          phase: "begin",
        });
      }
      emitVoiceSpatialInteraction({
        clientX: active.centerX + (active.cornerX - active.centerX) * ratio,
        clientY: active.centerY + (active.cornerY - active.centerY) * ratio,
        corner: "south-east",
        documentId: active.documentId,
        kind: "resize",
        phase: "move",
      });
      updateTarget(targetElement, active.element, "resize");
      const rect = active.element.getBoundingClientRect();
      setCursor({
        label: `Resize · ${Math.round(ratio * 100)}%`,
        mode: "resize",
        target: true,
        visible: true,
        x: rect.right,
        y: rect.bottom,
      });
      return;
    }

    finishGrab(grab);
    finishPinch(pinch);
    const point = recognition.point;
    if (!point) return;
    const raw = stagePoint(stageRect, point);
    const resolved = resolveSpatialTarget(stage, raw.x, raw.y);
    if (resolved) lastWidget.current = resolved.element;

    if (pose === "left_click") {
      if (!press.current && resolved) {
        press.current = { action: resolved.action, element: resolved.element };
      }
      const currentPress = press.current;
      if (currentPress) {
        updateTarget(targetElement, currentPress.element, "press");
        setCursor({
          label: currentPress.action ? "Select" : "Hold",
          mode: "press",
          target: true,
          visible: true,
          x: raw.x,
          y: raw.y,
        });
        return;
      }
    } else if (press.current) {
      finishPress(press, true);
    }

    updateTarget(targetElement, resolved?.element ?? null, resolved ? "aim" : "");
    setCursor({
      label: resolved ? (resolved.action ? "Select" : "Widget") : "",
      mode: "aim",
      target: Boolean(resolved),
      visible: true,
      x: raw.x,
      y: raw.y,
    });
  }, [enabled, recognition.palmPoint, recognition.phase, recognition.pinchRatio, recognition.point, recognition.pose, stageRef]);

  useEffect(() => () => {
    releaseAll(press, grab, pinch, false);
    updateTarget(targetElement, null, "");
  }, []);

  if (!cursor.visible) return null;
  const style = {
    "--airhand-x": `${cursor.x}px`,
    "--airhand-y": `${cursor.y}px`,
  } as CSSProperties;
  return (
    <div
      aria-hidden="true"
      className={`voice-airhand-pointer is-${cursor.mode}${cursor.target ? " has-target" : ""}`}
      data-airhand-cursor="true"
      style={style}
    >
      <i />
      {cursor.label ? <span>{cursor.label}</span> : null}
    </div>
  );
}

function stagePoint(stageRect: DOMRect, point: { x: number; y: number }): { x: number; y: number } {
  return {
    x: stageRect.left + expandCameraAxis(point.x, 0.08) * stageRect.width,
    y: stageRect.top + expandCameraAxis(point.y, 0.1) * stageRect.height,
  };
}

function releaseAll(
  pressRef: { current: PressState | null },
  grabRef: { current: GrabState | null },
  pinchRef: { current: PinchState | null },
  activate: boolean,
): void {
  finishPress(pressRef, activate);
  finishGrab(grabRef);
  finishPinch(pinchRef);
}

function finishPress(pressRef: { current: PressState | null }, activate: boolean): void {
  const current = pressRef.current;
  if (!current) return;
  if (activate && current.action?.isConnected && !isDisabledAction(current.action)) {
    current.action.click();
  }
  pressRef.current = null;
}

function finishGrab(grabRef: { current: GrabState | null }): void {
  const current = grabRef.current;
  if (!current) return;
  if (current.begun) {
    emitVoiceSpatialInteraction({
      clientX: current.lastX,
      clientY: current.lastY,
      documentId: current.documentId,
      kind: "drag",
      phase: "end",
    });
  }
  grabRef.current = null;
}

function finishPinch(pinchRef: { current: PinchState | null }): void {
  const current = pinchRef.current;
  if (!current) return;
  const rect = current.element.isConnected ? current.element.getBoundingClientRect() : null;
  emitVoiceSpatialInteraction({
    clientX: rect ? rect.right : current.cornerX,
    clientY: rect ? rect.bottom : current.cornerY,
    corner: "south-east",
    documentId: current.documentId,
    kind: "resize",
    phase: "end",
  });
  pinchRef.current = null;
}

function resolveSpatialTarget(stage: HTMLElement, x: number, y: number): SpatialTarget | null {
  const elements = document.elementsFromPoint(x, y);
  const directSurface = elements
    .map((element) => element.closest<HTMLElement>("[data-surface-document]"))
    .find((element): element is HTMLElement => Boolean(element && stage.contains(element)));
  const surfaces = Array.from(stage.querySelectorAll<HTMLElement>("[data-surface-document]"));
  const surface = directSurface ?? nearestSurface(surfaces, x, y, AIRHAND_CAPTURE_RADIUS);
  if (!surface) return null;
  const documentId = surface.dataset.surfaceDocument;
  if (!documentId) return null;
  const rect = surface.getBoundingClientRect();
  const action = directSurface
    ? elements.map(actionableElement).find((element): element is HTMLElement => Boolean(element && surface.contains(element))) ?? null
    : null;
  return { action, documentId, element: surface, rect };
}

function actionableElement(element: Element): HTMLElement | null {
  const actionable = element.closest<HTMLElement>("button:not(.voice-widget-drag-handle), a[href], input, select, textarea, [role='button']");
  if (!actionable || actionable.closest(".voice-widget-resize-handle")) return null;
  return actionable;
}

function nearestSurface(surfaces: HTMLElement[], x: number, y: number, radius: number): HTMLElement | null {
  let best: HTMLElement | null = null;
  let bestDistance = radius;
  for (const surface of surfaces) {
    const distance = distanceToRect(x, y, surface.getBoundingClientRect());
    if (distance > bestDistance) continue;
    best = surface;
    bestDistance = distance;
  }
  return best;
}

function updateTarget(targetRef: { current: HTMLElement | null }, next: HTMLElement | null, state: string): void {
  if (targetRef.current && targetRef.current !== next) delete targetRef.current.dataset.airhandTarget;
  targetRef.current = next;
  if (!next) return;
  if (state) next.dataset.airhandTarget = state;
  else delete next.dataset.airhandTarget;
}

function isDisabledAction(element: HTMLElement): boolean {
  return element.matches(":disabled, [aria-disabled='true']");
}

function distanceToRect(x: number, y: number, rect: DOMRect): number {
  const dx = Math.max(rect.left - x, 0, x - rect.right);
  const dy = Math.max(rect.top - y, 0, y - rect.bottom);
  return Math.hypot(dx, dy);
}

function expandCameraAxis(value: number, margin: number): number {
  return clamp((value - margin) / Math.max(0.01, 1 - margin * 2), 0, 1);
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
