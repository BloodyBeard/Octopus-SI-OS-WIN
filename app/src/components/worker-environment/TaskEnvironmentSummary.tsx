import { useId, useState } from "react";
import type { AdmittedClientSurfaceDocument } from "../../client-surfaces/types";
import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import { OcIconButton } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { ShellTooltip } from "../shell/ShellTooltip";
import { paneWorkerEnvironmentSurfaceLabel, workerEnvironmentClientSurfaces, type WorkerEnvironmentPaneTarget } from "./workerEnvironmentPane";
import { workerEnvironmentPanelKind, workerEnvironmentSurfaceGlyph, type WorkerEnvironmentPanelKind } from "./surfaceRegistry";
import { environmentRootName, projectedChangeStats, taskEnvironmentContext, workspaceRevision, type TaskWorkspace } from "./taskEnvironmentSummaryModel";
import "./taskEnvironmentSummary.css";

type Props = {
  environments: WorkerEnvironmentProjection[];
  surfaces: AdmittedClientSurfaceDocument[];
  onOpen: (environmentId: string, panelKind?: WorkerEnvironmentPanelKind, surfaceId?: string, clientSurface?: WorkerEnvironmentPaneTarget["clientSurface"]) => void;
};

export function TaskEnvironmentSummary({ environments, surfaces, onOpen }: Props) {
  const { workspaces, views } = taskEnvironmentContext(environments);
  return (
    <div className="task-environment-context">
      {views.map(({ environment, surfaces: available }, index) => <RoomSummary
        key={environment.id}
        environment={environment}
        workspaces={workspaces.filter((workspace) => workspace.environment.id === environment.id)}
        available={available}
        customViews={workerEnvironmentClientSurfaces(environment.id, surfaces)}
        defaultCollapsed={index > 0}
        onOpen={onOpen}
      />)}
    </div>
  );
}

function RoomSummary({ environment, workspaces, available, customViews, defaultCollapsed, onOpen }: {
  environment: WorkerEnvironmentProjection;
  workspaces: TaskWorkspace[];
  available: WorkerEnvironmentSurfaceProjection[];
  customViews: AdmittedClientSurfaceDocument[];
  defaultCollapsed: boolean;
  onOpen: Props["onOpen"];
}) {
  const [expanded, setExpanded] = useState(!defaultCollapsed);
  const contentId = useId();
  const name = workspaces.length === 1 ? environmentRootName(workspaces[0].root) : environment.label || environment.kind || environment.id;
  const state = [...new Set([environment.lifecycle, environment.status].map(readableState).filter(Boolean))].join(" · ");
  const identity = [environment.id, environment.root, environment.lifecycle, environment.status].filter(Boolean).join(" · ");
  return (
    <section className="task-environment-room" aria-label={name}>
      <header className="task-environment-heading">
        <button className="task-environment-toggle" aria-controls={contentId} aria-expanded={expanded} onClick={() => setExpanded((value) => !value)} title={identity} type="button">
          <span>{name}</span>
          <Glyph className={`task-environment-chevron${expanded ? " is-expanded" : ""}`} name="chevronDown" size={12} />
        </button>
        {state ? <span className="task-environment-state" title={identity}>{state}</span> : null}
        <ShellTooltip label={`Details for ${name}`}>
          <OcIconButton appearance="compact" glyph="more" label={`Details for ${name}`} onClick={() => onOpen(environment.id, "overview")} title="" />
        </ShellTooltip>
      </header>
      <div className="task-environment-room-body" id={contentId} hidden={!expanded}>
        {workspaces.map((workspace) => <WorkspaceSummary key={workspace.id} workspace={workspace} showHeading={workspaces.length > 1} onOpen={onOpen} />)}
        <SurfaceList environment={environment} surfaces={available} onOpen={onOpen} />
        {customViews.map((view) => {
          const target = view.document.target;
          if (target.kind !== "worker_environment.surface") return null;
          return <EnvironmentRow key={view.sourceDigest} glyph={view.document.glyph} label={view.document.title} onClick={() => onOpen(
            environment.id, "custom", target.surface_id,
            { documentId: view.document.id, revision: view.document.revision, sourceDigest: view.sourceDigest },
          )} />;
        })}
        {!workspaces.length && !available.length && !customViews.length ? <p className="task-environment-empty">No surfaces reported</p> : null}
      </div>
    </section>
  );
}

