import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import type { GlyphName } from "../IconGlyphs";

export type WorkerEnvironmentPanelKind =
  | "browser"
  | "connection"
  | "container"
  | "runtime"
  | "worktree"
  | "files"
  | "file"
  | "terminal"
  | "changes"
  | "desktop"
  | "custom"
  | "overview";

type SurfaceDefinition = {
  aliases: string[];
  glyph: GlyphName;
  launchable: boolean;
  panelKind: Exclude<WorkerEnvironmentPanelKind, "file" | "overview">;
  previewRank: number;
};

const PREVIEW_SURFACE_ALIASES = ["preview", "site_preview", "http_preview", "site_test", "site-test"];

const SURFACE_DEFINITIONS: SurfaceDefinition[] = [
  { aliases: ["browser"], glyph: "browser", launchable: true, panelKind: "browser", previewRank: 10 },
  { aliases: ["downloads"], glyph: "download", launchable: true, panelKind: "browser", previewRank: 15 },
  { aliases: ["desktop", "computer_use"], glyph: "desktop", launchable: true, panelKind: "desktop", previewRank: 12 },
  { aliases: PREVIEW_SURFACE_ALIASES, glyph: "sites", launchable: true, panelKind: "browser", previewRank: 14 },
  { aliases: ["review", "changes", "diff", "pull_request", "pull-request", "pr"], glyph: "changes", launchable: true, panelKind: "changes", previewRank: 8 },
  { aliases: ["terminal", "shell"], glyph: "terminal", launchable: true, panelKind: "terminal", previewRank: 30 },
  { aliases: ["files", "filesystem"], glyph: "folder", launchable: true, panelKind: "files", previewRank: 32 },
  { aliases: ["container", "linux"], glyph: "ee", launchable: true, panelKind: "container", previewRank: 180 },
  { aliases: ["runtime", "mcp", "code_mode", "code-mode"], glyph: "pulse", launchable: true, panelKind: "runtime", previewRank: 182 },
  { aliases: ["worktree", "worktree_status", "worktree-status", "setup_status"], glyph: "worktree", launchable: true, panelKind: "worktree", previewRank: 184 },
  { aliases: ["connection", "attach", "viewer_connection"], glyph: "interfaces", launchable: true, panelKind: "connection", previewRank: 190 },
];

const FALLBACK_DEFINITION: SurfaceDefinition = {
  aliases: [],
  glyph: "plugins",
  launchable: true,
  panelKind: "custom",
  previewRank: 160,
};

export function workerEnvironmentSurfaceDefinition(surface: Pick<WorkerEnvironmentSurfaceProjection, "kind" | "protocol">): SurfaceDefinition {
  const value = surface.kind.trim().toLowerCase();
  const exact = SURFACE_DEFINITIONS.find((definition) => definition.aliases.includes(value));
  if (exact) return exact;
  const protocol = surface.protocol.trim().toLowerCase();
  if (protocol === "cdp") return definitionForPanel("browser");
  if (protocol.includes("xterm")) return definitionForPanel("terminal");
  if (protocol.includes("desktop")) return definitionForPanel("desktop");
  return FALLBACK_DEFINITION;
}

export function workerEnvironmentPanelKind(surface: Pick<WorkerEnvironmentSurfaceProjection, "kind" | "protocol">): WorkerEnvironmentPanelKind {
  return workerEnvironmentSurfaceDefinition(surface).panelKind;
}

export function workerEnvironmentSurfaceGlyph(surface: Pick<WorkerEnvironmentSurfaceProjection, "kind" | "protocol">): GlyphName {
  return workerEnvironmentSurfaceDefinition(surface).glyph;
}

export function workerEnvironmentSurfaceIsLaunchable(surface: Pick<WorkerEnvironmentSurfaceProjection, "kind" | "protocol">): boolean {
  return workerEnvironmentSurfaceDefinition(surface).launchable;
}

export function workerEnvironmentSurfaceIsPreview(surface: Pick<WorkerEnvironmentSurfaceProjection, "kind">): boolean {
  return PREVIEW_SURFACE_ALIASES.includes(surface.kind.trim().toLowerCase());
}

export function workerEnvironmentProductSurfaces(surfaces: WorkerEnvironmentSurfaceProjection[]): WorkerEnvironmentSurfaceProjection[] {
  const launchable = surfaces.filter(workerEnvironmentSurfaceIsLaunchable);
  const directReviewRoots = [...new Set(launchable.flatMap((surface) => (
    workerEnvironmentPanelKind(surface) === "changes"
    && surface.mode === "direct"
    && surface.protocol.trim().toLowerCase() === "git.worktree"
    && surface.root.trim()
      ? [surface.root.trim()]
      : []
  )))];
  const reviewGroups = new Map<string, WorkerEnvironmentSurfaceProjection[]>();
  for (const surface of launchable) {
    if (workerEnvironmentPanelKind(surface) !== "changes") continue;
    const root = surface.root.trim();
    const key = root || (directReviewRoots.length === 1 ? directReviewRoots[0] : "__unscoped_review__");
    reviewGroups.set(key, [...(reviewGroups.get(key) ?? []), surface]);
  }
  const selectedIds = new Map<string, string>();
  const hiddenIds = new Set<string>();
  for (const group of reviewGroups.values()) {
    if (group.length < 2) continue;
    const selected = [...group].sort((left, right) => reviewSurfaceScore(left) - reviewSurfaceScore(right))[0];
    selectedIds.set(group[0].id, selected.id);
    for (const surface of group) hiddenIds.add(surface.id);
  }
  const byId = new Map(launchable.map((surface) => [surface.id, surface]));
  return launchable.flatMap((surface) => {
    const selectedId = selectedIds.get(surface.id);
    if (selectedId) return byId.get(selectedId) ?? [];
    return hiddenIds.has(surface.id) ? [] : [surface];
  });
}

export function workerEnvironmentGlyph(environment: WorkerEnvironmentProjection): GlyphName {
  const surface = primaryWorkerEnvironmentSurface(environment);
  return surface ? workerEnvironmentSurfaceGlyph(surface) : "ee";
}

export function primaryWorkerEnvironmentSurface(environment: WorkerEnvironmentProjection): WorkerEnvironmentSurfaceProjection | null {
  return workerEnvironmentProductSurfaces(environment.surfaces).sort((left, right) => (
    workerEnvironmentSurfaceDefinition(left).previewRank - workerEnvironmentSurfaceDefinition(right).previewRank
      || left.label.localeCompare(right.label)
  ))[0] ?? null;
}

function reviewSurfaceScore(surface: WorkerEnvironmentSurfaceProjection): number {
  if (surface.mode === "direct" && surface.protocol.trim().toLowerCase() === "git.worktree") return 0;
  if (Array.isArray(surface.data.diffs) || Array.isArray(surface.data.changes)) return 10;
  return 20;
}

function definitionForPanel(panelKind: SurfaceDefinition["panelKind"]): SurfaceDefinition {
  return SURFACE_DEFINITIONS.find((definition) => definition.panelKind === panelKind) ?? FALLBACK_DEFINITION;
}
