import { useShouldReduceMotion } from "../../appearance";
import { Glyph } from "../IconGlyphs";
import "./sidebarActivity.css";

export type SidebarTaskState = "running" | "failed" | "waiting" | "scheduled" | "unread";

const statusLabels: Record<SidebarTaskState, string> = {
  running: "Running",
  failed: "Failed",
  waiting: "Waiting",
  scheduled: "Scheduled",
  unread: "New",
};

const RUNNING_DOTS = Array.from({ length: 8 }, (_, index) => {
  const angle = (index / 8) * Math.PI * 2 - Math.PI / 2;
  return { cx: 7 + Math.cos(angle) * 4.6, cy: 7 + Math.sin(angle) * 4.6, opacity: 0.22 + (index / 7) * 0.78 };
});

export function SidebarTaskStatus({ state, label, text, detail }: {
  state: SidebarTaskState;
  label?: string;
  text?: string;
  detail?: string;
}) {
  const reduceMotion = useShouldReduceMotion();
  const accessibleLabel = label ?? statusLabels[state];
  return (
    <span
      aria-label={accessibleLabel}
      className="sidebar-task-status"
      data-reduce-motion={reduceMotion || undefined}
      data-state={state}
      role="img"
      title={detail ?? accessibleLabel}
    >
      {state === "running" ? (
        <svg aria-hidden="true" className="sidebar-task-status-orbit" viewBox="0 0 14 14">
          {RUNNING_DOTS.map((dot, index) => <circle cx={dot.cx} cy={dot.cy} fill="currentColor" key={index} opacity={dot.opacity} r="1.15" />)}
        </svg>
      ) : state === "failed" ? (
        <svg aria-hidden="true" className="sidebar-task-status-alert" viewBox="0 0 14 14">
          <circle cx="7" cy="7" fill="none" r="5.6" stroke="currentColor" strokeWidth="1.3" />
          <path d="M7 4.3v3.4" fill="none" stroke="currentColor" strokeLinecap="round" strokeWidth="1.4" />
          <circle cx="7" cy="9.75" fill="currentColor" r=".85" />
        </svg>
      ) : state === "scheduled" ? (
        <Glyph name="scheduled" size={12} />
      ) : (
        <span aria-hidden="true" className="sidebar-task-status-dot" />
      )}
      {text ? <span aria-hidden="true" className="sidebar-task-status-text">{text}</span> : null}
    </span>
  );
}
