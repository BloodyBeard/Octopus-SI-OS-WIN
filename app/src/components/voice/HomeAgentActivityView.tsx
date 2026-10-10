import type { ChatWorkspace } from "../chat";
import { ChatView } from "../ChatView";
import type { AdmittedClientSurfaceDocument } from "../../client-surfaces/types";
import type { TopologySnapshot } from "../../data/topology";
import type { FilesOpenTarget } from "../files/types";

export function HomeAgentActivityView({ commandsActive = true, onOpenFiles, searchFocusTick, surfaces, topology, workspace }: {
  commandsActive?: boolean;
  onOpenFiles: (target: FilesOpenTarget) => void;
  searchFocusTick: number;
  surfaces: AdmittedClientSurfaceDocument[];
  topology: TopologySnapshot;
  workspace: ChatWorkspace;
}) {
  return (
    <section aria-label="Home activity" className="home-activity-view">
      <div className="home-activity-view__body">
        <ChatView
          commandsActive={commandsActive}
          composerVisible={false}
          headerMode="none"
          onOpenFiles={onOpenFiles}
          searchFocusTick={searchFocusTick}
          surfaces={surfaces}
          topology={topology}
          workspace={workspace}
        />
      </div>
    </section>
  );
}
