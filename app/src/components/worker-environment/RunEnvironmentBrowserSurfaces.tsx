import { createElement, useEffect, useMemo, useRef, useState, type ButtonHTMLAttributes, type ReactNode, type PointerEvent as ReactPointerEvent } from "react";
import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import type {
  OctopusClientContextMenuItem,
  OctopusCdpViewerActionAccepted,
  OctopusCdpViewerActionRequest,
  OctopusCdpViewerFrameMetadata,
  OctopusCdpViewerState,
  OctopusNativeBrowserViewerActionRequest,
  OctopusNativeBrowserViewerIdentity,
  OctopusNativeBrowserViewerSyncRequest,
  OctopusNativeBrowserViewerState,
} from "../../electron";
import { formatProjectionError } from "../../projectionClient";
import { OcButton, OcIconButton, OcMenuItem, OcMenuSurface } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { OctopusEmptyState } from "../OctopusSurface";
import { MessageTooltip } from "../chat/MessageTooltip";
import "../shell/shellTooltip.css";
import {
  LiveSurfaceEmpty,
  ManualRoomControlWarning,
  dataText,
  isViewerUrl,
} from "./RunEnvironmentSurfacePrimitives";
import "./browser-status.css";
import { browserImagePoint } from "./browserCoordinates.mjs";
import { createBrowserCursor } from "./browser-cursor";
import browserCursorAsset from "../../assets/browser-cursor.png";
import { BrowserDownloads } from "./BrowserDownloads";

type CdpBrowserFrame = { data: string; metadata: OctopusCdpViewerFrameMetadata; mimeType: "image/jpeg" };

export function BrowserSurface({ active, environment, surface }: { active: boolean; environment: WorkerEnvironmentProjection; surface?: WorkerEnvironmentSurfaceProjection }) {
  if (surface?.mode === "direct" && surface.protocol === "browser.native.downloads.v1") {
    return <BrowserDownloads active={active} environment={environment} key={JSON.stringify([environment.id, surface.id, surface.data.host_generation, surface.data.formation_pin])} surface={surface} standalone />;
  }
  if (surface?.protocol === "browser.native.v1") {
    if (surface.mode === "evidence" || surface.status === "unavailable") {
      return <UnavailableNativeBrowserSurface environment={environment} surface={surface} />;
    }
    if (surface.mode === "direct") {
      const identity = JSON.stringify([environment.id, environment.runId, surface.id, surface.data.host_generation, surface.data.target_id, surface.data.formation_pin]);
      return <NativeBrowserSurface active={active} environment={environment} key={identity} surface={surface} />;
    }
  }
  return <ProjectedBrowserSurface active={active} environment={environment} surface={surface} />;
}

function BrowserToolbarButton({ glyph, label, reverse, className = "", ...props }: ButtonHTMLAttributes<HTMLButtonElement> & {
  glyph: GlyphName;
  label: string;
  reverse?: boolean;
}) {
  return <MessageTooltip delayMs={200} label={label} side="top" variant="shell">
    <OcIconButton {...props} appearance="compact" label={label} glyph={glyph} title="" className={`we-browser-control${reverse ? " is-reversed" : ""} ${className}`} />
  </MessageTooltip>;
}

function BrowserConnectionState({ title, description, children, onRetry }: {
  title: string;
  description: string;
  children?: ReactNode;
  onRetry?: () => void;
}) {
  return <section className="we-browser-connection-state">
    <div className="we-browser-connection-copy" role="status">
      <Glyph name="browser" size={32} />
      <h2>{title}</h2>
      <p>{description}</p>
    </div>
    {onRetry ? <OcButton onClick={onRetry} size="sm" variant="secondary">Try again</OcButton> : null}
    {children ? <details className="we-browser-details"><summary>Technical details</summary>{children}</details> : null}
  </section>;
}

function UnavailableNativeBrowserSurface({ environment, surface }: { environment: WorkerEnvironmentProjection; surface: WorkerEnvironmentSurfaceProjection }) {
  const previousSession = dataText(surface.data, "reason_code") === "host_unavailable";
  const lastAddress = dataText(surface.data, "last_known_url");
  const lastTitle = dataText(surface.data, "last_known_title");
  return <div className="we-browser-shell is-native is-unavailable">
    <div className="we-browser-toolbar" role="toolbar" aria-label="Browser navigation">
      <div className="we-browser-history" role="group" aria-label="Navigation">
        <BrowserToolbarButton disabled glyph="navigateBack" label="Back" />
        <BrowserToolbarButton disabled glyph="navigateBack" reverse label="Forward" />
        <span className="we-browser-navigation-divider" aria-hidden="true" />
        <BrowserToolbarButton disabled glyph="refresh" label="Reload page" />
      </div>
      <div className="we-browser-address-slot"><div className="we-browser-address">
        <input aria-label="Last known browser address" readOnly placeholder="Enter a URL" value={lastAddress} onFocus={(event) => event.currentTarget.select()} />
      </div></div>
      <div className="we-browser-actions"><BrowserToolbarButton disabled glyph="download" label="Downloads" /></div>
      <div className="we-browser-actions"><BrowserToolbarButton disabled glyph="more" label="More browser actions" /></div>
    </div>
    <div className="we-browser-body">
      <BrowserConnectionState title={previousSession ? "Browser session disconnected" : "Browser unavailable"}
        description={previousSession
          ? "This page belonged to a previous browser session that is no longer connected. Ask Octopus to open a new browser page in this task to continue."
          : "This browser page is unavailable. Ask Octopus to open a new browser page in this task to continue."}>
        <dl>
          {lastTitle ? <><dt>Last page</dt><dd>{lastTitle}</dd></> : null}
          {lastAddress ? <><dt>Last address</dt><dd>{lastAddress}</dd></> : null}
          <dt>Reason</dt><dd>{dataText(surface.data, "detail") || "No live browser connection is available."}</dd>
          <dt>Task</dt><dd>{environment.runId}</dd><dt>Room</dt><dd>{environment.id}</dd>
        </dl>
      </BrowserConnectionState>
    </div>
  </div>;
}

