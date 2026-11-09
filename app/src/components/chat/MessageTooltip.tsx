import { Children, cloneElement, createContext, useCallback, useContext, useEffect, useId, useLayoutEffect, useRef, useState, type HTMLAttributes, type ReactElement, type ReactNode, type Ref, type SyntheticEvent } from "react";
import { createPortal } from "react-dom";
import "./messageTooltip.css";

const TooltipTriggerContext = createContext(false);
let currentTooltip: { id: string; close: () => void } | null = null;
type TooltipTriggerProps = HTMLAttributes<HTMLElement> & { disabled?: boolean; ref?: Ref<HTMLElement> };

type Anchor = {
  side: "bottom" | "top" | "right" | "left";
  triggerBottom: number;
  triggerTop: number;
  triggerX: number;
  triggerY: number;
  triggerLeft: number;
  triggerRight: number;
  viewportHeight: number;
  viewportWidth: number;
  x: number;
  y: number;
};

export function MessageTooltip({ children, asChild = false, className, delayMs, label, onActivate, variant = "default", side = "top", disabled = false }: {
  children: ReactNode;
  asChild?: boolean;
  className?: string;
  delayMs?: number;
  label: ReactNode;
  onActivate?: () => void;
  variant?: "activity-target" | "default" | "shell";
  side?: "top" | "right" | "bottom";
  disabled?: boolean;
}) {
  const nested = useContext(TooltipTriggerContext);
  const child = asChild ? Children.only(children) as ReactElement<TooltipTriggerProps> : null;
  const inactive = disabled || nested || Boolean(child?.props.disabled) || !label;
  const resolvedDelayMs = delayMs ?? 700;
  const tooltipId = useId();
  const triggerRef = useRef<HTMLElement | null>(null);
  const childRef = child?.props.ref;
  const setTriggerRef = useCallback((node: HTMLElement | null) => {
    triggerRef.current = node;
    if (typeof childRef === "function") {
      const cleanup = childRef(node);
      if (cleanup) return () => { triggerRef.current = null; cleanup(); };
    } else if (childRef) childRef.current = node;
  }, [childRef]);
  const tooltipRef = useRef<HTMLDivElement>(null);
  const timerRef = useRef<number | null>(null);
  const interactionRef = useRef({ focus: false, hoverLatched: false });
  const openEpochRef = useRef(0);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const tooltipOpen = anchor !== null;

  const clearTimer = useCallback(() => {
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const measureAnchor = useCallback((): Anchor | null => {
    const trigger = triggerRef.current;
    if (!trigger?.isConnected) return null;
    const rect = trigger.getBoundingClientRect();
    const triggerX = rect.left + rect.width / 2;
    return {
      side,
      triggerBottom: rect.bottom,
      triggerTop: rect.top,
      triggerX,
      triggerY: rect.top + rect.height / 2,
      triggerLeft: rect.left,
      triggerRight: rect.right,
      viewportHeight: window.innerHeight,
      viewportWidth: window.innerWidth,
      x: triggerX,
      y: rect.top + rect.height / 2,
    };
  }, [side]);

  const close = useCallback(() => {
    openEpochRef.current += 1;
    clearTimer();
    if (currentTooltip?.id === tooltipId) currentTooltip = null;
    setAnchor(null);
  }, [clearTimer, tooltipId]);

  const claim = useCallback(() => {
    if (currentTooltip?.id !== tooltipId) currentTooltip?.close();
    currentTooltip = { id: tooltipId, close };
  }, [close, tooltipId]);

  const openImmediately = useCallback(() => {
    clearTimer();
    openEpochRef.current += 1;
    if (inactive || !tooltipInteractionActive(interactionRef.current) || document.visibilityState === "hidden") return;
    claim();
    setAnchor(measureAnchor());
  }, [claim, clearTimer, inactive, measureAnchor]);

  const scheduleOpen = useCallback(() => {
    if (inactive || anchor || timerRef.current !== null) return;
    claim();
    const openEpoch = ++openEpochRef.current;
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null;
      if (
        openEpoch !== openEpochRef.current
        || !tooltipInteractionActive(interactionRef.current)
        || document.visibilityState === "hidden"
      ) return;
      setAnchor(measureAnchor());
    }, resolvedDelayMs);
  }, [anchor, claim, inactive, measureAnchor, resolvedDelayMs]);

  useEffect(() => () => {
    openEpochRef.current += 1;
    clearTimer();
    if (currentTooltip?.id === tooltipId) currentTooltip = null;
  }, [clearTimer, tooltipId]);

  useEffect(() => {
    if (inactive) {
      interactionRef.current = { focus: false, hoverLatched: false };
      close();
    }
  }, [close, inactive]);

  useLayoutEffect(() => {
    const tooltip = tooltipRef.current;
    if (!anchor || !tooltip) return;
    const rect = tooltip.getBoundingClientRect();
    const viewportPadding = 8;
    const topSpace = anchor.triggerTop - viewportPadding - 2;
    const bottomSpace = anchor.viewportHeight - anchor.triggerBottom - viewportPadding - 2;
    const naturalHeight = tooltip.scrollHeight;
    if (anchor.side === "right" || anchor.side === "left") {
      const rightSpace = anchor.viewportWidth - anchor.triggerRight - 16;
      const leftSpace = anchor.triggerLeft - 16;
      const nextSide = tooltip.scrollWidth > rightSpace && leftSpace > rightSpace ? "left" : "right";
      const nextY = Math.max(viewportPadding + rect.height / 2, Math.min(anchor.triggerY, anchor.viewportHeight - viewportPadding - rect.height / 2));
      if (anchor.side !== nextSide || Math.abs(anchor.y - nextY) >= .5) setAnchor((current) => current ? { ...current, side: nextSide, y: nextY } : current);
      return;
    }
    let nextSide = anchor.side;
    let nextX = anchor.x;

    if (anchor.side === "top" && naturalHeight > topSpace && bottomSpace > topSpace) {
      nextSide = "bottom";
    } else if (anchor.side === "bottom" && naturalHeight > bottomSpace && topSpace >= bottomSpace) {
      nextSide = "top";
    }
    if (rect.left < viewportPadding) {
      nextX += viewportPadding - rect.left;
    } else if (rect.right > anchor.viewportWidth - viewportPadding) {
      nextX -= rect.right - (anchor.viewportWidth - viewportPadding);
    }

    if (nextSide !== anchor.side || Math.abs(nextX - anchor.x) >= 0.5) {
      setAnchor((current) => current ? { ...current, side: nextSide, x: nextX } : current);
    }
  }, [anchor]);

  useEffect(() => {
    if (!tooltipOpen) return;
    const update = () => {
      const measured = measureAnchor();
      setAnchor((current) => {
        if (!measured || !current) return null;
        const geometryChanged = Math.abs(measured.triggerBottom - current.triggerBottom) >= 0.5
          || Math.abs(measured.triggerTop - current.triggerTop) >= 0.5
          || Math.abs(measured.triggerX - current.triggerX) >= 0.5
          || Math.abs(measured.triggerY - current.triggerY) >= 0.5
          || Math.abs(measured.triggerRight - current.triggerRight) >= 0.5
          || measured.viewportHeight !== current.viewportHeight
          || measured.viewportWidth !== current.viewportWidth;
        const next = {
          ...measured,
          side: current.side,
          x: geometryChanged ? measured.triggerX : current.x,
          y: geometryChanged ? measured.triggerY : current.y,
        };
        return next;
      });
    };
    const dismissOnVisibilityChange = () => {
      if (document.visibilityState === "hidden") close();
    };
    const dismissOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    const resizeObserver = typeof ResizeObserver === "undefined"
      ? null
      : new ResizeObserver(update);
    if (triggerRef.current) resizeObserver?.observe(triggerRef.current);
    if (tooltipRef.current) resizeObserver?.observe(tooltipRef.current);
    window.addEventListener("blur", close);
    window.addEventListener("resize", update);
    document.addEventListener("keydown", dismissOnEscape);
    document.addEventListener("pointerdown", close, true);
    document.addEventListener("scroll", close, true);
    document.addEventListener("visibilitychange", dismissOnVisibilityChange);
    return () => {
      resizeObserver?.disconnect();
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", update);
      document.removeEventListener("keydown", dismissOnEscape);
      document.removeEventListener("pointerdown", close, true);
      document.removeEventListener("scroll", close, true);
      document.removeEventListener("visibilitychange", dismissOnVisibilityChange);
    };
  }, [close, measureAnchor, tooltipOpen]);

  const availableHeight = anchor
    ? Math.max(0, anchor.side === "right" || anchor.side === "left" ? anchor.viewportHeight - 16 : anchor.side === "top"
      ? anchor.triggerTop - 10
      : anchor.viewportHeight - anchor.triggerBottom - 10)
    : 0;

  const triggerProps: HTMLAttributes<HTMLElement> = {
    "aria-describedby": anchor && !inactive ? tooltipId : undefined,
    onBlur: () => {
      interactionRef.current.focus = false;
      close();
    },
    onClick: (event) => {
      if (!onActivate) return;
      event.stopPropagation();
      onActivate();
    },
    onClickCapture: close,
    onFocus: (event) => {
      const focusVisible = event.target instanceof Element && event.target.matches(":focus-visible");
      interactionRef.current.focus = focusVisible;
      if (focusVisible) openImmediately();
    },
    onKeyDown: onActivate ? (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      event.stopPropagation();
      close();
      onActivate();
    } : undefined,
    onPointerLeave: () => {
      interactionRef.current.hoverLatched = false;
      close();
    },
    onPointerMove: (event) => {
      if (inactive || event.buttons !== 0 || event.pointerType === "touch" || interactionRef.current.hoverLatched) return;
      interactionRef.current.hoverLatched = true;
      scheduleOpen();
    },
    onPointerDown: (event) => {
      if (onActivate) event.stopPropagation();
    },
    onPointerDownCapture: close,
    role: onActivate ? "link" : undefined,
    tabIndex: variant === "activity-target" ? 0 : undefined,
  };
  const trigger = child ? cloneElement(child, {
    ...triggerProps,
    className: [child.props.className, className].filter(Boolean).join(" ") || undefined,
    "aria-describedby": [child.props["aria-describedby"], triggerProps["aria-describedby"]].filter(Boolean).join(" ") || undefined,
    onBlur: composeHandlers(child.props.onBlur, triggerProps.onBlur),
    onClick: composeHandlers(child.props.onClick, triggerProps.onClick),
    onClickCapture: composeHandlers(child.props.onClickCapture, triggerProps.onClickCapture),
    onFocus: composeHandlers(child.props.onFocus, triggerProps.onFocus),
    onKeyDown: composeHandlers(child.props.onKeyDown, triggerProps.onKeyDown),
    onPointerLeave: composeHandlers(child.props.onPointerLeave, triggerProps.onPointerLeave),
    onPointerMove: composeHandlers(child.props.onPointerMove, triggerProps.onPointerMove),
    onPointerDown: composeHandlers(child.props.onPointerDown, triggerProps.onPointerDown),
    onPointerDownCapture: composeHandlers(child.props.onPointerDownCapture, triggerProps.onPointerDownCapture),
    ref: setTriggerRef,
    role: child.props.role ?? triggerProps.role,
    tabIndex: child.props.tabIndex ?? triggerProps.tabIndex,
  }) : <span
    {...triggerProps}
    className={`chat-tooltip-trigger${variant === "activity-target" ? " is-activity-target" : ""}${className ? ` ${className}` : ""}`}
    data-agent-activity-file-link={variant === "activity-target" ? true : undefined}
    ref={setTriggerRef}
  >{children}</span>;

  return (
    <TooltipTriggerContext.Provider value={nested || Boolean(label)}>
      {trigger}
      {anchor && !inactive
        ? createPortal(
            <div
              className={`chat-tooltip${variant !== "default" ? ` is-${variant}` : ""}`}
              data-side={anchor.side}
              id={tooltipId}
              ref={tooltipRef}
              role="tooltip"
              style={{
                left: anchor.side === "right" ? anchor.triggerRight : anchor.side === "left" ? anchor.triggerLeft : anchor.x,
                maxHeight: availableHeight,
                top: anchor.side === "right" || anchor.side === "left" ? anchor.y : anchor.side === "top" ? anchor.triggerTop : anchor.triggerBottom,
                zIndex: triggerRef.current?.closest('[data-oc-dialog]') ? 1200 : 300,
              }}
            >
              {label}
            </div>,
            document.body,
          )
        : null}
    </TooltipTriggerContext.Provider>
  );
}

function composeHandlers<E extends SyntheticEvent>(original?: (event: E) => void, tooltip?: (event: E) => void) {
  if (!original) return tooltip;
  if (!tooltip) return original;
  return (event: E) => { original(event); tooltip(event); };
}

function tooltipInteractionActive(interaction: { focus: boolean; hoverLatched: boolean }): boolean {
  return interaction.focus || interaction.hoverLatched;
}