function WorkspaceSummary({ workspace, showHeading, onOpen }: {
  workspace: TaskWorkspace;
  showHeading: boolean;
  onOpen: Props["onOpen"];
}) {
  const { environment, root, surfaces } = workspace;
  const revision = workspaceRevision(workspace);
  return (
    <div className="task-environment-workspace" aria-label={`${environmentRootName(root)} workspace`}>
      {showHeading ? <h3>{environmentRootName(root)}</h3> : null}
      <div className="task-environment-root" title={root}>{root}</div>
      {revision.branch || revision.conflict ? <EnvironmentRow
        glyph="worktree"
        label={revision.conflict ? "Conflicting branch reports" : revision.branch}
        meta={!revision.conflict && revision.base ? `from ${revision.base}` : ""}
      /> : null}
      <SurfaceList environment={environment} surfaces={surfaces} showRoots={false} onOpen={onOpen} />
    </div>
  );
}

function SurfaceList({ environment, surfaces, showRoots = true, onOpen }: {
  environment: WorkerEnvironmentProjection;
  surfaces: WorkerEnvironmentSurfaceProjection[];
  showRoots?: boolean;
  onOpen: Props["onOpen"];
}) {
  const [showAll, setShowAll] = useState(false);
  const visible = showAll ? surfaces : surfaces.slice(0, 4);
  return <>
    {visible.map((surface) => <SurfaceRow key={surface.id} surface={surface} environment={environment} showRoot={showRoots} onOpen={onOpen} />)}
    {surfaces.length > 4 ? <button className="task-environment-show-more" onClick={() => setShowAll((value) => !value)} type="button">
      {showAll ? "Show less" : `Show ${surfaces.length - visible.length} more`}
    </button> : null}
  </>;
}

function SurfaceRow({ environment, surface, showRoot, onOpen }: {
  environment: WorkerEnvironmentProjection;
  surface: WorkerEnvironmentSurfaceProjection;
  showRoot: boolean;
  onOpen: Props["onOpen"];
}) {
  const panel = workerEnvironmentPanelKind(surface);
  const meta = panel === "changes" ? projectedChangeStats(surface.data)
    : [readableState(surface.status), surface.readOnly ? "Read-only" : ""].filter(Boolean).join(" · ");
  return <EnvironmentRow
    glyph={workerEnvironmentSurfaceGlyph(surface)}
    label={panel === "changes" ? "Changes" : paneWorkerEnvironmentSurfaceLabel(environment, surface)}
    meta={meta}
    detail={showRoot ? surface.root || (panel === "terminal" && typeof surface.data.workspace === "string" ? surface.data.workspace : "") : ""}
    title={[surface.label, surface.id, surface.root, surface.status].filter(Boolean).join(" · ")}
    onClick={() => onOpen(environment.id, panel, surface.id)}
  />;
}

function EnvironmentRow({ glyph, label, meta, detail, onClick, title }: {
  glyph: GlyphName;
  label: string;
  meta?: string;
  detail?: string;
  onClick?: () => void;
  title?: string;
}) {
  const content = <>
    <span className="task-summary-row-leading"><Glyph name={glyph} size={16} /></span>
    <span className="task-summary-row-copy">
      <span className="task-environment-row-line">
        <span className="task-summary-row-title">{label}</span>
        {meta ? <span className="task-summary-row-meta" title={meta}>{meta}</span> : null}
      </span>
      {detail ? <span className="task-environment-row-detail" title={detail}>{detail}</span> : null}
    </span>
  </>;
  return onClick ? <button className="task-summary-row is-compact" onClick={onClick} title={title} type="button">{content}</button>
    : <div className="task-summary-row is-compact is-static" title={title}>{content}</div>;
}

function readableState(value: string): string {
  const state = value.trim().replace(/[_-]+/g, " ");
  return state ? state[0].toUpperCase() + state.slice(1) : "";
}