function ProjectedBrowserSurface({ active, environment, surface }: { active: boolean; environment: WorkerEnvironmentProjection; surface?: WorkerEnvironmentSurfaceProjection }) {
  const fixturePages = useMemo(() => browserFixturePages(surface), [surface]);
  const [fixtureHistory, setFixtureHistory] = useState<BrowserFixturePage[]>(fixturePages.length ? [fixturePages[0]] : []);
  const [fixtureCursor, setFixtureCursor] = useState(0);
  const [addressDraft, setAddressDraft] = useState(fixturePages[0]?.url ?? "");
  const [reloadKey, setReloadKey] = useState(0);
  const [fixtureLoading, setFixtureLoading] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [attachedCdpState, setCdpState] = useState<OctopusCdpViewerState | null>(null);
  const [cdpFrame, setCdpFrame] = useState<CdpBrowserFrame | null>(null);
  const [framePainted, setFramePainted] = useState(false);
  const [cdpError, setCdpError] = useState("");
  const [cdpOpening, setCdpOpening] = useState(false);
  const [cdpGeneration, setCdpGeneration] = useState(0);
  const [manualControlEnabled, setManualControlEnabled] = useState(false);
  const [manualControlWarningOpen, setManualControlWarningOpen] = useState(false);
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findFound, setFindFound] = useState<boolean | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const addressInputRef = useRef<HTMLInputElement>(null);
  const findInputRef = useRef<HTMLInputElement>(null);
  const fixtureLoadTimerRef = useRef<number | null>(null);
  const cdpSessionIdRef = useRef("");
  const activeRef = useRef(active);
  activeRef.current = active;
  const addressEditingRef = useRef(false);
  const manualControlEnabledRef = useRef(false);
  const pendingManualActionRef = useRef<OctopusCdpViewerActionRequest | null>(null);
  const pendingDirectReloadRef = useRef(false);
  const fixtureInteractive = surface?.mode === "fixture";
  const evidencePage = surface?.mode === "evidence" ? browserPageFromSurface(surface) : null;
  const fixturePage = fixtureHistory[fixtureCursor] ?? fixturePages[0] ?? browserPageFromSurface(surface);
  const protocol = surface?.protocol.trim().toLowerCase() ?? "";
  const requiresNativeAttach = surface?.mode === "direct" && protocol === "cdp";
  const cdpTransportAvailable = requiresNativeAttach
    && Boolean(window.octopusTransport?.openCdpViewer)
    && Boolean(window.octopusTransport?.cdpViewerAction)
    && Boolean(window.octopusTransport?.closeCdpViewer)
    && Boolean(window.octopusTransport?.onCdpViewerEvent);
  const projectedPageUrl = surface ? dataText(surface.data, "url") : "";
  const projectedTargetId = surface ? dataText(surface.data, "target_id") : "";
  const cdpState = requiresNativeAttach
    && environment.lifecycle === "active"
    && attachedCdpState?.workerEnvironmentId === environment.id
    && attachedCdpState.runId === environment.runId
    && attachedCdpState.surfaceId === surface?.id
    && attachedCdpState.targetId === projectedTargetId
    ? attachedCdpState : null;
  const address = fixtureInteractive
    ? addressDraft
    : requiresNativeAttach
      ? addressDraft
    : surface?.mode === "evidence"
      ? evidencePage?.url ?? ""
      : projectedPageUrl || surface?.viewerUrl || "";
  const directViewer = surface?.mode === "direct"
    && !requiresNativeAttach
    && isViewerUrl(surface.viewerUrl);
  const externalAddress = isViewerUrl(cdpState?.url || projectedPageUrl)
    ? cdpState?.url || projectedPageUrl
    : directViewer
      ? surface?.viewerUrl ?? ""
      : "";
  const loadError = cdpState?.navigationError || (surface ? dataText(surface.data, "load_error") : "");
  const readOnly = surface?.readOnly === true || cdpState?.readOnly === true;
  const activity = browserActivity(surface);
  const tabs = browserTabs(surface);
  const loading = fixtureLoading || cdpOpening || cdpState?.loading === true || surface?.data.loading === true;
  const manualControlAvailable = !readOnly
    && (directViewer || (requiresNativeAttach && cdpState?.connected === true && framePainted));

  useEffect(() => {
    const pages = browserFixturePages(surface);
    setFixtureHistory(pages.length ? [pages[0]] : []);
    setFixtureCursor(0);
    setAddressDraft(pages[0]?.url ?? "");
    setReloadKey(0);
    setFixtureLoading(false);
  }, [surface?.id]);

  useEffect(() => {
    manualControlEnabledRef.current = false;
    setManualControlEnabled(false);
    setManualControlWarningOpen(false);
    pendingManualActionRef.current = null;
    pendingDirectReloadRef.current = false;
    setFindOpen(false);
    setFindQuery("");
    setFindFound(null);
  }, [environment.id, surface?.id]);

  useEffect(() => {
    if (!findOpen) return;
    window.requestAnimationFrame(() => {
      findInputRef.current?.focus();
      findInputRef.current?.select();
    });
  }, [findOpen]);

  useEffect(() => () => {
    if (fixtureLoadTimerRef.current !== null) window.clearTimeout(fixtureLoadTimerRef.current);
  }, []);

  useEffect(() => {
    cdpSessionIdRef.current = "";
    setCdpState(null);
    setCdpFrame(null);
    setFramePainted(false);
    setCdpError("");
    setCdpOpening(false);
    setManualControlWarningOpen(false);
    pendingManualActionRef.current = null;
    addressEditingRef.current = false;
    manualControlEnabledRef.current = false;
    setManualControlEnabled(false);
    if (!requiresNativeAttach) return;
    setAddressDraft(projectedPageUrl);
    if (environment.lifecycle !== "active") {
      setCdpError("This browser room is no longer active.");
      return;
    }
    const transport = window.octopusTransport;
    if (!cdpTransportAvailable || !transport) {
      setCdpError("Native CDP viewing is available only in the Octopus desktop client.");
      return;
    }
    if (!projectedTargetId) {
      setCdpError("This room has not projected an exact browser target.");
      return;
    }
    const attachmentId = crypto.randomUUID();
    let released = false;
    cdpSessionIdRef.current = attachmentId;
    setCdpOpening(true);
    const acceptState = (state: OctopusCdpViewerState) => {
      if (released || cdpSessionIdRef.current !== attachmentId || state.sessionId !== attachmentId) return;
      setCdpState(state);
      manualControlEnabledRef.current = state.manualControl && activeRef.current;
      setManualControlEnabled(state.manualControl && activeRef.current);
      if (!addressEditingRef.current) setAddressDraft(state.url);
    };
    const unsubscribe = transport.onCdpViewerEvent((event) => {
      if (released || event.sessionId !== attachmentId || cdpSessionIdRef.current !== attachmentId) return;
      if (event.type === "state") acceptState(event.state);
      else if (event.type === "frame") setCdpFrame({ data: event.data, metadata: event.metadata, mimeType: event.mimeType });
      else if (event.type === "error") {
        cdpSessionIdRef.current = "";
        setCdpError(event.error);
        setCdpState(null);
        setCdpFrame(null);
        setFramePainted(false);
        manualControlEnabledRef.current = false;
        setManualControlEnabled(false);
      }
    });
    void transport.openCdpViewer({ attachmentId, visible: activeRef.current, workerEnvironmentId: environment.id, surfaceId: surface?.id ?? "" })
      .then((state) => {
        if (released) return;
        acceptState(state);
        if (state.visible !== activeRef.current) void runCdpAction({ sessionId: attachmentId, action: "setVisible", visible: activeRef.current });
      })
      .catch((failure) => {
        if (released) return;
        cdpSessionIdRef.current = "";
        setCdpError(formatProjectionError(failure));
      })
      .finally(() => { if (!released) setCdpOpening(false); });
    return () => {
      released = true;
      unsubscribe();
      if (cdpSessionIdRef.current === attachmentId) cdpSessionIdRef.current = "";
      void transport.closeCdpViewer({ sessionId: attachmentId }).catch(() => {});
    };
  }, [cdpGeneration, cdpTransportAvailable, environment.id, environment.runId, environment.lifecycle, projectedTargetId, requiresNativeAttach, surface?.id, surface?.viewerUrl]);

  useEffect(() => {
    const sessionId = cdpSessionIdRef.current;
    if (sessionId && cdpState?.connected) void runCdpAction({ sessionId, action: "setVisible", visible: active });
    if (!active) {
      manualControlEnabledRef.current = false;
      setManualControlEnabled(false);
      setManualControlWarningOpen(false);
      pendingManualActionRef.current = null;
    }
  }, [active, cdpState?.sessionId]);

  useEffect(() => {
    if (active) return;
    setMenuOpen(false);
    setFindOpen(false);
  }, [active]);

  useEffect(() => {
    if (!menuOpen) return;
    const dismiss = (event: globalThis.PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("pointerdown", dismiss);
    window.addEventListener("keydown", escape);
    return () => {
      window.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("keydown", escape);
    };
  }, [menuOpen]);

  function navigateBrowser() {
    if (!fixtureInteractive) {
      const sessionId = cdpSessionIdRef.current;
      if (!sessionId || !window.octopusTransport?.cdpViewerAction) return;
      const requested = addressDraft.trim();
      if (!requested) return;
      void runManualCdpAction(requested === cdpState?.url
        ? { sessionId, action: "reload" }
        : { sessionId, action: "navigate", url: requested });
      return;
    }
    const requested = addressDraft.trim();
    if (!requested) return;
    const page = fixturePages.find((candidate) => candidate.url === requested) ?? {
      url: requested,
      title: "Unavailable Test page",
      heading: "Page is not in this Test fixture",
      body: "The isolated browser does not contact the network. Add this URL to the projected fixture descriptor to simulate it.",
    };
    setFixtureHistory((current) => [...current.slice(0, fixtureCursor + 1), page]);
    setFixtureCursor((current) => current + 1);
    setAddressDraft(page.url);
    beginFixtureLoad();
  }

  function moveBrowserHistory(delta: number) {
    if (!fixtureInteractive) {
      const sessionId = cdpSessionIdRef.current;
      if (!sessionId || !window.octopusTransport?.cdpViewerAction) return;
      void runManualCdpAction({ sessionId, action: delta < 0 ? "back" : "forward" });
      return;
    }
    setFixtureCursor((current) => {
      const next = Math.max(0, Math.min(fixtureHistory.length - 1, current + delta));
      setAddressDraft(fixtureHistory[next]?.url ?? "");
      beginFixtureLoad();
      return next;
    });
  }

  function beginFixtureLoad() {
    if (!fixtureInteractive) return;
    if (fixtureLoadTimerRef.current !== null) window.clearTimeout(fixtureLoadTimerRef.current);
    setFixtureLoading(true);
    fixtureLoadTimerRef.current = window.setTimeout(() => {
      fixtureLoadTimerRef.current = null;
      setFixtureLoading(false);
    }, 220);
  }

  function reloadBrowser() {
    if (requiresNativeAttach) {
      const sessionId = cdpSessionIdRef.current;
      if (sessionId && cdpState?.connected && window.octopusTransport?.cdpViewerAction) {
        void runManualCdpAction({ sessionId, action: "reload" });
      } else {
        setCdpGeneration((current) => current + 1);
      }
      return;
    }
    if (directViewer && surface?.readOnly) return;
    if (directViewer && !manualControlEnabledRef.current) {
      pendingDirectReloadRef.current = true;
      setManualControlWarningOpen(true);
      return;
    }
    setReloadKey((current) => current + 1);
    beginFixtureLoad();
  }

  async function runCdpAction(request: OctopusCdpViewerActionRequest): Promise<OctopusCdpViewerState | OctopusCdpViewerActionAccepted | null> {
    const transport = window.octopusTransport;
    if (!transport?.cdpViewerAction) return null;
    try {
      const result = await transport.cdpViewerAction(request);
      if (cdpSessionIdRef.current !== request.sessionId) return null;
      if ("connected" in result) {
        setCdpState(result);
        manualControlEnabledRef.current = result.manualControl && activeRef.current;
        setManualControlEnabled(result.manualControl && activeRef.current);
        if (!addressEditingRef.current) setAddressDraft(result.url);
      }
      if ("found" in result) setFindFound(result.found ?? null);
      setCdpError("");
      return result;
    } catch (failure) {
      if (cdpSessionIdRef.current !== request.sessionId) return null;
      setCdpError(formatProjectionError(failure));
      return null;
    }
  }

  async function runManualCdpAction(request: OctopusCdpViewerActionRequest): Promise<OctopusCdpViewerState | OctopusCdpViewerActionAccepted | null> {
    if (readOnly || !active) return null;
    if (!manualControlEnabledRef.current) {
      if (!(request.action === "mouse" && request.eventType === "mouseMoved")) {
        pendingManualActionRef.current = request;
        setManualControlWarningOpen(true);
      }
      return null;
    }
    return runCdpAction(request);
  }

  async function enableManualControl() {
    if (readOnly || !active) return;
    if (requiresNativeAttach) {
      const result = await runCdpAction({ sessionId: cdpSessionIdRef.current, action: "claimControl" });
      if (!result || !("manualControl" in result) || !result.manualControl) {
        setManualControlWarningOpen(false);
        pendingManualActionRef.current = null;
        return;
      }
      if (!activeRef.current) {
        void runCdpAction({ sessionId: result.sessionId, action: "releaseControl" });
        return;
      }
    }
    manualControlEnabledRef.current = true;
    setManualControlEnabled(true);
    setManualControlWarningOpen(false);
    const pendingAction = pendingManualActionRef.current;
    pendingManualActionRef.current = null;
    if (pendingAction) void runCdpAction(pendingAction);
    if (pendingDirectReloadRef.current) {
      pendingDirectReloadRef.current = false;
      setReloadKey((current) => current + 1);
    }
  }

  function releaseManualControl() {
    manualControlEnabledRef.current = false;
    setManualControlEnabled(false);
    pendingManualActionRef.current = null;
    if (requiresNativeAttach) void runCdpAction({ sessionId: cdpSessionIdRef.current, action: "releaseControl" });
  }

  function cancelManualControl() {
    pendingManualActionRef.current = null;
    pendingDirectReloadRef.current = false;
    setManualControlWarningOpen(false);
  }

  function openBrowserFind() {
    setMenuOpen(false);
    setFindOpen(true);
    setFindFound(null);
  }

  function submitBrowserFind(backwards = false) {
    const sessionId = cdpSessionIdRef.current;
    if (!sessionId || !findQuery.trim()) return;
    void runManualCdpAction({ sessionId, action: "find", query: findQuery.trim(), backwards });
  }

  function setBrowserZoom(factor: number) {
    const sessionId = cdpSessionIdRef.current;
    if (!sessionId) return;
    void runManualCdpAction({ sessionId, action: "zoom", factor: Math.max(0.5, Math.min(2.5, factor)) });
  }

  return (
    <div
      className="we-browser-shell"
      onKeyDownCapture={(event) => {
        if (!(event.metaKey || event.ctrlKey) || event.altKey || !active) return;
        const key = event.key.toLowerCase();
        if (key === "l") {
          event.preventDefault();
          event.stopPropagation();
          addressInputRef.current?.focus();
          addressInputRef.current?.select();
          return;
        }
        if (key !== "f" || !cdpState?.connected || readOnly) return;
        event.preventDefault();
        event.stopPropagation();
        openBrowserFind();
      }}
    >
      <div className="we-browser-toolbar" role="toolbar" aria-label="Browser navigation">
        <div className="we-browser-history" role="group" aria-label="Navigation">
          <BrowserToolbarButton
            label="Back"
            glyph="navigateBack"
            disabled={fixtureInteractive ? fixtureCursor === 0 : readOnly || !cdpState?.canGoBack}
            onClick={() => moveBrowserHistory(-1)}
          />
          <BrowserToolbarButton
            label="Forward"
            glyph="navigateBack"
            reverse
            disabled={fixtureInteractive ? fixtureCursor >= fixtureHistory.length - 1 : readOnly || !cdpState?.canGoForward}
            onClick={() => moveBrowserHistory(1)}
          />
          <span className="we-browser-navigation-divider" aria-hidden="true" />
          <BrowserToolbarButton label={loading && cdpState?.connected ? "Stop loading page" : "Reload page"} glyph={loading && cdpState?.connected ? "close" : "refresh"} disabled={!fixtureInteractive && (readOnly || !(directViewer || cdpState?.connected))} onClick={() => {
            if (loading && cdpState?.connected) void runManualCdpAction({ sessionId: cdpState.sessionId, action: "stop" });
            else reloadBrowser();
          }} />
        </div>
        <div className="we-browser-address-slot"><div className="we-browser-address">
          <input
            aria-label="Browser address"
            autoCapitalize="none"
            autoComplete="off"
            readOnly={!fixtureInteractive && (readOnly || !cdpState?.connected)}
            onChange={(event) => setAddressDraft(event.currentTarget.value)}
            onFocus={(event) => { addressEditingRef.current = true; event.currentTarget.select(); }}
            onBlur={() => {
              addressEditingRef.current = false;
              if (requiresNativeAttach) setAddressDraft(cdpState?.url || projectedPageUrl);
            }}
            onKeyDown={(event) => {
              if (event.nativeEvent.isComposing) return;
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                setAddressDraft(fixtureInteractive ? fixturePage.url : cdpState?.url || projectedPageUrl);
                event.currentTarget.blur();
                return;
              }
              if (event.key !== "Enter") return;
              event.preventDefault();
              navigateBrowser();
              event.currentTarget.blur();
            }}
            placeholder="Enter a URL"
            ref={addressInputRef}
            spellCheck={false}
            type="text"
            value={address}
          />
          {externalAddress ? <BrowserToolbarButton
            label="Open in default browser"
            glyph="openExternal"
            onClick={() => window.open(externalAddress, "_blank", "noopener,noreferrer")}
          /> : null}
        </div></div>
        <div className="we-browser-actions" ref={menuRef}>
          <BrowserToolbarButton aria-expanded={menuOpen} aria-haspopup="menu" label="More browser actions" glyph="more" onClick={() => setMenuOpen((current) => !current)} />
          {menuOpen ? (
            <OcMenuSurface aria-label="Browser actions" className="we-browser-menu" role="menu">
              <OcMenuItem
                disabled={readOnly || !cdpState?.connected}
                label="Find in page"
                onClick={openBrowserFind}
                role="menuitem"
                trailing={<span>⌘F</span>}
              />
              <div aria-hidden="true" className="mx-1 my-1 h-px bg-token-border" />
              <div aria-label="Zoom" className="flex items-center gap-1 rounded-lg px-[var(--padding-row-x)] py-0.5 text-sm" role="group">
                <span className="min-w-0 flex-1 truncate">Zoom</span>
                <div className="flex shrink-0 items-center overflow-hidden rounded-md border border-token-border bg-token-foreground/5 text-xs">
                  <button
                    aria-label="Zoom out"
                    className="flex h-6 w-6 cursor-interaction items-center justify-center border-0 bg-transparent text-token-description-foreground outline-none hover:bg-token-list-hover-background disabled:cursor-not-allowed disabled:opacity-40"
                    disabled={readOnly || !cdpState?.connected || (cdpState.zoom ?? 1) <= 0.5}
                    onClick={() => setBrowserZoom((cdpState?.zoom ?? 1) - 0.1)}
                    type="button"
                  >−</button>
                  <div className="w-11 border-x border-token-border py-0.5 text-center tabular-nums">{Math.round((cdpState?.zoom ?? 1) * 100)}%</div>
                  <button
                    aria-label="Zoom in"
                    className="flex h-6 w-6 cursor-interaction items-center justify-center border-0 bg-transparent text-token-description-foreground outline-none hover:bg-token-list-hover-background disabled:cursor-not-allowed disabled:opacity-40"
                    disabled={readOnly || !cdpState?.connected || (cdpState.zoom ?? 1) >= 2.5}
                    onClick={() => setBrowserZoom((cdpState?.zoom ?? 1) + 0.1)}
                    type="button"
                  >+</button>
                </div>
                <button
                  aria-label="Reset"
                  className="flex h-6 w-6 shrink-0 cursor-interaction items-center justify-center rounded-md border-0 bg-transparent text-token-description-foreground outline-none hover:bg-token-list-hover-background disabled:cursor-not-allowed disabled:opacity-40"
                  disabled={readOnly || !cdpState?.connected || Math.abs((cdpState.zoom ?? 1) - 1) < 0.01}
                  onClick={() => setBrowserZoom(1)}
                  type="button"
                ><Glyph name="refresh" size={13} /></button>
              </div>
            </OcMenuSurface>
          ) : null}
        </div>
        <span aria-hidden="true" className={`we-browser-loading${loading ? " is-visible" : ""}`}><i /></span>
      </div>
      <div className="we-browser-status" role="status">
        <span className={activity?.status === "running" ? "we-browser-tool-active" : ""}>
          <Glyph name="browser" size={13} />
          {fixtureInteractive ? "Test fixture" : manualControlEnabled ? "Manual input enabled" : cdpOpening ? "Connecting to browser" : activity?.status === "running" ? "Browser tool running" : cdpState?.connected ? framePainted ? "Live browser" : "Waiting for browser frame" : evidencePage ? "Browser evidence" : "Browser not connected"}
        </span>
        {manualControlEnabled ? <button onClick={releaseManualControl} type="button">Release manual input</button> : null}
        <details>
          <summary>Details</summary>
          <dl>
            <dt>Task</dt><dd>{environment.runId}</dd>
            <dt>Room</dt><dd>{environment.id}</dd>
            <dt>Target</dt><dd>{cdpState?.targetId || projectedTargetId || "Not projected"}</dd>
            <dt>Input</dt><dd>{manualControlEnabled ? "Your actions are outside the artifact log. Worker tools remain independent." : readOnly ? "Read-only viewer" : cdpState?.manualControllerPresent ? "Manual input belongs to another viewer" : "Watching; manual input is disabled"}</dd>
            {activity ? <><dt>Tool</dt><dd>{activity.tool} · {activity.status} · movement {activity.sequence}</dd></> : null}
            {tabs.length ? <><dt>Observed tabs</dt><dd><ul>{tabs.map((tab) => <li key={tab.id} aria-current={tab.id === projectedTargetId ? "page" : undefined}>{tab.title || tab.url || tab.id}{tab.id === projectedTargetId ? " · room target" : ""}</li>)}</ul></dd></> : null}
          </dl>
        </details>
      </div>
      {cdpError ? <div className="we-browser-control-error" role="alert"><span>{cdpError}</span>{!cdpState?.connected && cdpTransportAvailable && projectedTargetId && environment.lifecycle === "active" ? <button disabled={cdpOpening} onClick={() => setCdpGeneration((current) => current + 1)} type="button">Reconnect</button> : cdpState?.connected ? <button onClick={() => setCdpError("")} type="button">Dismiss</button> : null}</div> : null}
      <div className="we-browser-body">
        {findOpen ? (
          <form
            className="we-browser-find"
            onSubmit={(event) => {
              event.preventDefault();
              submitBrowserFind(false);
            }}
          >
            <Glyph name="search" size={14} />
            <input
              aria-label="Find in page"
              onChange={(event) => {
                setFindQuery(event.currentTarget.value);
                setFindFound(null);
              }}
              onKeyDown={(event) => {
                if (event.key !== "Escape") return;
                event.preventDefault();
                setFindOpen(false);
              }}
              placeholder="Find"
              ref={findInputRef}
              value={findQuery}
            />
            <span>{findFound === false ? "No match" : findFound === true ? "Match" : ""}</span>
            <button aria-label="Previous match" disabled={!findQuery.trim()} onClick={() => submitBrowserFind(true)} type="button"><Glyph name="chevronUp" size={14} /></button>
            <button aria-label="Next match" disabled={!findQuery.trim()} onClick={() => submitBrowserFind(false)} type="button"><Glyph name="chevronDown" size={14} /></button>
            <button aria-label="Close find" onClick={() => setFindOpen(false)} type="button"><Glyph name="close" size={13} /></button>
          </form>
        ) : null}
        {loadError ? (
          <BrowserLoadError detail={loadError} onReload={fixtureInteractive || directViewer || requiresNativeAttach ? reloadBrowser : null} url={address} />
        ) : fixtureInteractive ? (
          <FixtureBrowserDocument key={`${fixturePage.url}:${reloadKey}`} page={fixturePage} />
        ) : evidencePage ? (
          <FixtureBrowserDocument page={evidencePage} />
        ) : directViewer ? (
          <iframe
            key={`${surface.viewerUrl}:${reloadKey}`}
            allow="clipboard-read; clipboard-write"
            referrerPolicy="no-referrer"
            sandbox="allow-forms allow-modals allow-popups allow-same-origin allow-scripts"
            src={surface.viewerUrl}
            style={{ pointerEvents: manualControlEnabled && !surface.readOnly ? "auto" : "none" }}
            tabIndex={manualControlEnabled && !surface.readOnly ? 0 : -1}
            title={surface.label}
          />
        ) : requiresNativeAttach && cdpTransportAvailable && cdpState?.connected ? (
          <CdpBrowserViewer frame={cdpFrame} interactionEnabled={manualControlEnabled && !readOnly && active} onAction={runManualCdpAction} onPainted={setFramePainted} sessionId={cdpState.sessionId} />
        ) : requiresNativeAttach && cdpTransportAvailable && cdpOpening ? (
          <LiveSurfaceEmpty glyph="browser" title="Connecting to browser" detail="Attaching to the exact CDP target projected by this Worker Environment…" />
        ) : (
          <LiveSurfaceEmpty
            glyph="browser"
            title={requiresNativeAttach
              ? "Browser attach is unavailable"
              : environment.session
                ? "Direct browser view is not advertised"
                : "No live browser session"}
            detail={requiresNativeAttach
              ? "The projected CDP target could not be attached in this client."
              : environment.session
                ? "This room projects a browser session, but no supported viewer URL."
                : "A live page appears here when this room opens a browser session."}
          />
        )}
        {manualControlAvailable && !manualControlEnabled && active ? (
          <button
            aria-label="Enable manual browser control"
            className="we-browser-manual-gate"
            onClick={() => setManualControlWarningOpen(true)}
            type="button"
          >
            <span><Glyph name="browser" size={14} />Enable manual control</span>
          </button>
        ) : null}
      </div>
      <ManualRoomControlWarning
        glyph="browser"
        onCancel={cancelManualControl}
        onContinue={enableManualControl}
        open={manualControlWarningOpen}
        title="Manual browser control"
      />
    </div>
  );
}


