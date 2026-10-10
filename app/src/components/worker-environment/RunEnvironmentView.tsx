import { AnimatePresence, motion } from "motion/react";
import { Component, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useClientPreferences } from "../../clientPreferences";
import { createPortal } from "react-dom";
import { useFloatingMenu } from "../../design-system/useFloatingMenu";
import { clientDialogOwnsCommands } from "../../keyboardShortcuts";
import { ClientSurfaceRenderer } from "../../client-surfaces/ClientSurfaceRenderer";
import type { AdmittedClientSurfaceDocument } from "../../client-surfaces/types";
import type { RunProjection, WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import type {
  OctopusClientAccessProfile,
  OctopusWorkerEnvironmentWindowTarget,
} from "../../electron";
import { OcButton, OcIconButton, OcMenuItem, OcMenuMessage, OcMenuSurface } from "../../design-system/components";
import type { GlyphName } from "../IconGlyphs";
import { Glyph } from "../IconGlyphs";
import "./worker-environment.css";
import {
  primaryWorkerEnvironmentSurface,
  workerEnvironmentProductSurfaces,
  workerEnvironmentGlyph,
  workerEnvironmentPanelKind,
  workerEnvironmentSurfaceGlyph,
  workerEnvironmentSurfaceIsPreview,
  type WorkerEnvironmentPanelKind,
} from "./surfaceRegistry";
import { defaultWorkerEnvironmentPaneTarget, paneWorkerEnvironmentSurfaceLabel } from "./workerEnvironmentPane";
import { PaneTabStrip } from "../shell/PaneTabStrip";
import { restorePaneTabs, usePaneTabs } from "../shell/paneTabs";
import { BrowserSurface } from "./RunEnvironmentBrowserSurfaces";
import { SiteTestSurface } from "./RunEnvironmentSiteTestSurface";
import { FilePreviewSurface, FilesSurface, fixtureFileEntries } from "./RunEnvironmentFileSurfaces";
import { TerminalSurface } from "./RunEnvironmentTerminalSurface";
import { LiveSurfaceEmpty, normalizeSurfaceStatus as normalizeStatus } from "./RunEnvironmentSurfacePrimitives";
import {
  ChangesSurface,
  ConnectionSurface,
  ContainerSurface,
  CustomEnvironmentSurface,
  DesktopSurface,
  EnvironmentOverview,
  RuntimeSurface,
  WorktreeSurface,
} from "./RunEnvironmentSubstrateSurfaces";
import type { ProjectedAssistantCodeComment } from "../chat/assistantDirectives";


type EnvironmentPanel = {
  id: string;
  kind: WorkerEnvironmentPanelKind;
  label: string;
  glyph: GlyphName;
  document?: AdmittedClientSurfaceDocument;
  filePath?: string;
  preview?: boolean;
  surface?: WorkerEnvironmentSurfaceProjection;
};

type Props = {
  accessProfile: OctopusClientAccessProfile;
  assistantCodeComments?: ProjectedAssistantCodeComment[];
  maximized: boolean;
  onClose: () => void;
  onMaximizedChange: (maximized: boolean) => void;
  openPanelKind?: WorkerEnvironmentPanelKind;
  openSurfaceId?: string;
  openFilePath?: string;
  openClientSurface?: OctopusWorkerEnvironmentWindowTarget["clientSurface"];
  openRequestId?: number;
  run: RunProjection | null;
  surfaces?: AdmittedClientSurfaceDocument[];
  windowed?: boolean;
  workerEnvironment?: WorkerEnvironmentProjection | null;
};

type StoredEnvironmentLayout = {
  version: 1;
  updatedAt: number;
  activePanelId: string;
  newTabOpen: boolean;
  panels: Array<{
    id: string;
    filePath?: string;
    preview?: boolean;
    surfaceId?: string;
  }>;
};

const ENVIRONMENT_LAYOUT_STORAGE_KEY = "octopus.worker-environment-layouts.v1";
const MAX_STORED_ENVIRONMENT_LAYOUTS = 60;

export function RunEnvironmentView({ accessProfile, assistantCodeComments = [], maximized, onClose, onMaximizedChange, openPanelKind, openSurfaceId, openFilePath, openClientSurface, openRequestId = 0, run, surfaces = [], windowed = false, workerEnvironment }: Props) {
  const clientPreferences = useClientPreferences();
  const availableEnvironments = useMemo(() => {
    if (workerEnvironment) return [workerEnvironment];
    if (run?.workerEnvironments.length) return run.workerEnvironments;
    return run?.workerEnvironment.id ? [run.workerEnvironment] : [];
  }, [run, workerEnvironment]);
  const environmentIds = availableEnvironments.map((candidate) => candidate.id).join(":");
  const [selectedEnvironmentId, setSelectedEnvironmentId] = useState(availableEnvironments[0]?.id ?? "");
  const [environmentMenu, setEnvironmentMenu] = useState<{ x: number; y: number } | null>(null);
  const environmentMenuRef = useRef<HTMLDivElement>(null);
  const handledOpenRequestRef = useRef("");
  const environment = availableEnvironments.find((candidate) => candidate.id === selectedEnvironmentId)
    ?? availableEnvironments[0]
    ?? null;
  const panels = useMemo(
    () => environmentPanels(environment, accessProfile, surfaces),
    [accessProfile, environment, surfaces],
  );
  const refusedFixtureSurfaceCount = accessProfile === "live"
    ? workerEnvironmentProductSurfaces(environment?.surfaces ?? []).filter((surface) => surface.mode === "fixture").length
    : 0;
  const panelSignature = panels.map((panel) => `${panel.id}:${panel.document?.sourceDigest ?? ""}`).join(":");
  const panelTabs = usePaneTabs<EnvironmentPanel>({ activeTabId: "", tabs: [] }, { allowEmptySelection: true });
  const openPanels = panelTabs.tabs;
  const activePanelId = panelTabs.activeTabId;
  const newTabOpen = !activePanelId;
  const presentedPanels = useMemo(() => openPanels.map((panel) => ({ ...panel, title: panel.label })), [openPanels]);
  const viewRef = useRef<HTMLElement>(null);
  const skipLayoutPersistRef = useRef(true);
  const closedPanels = panels.filter((panel) => !openPanels.some((openPanel) => openPanel.id === panel.id));
  const activePanel = activePanelId ? openPanels.find((panel) => panel.id === activePanelId) : undefined;
  const layoutBaseId = environment?.id || run?.id || "";
  const layoutId = layoutBaseId ? `${layoutBaseId}${windowed ? ":window" : ""}` : "";

  useEffect(() => {
    if (availableEnvironments.some((candidate) => candidate.id === selectedEnvironmentId)) return;
    setSelectedEnvironmentId(availableEnvironments[0]?.id ?? "");
  }, [environmentIds, selectedEnvironmentId]);

  useEffect(() => {
    const restored = clientPreferences.restoreWorkerEnvironmentLayouts && layoutId
      ? restoreEnvironmentLayout(accessProfile, layoutId, panels)
      : null;
    const preferred = !restored && clientPreferences.workerEnvironmentDefaultPanel !== "auto"
      ? panels.find((panel) => panel.kind === clientPreferences.workerEnvironmentDefaultPanel)
      : null;
    const defaultTarget = !restored && !preferred && environment
      ? defaultWorkerEnvironmentPaneTarget(environment, surfaces)
      : null;
    const defaultPanel = defaultTarget
      ? requestedEnvironmentPanel(
          panels,
          defaultTarget.panelKind ?? "overview",
          defaultTarget.surfaceId,
          defaultTarget.clientSurface,
        )
      : null;
    const initial = preferred
      ?? defaultPanel
      ?? (restored ? null : panels[0] ?? null);
    skipLayoutPersistRef.current = true;
    panelTabs.replaceState({
      tabs: restored?.panels ?? (initial ? [initial] : []),
      activeTabId: restored?.newTabOpen ? "" : restored?.activePanelId ?? initial?.id ?? "",
    });
  }, [accessProfile, clientPreferences.restoreWorkerEnvironmentLayouts, clientPreferences.workerEnvironmentDefaultPanel, layoutId, panelSignature]);

  useEffect(() => {
    if (!openPanelKind || openRequestId <= 0) return;
    const requestKey = `${layoutId}:${openRequestId}:${openPanelKind}:${openSurfaceId ?? ""}:${openFilePath ?? ""}:${openClientSurface?.sourceDigest ?? ""}`;
    if (handledOpenRequestRef.current === requestKey) return;
    const requested = requestedEnvironmentPanel(panels, openPanelKind, openSurfaceId, openClientSurface);
    const panel = openFilePath && requested?.surface
      ? {
          id: filePanelId(requested.surface.id, openFilePath),
          kind: "file" as const,
          label: basename(openFilePath),
          glyph: "file" as const,
          filePath: openFilePath,
          preview: false,
          surface: requested.surface,
        }
      : requested;
    if (!panel) return;
    handledOpenRequestRef.current = requestKey;
    panelTabs.openTab(panel);
  }, [openClientSurface?.documentId, openClientSurface?.revision, openClientSurface?.sourceDigest, openFilePath, openPanelKind, openRequestId, openSurfaceId, panelSignature]);

  useEffect(() => {
    if (!layoutId || !clientPreferences.restoreWorkerEnvironmentLayouts) return;
    if (skipLayoutPersistRef.current) {
      skipLayoutPersistRef.current = false;
      return;
    }
    persistEnvironmentLayout(accessProfile, layoutId, openPanels, activePanelId, newTabOpen);
  }, [accessProfile, activePanelId, clientPreferences.restoreWorkerEnvironmentLayouts, layoutId, newTabOpen, openPanels]);

  useEffect(() => {
    panelTabs.reconcileTabs(openPanels.flatMap((openPanel): EnvironmentPanel[] => {
      const declared = panels.find((candidate) => candidate.id === openPanel.id);
      if (declared) return [{ ...declared, preview: openPanel.preview }];
      if (openPanel.kind !== "file" || !openPanel.surface?.id) return [];
      const owner = panels.find((candidate) => candidate.surface?.id === openPanel.surface?.id)?.surface;
      return owner ? [{ ...openPanel, surface: owner }] : [];
    }));
  }, [environment?.revision, panelSignature]);

  useEffect(() => {
    const handleWorkspaceShortcut = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || clientDialogOwnsCommands() || !viewRef.current?.contains(event.target as Node)) return;
      const target = event.target as HTMLElement;
      const editing = target.matches("input, textarea, [contenteditable=true]");
      if (event.ctrlKey && !event.altKey && !event.metaKey && event.key === "Tab" && openPanels.length) {
        event.preventDefault();
        const current = Math.max(0, openPanels.findIndex((panel) => panel.id === activePanelId));
        const next = openPanels[(current + (event.shiftKey ? -1 : 1) + openPanels.length) % openPanels.length];
        panelTabs.selectTab(next.id);
        return;
      }
      if (!event.metaKey || event.altKey || event.ctrlKey || editing) return;
      if (event.shiftKey && (event.key === "[" || event.key === "]") && openPanels.length) {
        event.preventDefault();
        const current = Math.max(0, openPanels.findIndex((panel) => panel.id === activePanelId));
        const next = openPanels[(current + (event.key === "]" ? 1 : -1) + openPanels.length) % openPanels.length];
        panelTabs.selectTab(next.id);
        return;
      }
      if (!event.shiftKey && event.key.toLowerCase() === "w" && !newTabOpen && activePanel) {
        event.preventDefault();
        closePanel(activePanel.id);
        return;
      }
      if (!event.shiftKey && event.key.toLowerCase() === "t") {
        event.preventDefault();
        panelTabs.selectTab("");
      }
    };
    window.addEventListener("keydown", handleWorkspaceShortcut);
    return () => window.removeEventListener("keydown", handleWorkspaceShortcut);
  }, [activePanel?.id, activePanelId, newTabOpen, openPanels]);

  useFloatingMenu(Boolean(environmentMenu), environmentMenuRef, () => setEnvironmentMenu(null), environmentMenu ? { left: environmentMenu.x, top: environmentMenu.y } : null);

  function openPanel(panelId: string) {
    const panel = panels.find((candidate) => candidate.id === panelId);
    if (!panel) return;
    panelTabs.openTab(panel);
  }

  function openFilePanel(filePath: string, permanent = false, surface?: WorkerEnvironmentSurfaceProjection) {
    if (!permanent && !clientPreferences.previewWorkerEnvironmentFiles) return;
    if (!surface) return;
    const panel: EnvironmentPanel = {
      id: filePanelId(surface.id, filePath),
      kind: "file",
      label: basename(filePath),
      glyph: "file",
      filePath,
      preview: !permanent,
      surface,
    };
    panelTabs.setTabs((current) => {
      const existingIndex = current.findIndex((candidate) => candidate.id === panel.id);
      if (existingIndex >= 0) {
        if (!permanent || !current[existingIndex].preview) return current;
        return current.map((candidate, index) => index === existingIndex ? { ...candidate, preview: false } : candidate);
      }
      if (permanent) return [...current, { ...panel, preview: false }];
      const previewIndex = current.findIndex((candidate) => candidate.preview);
      if (previewIndex < 0) return [...current, panel];
      return current.map((candidate, index) => index === previewIndex ? panel : candidate);
    });
    panelTabs.selectTab(panel.id);
  }

  function pinPanel(panelId: string) {
    panelTabs.setTabs((current) => current.map((panel) => (
      panel.id === panelId && panel.preview ? { ...panel, preview: false } : panel
    )));
  }

  function closePanel(panelId: string) {
    panelTabs.closeTab(panelId);
  }

  function closeOtherPanels(panelId: string) {
    panelTabs.closeOtherTabs(panelId);
  }

  function closePanelsToRight(panelId: string) {
    panelTabs.closeTabsToRight(panelId);
  }

  if (!environment) {
    return (
      <EnvironmentUnavailable
        detail="The client has no formed Worker Environment projection to resolve."
        title="No Worker Environment"
      />
    );
  }

  const projected = Boolean(environment.id);
  const surfaceHostId = environment.id || run?.id || "worker-environment";

  return (
    <section className="run-environment-view" ref={viewRef}>
      <header className="run-environment-tabbar">
        <button
          aria-label="Worker Environment overview"
          className="run-environment-context"
          disabled={availableEnvironments.length < 2}
          onClick={(event) => {
            if (availableEnvironments.length < 2) return;
            const rect = event.currentTarget.getBoundingClientRect();
            setEnvironmentMenu((current) => current ? null : { x: rect.left, y: rect.bottom + 4 });
          }}
          title={projected ? environment.detail : "Not projected for this run"}
          type="button"
        >
          <Glyph name={workerEnvironmentGlyph(environment)} size={15} />
          <span>{projected ? environment.label : "Worker Environment"}</span>
          <i className={`run-environment-state-dot is-${environment.live ? "active" : normalizeStatus(environment.status)}`} />
          {availableEnvironments.length > 1 ? <Glyph className="run-environment-context-chevron" name="chevronDown" size={12} /> : null}
        </button>
        <PaneTabStrip
          activeTabId={activePanelId}
          ariaLabel="Worker Environment surfaces"
          className="run-environment-tabs-shell"
          onCloseOtherTabs={closeOtherPanels}
          onCloseTabsToRight={closePanelsToRight}
          onTabClose={closePanel}
          onTabPin={pinPanel}
          onTabReorder={panelTabs.reorderTabs}
          onTabSelect={panelTabs.selectTab}
          stretch
          tabs={presentedPanels}
        />
        <div className="run-environment-tab-actions">
          <OcIconButton
            aria-pressed={newTabOpen}
            glyph="plus"
            label="New Worker Environment tab"
            onClick={() => panelTabs.selectTab("")}
            selected={newTabOpen}
            size="sm"
            variant="ghost"
          />
          {!windowed ? (
            <>
              <OcIconButton
                aria-pressed={maximized}
                glyph="expand"
                label={maximized ? "Restore panel width" : "Expand panel"}
                onClick={() => onMaximizedChange(!maximized)}
                selected={maximized}
                size="sm"
                variant="ghost"
              />
              <OcIconButton glyph="close" label="Close Worker Environment" onClick={onClose} size="sm" variant="ghost" />
            </>
          ) : null}
        </div>
      </header>

      {environmentMenu ? createPortal((
        <OcMenuSurface
          aria-label="Worker Environments"
          data-settings-layer="true"
          className="run-environment-context-menu"
          role="menu"
          surfaceRef={environmentMenuRef}
          style={{ left: environmentMenu.x, top: environmentMenu.y }}
        >
          {availableEnvironments.map((candidate) => (
            <OcMenuItem
              description={`${candidate.kind} · ${candidate.status}`}
              key={candidate.id}
              label={candidate.label}
              leadingIcon={workerEnvironmentGlyph(candidate)}
              onClick={() => {
                setSelectedEnvironmentId(candidate.id);
                setEnvironmentMenu(null);
              }}
              role="menuitemradio"
              selected={candidate.id === environment.id}
            />
          ))}
        </OcMenuSurface>
      ), document.body) : null}

      <div className="run-environment-content">
        {openPanels.map((panel) => {
          const active = !newTabOpen && panel.id === activePanel?.id;
          return (
            <div
              aria-hidden={!active}
              aria-label={panel.label}
              className={`run-environment-panel is-persistent${active ? " is-active" : ""}`}
              data-environment-panel-kind={panel.kind}
              inert={!active}
              key={`${surfaceHostId}:${panel.id}`}
              role="tabpanel"
              tabIndex={-1}
            >
              <EnvironmentPanelErrorBoundary resetKey={`${surfaceHostId}:${panel.id}`}>
                <EnvironmentPanelContent accessProfile={accessProfile} active={active} assistantCodeComments={assistantCodeComments} environment={environment} onOpenFile={openFilePanel} panel={panel} run={run} />
              </EnvironmentPanelErrorBoundary>
            </div>
          );
        })}
        <AnimatePresence initial={false}>
          {newTabOpen ? (
            <motion.div
              animate="open"
              className="run-environment-panel is-active"
              exit="closed"
              initial="closed"
              key={`${surfaceHostId}:new-tab`}
              variants={newTabPageMotion}
            >
              <EnvironmentLauncher
                panels={closedPanels}
                refusedFixtureSurfaceCount={refusedFixtureSurfaceCount}
                onOpen={openPanel}
              />
            </motion.div>
          ) : null}
        </AnimatePresence>
      </div>
    </section>
  );
}

