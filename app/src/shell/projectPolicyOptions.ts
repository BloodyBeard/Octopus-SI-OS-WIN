export type ProjectPolicyOption = {
  id: string;
  label: string;
  description: string;
  source: string;
  goal: string;
  inheritsParentPolicy: boolean;
};

const PRESENTATIONS: Record<string, { label: string; description: string }> = {
  project: { label: "Project preset", description: "Use the installed project policy." },
  operator_space: { label: "General work", description: "Organize everyday work and nested projects." },
  operator_interaction: { label: "Focused task", description: "Complete a specific request with the permitted tools." },
  self_improvement: { label: "Framework development", description: "Work on Octopus configuration or source within the parent’s permissions." },
  startup_management: { label: "Startup and setup", description: "Manage startup and configuration work." },
  memory_maintenance: { label: "Knowledge maintenance", description: "Maintain the context artifacts available to this project." },
  context_maintenance: { label: "Context maintenance", description: "Prepare and maintain context for ongoing work." },
};

const ALLOWANCE_FIELDS = [
  "allowed_interfaces", "allowed_tools", "allowed_worker_environment_kinds",
  "allowed_root_slots", "allowed_artifact_verbs", "allowed_mark_outcomes",
] as const;

export function projectPolicyOption(value: unknown): ProjectPolicyOption | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (typeof row.id !== "string" || !row.id || row.bootstrap_root === true) return null;
  const inheritsParentPolicy = row.id === "project"
    && ALLOWANCE_FIELDS.every((field) => row[field] == null)
    && (row.hard_blocked_tools === undefined || Array.isArray(row.hard_blocked_tools) && row.hard_blocked_tools.length === 0)
    && (row.interface_policy_overrides === undefined || row.interface_policy_overrides !== null
      && typeof row.interface_policy_overrides === "object" && !Array.isArray(row.interface_policy_overrides)
      && Object.keys(row.interface_policy_overrides).length === 0);
  const presentation = PRESENTATIONS[row.id];
  const words = row.id.replace(/[_-]+/g, " ");
  return {
    id: row.id,
    label: inheritsParentPolicy ? "Same policy as parent" : presentation?.label ?? `${words[0].toUpperCase()}${words.slice(1)}`,
    description: inheritsParentPolicy
      ? "Keep the parent’s access limits. No additional access restrictions."
      : presentation?.description ?? "Use this installed policy, limited by the parent’s permissions.",
    source: `registry/ee_template/${row.id}/ee_template.json`,
    goal: typeof row.goal_class === "string" ? row.goal_class : "",
    inheritsParentPolicy,
  };
}

export function allowedProjectPolicies(
  templates: readonly ProjectPolicyOption[],
  allowedIds: readonly string[],
): ProjectPolicyOption[] {
  const allowed = new Set(allowedIds);
  return templates.filter((option) => allowed.has(option.id)).sort((left, right) => (
    Number(right.inheritsParentPolicy) - Number(left.inheritsParentPolicy) || left.label.localeCompare(right.label)
  ));
}
