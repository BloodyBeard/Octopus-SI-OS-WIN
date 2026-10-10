import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import { workerEnvironmentPanelKind, workerEnvironmentProductSurfaces } from "./surfaceRegistry";

export type TaskWorkspace = {
  id: string;
  environment: WorkerEnvironmentProjection;
  root: string;
  surfaces: WorkerEnvironmentSurfaceProjection[];
};

export function taskEnvironmentContext(environments: WorkerEnvironmentProjection[]) {
  const workspaces: TaskWorkspace[] = [];
  const views: Array<{ environment: WorkerEnvironmentProjection; surfaces: WorkerEnvironmentSurfaceProjection[] }> = [];
  const seen = new Set<string>();
  for (const environment of environments) {
    if (!environment.id || seen.has(environment.id)) continue;
    seen.add(environment.id);
    const surfaces = workerEnvironmentProductSurfaces(environment.surfaces);
    const groups = new Map<string, TaskWorkspace>();
    const worktrees = surfaces.filter((surface) => workerEnvironmentPanelKind(surface) === "worktree");
    const rootFor = (surface: WorkerEnvironmentSurfaceProjection) => {
      const panel = workerEnvironmentPanelKind(surface);
      if (panel === "worktree") return workspacePath(surface.data.worktree_workspace_root ?? surface.data.workspace_root)
        || workspacePath(surface.data.worktree_git_root ?? surface.data.git_root) || workspacePath(surface.root);
      return workspacePath(surface.root) || (panel === "terminal" ? workspacePath(surface.data.workspace) : "");
    };
    for (const surface of surfaces) {
      const panel = workerEnvironmentPanelKind(surface);
      const isWorkspace = panel === "changes" || panel === "worktree"
        || (panel === "files" && surface.data.slot === "workspace_root");
      if (!isWorkspace) continue;
      let root = rootFor(surface);
      if (!root) continue;
      if (panel === "changes") {
        const matching = worktrees.filter((worktree) => workspacePath(worktree.data.worktree_git_root ?? worktree.data.git_root) === root);
        if (matching.length === 1) root = rootFor(matching[0]);
      }
      const group: TaskWorkspace = groups.get(root) ?? { id: JSON.stringify([environment.id, root]), environment, root, surfaces: [] };
      group.surfaces.push(surface);
      groups.set(root, group);
    }
    const assigned = new Set([...groups.values()].flatMap((group) => group.surfaces.map((surface) => surface.id)));
    const remaining: WorkerEnvironmentSurfaceProjection[] = [];
    for (const surface of surfaces) {
      if (assigned.has(surface.id)) continue;
      const panel = workerEnvironmentPanelKind(surface);
      const group = groups.get(rootFor(surface));
      if (group && panel !== "connection" && panel !== "runtime") group.surfaces.push(surface);
      else remaining.push(surface);
    }
    workspaces.push(...groups.values());
    views.push({ environment, surfaces: remaining });
  }
  return { workspaces, views };
}

export function environmentRootName(root: string): string {
  return root.replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean).at(-1) || root;
}

export function workspaceRevision(workspace: TaskWorkspace) {
  const revisions = workspace.surfaces.filter((surface) => {
    const panel = workerEnvironmentPanelKind(surface);
    return panel === "worktree" || panel === "changes";
  });
  const branches = [...new Set(revisions.map((surface) => text(surface.data.branch)).filter(Boolean))];
  const bases = [...new Set(revisions.map((surface) => text(surface.data.base_branch ?? surface.data.baseBranch ?? surface.data.base)).filter(Boolean))];
  return {
    branch: branches.length === 1 ? branches[0] : "",
    base: bases.length === 1 ? bases[0] : "",
    reported: revisions.length > 0,
    conflict: branches.length > 1,
  };
}

export function projectedChangeStats(data: Record<string, unknown>): string {
  const raw = data.diffs ?? data.changes;
  const changes = Array.isArray(raw) ? raw.filter((row): row is Record<string, unknown> => row != null && typeof row === "object" && !Array.isArray(row)) : [];
  const count = (value: unknown) => typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.trunc(value) : null;
  const additions = count(data.additions ?? data.lines_added) ?? changes.reduce((sum, row) => sum + (count(row.additions ?? row.lines_added) ?? 0), 0);
  const deletions = count(data.deletions ?? data.lines_removed) ?? changes.reduce((sum, row) => sum + (count(row.deletions ?? row.lines_removed) ?? 0), 0);
  if (additions || deletions) return `+${additions} −${deletions}`;
  if (Array.isArray(raw) && raw.length === changes.length) return changes.length ? `${changes.length} changed` : "No changes reported";
  return "Open review";
}

function workspacePath(value: unknown): string {
  const path = text(value);
  if (/^[a-z]:[\\/]$/i.test(path)) return path;
  return path.replace(/[\\/]+$/, "") || (path.startsWith("/") ? "/" : "");
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