type WithoutAttachment<T> = T extends unknown ? Omit<T, keyof OctopusNativeBrowserViewerIdentity> : never;
type NativeBrowserAction = WithoutAttachment<OctopusNativeBrowserViewerActionRequest>;

function normalizeNativeBrowserAddress(value: string) {
  const input = value.trim();
  if (!input) return "";
  if (input.toLowerCase() === "about:blank") return "about:blank";
  const explicit = /^[a-z][a-z0-9+.-]*:\/\//i.test(input);
  const parsed = new URL(explicit ? input : `https://${input}`);
  if (!["http:", "https:"].includes(parsed.protocol)) throw new Error("Enter an HTTP or HTTPS address, or about:blank.");
  if (!explicit) {
    const authority = input.split(/[/?#]/, 1)[0];
    if (/\s/.test(input) || authority.includes("@")) throw new Error("Enter a browser address. Search is not configured for this room.");
    const local = ["localhost", "127.0.0.1", "0.0.0.0", "[::1]"].includes(parsed.hostname) || parsed.hostname.endsWith(".localhost");
    if (local) parsed.protocol = "http:";
    else if (!parsed.hostname.includes(".") && !parsed.hostname.startsWith("[") && !/:\d+$/.test(authority)) throw new Error("Enter a browser address. Search is not configured for this room.");
  }
  return parsed.href;
}

type OwnedBrowserElement = HTMLElement & {
  getOwnedGuestFrameToken(): string;
  adoptOwnedGuest(token: string, generation: number): Promise<void>;
  releaseOwnedGuest(): void;
};

function nativeBrowserGeometry(element: HTMLElement, model: OctopusNativeBrowserViewerState["modelViewport"], scaledPresentation: boolean) {
  const rect = element.getBoundingClientRect();
  let visible = !element.closest("[inert], [aria-hidden='true'], [hidden]");
  for (let ancestor: HTMLElement | null = element; ancestor; ancestor = ancestor.parentElement) {
    const style = window.getComputedStyle(ancestor);
    if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") visible = false;
  }
  const stageBounds = { x: rect.left, y: rect.top, width: rect.width, height: rect.height };
  const size = scaledPresentation ? model.emulatedSize : null;
  const scale = size && rect.width > 0 && rect.height > 0 ? Math.min(1, rect.width / size.width, rect.height / size.height) : 1;
  const width = size ? size.width : rect.width, height = size ? size.height : rect.height;
  const x = rect.left + Math.max(0, (rect.width - width * scale) / 2), y = rect.top;
  return { stageBounds, pageBounds: { x, y, width, height }, visualBounds: { x, y, width: width * scale, height: height * scale }, scale,
    visible: visible && rect.width > 0 && rect.height > 0 };
}

function layoutNativeBrowserGuest(guest: OwnedBrowserElement, geometry: ReturnType<typeof nativeBrowserGeometry>) {
  const styles = { left: `${geometry.pageBounds.x - geometry.stageBounds.x}px`, top: "0px", width: `${geometry.pageBounds.width}px`, height: `${geometry.pageBounds.height}px`, transform: `scale(${geometry.scale})` };
  for (const key of Object.keys(styles) as Array<keyof typeof styles>) if (guest.style[key] !== styles[key]) guest.style[key] = styles[key];
}

function nativeBrowserStageFits(element: HTMLElement, bounds: OctopusNativeBrowserViewerSyncRequest["stageBounds"]) {
  const viewport = window.visualViewport;
  const pixelRatio = window.devicePixelRatio;
  if (!viewport || !Number.isFinite(pixelRatio) || pixelRatio <= 0 || bounds.width <= 0 || bounds.height <= 0) return false;
  const pixelEdge = (edge: number) => Math.round(edge * pixelRatio);
  const left = pixelEdge(bounds.x), top = pixelEdge(bounds.y);
  const right = pixelEdge(bounds.x + bounds.width), bottom = pixelEdge(bounds.y + bounds.height);
  if (right <= left || bottom <= top) return false;
  if (left < pixelEdge(viewport.offsetLeft) || top < pixelEdge(viewport.offsetTop)
    || right > pixelEdge(viewport.offsetLeft + viewport.width) || bottom > pixelEdge(viewport.offsetTop + viewport.height)) return false;
  for (let ancestor = element.parentElement; ancestor; ancestor = ancestor.parentElement) {
    const style = window.getComputedStyle(ancestor);
    const paintClip = /\b(paint|strict|content)\b/.test(style.contain);
    const clipsX = paintClip || /^(hidden|clip|scroll|auto)$/.test(style.overflowX);
    const clipsY = paintClip || /^(hidden|clip|scroll|auto)$/.test(style.overflowY);
    if (!clipsX && !clipsY) continue;
    const rect = ancestor.getBoundingClientRect();
    if (clipsX && (left < pixelEdge(rect.left) || right > pixelEdge(rect.right))
      || clipsY && (top < pixelEdge(rect.top) || bottom > pixelEdge(rect.bottom))) return false;
  }
  return true;
}

function NativeBrowserSurface({ active, environment, surface }: { active: boolean; environment: WorkerEnvironmentProjection; surface: WorkerEnvironmentSurfaceProjection }) {
  const [state, setState] = useState<OctopusNativeBrowserViewerState | null>(null);
  const [error, setError] = useState("");
  const [errorCode, setErrorCode] = useState("");
  const [pageClosed, setPageClosed] = useState(false);
  const [opening, setOpening] = useState(false);
  const [retry, setRetry] = useState(0);
  const [draft, setDraft] = useState(dataText(surface.data, "url"));
  const [menu, setMenu] = useState(false);
  const [downloadRequest, setDownloadRequest] = useState(0);
  const [details, setDetails] = useState(false);
  const [find, setFind] = useState(false);
  const [findRequest, setFindRequest] = useState(0);
  const [query, setQuery] = useState("");
  const viewport = useRef<HTMLDivElement>(null);
  const page = useRef<OwnedBrowserElement>(null);
  const cursorLayer = useRef<HTMLDivElement>(null);
  const cursorController = useRef<ReturnType<typeof createBrowserCursor> | null>(null);
  const cursorFingerprint = useRef("");
  const address = useRef<HTMLInputElement>(null);
  const findInput = useRef<HTMLInputElement>(null);
  const nativeMenu = useRef<{ requestId: string; signature: string; transport: NonNullable<Window["octopusTransport"]> } | null>(null);
  const editing = useRef(false);
  const attachment = useRef("");
  const lease = useRef<OctopusNativeBrowserViewerIdentity | null>(null);
  const viewportSequence = useRef(0);
  const adopted = useRef(false);
  const presentationProtocol = useRef<OctopusNativeBrowserViewerState["presentationProtocol"] | null>(null);
  const acknowledged = useRef(false);
  const layoutVisible = useRef(false);
  const layoutMetricsRevision = useRef(-1);
  const current = useRef(state);
  const scheduleLayout = useRef<(() => void) | null>(null);
  const release = useRef<Promise<void>>(Promise.resolve());
  const activeRef = useRef(active);
  activeRef.current = active;
  const available = Boolean(window.octopusTransport);
  const targetId = dataText(surface.data, "target_id");
  const activity = browserActivity(surface);
  const readOnly = surface.readOnly === true || state?.readOnly === true;
  const readOnlyRef = useRef(readOnly);
  readOnlyRef.current = readOnly;

  const ready = state?.connected === true && state.presentationPhase === "ready";
  const previewError = surface.data.browser_preview && typeof surface.data.browser_preview === "object"
    ? dataText(surface.data.browser_preview as Record<string, unknown>, "error") : "";

  function present() {
    const next = current.current;
    if (page.current) {
      const value = adopted.current && layoutVisible.current && next?.presentationPhase === "ready" && next.visible
        && activeRef.current && !document.hidden && layoutMetricsRevision.current === next.modelViewport.revision ? "visible" : "hidden";
      if (page.current.style.visibility !== value) page.current.style.visibility = value;
    }
    const stage = viewport.current, layer = cursorLayer.current, controller = cursorController.current;
    if (stage && layer && controller && next) {
      const geometry = nativeBrowserGeometry(stage, next.modelViewport, next.engine.capabilities.scaledPresentation);
      const styles = { left: `${geometry.pageBounds.x - geometry.stageBounds.x}px`, top: "0px", width: `${geometry.pageBounds.width}px`, height: `${geometry.pageBounds.height}px`, transform: `scale(${geometry.scale})` };
      for (const key of Object.keys(styles) as Array<keyof typeof styles>) if (layer.style[key] !== styles[key]) layer.style[key] = styles[key];
      const packet = { cursor: next.cursor, visible: Boolean(next.engine.capabilities.cursorOverlay && next.modelActive && next.visible && next.presentationPhase === "ready" && layoutVisible.current && activeRef.current && !document.hidden && layoutMetricsRevision.current === next.modelViewport.revision),
        turnKey: `${next.run}:${next.modelTurnKey}`, viewportSize: { width: geometry.pageBounds.width, height: geometry.pageBounds.height } };
      const fingerprint = JSON.stringify(packet);
      if (cursorFingerprint.current !== fingerprint) { cursorFingerprint.current = fingerprint; controller.setState(packet); }
      const visibility = next.presentationPhase === "ready" && activeRef.current && !document.hidden ? "visible" : "hidden";
      if (layer.style.visibility !== visibility) layer.style.visibility = visibility;
    } else if (layer && layer.style.visibility !== "hidden") layer.style.visibility = "hidden";
  }
  function accept(next: OctopusNativeBrowserViewerState) {
    const owner = lease.current;
    if (!owner || next.attachmentId !== owner.attachmentId || next.hostGeneration !== owner.hostGeneration || next.mountGeneration !== owner.mountGeneration
      || next.viewportSequence < (current.current?.viewportSequence ?? 0) || next.modelViewport?.revision < (current.current?.modelViewport.revision ?? 0)) return false;
    if (next.protocol !== "browser.native.v1" || next.presentationProtocol !== presentationProtocol.current || next.workerEnvironmentId !== environment.id || next.run !== environment.runId
      || next.surfaceId !== surface.id || next.targetId !== targetId || next.hostGeneration !== dataText(surface.data, "host_generation")
      || next.engineHandle !== dataText(surface.data, "engine_handle") || next.browserContextId !== dataText(surface.data, "browser_context_id")) return false;
    acknowledged.current = next.presentationPhase === "ready";
    current.current = next; setState(next); present();
    if (!editing.current) setDraft(next.url);
    scheduleLayout.current?.();
    return true;
  }

  useEffect(() => {
    const layer = cursorLayer.current;
    if (!layer) return;
    const controller = createBrowserCursor(layer, browserCursorAsset, moveSequence => {
      const owner = lease.current, next = current.current;
      if (!owner || !next || !activeRef.current || document.hidden || !next.visible || !next.modelActive || next.presentationPhase !== "ready" || next.cursor?.moveSequence !== moveSequence) return;
      void window.octopusTransport?.nativeBrowserCursorArrived({ ...owner, moveSequence, metricsRevision: next.cursor.metricsRevision }).catch(failure => {
        if (lease.current === owner) setError(`Cursor presentation: ${formatProjectionError(failure)}`);
      });
    });
    cursorController.current = controller; cursorFingerprint.current = ""; present();
    return () => { cursorController.current = null; controller.destroy(); };
  }, []);

  useEffect(() => {
    setError(""); setErrorCode(""); setPageClosed(false); setState(null); current.current = null; adopted.current = false; presentationProtocol.current = null; acknowledged.current = false; layoutVisible.current = false; layoutMetricsRevision.current = -1;
    present();
    if (!active || environment.lifecycle !== "active") return;
    if (!available) { setError("Native browser viewing requires the Octopus desktop client."); return; }
    if (!targetId) { setError("This room has not projected an exact browser target."); return; }
    const element = page.current;
    if (!element) { setError("The browser presentation stage is unavailable."); return; }
    const ownedGuestAvailable = typeof element.getOwnedGuestFrameToken === "function"
      && typeof element.adoptOwnedGuest === "function" && typeof element.releaseOwnedGuest === "function";
    const transport = window.octopusTransport!;
    const id = crypto.randomUUID();
    let disposed = false, released = false, ownedPresentation = false;
    let prepared: OctopusNativeBrowserViewerIdentity | null = null;
    attachment.current = id; lease.current = null; viewportSequence.current = 0; setOpening(true);
    const close = async () => {
      if (!prepared || released) return;
      released = true;
      try { if (ownedPresentation) element.releaseOwnedGuest(); }
      finally { await transport.closeNativeBrowserViewer(prepared); }
    };
    const unsubscribe = transport.onNativeBrowserViewerEvent((event) => {
      const owner = lease.current;
      if (disposed || !owner || attachment.current !== id || event.attachmentId !== id || event.hostGeneration !== owner.hostGeneration || event.mountGeneration !== owner.mountGeneration) return;
      if (event.type === "state") accept(event.state);
      else if (event.type === "command") {
        if (!activeRef.current || current.current?.presentationPhase !== "ready") return;
        if (event.command === "focus-address") { address.current?.focus(); address.current?.select(); }
        else openFind();
      } else {
        setPageClosed(event.type === "closed" && event.error.code === "target_unavailable");
        setError(event.error.message); setErrorCode(event.error.code); setState(null); current.current = null; attachment.current = ""; adopted.current = false; present();
      }
    });
    const openingPromise = release.current.then(async () => {
      if (disposed) return;
      const result = await transport.prepareNativeBrowserViewer({ attachmentId: id, workerEnvironmentId: environment.id, surfaceId: surface.id,
        frameToken: ownedGuestAvailable ? element.getOwnedGuestFrameToken() : crypto.randomUUID() });
      prepared = { attachmentId: id, hostGeneration: result.hostGeneration, mountGeneration: result.mountGeneration };
      if (disposed) return;
      const engine = result.state.engine;
      if (!engine || engine.presentationProtocol !== result.state.presentationProtocol) throw new Error("Browser engine presentation contract is missing.");
      const requiresOwnedGuest = engine.mode === "owned" && result.state.presentationProtocol === "electron.ownedGuest.v1";
      if (requiresOwnedGuest) {
        if (!ownedGuestAvailable || !result.adoptionToken || !engine.capabilities.ownedGuestEmbedding) throw new Error("The reviewed owned-guest browser engine is unavailable in this client.");
      } else if (engine.mode !== "standard" || result.state.presentationProtocol !== "electron.webContentsView.v1"
        || result.adoptionToken !== null || !engine.capabilities.nativeView) throw new Error("Browser engine presentation contract is unsupported.");
      ownedPresentation = requiresOwnedGuest;
      presentationProtocol.current = result.state.presentationProtocol;
      lease.current = prepared;
      if (!accept(result.state)) throw new Error("Native browser preparation returned a different target or generation.");
      const stage = viewport.current;
      if (!stage) throw new Error("Native browser presentation stage is unavailable.");
      layoutNativeBrowserGuest(element, nativeBrowserGeometry(stage, result.state.modelViewport, engine.capabilities.scaledPresentation));
      await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
      if (disposed) return;
      if (ownedPresentation) await element.adoptOwnedGuest(result.adoptionToken!, result.mountGeneration);
      if (disposed) return;
      adopted.current = true; scheduleLayout.current?.();
    }).catch(async (failure) => {
      let releaseFailure: unknown;
      try { await close(); } catch (error) { releaseFailure = error; }
      if (!disposed) {
        setError(releaseFailure ? `Browser presentation could not be released: ${formatProjectionError(releaseFailure)}` : formatProjectionError(failure));
        setErrorCode(failure && typeof failure === "object" && "code" in failure ? String(failure.code) : "");
        attachment.current = ""; current.current = null; adopted.current = false; setState(null); present();
      }
      if (releaseFailure) throw releaseFailure;
    }).finally(() => { if (!disposed) setOpening(false); });
    void openingPromise.catch(() => {});
    return () => {
      disposed = true; unsubscribe(); adopted.current = false; present();
      if (attachment.current === id) { attachment.current = ""; lease.current = null; }
      release.current = openingPromise.then(close).catch((failure) => {
        throw new Error(`Browser presentation could not be released: ${formatProjectionError(failure)}`);
      });
      void release.current.catch(() => {});
    };
  }, [active, available, environment.lifecycle, retry]);

  useEffect(() => {
    const transport = window.octopusTransport;
    const element = viewport.current, guest = page.current;
    if (!available || !transport || !element || !guest) return;
    let disposed = false, animation = 0, syncing = false, dirty = false, last = "";
    const sync = async () => {
      animation = 0;
      const owner = lease.current;
      if (disposed || !owner || !adopted.current || !current.current?.connected) return;
      if (syncing) { dirty = true; return; }
      const model = current.current.modelViewport;
      const nativeView = current.current.engine.capabilities.nativeView;
      const geometry = nativeBrowserGeometry(element, model, current.current.engine.capabilities.scaledPresentation);
      geometry.visible = geometry.visible && activeRef.current && !document.hidden;
      if (nativeView && !nativeBrowserStageFits(element, geometry.stageBounds)) {
        layoutVisible.current = false; present();
        if (!acknowledged.current && geometry.visible) return;
        geometry.visible = false;
        geometry.stageBounds = geometry.pageBounds = geometry.visualBounds = { x: 0, y: 0, width: 0, height: 0 };
      }
      if (nativeView && geometry.visible) {
        const stage = geometry.stageBounds;
        geometry.visible = !Array.from(document.querySelectorAll<HTMLElement>('[role="dialog"], [role="menu"], [role="listbox"], [data-radix-popper-content-wrapper], .we-download-peek'))
          .some((overlay) => {
            if (overlay.contains(element) || overlay.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
            const rect = overlay.getBoundingClientRect();
            const style = window.getComputedStyle(overlay);
            return rect.width > 0 && rect.height > 0 && style.display !== "none" && style.visibility !== "hidden" && style.opacity !== "0"
              && rect.left < stage.x + stage.width && rect.right > stage.x && rect.top < stage.y + stage.height && rect.bottom > stage.y;
          });
      }
      const fingerprint = JSON.stringify({ ...geometry, metricsRevision: model.revision });
      if (last === fingerprint && acknowledged.current) { present(); return; }
      syncing = true;
      layoutNativeBrowserGuest(guest, geometry);
      layoutMetricsRevision.current = model.revision; layoutVisible.current = geometry.visible;
      if (!geometry.visible && guest.style.visibility !== "hidden") guest.style.visibility = "hidden";
      try {
        if (!nativeView) await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
        if (disposed || lease.current !== owner || !adopted.current) return;
        if (current.current?.modelViewport.revision !== model.revision) { dirty = true; return; }
        const packet: OctopusNativeBrowserViewerSyncRequest = { ...owner, ...geometry, metricsRevision: model.revision, viewportSequence: ++viewportSequence.current };
        const next = await (acknowledged.current ? transport.syncNativeBrowserViewer(packet) : transport.acknowledgeNativeBrowserViewer(packet));
        if (disposed || lease.current !== owner) return;
        if (next.modelViewport.revision < (current.current?.modelViewport.revision ?? 0)) { dirty = true; return; }
        if (!accept(next)) throw new Error("Native browser viewport acknowledgement is stale.");
        last = fingerprint; present();
      } catch (failure) {
        if (!disposed && lease.current === owner) setError(formatProjectionError(failure));
      } finally {
        syncing = false;
        if (dirty) { dirty = false; schedule(); }
      }
    };
    const schedule = () => { if (!disposed && !animation) animation = window.requestAnimationFrame(() => { void sync(); }); };
    scheduleLayout.current = schedule;
    const resize = new ResizeObserver(schedule);
    for (let ancestor: HTMLElement | null = element; ancestor; ancestor = ancestor.parentElement) resize.observe(ancestor);
    const mutations = new MutationObserver(schedule);
    mutations.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["inert", "aria-hidden", "style", "class", "hidden"] });
    window.addEventListener("resize", schedule); window.addEventListener("scroll", schedule, true); document.addEventListener("visibilitychange", schedule);
    schedule();
    return () => {
      disposed = true; window.cancelAnimationFrame(animation); resize.disconnect(); mutations.disconnect();
      window.removeEventListener("resize", schedule); window.removeEventListener("scroll", schedule, true); document.removeEventListener("visibilitychange", schedule);
      if (scheduleLayout.current === schedule) scheduleLayout.current = null;
    };
  }, [available, state?.attachmentId]);

  useEffect(() => { present(); scheduleLayout.current?.(); }, [active, find]);
  useEffect(() => {
    if (nativeMenu.current && nativeMenu.current.signature !== browserMenuSignature()) { cancelNativeMenu(); setMenu(false); }
  }, [active, readOnly, state]);
  useEffect(() => cancelNativeMenu, []);
  useEffect(() => {
    if (!active || !find) return;
    const animation = window.requestAnimationFrame(() => { findInput.current?.focus(); findInput.current?.select(); });
    return () => window.cancelAnimationFrame(animation);
  }, [active, find, findRequest]);

  async function action(request: NativeBrowserAction) {
    const owner = lease.current, id = attachment.current;
    if (!owner || !id || current.current?.presentationPhase !== "ready" || !activeRef.current || (readOnly && request.action !== "find")) return null;
    try {
      const next = await window.octopusTransport!.nativeBrowserViewerAction({ ...owner, ...request } as OctopusNativeBrowserViewerActionRequest);
      if (attachment.current !== id) return null;
      accept(next); setError("");
      if (request.action === "focus" && next.engine.capabilities.ownedGuestEmbedding) page.current?.focus();
      return next;
    } catch (failure) { if (attachment.current === id) setError(formatProjectionError(failure)); return null; }
  }
  function browserMenuSignature() {
    const next = current.current;
    return JSON.stringify([lease.current, activeRef.current, readOnlyRef.current, next?.connected, next?.visible, next?.presentationPhase, next?.url, next?.zoomFactor,
      next?.modelViewport.revision, next?.engine.capabilities.persistentDownloadHistory]);
  }
  function cancelNativeMenu() {
    const pending = nativeMenu.current;
    nativeMenu.current = null;
    if (pending) void pending.transport.cancelClientContextMenu({ requestId: pending.requestId }).catch(failure => {
      if (activeRef.current && browserMenuSignature() === pending.signature) setError(formatProjectionError(failure));
    });
  }
  async function toggleBrowserMenu(anchor: HTMLButtonElement) {
    if (nativeMenu.current) { cancelNativeMenu(); setMenu(false); return; }
    if (!activeRef.current) return;
    const transport = window.octopusTransport;
    if (!transport?.showClientContextMenu || !transport.cancelClientContextMenu) { setError("Native browser actions are unavailable."); return; }
    const next = current.current, connected = next?.connected === true && next.presentationPhase === "ready";
    const writable = connected && !readOnlyRef.current, zoomFactor = next?.zoomFactor ?? 1;
    const downloadsEnabled = next?.engine.capabilities.persistentDownloadHistory === true;
    const choices = [
      { id: "downloads", label: "Downloads", enabled: downloadsEnabled, select: () => setDownloadRequest(value => value + 1) },
      { id: "find", label: "Find in page", enabled: connected, accelerator: "CommandOrControl+F", select: openFind },
      { id: "external", label: "Open in default browser", enabled: isViewerUrl(next?.url ?? ""), select: () => window.open(next?.url, "_blank", "noopener,noreferrer") },
      { id: "zoom-out", label: "Zoom out", enabled: writable && zoomFactor > 0.5, select: () => { void action({ action: "zoom", params: { zoomFactor: Math.max(0.5, zoomFactor - 0.1) } }); } },
      { id: "zoom-in", label: "Zoom in", enabled: writable && zoomFactor < 2.5, select: () => { void action({ action: "zoom", params: { zoomFactor: Math.min(2.5, zoomFactor + 0.1) } }); } },
      { id: "zoom-reset", label: "Reset zoom", enabled: writable, select: () => { void action({ action: "zoom", params: { zoomFactor: 1 } }); } },
      { id: "details", label: "Browser details", enabled: true, select: () => setDetails(value => !value) },
    ];
    const itemsFor = (ids: string[]): OctopusClientContextMenuItem[] => choices.filter(choice => ids.includes(choice.id)).map(({ select: _select, ...item }) => item);
    const items: OctopusClientContextMenuItem[] = [
      ...itemsFor(downloadsEnabled ? ["downloads", "find", "external"] : ["find", "external"]),
      { type: "separator" },
      { id: "zoom", label: `Zoom (${Math.round(zoomFactor * 100)}%)`, submenu: itemsFor(["zoom-out", "zoom-in", "zoom-reset"]) },
      { type: "separator" }, ...itemsFor(["details"]),
    ];
    const bounds = anchor.getBoundingClientRect(), pending = { requestId: crypto.randomUUID(), signature: browserMenuSignature(), transport };
    nativeMenu.current = pending; setMenu(true);
    try {
      const result = await transport.showClientContextMenu({ requestId: pending.requestId, items, position: { x: Math.max(0, bounds.right), y: Math.max(0, bounds.bottom) } });
      if (nativeMenu.current !== pending) return;
      if (browserMenuSignature() !== pending.signature) return;
      if (result?.requestId !== pending.requestId || !(result.id === null || typeof result.id === "string")) throw new Error("Invalid native browser-menu response.");
      if (result.id === null) return;
      const choice = choices.find(choice => choice.id === result.id && choice.enabled);
      if (!choice) throw new Error("Native browser-menu action is unavailable.");
      choice.select();
    } catch (failure) {
      if (nativeMenu.current === pending && browserMenuSignature() === pending.signature) setError(formatProjectionError(failure));
    } finally {
      if (nativeMenu.current === pending) { nativeMenu.current = null; setMenu(false); }
    }
  }
  function openFind() {
    cancelNativeMenu();
    setMenu(false);
    setFind(true);
    setFindRequest(value => value + 1);
  }
  function closeFind() {
    setFind(false);
    setQuery("");
    const owner = lease.current;
    void action({ action: "find", params: { query: "", backwards: false, matchCase: false } }).then(next => {
      if (next && lease.current === owner && activeRef.current && document.activeElement === document.body) void action({ action: "focus" });
    });
  }
  function findPage(backwards = false, text = query) {
    void action({ action: "find", params: { query: text, backwards, matchCase: false } });
  }
  async function navigate() {
    let url, currentUrl;
    try { url = normalizeNativeBrowserAddress(draft); currentUrl = normalizeNativeBrowserAddress(state?.url ?? ""); }
    catch (failure) { setError(formatProjectionError(failure)); return; }
    if (!url) return;
    const id = attachment.current;
    const next = await action(url === currentUrl ? { action: "reload" } : { action: "navigate", params: { url } });
    if (next && attachment.current === id && activeRef.current && document.activeElement === document.body) void action({ action: "focus" });
  }
  return <div className="we-browser-shell is-native" onKeyDownCapture={(event) => {
    if (!active || !(event.metaKey || event.ctrlKey) || event.altKey || event.nativeEvent.isComposing) return;
    const key = event.key.toLowerCase();
    if (key !== "l" && (key !== "f" || !ready)) return;
    event.preventDefault(); event.stopPropagation();
    if (key === "l") { address.current?.focus(); address.current?.select(); } else openFind();
  }}>
    <div className="we-browser-toolbar" role="toolbar" aria-label="Browser navigation">
      <div className="we-browser-history" role="group" aria-label="Navigation">
        <BrowserToolbarButton label="Back" glyph="navigateBack" disabled={readOnly || !ready || !state?.canGoBack} onClick={() => { void action({ action: "back" }); }} />
        <BrowserToolbarButton label="Forward" glyph="navigateBack" reverse disabled={readOnly || !ready || !state?.canGoForward} onClick={() => { void action({ action: "forward" }); }} />
        <span className="we-browser-navigation-divider" aria-hidden="true" />
        <BrowserToolbarButton label={state?.loading ? "Stop loading page" : "Reload page"} glyph={state?.loading ? "close" : "refresh"} disabled={readOnly || !ready} onClick={() => { void action({ action: state?.loading ? "stop" : "reload" }); }} />
      </div>
      <div className="we-browser-address-slot"><div className="we-browser-address">
        <input aria-label="Browser address" autoCapitalize="none" autoComplete="off" placeholder="Enter a URL" ref={address} spellCheck={false} type="text" value={draft === "about:blank" ? "" : draft} readOnly={readOnly || !ready}
          onChange={(event) => setDraft(event.currentTarget.value)} onFocus={(event) => { editing.current = true; event.currentTarget.select(); }}
          onBlur={() => { editing.current = false; setDraft(current.current?.url || dataText(surface.data, "url")); }}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing) return;
            if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); setDraft(state?.url || dataText(surface.data, "url")); event.currentTarget.blur(); void action({ action: "focus" }); }
            else if (event.key === "Enter") { event.preventDefault(); void navigate(); event.currentTarget.blur(); }
          }} />
        {isViewerUrl(state?.url ?? "") ? <BrowserToolbarButton label="Open in default browser" glyph="openExternal" onClick={() => window.open(state?.url, "_blank", "noopener,noreferrer")} /> : null}
      </div></div>
      {state?.engine.capabilities.persistentDownloadHistory ? <div className="we-browser-downloads" title="Downloads"><BrowserDownloads active={active} environment={environment} surface={surface} openRequest={downloadRequest} /></div> : !ready ? <div className="we-browser-actions"><BrowserToolbarButton label="Downloads" glyph="download" disabled /></div> : null}
      <div className="we-browser-actions">
        <BrowserToolbarButton aria-expanded={menu} aria-haspopup="menu" label="More browser actions" glyph="more" onClick={(event) => { void toggleBrowserMenu(event.currentTarget); }} />
      </div>
      <span aria-hidden="true" className={`we-browser-loading${opening || state?.loading ? " is-visible" : ""}`}><i /></span>
    </div>
    {ready && readOnly ? <div className="we-browser-status" role="status">
      <span><Glyph name="lock" size={13} />Read-only browser</span>
    </div> : null}
    {details ? <details className="we-browser-details we-browser-details-panel" open onToggle={(event) => { if (!event.currentTarget.open) setDetails(false); }}><summary>Technical details</summary><dl>
        <dt>Task</dt><dd>{environment.runId}</dd><dt>Room</dt><dd>{environment.id}</dd><dt>Target</dt><dd>{targetId}</dd>
        <dt>Input</dt><dd>Your page interactions are outside the artifact log. Worker tools remain independent.</dd>
        {state ? <><dt>Browser</dt><dd>{state.engine.mode === "standard" ? "Standard Chromium" : "Owned Chromium"}</dd><dt>Capabilities</dt><dd>{state.engine.limits.join(" ")}</dd></> : null}
        {activity ? <><dt>Tool</dt><dd>{activity.tool} · {activity.status} · movement {activity.sequence}</dd></> : null}
        {previewError ? <><dt>Preview</dt><dd>{previewError}</dd></> : null}
      </dl></details> : null}
    {error && ready ? <div className="we-browser-control-error" role="alert"><span>Couldn’t complete that browser action.</span><details className="we-browser-details"><summary>Technical details</summary><p>{error}</p></details><button onClick={() => setError("")} type="button">Dismiss</button></div> : null}
    <div className={`we-browser-body${state?.engine.capabilities.nativeView ? " has-native-view" : ""}`}>
      {find ? <form className="we-browser-find" onSubmit={(event) => { event.preventDefault(); findPage(); }}>
        <Glyph name="search" size={14} />
        <input aria-label="Find in page" placeholder="Find" ref={findInput} value={query} maxLength={10000}
          onChange={(event) => {
            const value = event.currentTarget.value;
            setQuery(value);
            findPage(false, value);
          }}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing) return;
            if (event.key === "Enter") {
              event.preventDefault(); event.stopPropagation(); findPage(event.shiftKey);
            } else if (event.key === "Escape") {
              event.preventDefault(); event.stopPropagation(); closeFind();
            }
          }} />
        <span>{state?.findMatches !== undefined ? `${state.findActiveOrdinal ?? 0} / ${state.findMatches}` : ""}</span>
        <button aria-label="Previous match" disabled={!query} onClick={() => findPage(true)} type="button"><Glyph name="chevronUp" size={14} /></button>
        <button aria-label="Next match" disabled={!query} onClick={() => findPage()} type="button"><Glyph name="chevronDown" size={14} /></button>
        <button aria-label="Close find" onClick={closeFind} type="button"><Glyph name="close" size={13} /></button>
      </form> : null}
      <div className="we-native-browser-viewport" ref={viewport} aria-label="Native browser page">
        {createElement("webview", { ref: page, className: "we-native-browser-guest", "aria-label": "Browser page" })}
        <div className="we-native-browser-cursor" ref={cursorLayer} aria-hidden="true" />
        {!ready ? <BrowserConnectionState
          title={pageClosed ? "Browser page closed" : opening ? "Connecting to browser…" : errorCode === "host_unavailable" ? "Browser session disconnected" : state?.connected ? "Preparing browser…" : "Browser unavailable"}
          description={opening || state?.connected ? "The page will appear here when the connection is ready."
            : pageClosed ? "This browser page was closed and can no longer be used."
            : errorCode === "host_unavailable" ? "The previous browser session is no longer connected. Ask Octopus to open a new browser page in this task to continue."
            : environment.lifecycle !== "active" ? "This task’s browser is no longer active. Ask Octopus to open a new browser page to continue."
            : !available ? "Open this task in the Octopus desktop app to view its browser."
            : "This page couldn’t connect. Try again, or ask Octopus to open a new browser page in this task."}
          onRetry={!pageClosed && !opening && !state?.connected && errorCode !== "host_unavailable" && available && !!targetId && environment.lifecycle === "active" ? () => setRetry(value => value + 1) : undefined}>
          {error ? <p>{error}</p> : null}
        </BrowserConnectionState> : null}
      </div>
    </div>
  </div>;
}

