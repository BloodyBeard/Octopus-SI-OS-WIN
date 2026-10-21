import type { GlyphName } from "../IconGlyphs";
import type { ActivityKind, MarkActivityItem } from "./markActivity";

export type ActivityPresentation = { label: string; category: string; glyph: GlyphName; description: string };

const requestKinds: ReadonlySet<ActivityKind> = new Set(["approval", "user_input", "elicitation"]);

const kindPresentation: Record<Exclude<ActivityKind, "mark">, ActivityPresentation> = {
  message: { label: "New reply", category: "Message", glyph: "chat", description: "The task answered. Open it to read the reply and continue." },
  approval: { label: "Approval needed", category: "Request", glyph: "lock", description: "The task is waiting for you to approve or decline an action." },
  user_input: { label: "Your answer is needed", category: "Request", glyph: "user", description: "The task asked a question and is waiting for your answer." },
  elicitation: { label: "Details requested", category: "Request", glyph: "chat", description: "The task needs more information from you to continue." },
};

const markPresentations: Record<string, ActivityPresentation> = {
  "tool.call.allowed": { label: "Tool use approved", category: "Tools", glyph: "tool", description: "Octopus checked a tool call against the task's permissions and let it run." },
  "tool.result.corresponds": { label: "Tool result confirmed", category: "Tools", glyph: "tool", description: "A tool's result was matched to the exact call that asked for it." },
  "emit.tool_call.present": { label: "Tool call prepared", category: "Tools", glyph: "tool", description: "The model produced a well-formed tool call." },
  "memory.capture.completed": { label: "Saved to memory", category: "Memory", glyph: "memory", description: "Octopus finished saving what it learned from recent work to memory." },
  "memory.experience.admitted": { label: "New experience to remember", category: "Memory", glyph: "memory", description: "A finished piece of work was accepted for Octopus to learn from." },
  "operator.approval": { label: "You approved an action", category: "Approvals", glyph: "check", description: "Your approval was recorded and the task continued." },
  "operator.input.provided": { label: "You answered a request", category: "Approvals", glyph: "user", description: "Your answer was recorded and handed back to the task." },
  "task.summary.received": { label: "Task summary ready", category: "Tasks", glyph: "taskSummary", description: "A task delivered its summary back to where it was started from." },
  "coder.build.ok": { label: "Build passed", category: "Code", glyph: "code", description: "The project built successfully." },
  "context.recipe.valid": { label: "Context prepared", category: "Context", glyph: "layers", description: "The context a task reads was assembled and checked." },
  "email.authorization.requested": { label: "Email access requested", category: "Access", glyph: "lock", description: "A task asked for permission to use email." },
  "hook.gate.passed": { label: "Hook check passed", category: "Checks", glyph: "check", description: "A configured hook let the step continue." },
  "lifecycle.decision.allowed": { label: "Step allowed", category: "Checks", glyph: "check", description: "A lifecycle check allowed the next step." },
  "ocr.text.contains": { label: "Text found on screen", category: "Computer Use", glyph: "desktop", description: "Expected text was recognized on the screen." },
};

const sourcePresentations: Record<string, string> = {
  system_worker_memory_observer: "Memory",
  system_worker_memory_projection: "Memory",
  system_worker_memory_consolidation: "Memory",
  system_worker_operator_home: "Home",
  system_worker_operator_task: "Task",
  system_worker_cron: "Triggers",
  system_worker_computer_use: "Computer Use",
};

export function isActivityRequest(item: MarkActivityItem): boolean {
  return requestKinds.has(item.kind);
}

export function activityPresentation(item: MarkActivityItem): ActivityPresentation {
  if (item.kind !== "mark") return kindPresentation[item.kind];
  return markPresentations[item.name] ?? {
    label: sentence(item.name.replaceAll(/[._]+/g, " ")),
    category: "Activity",
    glyph: "activity",
    description: "Octopus recorded this step while your work ran.",
  };
}

export function activityTitle(item: MarkActivityItem): string {
  return activityPresentation(item).label;
}

export function activitySource(item: MarkActivityItem): string {
  if (item.agentName) return item.agentName;
  if (item.sourceTitle) return item.sourceTitle;
  const original = item.workerOriginal || item.worker;
  if (sourcePresentations[original]) return sourcePresentations[original];
  const readable = original.replace(/^(?:lab_)?system_worker_/, "").replace(/_[0-9a-f]{16,}$/, "").replaceAll(/[._]+/g, " ").trim();
  return readable ? sentence(readable) : "Octopus";
}

export type ActivityRowGroup = { id: string; lead: MarkActivityItem; items: MarkActivityItem[] };
export type ActivitySection = { key: string; label: string; rows: ActivityRowGroup[] };

export function activityRows(items: readonly MarkActivityItem[]): ActivityRowGroup[] {
  const rows: ActivityRowGroup[] = [];
  for (const item of items) {
    const previous = rows.at(-1);
    if (previous && item.kind === "mark" && previous.lead.kind === "mark" && previous.lead.name === item.name
      && activitySource(previous.lead) === activitySource(item) && previous.lead.runId === item.runId) {
      previous.items.push(item);
      continue;
    }
    rows.push({ id: item.id, lead: item, items: [item] });
  }
  return rows;
}

export function activitySections(items: readonly MarkActivityItem[], now: number): ActivitySection[] {
  const startOfToday = new Date(now);
  startOfToday.setHours(0, 0, 0, 0);
  const today = startOfToday.getTime();
  const yesterday = today - 86_400_000;
  const buckets: Array<{ key: string; label: string; items: MarkActivityItem[] }> = [
    { key: "requests", label: "Needs you", items: [] },
    { key: "today", label: "Today", items: [] },
    { key: "yesterday", label: "Yesterday", items: [] },
    { key: "earlier", label: "Earlier", items: [] },
  ];
  for (const item of items) {
    const at = item.createdAt ?? -1;
    const bucket = isActivityRequest(item) ? buckets[0] : at >= today ? buckets[1] : at >= yesterday ? buckets[2] : buckets[3];
    bucket.items.push(item);
  }
  return buckets.filter((bucket) => bucket.items.length).map((bucket) => ({ key: bucket.key, label: bucket.label, rows: activityRows(bucket.items) }));
}

export function relativeActivityTime(at: number | null, now: number): string {
  if (at === null) return "";
  const seconds = Math.max(0, Math.round((now - at) / 1000));
  if (seconds < 60) return "now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.round(hours / 24);
  if (days < 7) return `${days}d`;
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(at);
}

function sentence(value: string): string {
  return value ? value[0].toLocaleUpperCase() + value.slice(1) : value;
}
