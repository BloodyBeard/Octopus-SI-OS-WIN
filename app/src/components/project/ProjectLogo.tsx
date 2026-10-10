import { projectColor } from "../../design-system/projectColors";
import { Glyph, type GlyphName } from "../IconGlyphs";

export const PROJECT_ICON_OPTIONS: readonly GlyphName[] = [
  "briefcase", "folder", "code", "globe", "rocket", "palette", "storefront", "book",
];

export function ProjectLogo({ color, icon, size = 20, iconSize = 16 }: {
  color: string;
  icon: string;
  size?: number;
  iconSize?: number;
}) {
  const glyphName = (PROJECT_ICON_OPTIONS as readonly string[]).includes(icon) ? icon as GlyphName : "briefcase";
  return (
    <span
      aria-hidden="true"
      className="inline-flex shrink-0 items-center justify-center"
      data-project-icon={glyphName}
      style={{ color: projectColor(color), height: size, width: size }}
    >
      <Glyph name={glyphName} size={iconSize} />
    </span>
  );
}
