import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, animate, motion, useMotionValue, useTransform } from "motion/react";
import { PowerBurst, PowerDecoration } from "./providerModelPickerEffects";
import type { OctopusProviderInterface, OctopusProviderModel } from "../../electron";
import { useShouldReduceMotion } from "../../appearance";
import { subscribeClientCommands } from "../../clientActions";
import { Glyph } from "../IconGlyphs";
import { providerModels, providerReasoningEfforts, providerSelectedModel, providerServiceTiers, providerSelectedServiceTier } from "./chatWorkspaceCapabilities";
import "./providerModelPicker.css";

type Props = {
  providers: OctopusProviderInterface[];
  activeProvider?: OctopusProviderInterface;
  loading?: boolean;
  providerInterfaceId: string;
  providerModel: string;
  interfaceLocked?: boolean;
  reasoningEffort: string;
  serviceTier?: string;
  onModelChange: (providerInterfaceId: string, providerModel: string, reasoningEffort?: string) => void;
  onReasoningEffortChange: (reasoningEffort: string) => void;
  onServiceTierChange?: (serviceTier: string) => void;
  onRefreshModels?: () => void;
  listenToGlobalCommands?: boolean;
};
type ModelOption = { provider: OctopusProviderInterface; model: OctopusProviderModel };

function displayName(option: ModelOption, options: ModelOption[]): string {
  const name = (option.model.displayName || option.model.model).replace(/^gpt[-\s]+/i, "");
  const duplicate = options.some((other) => other.provider.id !== option.provider.id && other.model.model === option.model.model);
  const sameLabel = options.some((other) => other.provider.id !== option.provider.id && other.provider.label === option.provider.label && other.model.model === option.model.model);
  return duplicate ? name + " · " + (sameLabel ? option.provider.id : option.provider.label || option.provider.id) : name;
}
function effortLabel(effort: string): string {
  return ({ low: "Light", xhigh: "Extra High" } as Record<string, string>)[effort] || (effort ? effort[0].toUpperCase() + effort.slice(1) : "Power");
}