export function WorkerEnvironmentSurfacePanel({ accessProfile, assistantCodeComments = [], document, environment, filePath, onOpenFile, run, surface }: {
  accessProfile: OctopusClientAccessProfile;
  assistantCodeComments?: ProjectedAssistantCodeComment[];
  document?: AdmittedClientSurfaceDocument | null;
  environment: WorkerEnvironmentProjection;
  filePath?: string;
  onOpenFile?: (filePath: string, surface: WorkerEnvironmentSurfaceProjection) => void;
  run: RunProjection | null;
  surface: WorkerEnvironmentSurfaceProjection | null;
}) {
  const panel: EnvironmentPanel = document
    ? { document, id: `client:${document.document.id}:${document.sourceDigest}`, kind: "custom", label: document.document.title, glyph: document.document.glyph }
    : surface && filePath
    ? { id: filePanelId(surface.id, filePath), kind: "file", label: basename(filePath), glyph: "file", filePath, surface }
    : surface
      ? environmentPanelFromSurface(environment, surface)
      : { id: "overview", kind: "overview", label: "Overview", glyph: "ee" };
  return (
    <EnvironmentPanelErrorBoundary resetKey={`${environment.id}:${panel.id}`}>
      <EnvironmentPanelContent
        accessProfile={accessProfile}
        active
        assistantCodeComments={assistantCodeComments}
        environment={environment}
        onOpenFile={(path, _permanent, fileSurface) => {
          if (fileSurface) onOpenFile?.(path, fileSurface);
        }}
        panel={panel}
        run={run}
      />
    </EnvironmentPanelErrorBoundary>
  );
}

