import { lazy, Suspense } from "react";
import type { HomeViewMode } from "../voice/VoiceView";
import type { AdmittedClientSurfaceDocument, ClientSurfaceHostSource } from "../../client-surfaces/types";
import type { VoiceSession } from "../voice/useVoiceSession";
import { OcPaneGroup } from "../../design-system/layout";
import type { SelectionRef, TopologySnapshot } from "../../data/topology";
import type { DashboardFilters, DashboardScope, DashboardTopologyMode } from "../../uiState";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import type { ChatWorkspace } from "../chat";
import type { OctopusClientAccessProfile } from "../../electron";
import type { FilesOpenTarget } from "../files/types";
import { OctopusLoadingState } from "../OctopusLoadingLogo";
import { SurfaceLoadingState, SurfaceRenderBoundary } from "./SurfaceBoundary";
import type { InboxSurfaceState } from "../activity/InboxSurface";

const InboxSurface = lazy(() => import("../activity/InboxSurface").then((module) => ({ default: module.InboxSurface })));

const ChatView = lazy(() => import("../ChatView").then((module) => ({ default: module.ChatView })));
const CustomProjectionSurface = lazy(() => import("../CustomProjectionSurface").then((module) => ({ default: module.CustomProjectionSurface })));
const StaticSurface = lazy(() => import("../CustomProjectionSurface").then((module) => ({ default: module.StaticSurface })));
const EventLogView = lazy(() => import("../EventLogView").then((module) => ({ default: module.EventLogView })));
const Inspector = lazy(() => import("../Inspector").then((module) => ({ default: module.Inspector })));
const ApplicationsView = lazy(() => import("../ApplicationsView").then((module) => ({ default: module.ApplicationsView })));
const DriversView = lazy(() => import("../DriversView").then((module) => ({ default: module.DriversView })));
const RegistryView = lazy(() => import("../RegistryView").then((module) => ({ default: module.RegistryView })));
const ScheduledView = lazy(() => import("../scheduled/LiveScheduledView").then((module) => ({ default: module.LiveScheduledView })));
const TopologyStage = lazy(() => import("../TopologyStage").then((module) => ({ default: module.TopologyStage })));
const WorkerEnvironmentsView = lazy(() => import("../WorkerEnvironmentsView").then((module) => ({ default: module.WorkerEnvironmentsView })));
const HomeSurface = lazy(() => import("./HomeSurface").then((module) => ({ default: module.HomeSurface })));
const ClientSurfaceRenderer = lazy(() => import("../../client-surfaces/ClientSurfaceRenderer").then((module) => ({ default: module.ClientSurfaceRenderer })));
const BuiltinClientExtensionSurface = lazy(() => import("../../extensions/builtinSurfaceContributions").then((module) => ({ default: module.BuiltinClientExtensionSurface })));

export type SurfaceHostProps = {
  inbox?: InboxSurfaceState;
  focusedMovement?: { id: string; sequence: number | null } | null;
  onClearMovement?: () => void;
  accessProfile: OctopusClientAccessProfile;
  surface: ClientSurfaceDescriptor;
  topology: TopologySnapshot;
  query: string;
  onQueryChange: (value: string) => void;
  searchPlaceholder: string;
  searchFocusTick: number;
  dashboardScope: DashboardScope;
  dashboardTopologyMode: DashboardTopologyMode;
  dashboardFilters: DashboardFilters;
  selected: SelectionRef | null;
  chatWorkspace: ChatWorkspace;
  homeRunId: string;
  voiceSession: VoiceSession;
  homeViewMode: HomeViewMode;
  onHomeViewModeChange?: (mode: HomeViewMode) => void;
  paneRole: "primary" | "secondary";
  paneTabStripVisible?: boolean;
  commandsActive: boolean;
  inspectorOpen: boolean;
  onSelect: (selection: SelectionRef | null) => void;
  onCloseInspector: () => void;
  onOpenFiles: (target: FilesOpenTarget) => void;
  voiceSurfaces: AdmittedClientSurfaceDocument[];
  installedVoiceWidgetIds: string[];
  installedVoiceWidgetsReady: boolean;
  builtinVoiceWidgetSources: Record<string, ClientSurfaceHostSource[]>;
  workerEnvironmentSurfaces: AdmittedClientSurfaceDocument[];
  onOpenRun?: (runId: string, role: "home" | "task") => void;
};