export function ProviderModelPicker({
  providers, activeProvider, loading = false, providerInterfaceId, providerModel, interfaceLocked = false,
  reasoningEffort, serviceTier = "", onModelChange, onReasoningEffortChange, onServiceTierChange, onRefreshModels, listenToGlobalCommands = true,
}: Props) {
  const id = useId();
  const reducedMotion = useShouldReduceMotion();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const anchorRef = useRef<HTMLSpanElement>(null);
  const placeholderRef = useRef<HTMLSpanElement>(null);
  const [triggerSizing, setTriggerSizing] = useState<{ minimum: number; offset: number } | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);
  const simplePanel = useRef<HTMLDivElement>(null), advancedPanel = useRef<HTMLDivElement>(null);
  const [panelHeights, setPanelHeights] = useState<{ simple?: number; advanced?: number }>({});
  const [transitionsReady, setTransitionsReady] = useState(false);
  const tooltipTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const openRef = useRef(false);
  const keyboardOpen = useRef(false);
  const tooltipSuppressed = useRef(false);
  const typeahead = useRef({ text: "", time: 0 });
  const [open, setOpen] = useState(false);
  const [tooltip, setTooltip] = useState(false);
  const [menuView, setMenuView] = useState<"simple" | "advanced">("simple");
  const options = useMemo(() => (interfaceLocked ? activeProvider ? [activeProvider] : [] : providers)
    .flatMap((provider) => providerModels(provider).map((model) => ({ provider, model }))),
  [activeProvider, interfaceLocked, providers]);
  const activeModel = activeProvider ? providerSelectedModel(activeProvider, providerModel) : null;
  const selectedEffort = reasoningEffort || activeModel?.defaultReasoningEffort || "";
  const efforts = activeProvider && activeModel ? providerReasoningEfforts(activeProvider, activeModel.model) : [];
  const tiers = activeProvider && activeModel ? providerServiceTiers(activeProvider, activeModel.model) : [];
  const selectedTier = activeProvider ? providerSelectedServiceTier(activeProvider, providerModel, serviceTier) : "default";
  const tierLabel = tiers.find((tier) => tier.id === selectedTier)?.name ?? selectedTier + " · unavailable";
  const unavailableLabel = loading ? "Loading model…" : providerModel ? providerModel + " · unavailable" : "Select model";
  const modelLabel = activeProvider && activeModel ? displayName({ provider: activeProvider, model: activeModel }, options) : unavailableLabel;
  const disabled = options.length === 0;
  const effortAvailable = efforts.includes(selectedEffort);
  const view = efforts.length < 2 || !effortAvailable ? "advanced" : menuView;
  const [warning, setWarning] = useState(false);
  useEffect(() => {
    if (!warning) return;
    const timer = setTimeout(() => setWarning(false), 2000);
    return () => clearTimeout(timer);
  }, [warning]);

  const hideTooltip = useCallback(() => {
    if (tooltipTimer.current !== null) clearTimeout(tooltipTimer.current);
    tooltipTimer.current = null;
    setTooltip(false);
  }, []);
  const showTooltip = () => {
    if (openRef.current || tooltipSuppressed.current || tooltipTimer.current !== null) return;
    tooltipTimer.current = setTimeout(() => {
      tooltipTimer.current = null;
      if (!openRef.current) setTooltip(true);
    }, 650);
  };
  const closeMenu = useCallback((restoreFocus = true) => {
    openRef.current = false;
    setOpen(false);
    setTriggerSizing(null);
    setWarning(false);
    hideTooltip();
    if (restoreFocus && triggerRef.current?.isConnected) triggerRef.current.focus({ preventScroll: true });
  }, [hideTooltip]);
  const openMenu = useCallback(() => {
    if (disabled || openRef.current) return;
    const contentWidth = anchorRef.current?.getBoundingClientRect().width ?? 0;
    const minimum = Math.max(contentWidth, placeholderRef.current?.getBoundingClientRect().width ?? 0);
    const buttonWidth = triggerRef.current?.getBoundingClientRect().width ?? 0;
    setTriggerSizing({ minimum, offset: (buttonWidth - contentWidth + minimum - 254) / 2 });
    setTransitionsReady(false);
    hideTooltip();
    openRef.current = true;
    setOpen(true);
    onRefreshModels?.();
  }, [disabled, hideTooltip, onRefreshModels]);

  useEffect(() => subscribeClientCommands((command) => {
    if (listenToGlobalCommands && command === "open_model_picker") openMenu();
  }), [listenToGlobalCommands, openMenu]);
  useEffect(() => { if (disabled && open) closeMenu(); }, [closeMenu, disabled, open]);
  useEffect(() => () => {
    if (tooltipTimer.current !== null) clearTimeout(tooltipTimer.current);
  }, []);

  useLayoutEffect(() => {
    if (!open || !simplePanel.current || !advancedPanel.current) return;
    const measure = () => {
      const simple = simplePanel.current!.offsetHeight, advanced = advancedPanel.current!.offsetHeight;
      setPanelHeights(previous => previous.simple === simple && previous.advanced === advanced ? previous : { simple, advanced });
    };
    measure();
    const frame = requestAnimationFrame(() => setTransitionsReady(true));
    const observer = new ResizeObserver(measure);
    observer.observe(simplePanel.current); observer.observe(advancedPanel.current);
    return () => { cancelAnimationFrame(frame); observer.disconnect(); };
  }, [open, options.length, modelLabel, selectedEffort, selectedTier, tiers.length]);

  useLayoutEffect(() => {
    const popup = open ? menuRef.current : tooltip ? tooltipRef.current : null;
    const trigger = triggerRef.current;
    if (!popup || !trigger) return;
    const place = () => {
      const viewport = window.visualViewport;
      const width = viewport?.width ?? window.innerWidth;
      const height = viewport?.height ?? window.innerHeight;
      const originX = viewport?.offsetLeft ?? 0, originY = viewport?.offsetTop ?? 0;
      const triggerBox = trigger.getBoundingClientRect();
      const anchor = anchorRef.current?.getBoundingClientRect() ?? triggerBox;
      const above = Math.max(0, triggerBox.top - originY - 16);
      const below = Math.max(0, originY + height - triggerBox.bottom - 16);
      popup.style.maxWidth = Math.max(0, width - 16) + "px";
      popup.style.maxHeight = Math.max(above, below) + "px";
      // The opening transform affects the painted rectangle, not the menu's layout.
      const popupWidth = popup.offsetWidth, popupHeight = popup.offsetHeight;
      const side = above >= popupHeight || above >= below ? "top" : "bottom";
      popup.dataset.side = side;
      const desiredLeft = open && triggerSizing ? triggerBox.right - popupWidth - triggerSizing.offset - 1 : anchor.left + anchor.width / 2 - popupWidth / 2;
      const edge = open ? 7 : 8;
      const left = Math.max(originX + edge, Math.min(desiredLeft, originX + width - popupWidth - edge));
      const top = side === "top" ? triggerBox.top - popupHeight - (open ? 9 : 2) : triggerBox.bottom + (open ? 9 : 2);
      const scale = window.devicePixelRatio || 1;
      popup.style.left = Math.round(left * scale) / scale + "px";
      popup.style.top = Math.round(Math.max(originY + edge, Math.min(top, originY + height - popupHeight - edge)) * scale) / scale + "px";
    };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(popup);
    observer.observe(trigger);
    if (anchorRef.current) observer.observe(anchorRef.current);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    window.visualViewport?.addEventListener("resize", place);
    window.visualViewport?.addEventListener("scroll", place);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
      window.visualViewport?.removeEventListener("resize", place);
      window.visualViewport?.removeEventListener("scroll", place);
    };
  }, [view, open, tooltip, options.length, modelLabel, selectedEffort, panelHeights, triggerSizing]);

  useLayoutEffect(() => {
    if (!open) return;
    advancedPanel.current?.querySelector(".provider-model-list")?.scrollTo({ top: 0 });
    if (keyboardOpen.current) {
      const selected = view === "advanced" ? menuRef.current?.querySelector<HTMLElement>("[role=menuitemradio][aria-checked=true]") : null;
      const first = menuRef.current?.querySelector<HTMLElement>(view === "advanced" ? "[role=menuitemradio]" : "[data-model-picker-view-toggle]");
      (selected ?? first)?.focus({ preventScroll: view !== "advanced" });
    }
    else menuRef.current?.focus({ preventScroll: true });
  }, [open, view]);
  useEffect(() => {
    if (!open && !tooltip) return;
    const touchOutside = () => closeMenu(false);
    const cancelTouchOutside = () => document.removeEventListener("click", touchOutside);
    const outside = (event: globalThis.PointerEvent) => {
      if (!(event.target instanceof Node) || triggerRef.current?.contains(event.target) || menuRef.current?.contains(event.target)) return;
      if (event.pointerType === "touch") { cancelTouchOutside(); document.addEventListener("click", touchOutside, { once: true }); }
      else closeMenu(false);
    };
    const focusOutside = (event: FocusEvent) => {
      if (open && event.target instanceof Node && !triggerRef.current?.contains(event.target) && !menuRef.current?.contains(event.target)) closeMenu(false);
    };
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); closeMenu(open); }
    };
    const blur = () => closeMenu(false);
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("pointercancel", cancelTouchOutside);
    document.addEventListener("focusin", focusOutside);
    document.addEventListener("keydown", escape);
    window.addEventListener("blur", blur);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("pointercancel", cancelTouchOutside);
      cancelTouchOutside();
      document.removeEventListener("focusin", focusOutside);
      document.removeEventListener("keydown", escape);
      window.removeEventListener("blur", blur);
    };
  }, [closeMenu, open, tooltip]);

  const toggleView = () => { setWarning(false); setMenuView((view) => view === "simple" ? "advanced" : "simple"); };
  const focusHoveredItem = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.pointerType === "mouse" && event.buttons === 0) event.currentTarget.focus({ preventScroll: true });
  };
  const leaveItem = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.pointerType === "mouse" && event.buttons === 0 && document.activeElement === event.currentTarget) menuRef.current?.focus({ preventScroll: true });
  };
  const menuKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.defaultPrevented || event.ctrlKey || event.altKey || event.metaKey) return;
    const items = [...event.currentTarget.querySelectorAll<HTMLElement>(view === "advanced" ? "[role=menuitemradio]" : "[role=menuitem], [role=slider], [data-service-tier]")];
    const current = items.indexOf(document.activeElement as HTMLElement);
    if (event.key === "Tab") {
      event.preventDefault();
      const next = current < 0 ? event.shiftKey ? items.length - 1 : 0 : current + (event.shiftKey ? -1 : 1);
      if (view === "simple" && next >= 0 && next < items.length) {
        items[next].focus({ preventScroll: true });
      } else {
        closeMenu();
      }
      return;
    }
    if (event.key.length === 1 && event.key !== " ") {
      const now = performance.now();
      const text = (now - typeahead.current.time > 1000 ? "" : typeahead.current.text) + event.key.toLowerCase();
      typeahead.current = { text, time: now };
      const query = [...text].every(letter => letter === text[0]) ? text[0] : text;
      const ordered = [...items.slice(current + 1), ...items.slice(0, current + 1)];
      const match = ordered.find(item => (item.dataset.pickerSearchLabel || item.firstElementChild?.textContent || item.textContent || "").trim().toLowerCase().startsWith(query));
      if (match) { event.preventDefault(); match.focus({ preventScroll: true }); }
      return;
    }
    const target = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
      : event.key === "ArrowDown" ? (current + 1) % items.length
        : event.key === "ArrowUp" ? (current - 1 + items.length) % items.length : null;
    if (target !== null) { event.preventDefault(); items[target]?.focus({ preventScroll: true }); }
  };
  const selectModel = (option: ModelOption) => {
    setWarning(false);
    if (option.provider.id !== providerInterfaceId || option.model.model !== providerModel) {
      const effort = option.model.supportedReasoningEfforts.includes(selectedEffort)
        ? selectedEffort : option.model.defaultReasoningEffort;
      onModelChange(option.provider.id, option.model.model, effort);
    }
    setMenuView("simple");
  };
  const effortText = effortAvailable ? effortLabel(selectedEffort) : "";

  return <div className="provider-model-picker" data-provider-picker>
    <button ref={triggerRef} type="button" className="provider-model-trigger" data-provider-picker-trigger
      data-selected-reasoning-effort={selectedEffort} data-selected-service-tier={selectedTier} aria-haspopup="menu" aria-expanded={open}
      aria-controls={open ? id + "-menu" : undefined} aria-describedby={tooltip ? id + "-tooltip" : undefined}
      aria-label={"Select model: " + modelLabel + (selectedEffort ? ", " + effortText : "") + (onServiceTierChange ? ", " + tierLabel : "")} disabled={disabled}
      onPointerEnter={(event) => { if (event.pointerType !== "touch") showTooltip(); }} onPointerLeave={() => { tooltipSuppressed.current = false; hideTooltip(); }} onFocus={showTooltip} onBlur={() => { tooltipSuppressed.current = false; hideTooltip(); }}
      onPointerDown={(event) => { if (event.button === 0 && !event.ctrlKey) { keyboardOpen.current = false; if (!openRef.current) event.preventDefault(); openRef.current ? closeMenu() : openMenu(); } }}
      onClick={(event) => { if (event.detail === 0) { keyboardOpen.current = false; openRef.current ? closeMenu() : openMenu(); } }}
      onKeyDown={(event) => {
        if (["Enter", " ", "ArrowDown"].includes(event.key)) { event.preventDefault(); keyboardOpen.current = true; if (event.key !== "ArrowDown" && openRef.current) closeMenu(); else openMenu(); }
      }}>
      <span ref={placeholderRef} className="provider-model-placeholder" aria-hidden="true" data-provider-picker-placeholder>Select model</span>
      <span ref={anchorRef} className="provider-model-trigger-label" data-provider-picker-anchor style={{ minWidth: triggerSizing?.minimum }}>
        <span>{open && view === "simple" ? "Select model" : modelLabel}</span>{!(open && view === "simple") && effortAvailable ? <span className="provider-model-trigger-effort" data-maximum={selectedEffort === "ultra" || undefined}>{effortLabel(selectedEffort)}</span> : null}
        {!(open && view === "simple") && onServiceTierChange && selectedTier !== "default" ? <span className="provider-model-trigger-effort">{tierLabel}</span> : null}
      </span>
      <span className="provider-model-trigger-chevron" aria-hidden="true" />
    </button>
    {tooltip && !open ? createPortal(<div ref={tooltipRef} id={id + "-tooltip"} role="tooltip" className="provider-model-tooltip">Select model</div>, document.body) : null}
    {open ? createPortal(<div ref={menuRef} id={id + "-menu"} role="menu" tabIndex={-1} aria-label="Model and Power"
      className="provider-model-menu" data-provider-picker-menu data-view={view} data-reduced-motion={reducedMotion || undefined}
      onKeyDown={menuKey} onFocusCapture={(event) => {
        for (const item of event.currentTarget.querySelectorAll<HTMLElement>("[role=menuitem], [role=menuitemradio], [role=slider]")) {
          item.tabIndex = item === event.target ? 0 : -1;
        }
      }}>
      <div className="provider-model-views" data-transitions-ready={transitionsReady} style={{ height: panelHeights[view] }}>
      <div ref={simplePanel} className="provider-model-view-panel" data-active={view === "simple"} aria-hidden={view !== "simple"} inert={view !== "simple"} data-provider-picker-panel="simple">
      <div className="provider-model-controls" data-warning={warning || undefined}>
      <button type="button" role="menuitem" className="provider-model-view-toggle" data-model-picker-view-toggle
        data-picker-search-label={modelLabel + effortText}
        onPointerMove={focusHoveredItem} onPointerLeave={leaveItem}
        aria-label={view === "simple" ? "Choose a model" : "Back to Power"} onClick={toggleView}
        onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); toggleView(); } }}>
        <span className="provider-model-view-label" data-maximum={selectedEffort === "ultra" || undefined}>
          <span className="provider-model-effort-text">{effortText}</span><span className="provider-model-chevron" aria-hidden="true" />
          <span className="provider-model-current">{modelLabel}</span>
        </span>
      </button>
      <span className="provider-model-ultra-warning" aria-hidden="true"><motion.span key={String(warning)}
        initial={{ backgroundPosition: warning && !reducedMotion ? "100% 0" : "0% 0" }} animate={{ backgroundPosition: "0% 0" }}
        transition={{ duration: reducedMotion ? 0 : 1.1, ease: "easeOut" }}
        style={{ backgroundImage: "linear-gradient(100deg, " + [[42,100],[47,70],[50,40],[53,70],[58,100]].map(([stop,purple]) => "color-mix(in srgb, var(--color-chart-purple) " + purple + "%, var(--color-text)) " + stop + "%").join(", ") + ")" }}>Consumes usage limits faster</motion.span></span>
      </div>
      <div className="provider-model-power-panel">
        {efforts.length > 1 && effortAvailable ? <PowerSlider efforts={efforts} value={selectedEffort} onChange={onReasoningEffortChange} reducedMotion={reducedMotion} onComplete={() => { tooltipSuppressed.current = true; closeMenu(); }} onUltraDrag={() => setWarning(true)} active={view === "simple"} />
          : null}
      </div></div><div ref={advancedPanel} className="provider-model-view-panel" data-active={view === "advanced"} aria-hidden={view !== "advanced"} inert={view !== "advanced"} data-provider-picker-panel="advanced">
      <div className="provider-model-list">
        <div className="provider-model-section-label">Select model</div>
        {options.map((option) => <button type="button" role="menuitemradio" className="provider-model-option"
          key={JSON.stringify([option.provider.id, option.model.model])} tabIndex={-1} data-provider-id={option.provider.id} data-model-id={option.model.model}
          aria-checked={option.provider.id === providerInterfaceId && option.model.model === providerModel}
          onPointerMove={focusHoveredItem} onPointerLeave={leaveItem}
          onClick={() => selectModel(option)} onKeyDown={(event) => {
            if (event.key === "Enter" || event.key === " ") { event.preventDefault(); selectModel(option); }
          }}>
          <span title={option.provider.id + " · " + option.model.model}>{displayName(option, options)}</span>{option.provider.id === providerInterfaceId && option.model.model === providerModel ? <Glyph name="check" size={16} className="provider-model-check" /> : null}
        </button>)}
      </div></div></div>
      {onServiceTierChange && activeModel ? <div className="provider-model-speed-panel">
        <span className="provider-model-section-label">Speed · {tierLabel}</span>
        {tiers.map((tier) => <button type="button" role="menuitemradio" tabIndex={-1}
          className="provider-model-option" key={tier.id} data-service-tier={tier.id}
          aria-checked={selectedTier === tier.id} title={tier.description}
          onPointerMove={focusHoveredItem} onPointerLeave={leaveItem}
          onClick={() => onServiceTierChange(tier.id)} onKeyDown={(event) => {
            if (event.key === "Enter" || event.key === " ") { event.preventDefault(); onServiceTierChange(tier.id); }
          }}>
          <span>{tier.name}</span>{selectedTier === tier.id ? <Glyph name="check" size={16} className="provider-model-check" /> : null}
        </button>)}
      </div> : null}
    </div>, document.body) : null}
  </div>;
}