class EnvironmentPanelErrorBoundary extends Component<{
  children: ReactNode;
  resetKey: string;
}, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidUpdate(previous: Readonly<{ children: ReactNode; resetKey: string }>) {
    if (previous.resetKey !== this.props.resetKey && this.state.failed) this.setState({ failed: false });
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="we-panel-render-error" role="alert">
        <strong>Tab content couldn&apos;t render</strong>
        <OcButton onClick={() => this.setState({ failed: false })} size="sm" variant="secondary">Try again</OcButton>
      </div>
    );
  }
}

function EnvironmentPanelContent({ accessProfile, active, assistantCodeComments = [], environment, onOpenFile, panel, run }: {
  accessProfile: OctopusClientAccessProfile;
  active: boolean;
  assistantCodeComments?: ProjectedAssistantCodeComment[];
  environment: WorkerEnvironmentProjection;
  onOpenFile: (filePath: string, permanent?: boolean, surface?: WorkerEnvironmentSurfaceProjection) => void;
  panel: EnvironmentPanel;
  run: RunProjection | null;
}) {
  if (accessProfile === "live" && panel.surface?.mode === "fixture") {
    return (
      <LiveSurfaceEmpty
        detail="Fixture surfaces belong to the isolated Test profile and cannot be opened by Administrator."
        glyph="ee"
        title="Test-only surface refused"
      />
    );
  }
  if (panel.document) return <ClientSurfaceRenderer accessProfile={accessProfile} admitted={panel.document} />;
  if (panel.kind === "browser" || panel.surface?.protocol === "browser.native.v1") {
    return panel.surface && workerEnvironmentSurfaceIsPreview(panel.surface)
      ? <SiteTestSurface environment={environment} surface={panel.surface} />
      : <BrowserSurface active={active} environment={environment} surface={panel.surface} />;
  }
  if (panel.kind === "connection") return <ConnectionSurface surface={panel.surface} />;
  if (panel.kind === "container") return <ContainerSurface surface={panel.surface} />;
  if (panel.kind === "runtime") return <RuntimeSurface surface={panel.surface} />;
  if (panel.kind === "worktree") return <WorktreeSurface surface={panel.surface} />;
  if (panel.kind === "files") return <FilesSurface active={active} environment={environment} onOpenFile={onOpenFile} surface={panel.surface} />;
  if (panel.kind === "terminal") return <TerminalSurface active={active} environment={environment} surface={panel.surface} />;
  if (panel.kind === "file" && panel.filePath) return <FilePreviewSurface active={active} environment={environment} path={panel.filePath} surface={panel.surface} />;
  if (panel.kind === "changes") return <ChangesSurface active={active} assistantCodeComments={assistantCodeComments} environment={environment} onOpenFile={onOpenFile} run={run} surface={panel.surface} />;
  if (panel.kind === "desktop") return <DesktopSurface active={active} environment={environment} surface={panel.surface} />;
  if (panel.kind === "custom") return <CustomEnvironmentSurface surface={panel.surface} />;
  return <EnvironmentOverview environment={environment} />;
}

