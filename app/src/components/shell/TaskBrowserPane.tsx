import { useMotionValue } from "motion/react";
import { lazy, Suspense, useState, type ReactNode } from "react";
import type { TopologySnapshot } from "../../data/topology";
import type { OctopusClientAccessProfile } from "../../electron";
import type { AdmittedClientSurfaceDocument } from "../../client-surfaces/types";
import { appToast } from "../../toasts";
import { isControllableNativeBrowserSurface, paneTargetTitle, resolvePaneSurface, resolveWorkerEnvironment, type WorkerEnvironmentPaneTarget } from "../worker-environment/workerEnvironmentPane";
import { WorkspacePaneManager } from "./WorkspacePaneManager";
import { WorkspaceSurfacePane } from "./WorkspaceSurfacePane";
import { closePaneTab, type PaneTabState } from "./paneTabs";
import { SurfaceLoadingState } from "./SurfaceBoundary";

const WorkerEnvironmentPaneTab = lazy(() => import("../worker-environment/WorkerEnvironmentPaneTab").then((module) => ({ default: module.WorkerEnvironmentPaneTab })));

export type TaskBrowserTab = { id: string; target: WorkerEnvironmentPaneTarget };
export type TaskBrowserTabs = PaneTabState<TaskBrowserTab> & { open: boolean };
export const emptyTaskBrowserTabs: TaskBrowserTabs = { activeTabId: "", tabs: [], open: false };

type Props = {
  accessProfile: OctopusClientAccessProfile;
  active: boolean;
  children: ReactNode;
  onChange: (update: (current: TaskBrowserTabs) => TaskBrowserTabs) => void;
  snapshot: TopologySnapshot;
  state: TaskBrowserTabs;
  surfaces: AdmittedClientSurfaceDocument[];
};

export function TaskBrowserPane({ accessProfile, active, children, onChange, snapshot, state, surfaces }: Props) {
  const width = useMotionValue(0);
  const [widthPercent, setWidthPercent] = useState(50);
  const [focused, setFocused] = useState(false);
  const selected = state.tabs.find((tab) => tab.id === state.activeTabId);
  const tabs = state.tabs.map((tab) => {
    const room = resolveWorkerEnvironment(snapshot, tab.target.workerEnvironmentId);
    return { id: tab.id, showWhenSingle: true, ...(room ? paneTargetTitle(room.workerEnvironment, tab.target) : { title: "Environment", glyph: "browser" as const }) };
  });
  const close = () => { setFocused(false); onChange((current) => ({ ...current, open: false })); };
  const closeTabs = async (ids: string[]) => {
    try {
      for (const id of ids) {
        const tab = state.tabs.find((candidate) => candidate.id === id);
        if (!tab) continue;
        const room = resolveWorkerEnvironment(snapshot, tab.target.workerEnvironmentId);
        const surface = room && resolvePaneSurface(room.workerEnvironment, tab.target);
        if (isControllableNativeBrowserSurface(surface)) {
          if (!window.octopusTransport?.closeNativeBrowserPage) throw new Error("The native browser is not connected.");
          await window.octopusTransport.closeNativeBrowserPage({ home: snapshot.projection.endpoint, workerEnvironmentId: tab.target.workerEnvironmentId, surfaceId: surface.id });
        }
        onChange((current) => ({ ...current, ...closePaneTab(current, id, false) }));
      }
    } catch (error) { appToast.danger(error instanceof Error ? error.message : String(error)); }
  };
  return <WorkspacePaneManager
    primary={children}
    primaryPaneRole="secondary"
    secondaryOpen={active && state.open && Boolean(selected)}
    secondaryFocused={active && state.open && Boolean(selected) && focused}
    secondaryWidthPercent={widthPercent}
    resolvedWidthMotion={width}
    onOpenChange={(open) => { if (!open) close(); }}
    onSecondaryFocusedChange={setFocused}
    onSecondaryWidthPercentChange={setWidthPercent}
    secondary={<WorkspaceSurfacePane
      activeTabId={state.activeTabId}
      addActions={[]}
      focused={focused}
      tabs={tabs}
      onAddAction={() => {}}
      onClose={close}
      onFocusedChange={setFocused}
      onTabClose={(id) => { void closeTabs([id]); }}
      onCloseOtherTabs={(id) => { void closeTabs(state.tabs.filter((tab) => tab.id !== id).map((tab) => tab.id)); }}
      onCloseTabsToRight={(id) => { const index = state.tabs.findIndex((tab) => tab.id === id); if (index >= 0) void closeTabs(state.tabs.slice(index + 1).map((tab) => tab.id)); }}
      onTabReorder={(id, over) => onChange((current) => {
        const from = current.tabs.findIndex((tab) => tab.id === id), to = current.tabs.findIndex((tab) => tab.id === over);
        if (from < 0 || to < 0 || from === to) return current;
        const next = [...current.tabs];
        next.splice(to, 0, next.splice(from, 1)[0]);
        return { ...current, tabs: next };
      })}
      onTabSelect={(id) => onChange((current) => current.tabs.some((tab) => tab.id === id) ? { ...current, activeTabId: id } : current)}
    >
      <Suspense fallback={<SurfaceLoadingState label="Opening environment…" />}>
        {active && state.open && selected ? <WorkerEnvironmentPaneTab accessProfile={accessProfile} snapshot={snapshot} surfaces={surfaces} target={selected.target} /> : null}
      </Suspense>
    </WorkspaceSurfacePane>}
  />;
}
