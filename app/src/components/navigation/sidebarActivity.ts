import type { ChatRun } from "../../chatRuns";
import type { ClientChatProject } from "../../shell/clientWorkspaces";

export function projectWorkingTaskCounts(projects: readonly ClientChatProject[], tasks: readonly ChatRun[]): Map<string, number> {
  const working = new Set(tasks.filter((task) => task.turnState === "working").map((task) => task.runId));
  const byId = new Map(projects.map((project) => [project.id, project]));
  const projectTasks = new Map<string, Set<string>>();
  for (const project of projects) {
    const runIds = [...project.chatRunIds, ...(project.readOnlyRunIds ?? [])].filter((runId) => working.has(runId));
    if (!runIds.length) continue;
    const visited = new Set<string>();
    let owner: ClientChatProject | undefined = project;
    while (owner && !visited.has(owner.id)) {
      visited.add(owner.id);
      const ids = projectTasks.get(owner.id) ?? new Set<string>();
      for (const runId of runIds) ids.add(runId);
      projectTasks.set(owner.id, ids);
      owner = owner.parentId ? byId.get(owner.parentId) : undefined;
    }
  }
  return new Map([...projectTasks].map(([id, tasks]) => [id, tasks.size]));
}
