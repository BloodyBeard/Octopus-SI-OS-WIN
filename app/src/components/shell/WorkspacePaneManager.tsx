import { motion, type MotionValue } from "motion/react";
import { createContext, useContext, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { restoreModalFocus } from "../../design-system/modalFocus";
import { clientDialogOwnsCommands } from "../../keyboardShortcuts";
import { PanelResizeHandle, useAnimatedPanelSize } from "./PanelResizeHandle";

const DEFAULT_SECONDARY_WIDTH_PERCENT = 34;
const MIN_SECONDARY_WIDTH = 320;
const MAIN_CONTENT_MIN_WIDTH = 352;
const COMPACT_OVERLAY_WIDTH = 720;
const PanePresentationContext = createContext<{ compactOverlay: boolean; managerId: string } | null>(null);

export function useWorkspacePanePresentation() {
  const presentation = useContext(PanePresentationContext);
  if (!presentation) throw new Error("Pane controls require their workspace pane manager");
  return presentation;
}

type Props = {
  primary: ReactNode;
  secondary: ReactNode;
  secondaryFocused: boolean;
  secondaryOpen: boolean;
  secondaryWidthPercent: number;
  onOpenChange: (open: boolean) => void;
  resolvedWidthMotion: MotionValue<number>;
  onSecondaryFocusedChange: (focused: boolean) => void;
  onSecondaryWidthPercentChange: (width: number) => void;
  primaryPaneRole?: "primary" | "secondary";
  onPrimaryBlockedChange?: (blocked: boolean) => void;
};

type ContainerSize = {
  width: number;
  height: number;
};

export function WorkspacePaneManager({
  primary,
  secondary,
  secondaryFocused,
  secondaryOpen,
  secondaryWidthPercent,
  onOpenChange,
  resolvedWidthMotion,
  onSecondaryFocusedChange,
  onSecondaryWidthPercentChange,
  primaryPaneRole = "primary",
  onPrimaryBlockedChange,
}: Props) {
  const rootRef = useRef<HTMLDivElement>(null);
  const primaryRef = useRef<HTMLElement>(null);
  const secondaryRef = useRef<HTMLElement>(null);
  const managerId = useId();
  const previousOpenRef = useRef(secondaryOpen);
  const instantOpenRef = useRef(false);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const lastFocusRef = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const lastFocusOwnerRef = useRef<string | null>(null);
  const [containerSize, setContainerSize] = useState<ContainerSize>(() => ({
    width: window.innerWidth,
    height: window.innerHeight,
  }));
  const [liveWidth, setLiveWidth] = useState<number | null>(null);
  const [resizing, setResizing] = useState(false);

  useLayoutEffect(() => {
    const element = rootRef.current;
    if (!element) return;
    const update = () => {
      const rect = element.getBoundingClientRect();
      setContainerSize({ width: rect.width, height: rect.height });
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const limits = useMemo(() => panelLimits(containerSize.width), [containerSize.width]);
  const compactOverlay = containerSize.width <= COMPACT_OVERLAY_WIDTH;
  const primaryBlocked = secondaryOpen && (secondaryFocused || compactOverlay);
  const preferredWidth = clampWidth(containerSize.width * clampPercent(secondaryWidthPercent) / 100, limits);
  const resolvedWidth = secondaryFocused || compactOverlay
    ? containerSize.width
    : liveWidth == null
      ? preferredWidth
      : Math.max(0, Math.min(limits.maximum, liveWidth));
  if (!secondaryOpen) instantOpenRef.current = false;
  else if (secondaryFocused && !previousOpenRef.current) instantOpenRef.current = true;
  const panelMotion = useAnimatedPanelSize(secondaryOpen, resolvedWidth, resolvedWidthMotion, !secondaryOpen || resizing || instantOpenRef.current);

  useLayoutEffect(() => {
    onPrimaryBlockedChange?.(primaryBlocked);
  }, [onPrimaryBlockedChange, primaryBlocked]);

  useEffect(() => {
    if (!secondaryOpen) return;
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || !(secondaryFocused || compactOverlay) || event.defaultPrevented || clientDialogOwnsCommands()) return;
      if (!(event.target instanceof Element) || event.target.closest<HTMLElement>('.workspace-pane-manager, [data-pane-manager-controls]')?.dataset.paneManagerId !== managerId) return;
      event.preventDefault();
      if (compactOverlay) onOpenChange(false);
      else onSecondaryFocusedChange(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [compactOverlay, managerId, onOpenChange, onSecondaryFocusedChange, secondaryFocused, secondaryOpen]);

  useEffect(() => {
    const rememberFocus = (event: FocusEvent) => {
      if (!(event.target instanceof HTMLElement) || event.target === document.body) return;
      lastFocusRef.current = event.target;
      const pane = event.target.closest<HTMLElement>('[data-pane-manager-controls], .workspace-pane-secondary');
      lastFocusOwnerRef.current = pane?.dataset.paneManagerId ?? pane?.closest<HTMLElement>('.workspace-pane-manager')?.dataset.paneManagerId ?? null;
    };
    window.addEventListener("focusin", rememberFocus);
    return () => window.removeEventListener("focusin", rememberFocus);
  }, []);

  useLayoutEffect(() => {
    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (secondaryOpen && !previousOpenRef.current) returnFocusRef.current = active === document.body ? lastFocusRef.current : active;
    if (primaryBlocked && active && primaryRef.current?.contains(active)) {
      const target = [...secondaryRef.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), [tabindex="0"]') ?? []]
        .find((element) => !element.closest('[inert], [hidden], [aria-hidden="true"]') && element.getClientRects().length > 0) ?? secondaryRef.current;
      target?.focus({ preventScroll: true });
    }
    if (!secondaryOpen && previousOpenRef.current) {
      if (lastFocusOwnerRef.current === managerId && (!active || active === document.body || active === lastFocusRef.current)) {
        restoreModalFocus(returnFocusRef.current?.isConnected ? returnFocusRef.current : primaryRef.current);
      }
    }
    previousOpenRef.current = secondaryOpen;
  }, [managerId, primaryBlocked, secondaryOpen]);

  useEffect(() => () => resolvedWidthMotion.set(0), [resolvedWidthMotion]);

  return (
    <PanePresentationContext.Provider value={{ compactOverlay, managerId }}><div
      className={`workspace-pane-manager${resizing ? " is-resizing" : ""}${compactOverlay ? " is-secondary-overlay" : ""}${secondaryFocused && secondaryOpen ? " is-secondary-focused" : ""}`}
      ref={rootRef}
      data-pane-manager-id={managerId}
    >
      <section aria-hidden={primaryBlocked || undefined} className="workspace-pane workspace-pane-primary" aria-label="Primary workspace" data-pane-role={primaryPaneRole} inert={primaryBlocked || undefined} ref={primaryRef} tabIndex={-1}>
        {primary}
      </section>
      {secondaryOpen || resizing ? (
          <motion.aside
            aria-label="Secondary workspace"
            className="workspace-pane workspace-pane-secondary"
            aria-hidden={!secondaryOpen || undefined}
            inert={!secondaryOpen || undefined}
            ref={secondaryRef}
            tabIndex={-1}
            data-pane-role="secondary"
            style={{ opacity: panelMotion.opacity, width: panelMotion.animatedSize }}
          >
            {!secondaryFocused && !compactOverlay ? (
              <PanelResizeHandle
                ariaLabel="Resize secondary pane"
                className="workspace-pane-separator"
                defaultSize={clampWidth(containerSize.width * DEFAULT_SECONDARY_WIDTH_PERCENT / 100, limits)}
                edge="left"
                maximum={limits.maximum}
                minimum={limits.minimum}
                onChange={setLiveWidth}
                onCommit={(width) => commitWidth(width, containerSize.width, limits, onSecondaryWidthPercentChange)}
                onOpenChange={onOpenChange}
                onResizingChange={(next) => {
                  setResizing(next);
                  setLiveWidth(next ? resolvedWidth : null);
                }}
                title="Drag to resize · double-click to reset"
                value={resolvedWidth}
              />
            ) : null}
            <div className="workspace-pane-secondary-clip">
              <div
                className="workspace-pane-secondary-content"
                style={{ minWidth: resolvedWidth, width: resolvedWidth }}
              >
                {secondary}
              </div>
            </div>
          </motion.aside>
      ) : null}
    </div></PanePresentationContext.Provider>
  );
}

function panelLimits(mainContentWidth: number) {
  const maximum = Math.max(MIN_SECONDARY_WIDTH, mainContentWidth - MAIN_CONTENT_MIN_WIDTH);
  return {
    minimum: Math.min(MIN_SECONDARY_WIDTH, maximum),
    maximum,
  };
}

function commitWidth(
  width: number,
  containerWidth: number,
  limits: ReturnType<typeof panelLimits>,
  onChange: (value: number) => void,
) {
  if (containerWidth <= 0) return;
  const percentage = clampWidth(width, limits) / containerWidth * 100;
  onChange(Math.round(percentage * 10) / 10);
}

function clampWidth(width: number, limits: ReturnType<typeof panelLimits>) {
  return Math.max(limits.minimum, Math.min(limits.maximum, width));
}

function clampPercent(value: number) {
  return Math.max(0, Math.min(100, value));
}
