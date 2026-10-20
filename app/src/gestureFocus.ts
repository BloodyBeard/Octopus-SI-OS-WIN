import { useEffect } from "react";
import { subscribeGestureState } from "./gestureRecognition";
import { subscribeAirHandGestures } from "./gestureShortcuts";
import { clientDialogOwnsCommands } from "./keyboardShortcuts";

const FOCUS_STEP = 0.055;
const FOCUS_IDLE_MS = 2_500;
const FOCUS_MAX_SPEED = 0.45;
const FOCUS_ATTR = "data-gesture-focused";
const TARGET_SELECTOR = "[data-gesture-focus], [data-surface-document]";

export function useGestureFocus(enabled: boolean): void {
  useEffect(() => {
    if (!enabled) return;
    let anchorX: number | null = null;
    let lastX: number | null = null;
    let lastAt = 0;
    let idleTimer = 0;
    const forget = () => {
      window.clearTimeout(idleTimer);
      idleTimer = 0;
      anchorX = null;
      clearGestureFocus();
    };
    const unsubscribeState = subscribeGestureState((state) => {
      if (state.pose === "none") {
        if (!idleTimer) idleTimer = window.setTimeout(forget, FOCUS_IDLE_MS);
      } else if (idleTimer) {
        window.clearTimeout(idleTimer);
        idleTimer = 0;
      }
      if (state.pose !== "open_palm" || !state.palmPoint) {
        anchorX = null;
        lastX = null;
        return;
      }
      if (clientDialogOwnsCommands()) {
        anchorX = null;
        return;
      }
      const x = state.palmPoint.x;
      const now = performance.now();
      const elapsed = (now - lastAt) / 1000;
      const fast = lastX != null && elapsed > 0 && Math.abs(x - lastX) / elapsed > FOCUS_MAX_SPEED;
      lastX = x;
      lastAt = now;
      if (fast) {
        anchorX = null;
        return;
      }
      if (anchorX == null) {
        anchorX = x;
        return;
      }
      const delta = x - anchorX;
      if (Math.abs(delta) < FOCUS_STEP) return;
      anchorX += Math.sign(delta) * FOCUS_STEP;
      stepGestureFocus(delta > 0 ? 1 : -1);
    });
    window.addEventListener("pointerdown", forget, true);
    window.addEventListener("keydown", forget, true);
    return () => {
      unsubscribeState();
      window.removeEventListener("pointerdown", forget, true);
      window.removeEventListener("keydown", forget, true);
      forget();
    };
  }, [enabled]);

  useEffect(() => {
    if (!enabled) return;
    return subscribeAirHandGestures((gesture) => {
      if (gesture !== "left_click") return;
      if (document.querySelector("[data-airhand-cursor]")) return;
      if (clientDialogOwnsCommands()) return;
      activateGestureFocus();
    });
  }, [enabled]);

  useEffect(() => () => clearGestureFocus(), []);
}

function focusTargets(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>(TARGET_SELECTOR)).filter((element) => {
    if (element.closest("[inert], [aria-hidden='true'], [data-state='closed']")) return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  });
}

function stepGestureFocus(direction: number): void {
  const targets = focusTargets();
  if (targets.length === 0) return;
  const currentIndex = targets.findIndex((element) => element.hasAttribute(FOCUS_ATTR));
  clearGestureFocus();
  const nextIndex = currentIndex < 0
    ? (direction > 0 ? 0 : targets.length - 1)
    : (currentIndex + direction + targets.length) % targets.length;
  const next = targets[nextIndex];
  if (!next) return;
  next.setAttribute(FOCUS_ATTR, "true");
  next.scrollIntoView({ block: "nearest" });
}

function activateGestureFocus(): void {
  const focused = document.querySelector<HTMLElement>(`[${FOCUS_ATTR}]`);
  if (!focused || !focused.isConnected) return;
  if (focused.matches("[data-surface-document]")) return;
  if (focused.matches(":disabled, [aria-disabled='true']")) return;
  clearGestureFocus();
  focused.click();
}

function clearGestureFocus(): void {
  for (const element of Array.from(document.querySelectorAll(`[${FOCUS_ATTR}]`))) {
    element.removeAttribute(FOCUS_ATTR);
  }
}