function EnvironmentLauncher({ panels, refusedFixtureSurfaceCount, onOpen }: {
  panels: EnvironmentPanel[];
  refusedFixtureSurfaceCount: number;
  onOpen: (panel: string) => void;
}) {
  const [query, setQuery] = useState("");
  const normalizedQuery = query.trim().toLowerCase();
  const visiblePanels = normalizedQuery
    ? panels.filter((panel) => `${panel.label} ${panel.kind} ${panel.surface?.protocol ?? ""}`.toLowerCase().includes(normalizedQuery))
    : panels;
  return (
    <div className="we-new-tab">
      <div className="we-new-tab-content">
        {panels.length > 6 ? (
          <label className="we-new-tab-search">
            <Glyph name="search" size={14} />
            <input aria-label="Search Worker Environment tools" autoFocus onChange={(event) => setQuery(event.currentTarget.value)} placeholder="Search tools" value={query} />
            {query ? <button aria-label="Clear surface search" onClick={() => setQuery("")} type="button"><Glyph name="close" size={12} /></button> : null}
          </label>
        ) : null}
        {visiblePanels.length ? (
          <motion.ul className="we-new-tab-list" variants={newTabListMotion}>
            {visiblePanels.map((panel) => (
              <motion.li key={panel.id} variants={newTabItemMotion}>
                <OcMenuItem description={environmentPanelDescription(panel)} label={panel.label} leadingIcon={panel.glyph} onClick={() => onOpen(panel.id)} />
              </motion.li>
            ))}
          </motion.ul>
        ) : (
          <OcMenuMessage className="we-new-tab-empty">{panels.length ? "No surfaces match this search." : "No surfaces are available for this Worker Environment."}</OcMenuMessage>
        )}
        {refusedFixtureSurfaceCount > 0 ? (
          <OcMenuMessage className="we-new-tab-empty">
            {refusedFixtureSurfaceCount} Test-only {refusedFixtureSurfaceCount === 1 ? "surface was" : "surfaces were"} refused.
          </OcMenuMessage>
        ) : null}
      </div>
    </div>
  );
}