function BrowserLoadError({ detail, onReload, url }: { detail: string; onReload: (() => void) | null; url: string }) {
  let host = "This page";
  try {
    host = new URL(url).hostname || host;
  } catch {}
  return (
    <section className="we-browser-load-error" role="alert">
      <Glyph name="browser" size={22} />
      <h2>This site can&apos;t be reached</h2>
      <p><strong>{host}</strong> could not be loaded.</p>
      <small>{detail}</small>
      {onReload ? <OcButton leadingIcon="refresh" onClick={onReload} size="sm" variant="primary">Reload</OcButton> : null}
    </section>
  );
}

function CdpBrowserViewer({ frame, interactionEnabled, onAction, onPainted, sessionId }: {
  frame: CdpBrowserFrame | null;
  interactionEnabled: boolean;
  onAction: (request: OctopusCdpViewerActionRequest) => Promise<unknown>;
  onPainted: (painted: boolean) => void;
  sessionId: string;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const paintedMetadataRef = useRef<OctopusCdpViewerFrameMetadata | null>(null);
  const receiveFrameRef = useRef<((next: CdpBrowserFrame | null) => void) | null>(null);
  const [painted, setPainted] = useState(false);
  const [paintError, setPaintError] = useState("");
  const pointerFrameRef = useRef<number | null>(null);
  const pendingPointerRef = useRef<{ buttons: number; x: number; y: number } | null>(null);
  const actionRef = useRef(onAction);
  actionRef.current = onAction;

  useEffect(() => {
    if (interactionEnabled) hostRef.current?.focus({ preventScroll: true });
  }, [interactionEnabled, sessionId]);

  useEffect(() => {
    const host = hostRef.current;
    if (!host || !interactionEnabled) return;
    const scroll = (event: WheelEvent) => {
      const position = browserImagePoint(canvasRef.current?.getBoundingClientRect(), paintedMetadataRef.current?.deviceWidth, event.clientX, event.clientY);
      if (!position) return;
      event.preventDefault();
      const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? host.clientHeight : 1;
      void actionRef.current({
        sessionId, action: "mouse", eventType: "mouseWheel", button: "none", buttons: 0,
        x: position.x, y: position.y,
        deltaX: event.deltaX * unit / position.zoom,
        deltaY: event.deltaY * unit / position.zoom,
        modifiers: cdpModifiers(event),
      });
    };
    host.addEventListener("wheel", scroll, { passive: false });
    return () => host.removeEventListener("wheel", scroll);
  }, [interactionEnabled, sessionId]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    let disposed = false;
    let decoding = false;
    let pending: CdpBrowserFrame | null = null;
    paintedMetadataRef.current = null;
    setPainted(false);
    onPainted(false);
    setPaintError(context ? "" : "Browser frames cannot be painted in this client.");
    async function paintNext() {
      if (disposed || decoding || !pending || !canvas || !context) return;
      const next = pending;
      pending = null;
      decoding = true;
      const image = new Image();
      try {
        image.src = `data:${next.mimeType};base64,${next.data}`;
        await image.decode();
        if (disposed) return;
        canvas.width = image.naturalWidth;
        canvas.height = image.naturalHeight;
        context.drawImage(image, 0, 0);
        paintedMetadataRef.current = next.metadata;
        setPaintError("");
        setPainted(true);
        onPainted(true);
      } catch {
        if (disposed) return;
        paintedMetadataRef.current = null;
        setPainted(false);
        onPainted(false);
        setPaintError("The browser frame could not be decoded. Waiting for a valid frame.");
      } finally {
        decoding = false;
        if (!disposed && pending) void paintNext();
      }
    }
    receiveFrameRef.current = (next) => {
      pending = next;
      void paintNext();
    };
    return () => {
      disposed = true;
      pending = null;
      receiveFrameRef.current = null;
      paintedMetadataRef.current = null;
    };
  }, [onPainted, sessionId]);

  useEffect(() => { receiveFrameRef.current?.(frame); }, [frame, sessionId]);

  useEffect(() => () => {
    if (pointerFrameRef.current != null) window.cancelAnimationFrame(pointerFrameRef.current);
    pointerFrameRef.current = null;
    pendingPointerRef.current = null;
  }, [interactionEnabled, sessionId]);

  function point(event: ReactPointerEvent<HTMLDivElement>) {
    if (!interactionEnabled) return null;
    const position = browserImagePoint(canvasRef.current?.getBoundingClientRect(), paintedMetadataRef.current?.deviceWidth, event.clientX, event.clientY, event.currentTarget.hasPointerCapture(event.pointerId));
    return position ? { x: position.x, y: position.y } : null;
  }

  function sendPointerMove() {
    pointerFrameRef.current = null;
    const pending = pendingPointerRef.current;
    pendingPointerRef.current = null;
    if (!pending) return;
    void onAction({
      sessionId,
      action: "mouse",
      eventType: "mouseMoved",
      button: "none",
      buttons: pending.buttons,
      x: pending.x,
      y: pending.y,
    });
  }

  return (
    <div
      aria-label="Live Worker Environment browser"
      className="we-cdp-viewer"
      onContextMenu={(event) => event.preventDefault()}
      onKeyDown={(event) => {
        if (!interactionEnabled || !paintedMetadataRef.current || event.nativeEvent.isComposing) return;
        event.preventDefault();
        const text = event.key.length === 1 && !event.ctrlKey && !event.metaKey ? event.key : "";
        void onAction({
          sessionId,
          action: "key",
          eventType: "keyDown",
          key: event.key,
          code: event.code,
          text,
          unmodifiedText: text,
          windowsVirtualKeyCode: event.keyCode,
          nativeVirtualKeyCode: event.keyCode,
          modifiers: cdpModifiers(event),
          autoRepeat: event.repeat,
        });
      }}
      onKeyUp={(event) => {
        if (!interactionEnabled || !paintedMetadataRef.current || event.nativeEvent.isComposing) return;
        event.preventDefault();
        void onAction({
          sessionId,
          action: "key",
          eventType: "keyUp",
          key: event.key,
          code: event.code,
          windowsVirtualKeyCode: event.keyCode,
          nativeVirtualKeyCode: event.keyCode,
          modifiers: cdpModifiers(event),
        });
      }}
      onPointerCancel={(event) => {
        if (!event.currentTarget.hasPointerCapture(event.pointerId)) return;
        const position = point(event);
        event.currentTarget.releasePointerCapture(event.pointerId);
        if (position) void onAction({ sessionId, action: "mouse", eventType: "mouseReleased", button: cdpMouseButton(event.button), buttons: 0, ...position });
      }}
      onPointerDown={(event) => {
        const position = point(event);
        if (!position) return;
        event.preventDefault();
        event.currentTarget.focus();
        event.currentTarget.setPointerCapture(event.pointerId);
        void onAction({
          sessionId,
          action: "mouse",
          eventType: "mousePressed",
          button: cdpMouseButton(event.button),
          buttons: event.buttons,
          clickCount: Math.max(1, event.detail),
          modifiers: cdpModifiers(event),
          ...position,
        });
      }}
      onPointerMove={(event) => {
        if (!interactionEnabled) return;
        const position = point(event);
        if (!position) return;
        pendingPointerRef.current = { buttons: event.buttons, ...position };
        pointerFrameRef.current ??= window.requestAnimationFrame(sendPointerMove);
      }}
      onPointerUp={(event) => {
        const position = point(event);
        if (!position) return;
        if (pointerFrameRef.current !== null) window.cancelAnimationFrame(pointerFrameRef.current);
        sendPointerMove();
        if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
        void onAction({
          sessionId,
          action: "mouse",
          eventType: "mouseReleased",
          button: cdpMouseButton(event.button),
          buttons: event.buttons,
          clickCount: Math.max(1, event.detail),
          modifiers: cdpModifiers(event),
          ...position,
        });
      }}
      ref={hostRef}
      role="application"
      tabIndex={interactionEnabled ? 0 : -1}
    >
      <canvas aria-label="Live browser page" hidden={!painted} ref={canvasRef} />
      {paintError ? <span className="we-browser-frame-error" role="alert">{paintError}</span> : !painted ? <span role="status">Waiting for the first browser frame…</span> : null}
    </div>
  );
}

