import type { ReactNode } from "react";
import { MessageTooltip } from "./chat/MessageTooltip";
import "./shell/shellTooltip.css";
import "./dashboard.css";

const LABEL_SEGMENTER = new Intl.Segmenter(undefined, { granularity: "grapheme" });
export const DASHBOARD_LABEL_LIMIT = 24;

export function compactDashboardLabel(text: string, limit = DASHBOARD_LABEL_LIMIT): string {
  const parts = Array.from(LABEL_SEGMENTER.segment(text), ({ segment }) => segment);
  if (parts.length <= limit) return text;
  const suffixLength = Math.min(7, Math.floor((limit - 1) / 3));
  return `${parts.slice(0, limit - suffixLength - 1).join("")}…${parts.slice(-suffixLength).join("")}`;
}

export function DashboardTooltip({ text, children, asChild = false, compact = false }: { text: string; children: ReactNode; asChild?: boolean; compact?: boolean }) {
  return <MessageTooltip asChild={asChild} delayMs={200} side="bottom" variant="shell"
    label={<span className={`dashboard-tooltip-label${compact ? " is-compact" : ""}`}>{text}</span>}>
    {children}
  </MessageTooltip>;
}

export function DashboardLabel({ text, tooltip = text, limit = DASHBOARD_LABEL_LIMIT }: {
  text: string;
  tooltip?: string;
  limit?: number;
}) {
  return <span className="dashboard-label">
    <DashboardTooltip text={tooltip}>
      <span className="dashboard-label-text" aria-hidden="true">{compactDashboardLabel(text, limit)}</span>
      <span className="oc-visually-hidden">{tooltip}</span>
    </DashboardTooltip>
  </span>;
}

export function DashboardSvgLabel({ text, tooltip = text, x, y, centered = false, width = 180, limit = DASHBOARD_LABEL_LIMIT }: {
  text: string;
  tooltip?: string;
  x: number;
  y: number;
  centered?: boolean;
  width?: number;
  limit?: number;
}) {
  return <foreignObject
    className="dashboard-svg-label"
    x={centered ? x - width / 2 : x}
    y={y - 12}
    width={width}
    height={18}
  >
    <div className={`dashboard-svg-label-content${centered ? " is-centered" : ""}`}>
      <DashboardLabel text={text} tooltip={tooltip} limit={limit} />
    </div>
  </foreignObject>;
}