function environmentPanelDescription(panel: EnvironmentPanel): string {
  const surface = panel.surface;
  if (!surface) return panel.kind === "overview" ? "Door, capabilities, tools, roots, and room state" : "Open";
  if (panel.kind === "changes") return "Review project changes";
  if (panel.kind === "terminal") return "Run commands in this environment";
  if (panel.kind === "files") return surface.readOnly ? "Browse projected files" : "Browse and edit projected files";
  if (panel.kind === "browser") return workerEnvironmentSurfaceIsPreview(surface) ? "Open preview" : "Open browser";
  if (panel.kind === "desktop") return "Open desktop";
  return "Open";
}

const newTabPageMotion = {
  closed: { opacity: 0, y: -8, transition: { duration: 0.06, ease: [0.19, 1, 0.22, 1] as const, when: "afterChildren" as const } },
  open: { opacity: 1, y: 0, transition: { duration: 0.09, ease: [0.19, 1, 0.22, 1] as const, when: "beforeChildren" as const } },
};

const newTabListMotion = {
  closed: { transition: { staggerChildren: 0.008, staggerDirection: -1 } },
  open: { transition: { delayChildren: 0.012, staggerChildren: 0.016 } },
};

const newTabItemMotion = {
  closed: { opacity: 0, y: -6, transition: { duration: 0.045, ease: [0.19, 1, 0.22, 1] as const } },
  open: { opacity: 1, y: 0, transition: { duration: 0.075, ease: [0.19, 1, 0.22, 1] as const } },
};

