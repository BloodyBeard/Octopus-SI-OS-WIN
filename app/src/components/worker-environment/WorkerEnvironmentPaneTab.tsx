import type { AdmittedClientSurfaceDocument } from "../../client-surfaces/types";
import "../icon-glyphs/worker";
import type { TopologySnapshot } from "../../data/topology";
import type { OctopusClientAccessProfile } from "../../electron";
import { Glyph } from "../IconGlyphs";
import { OctopusEmptyState } from "../OctopusSurface";
import type { ProjectedAssistantCodeComment } from "../chat/assistantDirectives";
import { WorkerEnvironmentSurfacePanel } from "./RunEnvironmentView";
import {
  dispatchOpenWorkerEnvironmentPane,
  resolvePaneClientSurface,
  resolvePaneSurface,
  resolveWorkerEnvironment,
  type WorkerEnvironmentPaneTarget,
} from "./workerEnvironmentPane";

type Props = {
  accessProfile: OctopusClientAccessProfile;
  assistantCodeComments?: ProjectedAssistantCodeComment[];
  snapshot: TopologySnapshot;
  surfaces: AdmittedClientSurfaceDocument[];
  target: WorkerEnvironmentPaneTarget;
};

export function WorkerEnvironmentPaneTab({ accessProfile, assistantCodeComments = [], snapshot, surfaces, target }: Props) {
  const resolved = resolveWorkerEnvironment(snapshot, target.workerEnvironmentId);
  if (!resolved) {
    return (
      <div className="flex h-full min-h-0 flex-1 flex-col bg-token-main-surface-primary" role="status">
        <OctopusEmptyState
          className="h-full min-h-0 flex-1 select-none"
          description={snapshot.projection.mode === "connecting"
            ? snapshot.projection.detail
            : "This room is no longer projected by the runtime. Its tab stays until you close it."}
          icon={<Glyph name="ee" size={32} />}
          title={snapshot.projection.mode === "connecting" ? "Connecting to Octopus" : "Worker Environment is not projected"}
        />
      </div>
    );
  }
  const document = resolvePaneClientSurface(surfaces, target);
  if (target.clientSurface && !document) {
    return (
      <div className="flex h-full min-h-0 flex-1 flex-col bg-token-main-surface-primary" role="status">
        <OctopusEmptyState
          className="h-full min-h-0 flex-1 select-none"
          description="The exact admitted client-surface revision behind this tab is no longer projected. Close the tab and reopen the current revision."
          icon={<Glyph name="plugins" size={32} />}
          title="Worker Environment surface changed"
        />
      </div>
    );
  }
  const surface = document ? null : resolvePaneSurface(resolved.workerEnvironment, target);
  if (!document && !surface && (target.surfaceId || target.filePath || target.panelKind && target.panelKind !== "overview")) {
    return (
      <div className="flex h-full min-h-0 flex-1 flex-col bg-token-main-surface-primary" role="status">
        <OctopusEmptyState
          className="h-full min-h-0 flex-1 select-none"
          description="The exact projected surface behind this tab is no longer available. Close the tab and reopen a surface from the current room projection."
          icon={<Glyph name="ee" size={32} />}
          title="Worker Environment surface changed"
        />
      </div>
    );
  }
  return (
    <div className="we-pane-tab">
      <WorkerEnvironmentSurfacePanel
        accessProfile={accessProfile}
        assistantCodeComments={assistantCodeComments}
        document={document}
        environment={resolved.workerEnvironment}
        filePath={target.filePath}
        onOpenFile={(filePath, fileSurface) => dispatchOpenWorkerEnvironmentPane({
          workerEnvironmentId: resolved.workerEnvironment.id,
          surfaceId: fileSurface.id,
          panelKind: "file",
          filePath,
        })}
        run={resolved.run}
        surface={surface}
      />
    </div>
  );
}