export function SurfaceHost(props: SurfaceHostProps) {
  return <SurfaceRenderBoundary resetKey={JSON.stringify([props.accessProfile, props.surface.id, props.chatWorkspace.activeRunId])}>
    <SurfaceContent {...props} />
  </SurfaceRenderBoundary>;
}

function SurfaceContent({
  inbox,
  focusedMovement,
  onClearMovement,
  accessProfile,
  surface,
  topology,
  query,
  onQueryChange,
  searchPlaceholder,
  searchFocusTick,
  dashboardScope,
  dashboardTopologyMode,
  dashboardFilters,
  selected,
  chatWorkspace,
  homeRunId,
  voiceSession,
  homeViewMode,
  onHomeViewModeChange,
  paneRole,
  paneTabStripVisible = false,
  commandsActive,
  inspectorOpen,
  onSelect,
  onCloseInspector,
  onOpenFiles,
  voiceSurfaces,
  installedVoiceWidgetIds,
  installedVoiceWidgetsReady,
  builtinVoiceWidgetSources,
  workerEnvironmentSurfaces,
  onOpenRun,
}: SurfaceHostProps) {
  if (surface.rendererKind === "builtin.inbox") {
    return inbox ? <Suspense fallback={<SurfaceLoadingState label="Loading Inbox…" />}><InboxSurface {...inbox} /></Suspense>
      : <div className="inbox-summary" role="alert">Inbox activity is unavailable.</div>;
  }
  const surfaceFallback = surface.rendererKind === "builtin.chat" || surface.rendererKind === "builtin.voice"
    ? <OctopusLoadingState className="surface-content-loading-state" label={surface.rendererKind === "builtin.chat" ? "Loading task" : "Opening Home"} />
    : <SurfaceLoadingState label={surfaceLoadingLabel(surface)} />;
  if (surface.rendererKind === "builtin.dashboard") {
    return (
      <OcPaneGroup
        className={`workspace-grid${inspectorOpen ? "" : " is-inspector-collapsed"}`}
        asideOpen={inspectorOpen}
        mainLabel="Topology"
        asideLabel="Inspector"
        main={(
          <Suspense fallback={<SurfaceLoadingState label="Loading topology…" />}>
            <TopologyStage
              snapshot={topology}
              query={query}
              scope={dashboardScope}
              viewMode={dashboardTopologyMode}
              filters={dashboardFilters}
              selected={selected}
              onSelect={onSelect}
            />
          </Suspense>
        )}
        aside={<Suspense fallback={<SurfaceLoadingState label="Loading inspector…" />}><Inspector snapshot={topology} selection={selected} onSelect={onSelect} onClose={onCloseInspector} surfaces={workerEnvironmentSurfaces} /></Suspense>}
      />
    );
  }
  if (surface.rendererKind === "builtin.event_log") {
    return (
      <Suspense fallback={<SurfaceLoadingState label="Loading movements…" />}>
        <EventLogView
          accessProfile={accessProfile}
          focusedMovement={focusedMovement}
          onClearMovement={onClearMovement}
          onQueryChange={onQueryChange}
          placeholder={searchPlaceholder}
          query={query}
          searchFocusTick={searchFocusTick}
          title={surface.label}
          titleInWindowChrome={paneRole === "primary" || paneTabStripVisible}
        />
      </Suspense>
    );
  }

  return (
    <div className={workspacePanelClass(surface)}>
      <Suspense fallback={surfaceFallback}>
        {renderSurface(accessProfile, surface, topology, query, searchFocusTick, chatWorkspace, homeRunId, voiceSession, homeViewMode, onHomeViewModeChange, paneRole, paneTabStripVisible, commandsActive, onSelect, onOpenFiles, voiceSurfaces, installedVoiceWidgetIds, installedVoiceWidgetsReady, builtinVoiceWidgetSources, workerEnvironmentSurfaces, onOpenRun)}
      </Suspense>
    </div>
  );
}