function EnvironmentUnavailable({ detail, title }: { detail: string; title: string }) {
  return (
    <section className="run-environment-view">
      <div className="run-environment-content">
        <LiveSurfaceEmpty detail={detail} glyph="ee" title={title} />
      </div>
    </section>
  );
}

function environmentPanels(
  environment: WorkerEnvironmentProjection | null,
  accessProfile: OctopusClientAccessProfile,
  clientSurfaces: AdmittedClientSurfaceDocument[],
): EnvironmentPanel[] {
  if (!environment) return [{ id: "overview", kind: "overview", label: "Overview", glyph: "ee" }];
  const panels: EnvironmentPanel[] = [{ id: "overview", kind: "overview", label: "Overview", glyph: "ee" }];
  panels.push(...workerEnvironmentProductSurfaces(environment.surfaces)
    .filter((surface) => accessProfile === "lab" || surface.mode !== "fixture")
    .map((surface) => environmentPanelFromSurface(environment, surface)));
  const clientPanels = clientSurfaces.flatMap((admitted): EnvironmentPanel[] => {
    const target = admitted.document.target;
    if (target.kind !== "worker_environment.surface" || target.worker_environment_id !== environment.id) return [];
    return [{
      document: admitted,
      glyph: admitted.document.glyph,
      id: `client:${target.surface_id}`,
      kind: "custom",
      label: admitted.document.title,
    }];
  }).sort((left, right) => {
    const leftTarget = left.document?.document.target;
    const rightTarget = right.document?.document.target;
    const leftOrder = leftTarget?.kind === "worker_environment.surface" ? leftTarget.order : 0;
    const rightOrder = rightTarget?.kind === "worker_environment.surface" ? rightTarget.order : 0;
    return leftOrder - rightOrder || left.label.localeCompare(right.label);
  });
  panels.push(...clientPanels);
  return panels;
}

