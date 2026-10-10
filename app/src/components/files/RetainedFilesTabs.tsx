import { lazy, Suspense, useState } from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import type { WorkspacePaneTabState } from "../../shell/clientWindowModel";
import { SurfaceLoadingState, SurfaceRenderBoundary } from "../shell/SurfaceBoundary";
import { filesPaneKey } from "./filesPaneLifecycle";

const FilesView = lazy(() => import("./FilesView").then((module) => ({ default: module.FilesView })));
type Props = { accessProfile: OctopusClientAccessProfile; active: boolean; activeTabId: string; tabs: WorkspacePaneTabState[] };

export function RetainedFilesTabs({ accessProfile, active, activeTabId, tabs }: Props) {
  return tabs.map((tab) => tab.target.kind === "files" ? <RetainedFilesTab accessProfile={accessProfile} active={active && tab.id === activeTabId} key={`${accessProfile}:${tab.id}`} tab={tab} /> : null);
}

function RetainedFilesTab({ accessProfile, active, tab }: Pick<Props, "accessProfile" | "active"> & { tab: WorkspacePaneTabState }) {
  const [visited, setVisited] = useState(active);
  if (active && !visited) setVisited(true);
  if (tab.target.kind !== "files" || !active && !visited && !tab.target.snapshot) return null;
  return <div aria-hidden={!active || undefined} className="flex min-h-0 flex-1 flex-col overflow-hidden" hidden={!active} inert={!active} style={{ display: active ? "flex" : "none" }}>
    <SurfaceRenderBoundary resetKey={tab.id}>
      <Suspense fallback={<SurfaceLoadingState label="Loading files…" />}>
        <FilesView accessProfile={accessProfile} active={active} paneKey={filesPaneKey(accessProfile, tab.id)} snapshot={tab.target.snapshot} target={tab.target.openTarget} />
      </Suspense>
    </SurfaceRenderBoundary>
  </div>;
}