function cdpMouseButton(button: number): "none" | "left" | "middle" | "right" | "back" | "forward" {
  if (button === 0) return "left";
  if (button === 1) return "middle";
  if (button === 2) return "right";
  if (button === 3) return "back";
  if (button === 4) return "forward";
  return "none";
}

function cdpModifiers(event: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }): number {
  return (event.altKey ? 1 : 0) | (event.ctrlKey ? 2 : 0) | (event.metaKey ? 4 : 0) | (event.shiftKey ? 8 : 0);
}

export type BrowserFixturePage = {
  body: string;
  heading: string;
  title: string;
  url: string;
};

export function FixtureBrowserDocument({ page }: { page: BrowserFixturePage }) {
  return (
    <article className="we-browser-fixture">
      <OctopusEmptyState
        className="h-full min-h-0 flex-1 select-none"
        description={page.body}
        icon={<Glyph name="browser" size={32} />}
        title={page.heading}
      />
    </article>
  );
}

function browserFixturePages(surface?: WorkerEnvironmentSurfaceProjection): BrowserFixturePage[] {
  if (!surface) return [];
  const pages = Array.isArray(surface.data.pages) ? surface.data.pages : [];
  const admitted = pages.flatMap((value) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return [];
    const record = value as Record<string, unknown>;
    const url = typeof record.url === "string" ? record.url.trim() : "";
    if (!url) return [];
    return [{
      url,
      title: typeof record.title === "string" && record.title.trim() ? record.title.trim() : surface.label,
      heading: typeof record.heading === "string" && record.heading.trim() ? record.heading.trim() : "Worker Environment browser",
      body: typeof record.body === "string" && record.body.trim() ? record.body.trim() : "Isolated Test browser surface.",
    }];
  });
  return admitted.length ? admitted : [browserPageFromSurface(surface)];
}

