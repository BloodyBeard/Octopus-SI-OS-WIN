import { OcLoadingText } from "../../design-system/LoadingSkeleton";
import { cloneElement, useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type HTMLAttributes, type PointerEvent, type ReactElement, type RefAttributes } from "react";
import { createPortal } from "react-dom";
import { visibleClientAgents, type ClientAgentDefinition } from "../../agentPresets";
import type { ChatRun } from "../../chatRuns";
import type { ClientChatProject } from "../../shell/clientWorkspaces";
import { readProjection } from "../../projectionClient";
import { workerEnvironmentProjection } from "../../runtimeTopology";
import { Glyph } from "../IconGlyphs";
import { AgentPresetLogo } from "../agent/AgentPresetLogo";
import { ProjectLogo } from "../project/ProjectLogo";
import { taskEnvironmentContext } from "../worker-environment/taskEnvironmentSummaryModel";
import { workerEnvironmentPanelKind } from "../worker-environment/surfaceRegistry";
import "./taskHoverCard.css";

export type TaskHoverCardDetails = {
  runId: string;
  eeId: string;
  project?: ClientChatProject;
  agent?: ClientAgentDefinition;
};

type TaskHoverEnvironmentDetails = {
  folders: Array<{ id: string; path: string; worktree: boolean }>;
};

export function taskHoverCardDetails(
  task: ChatRun,
  project: ClientChatProject | undefined,
  agents: readonly ClientAgentDefinition[],
): TaskHoverCardDetails {
  const matches = visibleClientAgents(agents).filter((agent) => (
    agent.projectEeId === task.eeId
    && agent.workerInterfaceId === (task.workerChoiceInterfaceId || task.workerInterfaceId)
  ));
  return {
    runId: task.runId,
    eeId: task.eeId,
    project: project?.id === task.eeId ? project : undefined,
    agent: matches.length === 1 ? matches[0] : undefined,
  };
}

export async function loadTaskHoverEnvironmentDetails(runId: string, eeId: string): Promise<TaskHoverEnvironmentDetails> {
  const response = await readProjection<{
    run?: { run_id?: unknown; ee_id?: unknown };
    worker_environments?: unknown[];
  }>("projection.runs.get", { run_id: runId, include_ledger: false });
  if (response?.run?.run_id !== runId || response.run.ee_id !== eeId || !Array.isArray(response.worker_environments)) {
    throw new Error("Task environment identity does not match.");
  }
  const environments = response.worker_environments.map(workerEnvironmentProjection);
  if (environments.some((environment) => !environment.id || environment.runId !== runId || environment.eeId !== eeId)) {
    throw new Error("Task environment identity does not match.");
  }
  const { workspaces } = taskEnvironmentContext(environments);
  return {
    folders: workspaces.map((workspace) => ({
      id: workspace.id,
      path: workspace.root,
      worktree: workspace.surfaces.some((surface) => ["changes", "worktree"].includes(workerEnvironmentPanelKind(surface))),
    })),
  };
}

const OPEN_DELAY = 700;
const CLOSE_DELAY = 100;
const WARM_INTERVAL = 300;
const VIEWPORT_MARGIN = 8;
let activeCard: { id: string; close: () => void; inTransit: boolean } | null = null;
let warmUntil = 0;

type Point = { x: number; y: number };
type Position = { left: number; top: number; side: "left" | "right" };

