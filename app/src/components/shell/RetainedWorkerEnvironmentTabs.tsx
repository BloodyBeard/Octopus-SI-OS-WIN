import { lazy, Suspense, useState } from "react";
import type { AdmittedClientSurfaceDocument } from "../../client-surfaces/types";
import type { TopologySnapshot } from "../../data/topology";
import type { OctopusClientAccessProfile } from "../../electron";
import type { WorkspacePaneTabState } from "../../shell/clientWindowModel";
import type { ProjectedAssistantCodeComment } from "../chat/assistantDirectives";
import type { WorkerEnvironmentPaneTarget } from "../worker-environment/workerEnvironmentPane";
import { SurfaceLoadingState, SurfaceRenderBoundary } from "./SurfaceBoundary";

const WorkerEnvironmentPaneTab = lazy(() => import("../worker-environment/WorkerEnvironmentPaneTab").then((module) => ({ default: module.WorkerEnvironmentPaneTab })));

type Props = {
  accessProfile: OctopusClientAccessProfile;
  active: boolean;
  activeTabId: string;
  assistantCodeComments?: ProjectedAssistantCodeComment[];
  snapshot: TopologySnapshot;
  surfaces: AdmittedClientSurfaceDocument[];
  tabs: WorkspacePaneTabState[];
};

export function RetainedWorkerEnvironmentTabs({ accessProfile, active, activeTabId, assistantCodeComments, snapshot, surfaces, tabs }: Props) {
  return tabs.map((tab) => tab.target.kind === "worker_environment" ? (
    <WorkerEnvironmentTab
      accessProfile={accessProfile}
      active={active && tab.id === activeTabId}
      assistantCodeComments={active && tab.id === activeTabId ? assistantCodeComments : undefined}
      key={JSON.stringify([accessProfile, tab.id])}
      snapshot={snapshot}
      surfaces={surfaces}
      tabId={tab.id}
      target={tab.target.target}
    />
  ) : null);
}

function WorkerEnvironmentTab({ accessProfile, active, assistantCodeComments, snapshot, surfaces, tabId, target }: Omit<Props, "activeTabId" | "tabs"> & { tabId: string; target: WorkerEnvironmentPaneTarget }) {
  const [visited, setVisited] = useState(active);
  if (active && !visited) setVisited(true);
  const retain = target.panelKind === "file" && target.surfaceId && target.filePath && !target.clientSurface;
  if (!active && (!visited || !retain)) return null;
  return (
    <div
      aria-hidden={!active || undefined}
      className="flex min-h-0 flex-1 flex-col overflow-hidden"
      hidden={!active}
      inert={!active}
      style={{ display: active ? "flex" : "none" }}
    >
      <SurfaceRenderBoundary resetKey={tabId}>
        <Suspense fallback={<SurfaceLoadingState label="Loading environment…" />}>
          <WorkerEnvironmentPaneTab
            accessProfile={accessProfile}
            assistantCodeComments={assistantCodeComments}
            snapshot={snapshot}
            surfaces={surfaces}
            target={target}
          />
        </Suspense>
      </SurfaceRenderBoundary>
    </div>
  );
}
