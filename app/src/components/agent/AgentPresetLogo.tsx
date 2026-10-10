import {
  AGENT_PRESET_ICONS,
  DEFAULT_AGENT_PRESET_ICON,
  type ClientAgentPresetIcon,
} from "../../agentPresets";
import { projectColor } from "../../design-system/projectColors";

export function AgentPresetLogo({ color, icon, size = 24 }: {
  color: string | null | undefined;
  icon: ClientAgentPresetIcon;
  size?: number;
}) {
  const fill = projectColor(color);
  const resolvedIcon = (AGENT_PRESET_ICONS as readonly string[]).includes(icon) ? icon : DEFAULT_AGENT_PRESET_ICON;
  const maskImage = `url("./agent-icons/octopus-${resolvedIcon}.png")`;
  return (
    <span
      aria-hidden="true"
      className="inline-flex shrink-0 items-center justify-center"
      style={{
        color: fill,
        height: size,
        width: size,
      }}
    >
      <span
        data-agent-icon={resolvedIcon}
        style={{
          WebkitMaskImage: maskImage,
          WebkitMaskPosition: "center",
          WebkitMaskRepeat: "no-repeat",
          WebkitMaskSize: "contain",
          backgroundColor: "currentColor",
          height: "100%",
          maskImage,
          maskPosition: "center",
          maskRepeat: "no-repeat",
          maskSize: "contain",
          width: "100%",
        }}
      />
    </span>
  );
}