function environmentPanelFromSurface(environment: WorkerEnvironmentProjection, surface: WorkerEnvironmentSurfaceProjection): EnvironmentPanel {
  const kind = workerEnvironmentPanelKind(surface);
  return {
    id: surface.id,
    kind,
    label: paneWorkerEnvironmentSurfaceLabel(environment, surface),
    glyph: workerEnvironmentSurfaceGlyph(surface),
    surface,
  };
}

function requestedEnvironmentPanel(
  panels: EnvironmentPanel[],
  panelKind: WorkerEnvironmentPanelKind,
  surfaceId?: string,
  clientSurface?: OctopusWorkerEnvironmentWindowTarget["clientSurface"],
): EnvironmentPanel | null {
  if (clientSurface) {
    return panels.find((panel) => {
      const target = panel.document?.document.target;
      return target?.kind === "worker_environment.surface"
        && (!surfaceId || target.surface_id === surfaceId)
        && panel.document?.document.id === clientSurface.documentId
        && panel.document.document.revision === clientSurface.revision
        && panel.document.sourceDigest === clientSurface.sourceDigest;
    }) ?? null;
  }
  if (surfaceId) {
    return panels.find((panel) => {
      if (panel.surface?.id === surfaceId) return true;
      const target = panel.document?.document.target;
      return target?.kind === "worker_environment.surface" && target.surface_id === surfaceId;
    }) ?? null;
  }
  return panels.find((panel) => panel.kind === panelKind) ?? null;
}


function basename(value: string): string {
  const normalized = value.replace(/[\\/]+$/, "");
  return normalized.split(/[\\/]/).filter(Boolean).at(-1) || value;
}

function filePanelId(surfaceId: string, filePath: string): string {
  return `file:${surfaceId}:${filePath}`;
}