export function TaskHoverCard({ children, details, disabled = false, title, age }: {
  children: ReactElement<HTMLAttributes<HTMLDivElement> & RefAttributes<HTMLDivElement>>;
  details: TaskHoverCardDetails;
  disabled?: boolean;
  title: string;
  age?: string;
}) {
  const id = useId();
  const triggerRef = useRef<HTMLDivElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const graceCleanup = useRef<(() => void) | null>(null);
  const dismissed = useRef(false);
  const loadEpoch = useRef(0);
  const phaseRef = useRef("closed");
  const [phase, setPhase] = useState("closed");
  const [position, setPosition] = useState<Position | null>(null);
  const [environment, setEnvironment] = useState<{ data?: TaskHoverEnvironmentDetails; failed?: boolean } | null>(null);

  const clearPending = useCallback(() => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
    graceCleanup.current?.();
    graceCleanup.current = null;
    if (activeCard?.id === id) activeCard.inTransit = false;
  }, [id]);
  const close = useCallback(() => {
    loadEpoch.current += 1;
    clearPending();
    if (activeCard?.id === id) {
      activeCard = null;
      warmUntil = Date.now() + WARM_INTERVAL;
    }
    phaseRef.current = "closed";
    setPhase("closed");
    setPosition(null);
    setEnvironment(null);
  }, [clearPending, id]);
  const dismiss = useCallback(() => { dismissed.current = true; close(); }, [close]);

  const scheduleOpen = (immediate = false) => {
    if (disabled || dismissed.current || (activeCard?.id !== id && activeCard?.inTransit)) return;
    if (phaseRef.current === "open") { clearPending(); return; }
    if (phaseRef.current === "pending" && !immediate) return;
    clearPending();
    const open = () => {
      timer.current = null;
      if (!triggerRef.current?.isConnected || document.visibilityState === "hidden") { close(); return; }
      activeCard?.close();
      activeCard = { id, close, inTransit: false };
      phaseRef.current = "open";
      setPhase("open");
      setEnvironment(null);
      const epoch = ++loadEpoch.current;
      void loadTaskHoverEnvironmentDetails(details.runId, details.eeId).then((data) => {
        if (epoch === loadEpoch.current && phaseRef.current === "open") setEnvironment({ data });
      }).catch(() => {
        if (epoch === loadEpoch.current && phaseRef.current === "open") setEnvironment({ failed: true });
      });
    };
    if (immediate || Date.now() < warmUntil) open();
    else {
      phaseRef.current = "pending";
      setPhase("pending");
      timer.current = setTimeout(open, OPEN_DELAY);
    }
  };
  const pointerOpen = (event: PointerEvent<HTMLDivElement>) => {
    if (event.pointerType !== "touch" && !event.buttons) scheduleOpen();
  };
  const leave = (event: PointerEvent<HTMLDivElement>, destination: HTMLDivElement | null) => {
    dismissed.current = false;
    clearPending();
    if (phaseRef.current !== "open" || !destination) { close(); return; }
    const source = event.currentTarget;
    if (event.relatedTarget instanceof Node && destination.contains(event.relatedTarget)) return;
    const start = { x: event.clientX, y: event.clientY };
    const rect = destination.getBoundingClientRect();
    const sourceRect = source.getBoundingClientRect();
    const edge = rect.left >= sourceRect.right ? rect.left : rect.right;
    const a = { x: edge, y: rect.top - VIEWPORT_MARGIN };
    const b = { x: edge, y: rect.bottom + VIEWPORT_MARGIN };
    const deferClose = () => {
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = setTimeout(close, CLOSE_DELAY);
    };
    const move = (pointer: globalThis.PointerEvent) => {
      if (pointer.composedPath().includes(destination) || pointer.composedPath().includes(source)) { clearPending(); return; }
      if (insideTriangle({ x: pointer.clientX, y: pointer.clientY }, start, a, b)) deferClose();
      else close();
    };
    if (activeCard?.id === id) activeCard.inTransit = true;
    document.addEventListener("pointermove", move, true);
    graceCleanup.current = () => document.removeEventListener("pointermove", move, true);
    deferClose();
  };

  useEffect(() => { if (disabled) close(); }, [close, disabled]);
  useEffect(() => () => {
    loadEpoch.current += 1;
    phaseRef.current = "closed";
    clearPending();
    if (activeCard?.id === id) { activeCard = null; warmUntil = 0; }
  }, [clearPending, id]);
  useEffect(() => {
    if (phase === "closed") return;
    const escape = (event: KeyboardEvent) => { if (event.key === "Escape") dismiss(); };
    const scroll = (event: Event) => { if (!(event.target instanceof Node && cardRef.current?.contains(event.target))) dismiss(); };
    const visibility = () => { if (document.visibilityState === "hidden") dismiss(); };
    document.addEventListener("keydown", escape);
    document.addEventListener("pointerdown", dismiss, true);
    document.addEventListener("scroll", scroll, true);
    document.addEventListener("visibilitychange", visibility);
    window.addEventListener("blur", dismiss);
    return () => {
      document.removeEventListener("keydown", escape);
      document.removeEventListener("pointerdown", dismiss, true);
      document.removeEventListener("scroll", scroll, true);
      document.removeEventListener("visibilitychange", visibility);
      window.removeEventListener("blur", dismiss);
    };
  }, [dismiss, phase]);
  useLayoutEffect(() => {
    if (phase !== "open") return;
    const measure = () => {
      if (!triggerRef.current?.isConnected || !cardRef.current) { close(); return; }
      const anchor = triggerRef.current.getBoundingClientRect();
      const card = cardRef.current.getBoundingClientRect();
      const rightSpace = window.innerWidth - VIEWPORT_MARGIN - anchor.right - 2;
      const leftSpace = anchor.left - VIEWPORT_MARGIN - 2;
      const side = rightSpace < card.width && leftSpace > rightSpace ? "left" : "right";
      const left = Math.max(VIEWPORT_MARGIN, Math.min(side === "right" ? anchor.right + 2 : anchor.left - card.width - 2, window.innerWidth - VIEWPORT_MARGIN - card.width));
      const top = Math.max(VIEWPORT_MARGIN, Math.min(anchor.top, window.innerHeight - VIEWPORT_MARGIN - card.height));
      setPosition((previous) => previous?.left === left && previous.top === top && previous.side === side ? previous : { left, top, side });
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    if (triggerRef.current) observer?.observe(triggerRef.current);
    if (cardRef.current) observer?.observe(cardRef.current);
    window.addEventListener("resize", measure);
    return () => { observer?.disconnect(); window.removeEventListener("resize", measure); };
  }, [close, environment, phase]);

  return <>
    {cloneElement(children, {
      ref: triggerRef,
      "aria-describedby": [children.props["aria-describedby"], phase === "open" ? id : ""].filter(Boolean).join(" ") || undefined,
      onPointerEnter: (event) => { children.props.onPointerEnter?.(event); pointerOpen(event); },
      onPointerMove: (event) => { children.props.onPointerMove?.(event); pointerOpen(event); },
      onPointerLeave: (event) => { children.props.onPointerLeave?.(event); leave(event, cardRef.current); },
      onFocus: (event) => {
        children.props.onFocus?.(event);
        if (event.target === event.currentTarget && event.target.matches(":focus-visible")) {
          dismissed.current = false;
          scheduleOpen(true);
        }
      },
      onBlur: (event) => { children.props.onBlur?.(event); dismiss(); },
      onClick: (event) => { dismiss(); children.props.onClick?.(event); },
      onContextMenu: (event) => { dismiss(); children.props.onContextMenu?.(event); },
      onKeyDown: (event) => {
        if (event.key === "Enter" || event.key === " " || event.key === "Escape") dismiss();
        children.props.onKeyDown?.(event);
      },
    })}
    {phase === "open" ? createPortal(
      <div
        className="task-hover-card"
        data-side={position?.side}
        id={id}
        onPointerEnter={clearPending}
        onPointerLeave={(event) => leave(event, triggerRef.current)}
        ref={cardRef}
        role="tooltip"
        style={{ left: position?.left ?? 0, top: position?.top ?? 0, visibility: position ? "visible" : "hidden" }}
      >
        <div className="task-hover-card-heading">
          <div className="task-hover-card-title" dir="auto">{title}</div>
          {age ? <div className="task-hover-card-meta"><span>{age}</span></div> : null}
        </div>
        {details.project ? <div className="task-hover-card-row">
          <ProjectLogo color={details.project.color} icon={details.project.icon} size={16} iconSize={16} />
          <span>{details.project.name}</span>
        </div> : null}
        {environment?.data?.folders.map((folder) => <div className="task-hover-card-row task-hover-card-folder" key={folder.id}>
          <span aria-label={folder.worktree ? "Git working tree" : "Folder"} role="img"><Glyph name={folder.worktree ? "worktree" : "folder"} size={16} /></span>
          <span dir="auto">{folder.path}</span>
        </div>)}
        {details.agent ? <div className="task-hover-card-row">
          <AgentPresetLogo color={details.project?.color} icon={details.agent.icon} size={16} />
          <span>{details.agent.name}</span>
        </div> : null}
        {!environment || environment.failed || environment.data?.folders.length === 0 ? <div className="task-hover-card-notice">
          {environment?.failed ? "Environment details unavailable" : environment?.data ? "No workspace attached to this task" : <OcLoadingText label="Loading environment…" />}
        </div> : null}
      </div>, document.body,
    ) : null}
  </>;
}

function insideTriangle(point: Point, start: Point, a: Point, b: Point): boolean {
  const cross = (p: Point, q: Point, r: Point) => (p.x - r.x) * (q.y - r.y) - (q.x - r.x) * (p.y - r.y);
  const signs = [cross(point, start, a), cross(point, a, b), cross(point, b, start)];
  return !(signs.some((value) => value < 0) && signs.some((value) => value > 0));
}