function renderSurface(
  accessProfile: OctopusClientAccessProfile,
  surface: ClientSurfaceDescriptor,
  topology: TopologySnapshot,
  query: string,
  searchFocusTick: number,
  chatWorkspace: ChatWorkspace,
  homeRunId: string,
  voiceSession: VoiceSession,
  homeViewMode: HomeViewMode,
  onHomeViewModeChange: SurfaceHostProps["onHomeViewModeChange"],
  paneRole: "primary" | "secondary",
  paneTabStripVisible: boolean,
  commandsActive: boolean,
  onSelect: (selection: SelectionRef | null) => void,
  onOpenFiles: (target: FilesOpenTarget) => void,
  voiceSurfaces: AdmittedClientSurfaceDocument[],
  installedVoiceWidgetIds: string[],
  installedVoiceWidgetsReady: boolean,
  builtinVoiceWidgetSources: Record<string, ClientSurfaceHostSource[]>,
  workerEnvironmentSurfaces: AdmittedClientSurfaceDocument[],
  onOpenRun: SurfaceHostProps["onOpenRun"],
) {
  switch (surface.rendererKind) {
    case "builtin.registry":
      return <RegistryView accessProfile={accessProfile} onOpenFiles={onOpenFiles} query={query} />;
    case "extension.builtin":
      return surface.extension
        ? <BuiltinClientExtensionSurface query={query} searchFocusTick={searchFocusTick} surfaceId={surface.extension.surfaceId} />
        : <StaticSurface surface={{ ...surface, staticContent: { title: "Extension unavailable", body: "The installed extension has no admitted surface contribution." } }} />;
    case "builtin.applications":
      return <ApplicationsView accessProfile={accessProfile} searchFocusTick={searchFocusTick} />;
    case "builtin.drivers":
      return <DriversView accessProfile={accessProfile} searchFocusTick={searchFocusTick} />;
    case "builtin.chat":
      return <ChatView commandsActive={commandsActive} composerVisible={chatWorkspace.activeRun?.writable !== false} headerMode={paneRole === "primary" ? "window" : paneTabStripVisible ? "none" : "pane"} onOpenFiles={onOpenFiles} searchFocusTick={searchFocusTick} surfaces={workerEnvironmentSurfaces} topology={topology} workspace={chatWorkspace} />;
    case "builtin.voice":
      return <HomeSurface accessProfile={accessProfile} commandsActive={commandsActive} homeRunId={homeRunId} voiceSession={voiceSession} homeViewMode={homeViewMode} onHomeViewModeChange={onHomeViewModeChange} searchFocusTick={searchFocusTick} chatWorkspace={chatWorkspace} onOpenFiles={onOpenFiles} voiceSurfaces={voiceSurfaces} installedVoiceWidgetIds={installedVoiceWidgetIds} installedVoiceWidgetsReady={installedVoiceWidgetsReady} builtinVoiceWidgetSources={builtinVoiceWidgetSources} workerEnvironmentSurfaces={workerEnvironmentSurfaces} topology={topology} />;
    case "builtin.scheduled":
      return <ScheduledView searchFocusTick={searchFocusTick} navigationInPane={paneRole === "secondary"} />;
    case "builtin.worker_environments":
      return <WorkerEnvironmentsView accessProfile={accessProfile} chatRuns={chatWorkspace.runs} onOpenRun={onOpenRun} onSelect={onSelect} searchFocusTick={searchFocusTick} snapshot={topology} surfaces={workerEnvironmentSurfaces} />;
    case "custom.projection":
      return <CustomProjectionSurface surface={surface} query={query} />;
    case "custom.document":
      return surface.document
        ? <ClientSurfaceRenderer accessProfile={accessProfile} admitted={surface.document} />
        : <StaticSurface surface={surface} />;
    case "custom.static":
      return <StaticSurface surface={surface} />;
    default:
      return <StaticSurface surface={surface} />;
  }
}

function workspacePanelClass(surface: ClientSurfaceDescriptor): string {
  const classes = ["workspace-panel", `surface-${safeClass(surface.id)}`];
  if (surface.chrome.workspace === "immersive") classes.push("is-immersive");
  if (surface.rendererKind === "builtin.chat") classes.push("is-chat");
  if (surface.rendererKind === "builtin.voice") classes.push("is-voice");
  if (surface.hostClassName) classes.push(surface.hostClassName);
  return classes.join(" ");
}

function safeClass(value: string): string {
  return value.replace(/[^a-z0-9_-]/g, "-");
}

function surfaceLoadingLabel(surface: ClientSurfaceDescriptor): string {
  if (surface.rendererKind === "builtin.voice") return "Loading Home…";
  return `Loading ${surface.label}…`;
}
