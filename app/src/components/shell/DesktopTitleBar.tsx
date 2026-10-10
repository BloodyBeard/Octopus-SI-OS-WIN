import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type PointerEvent, type ReactNode, type Ref } from "react";
import { AnimatePresence, motion, useMotionValueEvent, useTransform, type MotionValue } from "motion/react";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { clientDialogOwnsCommands, formatKeyboardShortcuts, useKeyboardShortcuts } from "../../keyboardShortcuts";
import type { OctopusClientAccessProfile } from "../../electron";
import { ShellTooltip } from "./ShellTooltip";

type Props = {
  accessProfile: OctopusClientAccessProfile;
  onPrimaryActionsHostChange?: (host: HTMLDivElement | null) => void;
  onPaneControlsHostChange?: (host: HTMLDivElement | null) => void;
  contentDisabled?: boolean;
  primaryPaneBlocked?: boolean;
  settingsOpen?: boolean;
  onCloseSettings?: () => void;
  canNavigateBack: boolean;
  canNavigateForward: boolean;
  sidebarOpen: boolean;
  sidebarAvailable?: boolean;
  secondaryPaneFocused: boolean;
  secondaryPaneOpen: boolean;
  secondaryPaneWidth: MotionValue<number>;
  surfaceActions?: TitlebarSurfaceAction[];
  surfaceContext?: TitlebarSurfaceContext;
  surfaceContextAccessory?: ReactNode;
  surfaceTitle?: string;
  sidebarToggleRef?: Ref<HTMLButtonElement>;
  onSurfaceTitleClick?: () => void;
  onSurfaceContextPointerDown?: (event: PointerEvent<HTMLSpanElement>) => void;
  onNavigateBack: () => void;
  onNavigateForward: () => void;
  onToggleSidebar: () => void;
  onToggleSecondaryPane: () => void;
  onSlotWidthsChange: (widths: { left: number; right: number }) => void;
};

export type TitlebarSurfaceAction = {
  actionId?: string;
  badge?: boolean;
  glyph: GlyphName;
  global?: boolean;
  label: string;
  onClick: () => void;
  pressed?: boolean;
};

export type TitlebarSurfaceContext = {
  glyph?: GlyphName;
  title: string;
};