function restoreEnvironmentLayout(accessProfile: OctopusClientAccessProfile, layoutId: string, availablePanels: EnvironmentPanel[]): {
  panels: EnvironmentPanel[];
  activePanelId: string;
  newTabOpen: boolean;
} | null {
  const stored = readEnvironmentLayouts(accessProfile)[layoutId];
  if (!stored) return null;
  const byId = new Map(availablePanels.map((panel) => [panel.id, panel]));
  const restored = restorePaneTabs({
    activeTabId: stored.newTabOpen ? "" : stored.activePanelId,
    allowEmptySelection: stored.newTabOpen,
    storedTabs: stored.panels,
    resolve: (candidate) => {
      const declared = byId.get(candidate.id);
      if (declared) return { id: candidate.id, tab: declared };
      if (!candidate.filePath || !candidate.surfaceId || candidate.id !== filePanelId(candidate.surfaceId, candidate.filePath)) {
        return { id: candidate.id, tab: null };
      }
      const owner = availablePanels.find((panel) => panel.surface?.id === candidate.surfaceId)?.surface;
      if (!owner) return { id: candidate.id, tab: null };
      if (owner.mode !== "direct" && !fixtureFileEntries(owner).some((entry) => entry.kind === "file" && entry.path === candidate.filePath)) {
        return { id: candidate.id, tab: null };
      }
      return {
        id: candidate.id,
        tab: {
          id: candidate.id,
          kind: "file" as const,
          label: basename(candidate.filePath),
          glyph: "file" as const,
          filePath: candidate.filePath,
          preview: candidate.preview === true,
          surface: owner,
        },
      };
    },
  });
  return {
    panels: restored.tabs,
    activePanelId: restored.activeTabId,
    newTabOpen: !restored.activeTabId,
  };
}

function persistEnvironmentLayout(
  accessProfile: OctopusClientAccessProfile,
  layoutId: string,
  panels: EnvironmentPanel[],
  activePanelId: string,
  newTabOpen: boolean,
) {
  const layouts = readEnvironmentLayouts(accessProfile);
  layouts[layoutId] = {
    version: 1,
    updatedAt: Date.now(),
    activePanelId,
    newTabOpen,
    panels: panels.slice(0, 24).map((panel) => ({
      id: panel.id,
      ...(panel.filePath ? { filePath: panel.filePath } : {}),
      ...(panel.preview ? { preview: true } : {}),
      ...(panel.surface?.id ? { surfaceId: panel.surface.id } : {}),
    })),
  };
  const pruned = Object.fromEntries(
    Object.entries(layouts)
      .sort((left, right) => right[1].updatedAt - left[1].updatedAt)
      .slice(0, MAX_STORED_ENVIRONMENT_LAYOUTS),
  );
  try {
    window.localStorage.setItem(environmentLayoutStorageKey(accessProfile), JSON.stringify(pruned));
  } catch {
    return;
  }
}

function environmentLayoutStorageKey(accessProfile: OctopusClientAccessProfile): string {
  return `${ENVIRONMENT_LAYOUT_STORAGE_KEY}.${accessProfile}`;
}

function readEnvironmentLayouts(accessProfile: OctopusClientAccessProfile): Record<string, StoredEnvironmentLayout> {
  try {
    const raw = window.localStorage.getItem(environmentLayoutStorageKey(accessProfile));
    if (!raw || raw.length > 256 * 1024) return {};
    const value = JSON.parse(raw) as unknown;
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value).flatMap(([key, candidate]) => {
      if (!key || !candidate || typeof candidate !== "object" || Array.isArray(candidate)) return [];
      const layout = candidate as Partial<StoredEnvironmentLayout>;
      if (
        layout.version !== 1
        || typeof layout.updatedAt !== "number"
        || !Number.isFinite(layout.updatedAt)
        || typeof layout.activePanelId !== "string"
        || typeof layout.newTabOpen !== "boolean"
        || !Array.isArray(layout.panels)
      ) return [];
      const panels = layout.panels.flatMap((panel) => {
        if (!panel || typeof panel !== "object" || Array.isArray(panel)) return [];
        const record = panel as Record<string, unknown>;
        if (typeof record.id !== "string" || !record.id || record.id.length > 512) return [];
        if (record.filePath !== undefined && (typeof record.filePath !== "string" || record.filePath.length > 4096)) return [];
        if (record.surfaceId !== undefined && (typeof record.surfaceId !== "string" || record.surfaceId.length > 512)) return [];
        return [{
          id: record.id,
          ...(typeof record.filePath === "string" ? { filePath: record.filePath } : {}),
          ...(record.preview === true ? { preview: true } : {}),
          ...(typeof record.surfaceId === "string" ? { surfaceId: record.surfaceId } : {}),
        }];
      });
      return [[key, { ...layout, panels } as StoredEnvironmentLayout]];
    }));
  } catch {
    return {};
  }
}
