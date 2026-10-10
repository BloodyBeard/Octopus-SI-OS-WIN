export const HOST_FILESYSTEM_ROOT_SLOT = "host_filesystem_root";

export const TASK_ACCESS_MODES = ["project_access", "full_access"] as const;

export type TaskAccessMode = typeof TASK_ACCESS_MODES[number];

export function normalizeTaskAccessMode(value: unknown): TaskAccessMode {
  return value === "full_access" ? "full_access" : "project_access";
}

export function taskAccessModeFromDoorHostSlots(
  doorHostSlots: Readonly<Record<string, Readonly<Record<string, string>>>>,
): TaskAccessMode {
  return Object.values(doorHostSlots).some((slots) => Boolean(slots[HOST_FILESYSTEM_ROOT_SLOT]))
    ? "full_access"
    : "project_access";
}