export function DesktopTitleBar({
  accessProfile,
  onPrimaryActionsHostChange,
  onPaneControlsHostChange,
  contentDisabled = false,
  primaryPaneBlocked = false,
  settingsOpen = false,
  onCloseSettings,
  canNavigateBack,
  canNavigateForward,
  sidebarOpen,
  sidebarAvailable = true,
  secondaryPaneFocused,
  secondaryPaneOpen,
  secondaryPaneWidth,
  surfaceActions = [],
  surfaceContext,
  surfaceContextAccessory,
  surfaceTitle,
  sidebarToggleRef,
  onSurfaceTitleClick,
  onSurfaceContextPointerDown,
  onNavigateBack,
  onNavigateForward,
  onToggleSidebar,
  onToggleSecondaryPane,
  onSlotWidthsChange,
}: Props) {
  const shortcuts = useKeyboardShortcuts(accessProfile);
  const isElectron = Boolean(window.octopusTransport);
  const [fullScreen, setFullScreen] = useState(false);
  const [nativeInset, setNativeInset] = useState(isElectron ? 76 : 0);
  const [secondaryPaneCollapsed, setSecondaryPaneCollapsed] = useState(() => secondaryPaneWidth.get() < 1);
  const secondaryPaneCollapsedRef = useRef(secondaryPaneCollapsed);
  const controlsRef = useRef<HTMLDivElement>(null);
  const paneActionsRef = useRef<HTMLDivElement>(null);
  const paneActionsRight = useTransform(secondaryPaneWidth, (width) => width + 8);

  useMotionValueEvent(secondaryPaneWidth, "change", (width) => {
    const collapsed = width < 1;
    if (secondaryPaneCollapsedRef.current === collapsed) return;
    secondaryPaneCollapsedRef.current = collapsed;
    setSecondaryPaneCollapsed(collapsed);
  });

  useEffect(() => {
    const transport = window.octopusTransport;
    if (!transport) return;
    let cancelled = false;
    void transport.windowState().then((state) => {
      if (!cancelled) { setFullScreen(state.fullScreen); setNativeInset(state.nativeControlInset); }
    });
    const unsubscribe = transport.onWindowStateChanged((state) => { setFullScreen(state.fullScreen); setNativeInset(state.nativeControlInset); });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, []);

  useEffect(() => {
    const transport = window.octopusTransport;
    if (!transport) return;
    const blocked = clientDialogOwnsCommands();
    transport.setHistoryNavigationState({ canNavigateBack: !blocked && canNavigateBack, canNavigateForward: !blocked && canNavigateForward });
    const unsubscribeBack = transport.onNavigateBack(() => { if (!clientDialogOwnsCommands()) onNavigateBack(); });
    const unsubscribeForward = transport.onNavigateForward(() => { if (!clientDialogOwnsCommands()) onNavigateForward(); });
    return () => {
      unsubscribeBack();
      unsubscribeForward();
    };
  }, [canNavigateBack, canNavigateForward, onNavigateBack, onNavigateForward, settingsOpen]);

  useLayoutEffect(() => {
    const controls = controlsRef.current;
    const paneActions = paneActionsRef.current;
    if (!controls || !paneActions) return;

    const measure = () => {
      const controlsRect = controls.getBoundingClientRect();
      const paneActionsRect = paneActions.getBoundingClientRect();
      onSlotWidthsChange({
        left: Math.ceil(controlsRect.right),
        right: Math.ceil(paneActionsRect.width + 8),
      });
    };

    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(controls);
    observer.observe(paneActions);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [fullScreen, nativeInset, onSlotWidthsChange, secondaryPaneFocused, secondaryPaneOpen]);

  return (
    <header
      aria-label="Window navigation"
      className={`app-titlebar draggable${isElectron ? " is-electron" : ""}${fullScreen ? " is-fullscreen" : ""}`}
      style={{ "--app-titlebar-safe-left": `${(fullScreen ? 0 : nativeInset) + 6}px` } as CSSProperties}
    >
      <div aria-hidden="true" className="app-titlebar-drag-region" />
      <div className="app-titlebar-controls no-drag" ref={controlsRef}>
        <span className="app-titlebar-history">
          <TitlebarButton
            disabled={!canNavigateBack}
            glyph="navigateBack"
            label="Back"
            onClick={onNavigateBack}
            shortcut={formatKeyboardShortcuts(shortcuts.navigate_back ?? [])}
          />
          <TitlebarButton
            disabled={!canNavigateForward}
            glyph="navigateBack"
            glyphClassName="-scale-x-100"
            label="Forward"
            onClick={onNavigateForward}
            shortcut={formatKeyboardShortcuts(shortcuts.navigate_forward ?? [])}
          />
        </span>
        {!settingsOpen && sidebarAvailable ? <TitlebarButton
          buttonRef={sidebarToggleRef}
          glyph={sidebarOpen ? "sidebarOpen" : "sidebarClosed"}
          label={`${sidebarOpen ? "Hide" : "Show"} sidebar`}
          onClick={onToggleSidebar}
          shortcut={formatKeyboardShortcuts(shortcuts.toggle_sidebar ?? [])}
        /> : null}
      </div>
      {!settingsOpen && !primaryPaneBlocked && surfaceContext ? (
        <div className="app-titlebar-surface-context-slot">
          <span aria-hidden="true" className="app-titlebar-surface-context-divider" />
          <span
            className={`app-titlebar-surface-context${onSurfaceContextPointerDown ? " is-transferable no-drag" : ""}`}
            onPointerDown={onSurfaceContextPointerDown}
          >
            {surfaceContext.glyph ? <Glyph className="icon-xs shrink-0" name={surfaceContext.glyph} size={16} /> : null}
            <span title={surfaceContext.title}>{surfaceContext.title}</span>
            {surfaceContextAccessory}
          </span>
        </div>
      ) : null}
      <AnimatePresence initial={false}>
        {settingsOpen ? (
          <div className="app-titlebar-settings-title">
            <span aria-hidden="true" className="app-titlebar-surface-context-divider" />
            <Glyph className="icon-xs shrink-0" name="settings" size={16} />
            <span>Settings</span>
          </div>
        ) : !primaryPaneBlocked && surfaceTitle ? (
          <motion.div
            animate={{ opacity: 1 }}
            className="app-titlebar-surface-title-slot"
            exit={{ opacity: 0 }}
            initial={{ opacity: 0 }}
            key={surfaceTitle}
            transition={{ duration: 0.18, ease: [0.23, 1, 0.32, 1] }}
          >
            <button
              aria-label={onSurfaceTitleClick ? `Close ${surfaceTitle}` : surfaceTitle}
              className="app-titlebar-surface-title no-drag"
              onClick={onSurfaceTitleClick}
              type="button"
            >
              {onSurfaceTitleClick ? <Glyph className="icon-2xs" name="chevronUp" size={12} /> : null}
              <span>{surfaceTitle}</span>
            </button>
          </motion.div>
        ) : null}
      </AnimatePresence>
      <motion.div
        className="app-titlebar-pane-actions no-drag"
        data-pane-role="primary"
        hidden={settingsOpen || primaryPaneBlocked}
        inert={contentDisabled || settingsOpen || primaryPaneBlocked}
        ref={paneActionsRef}
        style={{ right: paneActionsRight }}
      >
        <div className="workspace-pane-header-actions no-drag" ref={onPrimaryActionsHostChange} />
        {(settingsOpen ? [] : surfaceActions).map((surfaceAction) => (
          <TitlebarButton
            actionId={surfaceAction.actionId}
            badge={surfaceAction.badge}
            glyph={surfaceAction.glyph}
            key={surfaceAction.label}
            label={surfaceAction.label}
            onClick={surfaceAction.onClick}
            open={surfaceAction.pressed}
            surfaceAction
          />
        ))}
        {!settingsOpen && !secondaryPaneOpen && secondaryPaneCollapsed ? (
          <TitlebarButton
            glyph="splitPane"
            label="Open secondary pane"
            onClick={onToggleSecondaryPane}
            shortcut={formatKeyboardShortcuts(shortcuts.toggle_split ?? [])}
          />
        ) : null}
      </motion.div>
      {settingsOpen ? (
        <div className="app-titlebar-secondary-controls no-drag">
          <ShellTooltip label="Close Settings"><button aria-label="Close Settings" className="app-titlebar-action no-drag" id="settings-window-close" onClick={onCloseSettings} type="button"><Glyph name="close" size={16} /></button></ShellTooltip>
        </div>
      ) : secondaryPaneOpen || !secondaryPaneCollapsed ? <motion.div
        className="app-titlebar-secondary-pane"
        aria-hidden={!secondaryPaneOpen || undefined}
        inert={contentDisabled || !secondaryPaneOpen}
        ref={onPaneControlsHostChange}
        style={{ width: secondaryPaneWidth }}
      /> : null}
    </header>
  );
}

function TitlebarButton({
  actionId,
  badge = false,
  buttonRef,
  disabled = false,
  glyph,
  glyphClassName,
  label,
  onClick,
  open = false,
  surfaceAction = false,
  shortcut,
}: {
  actionId?: string;
  badge?: boolean;
  buttonRef?: Ref<HTMLButtonElement>;
  disabled?: boolean;
  glyph: GlyphName;
  glyphClassName?: string;
  label: string;
  onClick: () => void;
  open?: boolean;
  surfaceAction?: boolean;
  shortcut?: string;
}) {
  return (
    <ShellTooltip label={label} shortcut={shortcut} disabled={disabled}>
      <button
        aria-label={label}
        className="app-titlebar-action no-drag"
        data-state={open ? "open" : "closed"}
        data-titlebar-action={actionId}
        data-titlebar-surface-action={surfaceAction || undefined}
        disabled={disabled}
        onClick={(event) => {
          event.stopPropagation();
          onClick();
        }}
        onPointerDown={(event) => event.stopPropagation()}
        ref={buttonRef}
        type="button"
      >
        <Glyph className={`icon-xs${glyphClassName ? ` ${glyphClassName}` : ""}`} name={glyph} size={16} />
        {badge ? <i aria-hidden="true" className="app-titlebar-action-badge" /> : null}
      </button>
    </ShellTooltip>
  );
}