export function browserPageFromSurface(surface?: WorkerEnvironmentSurfaceProjection): BrowserFixturePage {
  return {
    url: surface ? dataText(surface.data, "url") : "",
    title: surface ? dataText(surface.data, "title") || surface.label : "Browser",
    heading: surface ? dataText(surface.data, "heading") || "Worker Environment browser" : "Worker Environment browser",
    body: surface ? dataText(surface.data, "body") || dataText(surface.data, "detail") || "Projected browser evidence." : "Projected browser evidence.",
  };
}

function browserActivity(surface?: WorkerEnvironmentSurfaceProjection) {
  const value = surface?.data.activity;
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (typeof row.tool !== "string" || !Number.isSafeInteger(row.source_movement_seq) || !["running", "completed", "failed", "interrupted"].includes(String(row.status))) return null;
  return { tool: row.tool, sequence: row.source_movement_seq as number, status: String(row.status) };
}

function browserTabs(surface?: WorkerEnvironmentSurfaceProjection) {
  const tabs = surface?.data.tabs;
  if (!Array.isArray(tabs)) return [];
  return tabs.flatMap((value) => {
    if (!value || typeof value !== "object" || typeof value.target_id !== "string") return [];
    return [{ id: value.target_id as string, title: typeof value.title === "string" ? value.title : "", url: typeof value.url === "string" ? value.url : "" }];
  });
}
