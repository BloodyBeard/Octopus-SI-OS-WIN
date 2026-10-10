export const PROJECT_COLOR_OPTIONS = [
  "black", "red", "orange", "amber", "green", "blue", "violet", "pink",
  "indigo", "cyan", "teal",
] as const;

export type ProjectColor = typeof PROJECT_COLOR_OPTIONS[number];

export const DEFAULT_PROJECT_COLOR: ProjectColor = "black";

const PROJECT_COLOR_ALIASES: Readonly<Record<string, ProjectColor>> = {
  purple: "violet",
  yellow: "amber",
};

export function projectColorKey(color: string | null | undefined): ProjectColor | undefined {
  const value = color?.trim().toLowerCase();
  if (!value) return undefined;
  if (Object.hasOwn(PROJECT_COLOR_ALIASES, value)) return PROJECT_COLOR_ALIASES[value];
  return (PROJECT_COLOR_OPTIONS as readonly string[]).includes(value) ? value as ProjectColor : undefined;
}

export function projectColor(color: string | null | undefined): string {
  return `var(--project-color-${projectColorKey(color) ?? DEFAULT_PROJECT_COLOR})`;
}