function PowerSlider({ efforts, value, onChange, reducedMotion, onComplete, onUltraDrag, active }: {
  efforts: string[]; value: string; onChange: (effort: string) => void; reducedMotion: boolean;
  onComplete: () => void; onUltraDrag: () => void; active: boolean;
}) {
  const index = efforts.indexOf(value), intervals = efforts.length - 1;
  const progress = intervals > 0 ? Math.max(0, index) / intervals * 100 : 0;
  const percentage = useMotionValue(progress);
  const [pressed, setPressed] = useState(false), [hovered, setHovered] = useState(false), [moved, setMoved] = useState(false), [burst, setBurst] = useState(0);
  const dragging = useRef<number | null>(null), sliderRef = useRef<HTMLDivElement>(null);
  const movedRef = useRef(false);
  const wheel = useRef({ time: 0, amount: 0 });
  const disabled = intervals <= 0;
  const maximum = intervals > 0 && value === "ultra";
  const correction = (position: number) => (13 - position * .26) * (intervals > 0 ? Math.min(position * intervals / 100, (100 - position) * intervals / 100, 1) : 0);
  const fillPosition = useTransform(percentage, position => "translateX(calc(" + (position - 100) + "% + " + correction(position) + "px))");
  const fieldPosition = useTransform(percentage, position => "translateX(calc(" + ((100 - position) * .9) + "% - " + (correction(position) * .9) + "px))");
  const thumbPosition = useTransform(percentage, position => "calc(" + position + "% + " + (13 - position * .26) + "px)");
  useEffect(() => {
    if (reducedMotion || !active) { percentage.jump(progress); return; }
    const control = animate(percentage, progress, { duration: moved ? .15 : .3, ease: [.23,1,.32,1] });
    return () => control.stop();
  }, [progress, moved, percentage, reducedMotion, active]);
  const select = (next: number, decorated = false) => {
    if (disabled) return;
    const effort = efforts[Math.max(0, Math.min(next, intervals))];
    if (effort !== value) {
      if (decorated && effort === "ultra" && !maximum) {
        setBurst(count => count + 1);
        if (movedRef.current) onUltraDrag();
      } else if (effort !== "ultra") setBurst(0);
      onChange(effort);
    }
  };
  useEffect(() => {
    const element = sliderRef.current;
    if (!element) return;
    const scroll = (event: WheelEvent) => {
      if (event.ctrlKey || disabled) return;
      event.preventDefault(); event.stopPropagation();
      let delta = Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : -event.deltaY;
      if ((event as WheelEvent & { webkitDirectionInvertedFromDevice?: boolean }).webkitDirectionInvertedFromDevice) delta *= -1;
      if (event.deltaMode !== 0) delta = Math.sign(delta) * 30;
      if (!delta) return;
      if (event.timeStamp - wheel.current.time > 160 || Math.sign(delta) !== Math.sign(wheel.current.amount)) wheel.current.amount = 0;
      wheel.current.time = event.timeStamp; wheel.current.amount += delta;
      if (Math.abs(wheel.current.amount) < 30) return;
      const step = Math.sign(wheel.current.amount);
      wheel.current.amount -= step * 30;
      const next = Math.max(0, Math.min(index + step, intervals));
      if (next === index) wheel.current.amount = 0;
      else select(next, true);
    };
    element.addEventListener("wheel", scroll, { passive: false });
    return () => element.removeEventListener("wheel", scroll);
  }, [disabled, index, intervals, value, onChange]);
  const point = (event: PointerEvent<HTMLDivElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect();
    const width = bounds.width - 40;
    if (width <= 0) return;
    select(Math.round(Math.max(0, Math.min(1, (event.clientX - bounds.left - 20) / width)) * intervals), true);
  };
  const keyboard = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Enter") { event.preventDefault(); onComplete(); return; }
    const composerNavigation = event.code === "ComposerNavigation";
    const step = event.key === "ArrowRight" || composerNavigation && event.key === "ArrowDown" ? 1
      : event.key === "ArrowLeft" || composerNavigation && event.key === "ArrowUp" ? -1 : 0;
    if (step) {
      event.preventDefault();
      select(index + step);
    }
  };
  const release = () => { dragging.current = null; movedRef.current = false; setPressed(false); setMoved(false); setHovered(false); };
  return <div ref={sliderRef} className="provider-model-power" role="slider" tabIndex={0} dir="ltr" aria-label="Power" aria-valuemin={0}
    aria-valuemax={intervals} aria-valuenow={index} aria-valuetext={({ medium: "Standard", high: "Extended" } as Record<string,string>)[value] || effortLabel(value)} aria-disabled={disabled || undefined}
    data-reasoning-slider data-selected-effort={value} data-maximum={maximum || undefined} data-dragging={moved || undefined}
    data-disabled={disabled || undefined} data-reduced-motion={reducedMotion || undefined} onKeyDown={keyboard}
    onPointerDown={(event) => {
      if (event.button !== 0 || disabled) return;
      if (dragging.current !== null) return;
      event.preventDefault(); event.stopPropagation(); event.currentTarget.focus({ preventScroll: true });
      dragging.current = event.pointerId; setPressed(true);
      event.currentTarget.setPointerCapture(event.pointerId);
      if (!(event.target instanceof Element && event.target.closest(".provider-model-power-thumb-position"))) point(event);
    }} onPointerMove={(event) => {
      if (dragging.current !== event.pointerId) return;
      event.stopPropagation();
      movedRef.current = true; setMoved(true);
      point(event);
    }} onPointerUp={(event) => {
      if (dragging.current !== event.pointerId) return;
      if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
      release();
    }} onPointerCancel={release} onLostPointerCapture={release}>
    <span className="provider-model-power-track" aria-hidden="true">
      <motion.span className="provider-model-power-fill" style={{ transform: fillPosition }}>
        <AnimatePresence initial={false}>{maximum ? <motion.span className="provider-power-field-position"
          key="maximum" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}
          transition={{ duration: reducedMotion ? 0 : .3, ease: [.23,1,.32,1] }} style={{ transform: fieldPosition }}>
          <PowerDecoration reducedMotion={reducedMotion} reveal={burst > 0} active={active} />
        </motion.span> : null}</AnimatePresence>
      </motion.span>
      {efforts.map((effort, position) => <span className="provider-model-power-tick" key={effort}
        data-filled={position <= index || undefined} style={{ left: "calc(" + (position / intervals * 100) + "% + " + (13 - position / intervals * 26) + "px)" }} />)}
    </span>
    <span className="provider-model-power-rail" aria-hidden="true">
      <motion.span className="provider-model-power-thumb-position" style={{ left: thumbPosition }}
        onPointerEnter={() => setHovered(true)} onPointerLeave={() => setHovered(false)}>
        {maximum && burst > 0 && !reducedMotion ? <PowerBurst key={burst} /> : null}
        <motion.span className="provider-model-power-thumb" initial={false} animate={{ scale: !disabled && !reducedMotion && (pressed || hovered) ? 32 / 28 : 1 }}
          transition={reducedMotion ? { duration: 0 } : pressed || hovered ? { type: "spring", stiffness: 420, damping: 38, mass: 1 } : { type: "spring", stiffness: 220, damping: 26, mass: 1 }} />
      </motion.span>
    </span>
  </div>;
}
