import { lazy, Suspense, useState } from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import type { SecondaryPaneTabState } from "./operatorShellState";
import { SurfaceLoadingState, SurfaceRenderBoundary } from "./SurfaceBoundary";

const TerminalTab = lazy(() => import("../terminal/TerminalTab").then((module) => ({ default: module.TerminalTab })));

type Props = {
  accessProfile: OctopusClientAccessProfile;
  active: boolean;
  activeTabId: string;
  tabs: SecondaryPaneTabState[];
  onNewTerminalTab: () => void;
  onTitleChange: (terminalId: string, title: string) => void;
};

export function RetainedTerminalTabs({ accessProfile, active, activeTabId, tabs, onNewTerminalTab, onTitleChange }: Props) {
  return tabs.map((tab) => tab.target.kind === "terminal" ? (
    <RetainedTerminalTab
      accessProfile={accessProfile}
      active={active && tab.id === activeTabId}
      cwd={tab.target.cwd}
      key={JSON.stringify([accessProfile, tab.id, tab.target.terminalId, tab.target.cwd])}
      onNewTerminalTab={onNewTerminalTab}
      onTitleChange={onTitleChange}
      terminalId={tab.target.terminalId}
    />
  ) : null);
}

function RetainedTerminalTab({ accessProfile, active, cwd, onNewTerminalTab, onTitleChange, terminalId }: Omit<Props, "activeTabId" | "tabs"> & { cwd?: string; terminalId: string }) {
  const [visited, setVisited] = useState(active);
  if (active && !visited) setVisited(true);
  if (!active && !visited) return null;
  return (
    <div
      aria-hidden={!active || undefined}
      className="flex min-h-0 flex-1 flex-col overflow-hidden"
      hidden={!active}
      inert={!active}
      style={{ display: active ? "flex" : "none" }}
    >
      <SurfaceRenderBoundary resetKey={terminalId}>
        <Suspense fallback={<SurfaceLoadingState label="Loading terminal…" />}>
          <TerminalTab
            accessProfile={accessProfile}
            active={active}
            cwd={cwd}
            onNewTerminalTab={onNewTerminalTab}
            onTitleChange={(title) => onTitleChange(terminalId, title)}
            terminalId={terminalId}
          />
        </Suspense>
      </SurfaceRenderBoundary>
    </div>
  );
}
