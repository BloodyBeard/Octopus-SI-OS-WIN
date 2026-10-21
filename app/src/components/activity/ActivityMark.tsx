import { Glyph } from "../IconGlyphs";
import { AgentPresetLogo } from "../agent/AgentPresetLogo";
import type { MarkActivityItem } from "./markActivity";
import { activityPresentation, isActivityRequest } from "./markPresentation";
import "./markActivity.css";

export function ActivityMark({ item, size = 16, glyphSize = size }: { item: MarkActivityItem; size?: number; glyphSize?: number }) {
  return (
    <span className="activity-mark" data-request={isActivityRequest(item) || undefined} aria-hidden="true">
      {item.agentIcon
        ? <AgentPresetLogo color={item.agentColor} icon={item.agentIcon} size={size} />
        : <Glyph name={activityPresentation(item).glyph} size={glyphSize} />}
    </span>
  );
}
