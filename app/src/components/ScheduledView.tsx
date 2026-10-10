import { useMemo, useState } from "react";
import "./icon-glyphs/worker";
import type { OctopusClientAccessProfile } from "../electron";
import { OcButton, OcDialog, OcDialogFooter, OcDialogHeader, OcIconButton, OcSegmented, OcTextArea, OcTextField, cx } from "../design-system/components";
import { appToast } from "../toasts";
import { Glyph, type GlyphName } from "./IconGlyphs";
import { AutomationRow, AutomationStatusToggle, SCHEDULED_SEARCH_ROW_CLASS, ScheduledSearchField } from "./scheduled/automationRow";
import { LiveScheduledView } from "./scheduled/LiveScheduledView";

type ScheduledFilter = "all" | "active" | "paused";
type ScheduledStatus = "active" | "paused";

type ScheduledRun = {
  id: string;
  createdAtMs: number;
  status: "completed" | "in_progress";
};

type ScheduledTask = {
  id: string;
  name: string;
  description: string;
  prompt: string;
  schedule: string;
  status: ScheduledStatus;
  nextRunAtMs: number;
  runs: ScheduledRun[];
};

type ScheduledDraft = Pick<ScheduledTask, "name" | "description" | "prompt" | "schedule">;

const STORAGE_KEY = "octopus.test.scheduled-tasks.v2";

const suggestions: Array<ScheduledDraft & { glyph: GlyphName; color: string }> = [
  {
    name: "Daily brief",
    description: "Start each weekday with a summary of recent work and priorities",
    prompt: "Summarize recent work, active runs, blockers, and the priorities that need attention today.",
    schedule: "Weekdays at 8:00 AM",
    glyph: "pulse",
    color: "text-token-charts-blue",
  },
  {
    name: "Weekly review",
    description: "Turn recent agent activity into a concise status update every Friday",
    prompt: "Review this week's completed and active runs and draft a concise status update.",
    schedule: "Fridays at 4:00 PM",
    glyph: "registry",
    color: "text-token-charts-purple",
  },
  {
    name: "Follow-up monitor",
    description: "Review recent run activity and flag anything that needs attention",
    prompt: "Review recent run evidence, identify material changes, and flag anything that needs attention.",
    schedule: "Weekdays at 9:00 AM",
    glyph: "dashboard",
    color: "text-token-charts-green",
  },
];

export function ScheduledView({ accessProfile, searchFocusTick, navigationInPane = false }: { accessProfile: OctopusClientAccessProfile; searchFocusTick: number; navigationInPane?: boolean }) {
  return accessProfile === "lab" ? <LabScheduledView searchFocusTick={searchFocusTick} /> : <LiveScheduledView navigationInPane={navigationInPane} searchFocusTick={searchFocusTick} />;
}

function LabScheduledView({ searchFocusTick }: { searchFocusTick: number }) {
  const testMode = true;
  const [tasks, setTasks] = useState<ScheduledTask[]>(() => testMode ? readTasks() : []);
  const [filter, setFilter] = useState<ScheduledFilter>("all");
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<ScheduledDraft | null>(null);
  const [editingTaskId, setEditingTaskId] = useState("");
  const [deleteCandidate, setDeleteCandidate] = useState<ScheduledTask | null>(null);
  const selected = tasks.find((task) => task.id === selectedId) ?? null;
  const visible = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    return tasks.filter((task) => (
      (filter === "all" || task.status === filter)
      && (!needle || `${task.name} ${task.description} ${task.schedule}`.toLocaleLowerCase().includes(needle))
    ));
  }, [filter, query, tasks]);

  function commit(next: ScheduledTask[]) {
    setTasks(next);
    if (testMode) window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  }

  function updateTask(id: string, update: (task: ScheduledTask) => ScheduledTask) {
    commit(tasks.map((task) => task.id === id ? update(task) : task));
  }

  function createTask(next: ScheduledDraft) {
    const task: ScheduledTask = {
      ...next,
      id: `scheduled_${crypto.randomUUID()}`,
      status: "active",
      nextRunAtMs: nextOccurrence(),
      runs: [],
    };
    commit([task, ...tasks]);
    setDraft(null);
    setSelectedId(task.id);
  }

  function saveDraft(next: ScheduledDraft) {
    if (!editingTaskId) {
      createTask(next);
      return;
    }
    updateTask(editingTaskId, (task) => ({ ...task, ...next }));
    setDraft(null);
    setEditingTaskId("");
  }

  function runNow(task: ScheduledTask) {
    const runId = `scheduled_run_${crypto.randomUUID()}`;
    updateTask(task.id, (current) => ({
      ...current,
      runs: [{ id: runId, createdAtMs: Date.now(), status: "in_progress" }, ...current.runs],
    }));
    window.setTimeout(() => {
      setTasks((current) => {
        const next = current.map((candidate) => candidate.id === task.id ? {
          ...candidate,
          runs: candidate.runs.map((run) => run.id === runId ? { ...run, status: "completed" as const } : run),
        } : candidate);
        window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
        return next;
      });
    }, 900);
  }

  function deleteTask(task: ScheduledTask) {
    const index = tasks.findIndex((candidate) => candidate.id === task.id);
    if (index < 0) return;
    commit(tasks.filter((candidate) => candidate.id !== task.id));
    if (selectedId === task.id) setSelectedId("");
    setDeleteCandidate(null);
    appToast.info(`${task.name} deleted`, 8, {
      dedupeKey: `scheduled-lab-delete:${task.id}`,
      action: {
        label: "Undo",
        onSelect: () => {
          setTasks((current) => {
            if (current.some((candidate) => candidate.id === task.id)) return current;
            const next = [...current];
            next.splice(Math.min(index, next.length), 0, task);
            window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
            return next;
          });
        },
      },
    });
  }

  const deleteDialog = (
    <OcDialog
      ariaLabelledBy="scheduled-lab-delete-title"
      onClose={() => setDeleteCandidate(null)}
      open={Boolean(deleteCandidate)}
      panelClassName="max-w-md"
    >
      {deleteCandidate ? (
        <>
          <OcDialogHeader
            description="This removes the local Test-profile task and its local run history. You can undo the deletion for eight seconds."
            onClose={() => setDeleteCandidate(null)}
            title={`Delete ${deleteCandidate.name}?`}
            titleId="scheduled-lab-delete-title"
          />
          <OcDialogFooter>
            <OcButton onClick={() => setDeleteCandidate(null)} variant="ghost">Cancel</OcButton>
            <OcButton onClick={() => deleteTask(deleteCandidate)} tone="danger" variant="primary">Delete</OcButton>
          </OcDialogFooter>
        </>
      ) : null}
    </OcDialog>
  );

  if (draft) {
    return (
      <ScheduledEditor
        draft={draft}
        editing={Boolean(editingTaskId)}
        onCancel={() => {
          setDraft(null);
          setEditingTaskId("");
        }}
        onSave={saveDraft}
      />
    );
  }

  if (selected) {
    return (
      <>
        <ScheduledDetail
          onBack={() => setSelectedId("")}
          onDelete={() => setDeleteCandidate(selected)}
          onEdit={() => {
            setEditingTaskId(selected.id);
            setDraft({ name: selected.name, description: selected.description, prompt: selected.prompt, schedule: selected.schedule });
          }}
          onRunNow={() => runNow(selected)}
          onStatusChange={(status) => updateTask(selected.id, (task) => ({ ...task, status }))}
          task={selected}
        />
        {deleteDialog}
      </>
    );
  }

  return (
    <>
    <section aria-label="Scheduled tasks" className="scheduled-view relative h-full min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]">
      <div className="flex min-h-full w-full flex-col">
        <div className={cx("mx-auto w-full max-w-[var(--thread-content-max-width)] px-panel pt-panel", tasks.length ? null : "pb-4")}>
          <div className="flex items-start justify-between gap-4 px-2">
            <div className="flex min-w-0 flex-col gap-2">
              <h1 className="heading-xl font-normal text-token-foreground">Scheduled tasks</h1>
              <div className="text-lg leading-6 text-token-text-secondary">
                Create time interfaces that deliver artifacts to exact tasks and runs
              </div>
            </div>
            <div className="shrink-0">
              <OcButton
                disabled={!testMode}
                leadingIcon="plus"
                onClick={() => setDraft(emptyDraft())}
                size="sm"
                title={testMode ? "Create scheduled task" : "No scheduler interface is projected"}
              >
                Create
              </OcButton>
            </div>
          </div>
        </div>

        {tasks.length ? (
          <div className={SCHEDULED_SEARCH_ROW_CLASS}>
            <div className="mx-auto flex w-full max-w-[var(--thread-content-max-width)] items-center gap-2 px-panel pt-panel pb-2">
              <ScheduledSearchField
                searchFocusTick={searchFocusTick}
                id="scheduled-lab-search"
                label="Search scheduled tasks"
                onChange={setQuery}
                placeholder="Search scheduled tasks"
                value={query}
              />
            </div>
          </div>
        ) : null}

        <div className={cx("mx-auto flex min-h-0 w-full max-w-[var(--thread-content-max-width)] flex-1 flex-col px-panel pb-panel", tasks.length ? "pt-5" : "pt-panel")}>
          {tasks.length ? (
            <div className="flex items-center justify-between gap-4 px-3 pb-2">
              <OcSegmented
                label="Scheduled task status"
                onChange={setFilter}
                options={[
                  { value: "all", label: "All" },
                  { value: "active", label: "Active" },
                  { value: "paused", label: "Paused" },
                ]}
                size="sm"
                value={filter}
              />
            </div>
          ) : null}

          {visible.length ? (
            <div className="flex flex-col gap-1" role="list">
              {visible.map((task) => (
                <div className="relative" key={task.id} role="listitem">
                  <ScheduledRow
                    onDelete={() => setDeleteCandidate(task)}
                    onRunNow={() => runNow(task)}
                    onSelect={() => setSelectedId(task.id)}
                    onToggle={() => updateTask(task.id, (current) => ({ ...current, status: current.status === "paused" ? "active" : "paused" }))}
                    task={task}
                  />
                </div>
              ))}
            </div>
          ) : tasks.length || filter !== "all" || query ? (
            <div className="flex min-h-56 items-center justify-center px-4 text-center text-sm text-tertiary">
              {testMode ? "No scheduled tasks found" : "No scheduler interface is projected for this client"}
            </div>
          ) : null}

          {testMode && filter === "all" && !query ? (
            <div className={cx(visible.length > 0 && "mt-3 border-t border-default pt-3")}>
              <h2 className="px-2 pb-2 text-lg leading-6 text-token-text-secondary">Suggestions</h2>
              <div className="flex flex-col gap-1" role="list">
                {suggestions.map((suggestion) => (
                  <div className="relative" key={suggestion.name} role="listitem">
                    <SuggestionRow onSelect={() => setDraft(suggestion)} suggestion={suggestion} />
                  </div>
                ))}
              </div>
            </div>
          ) : null}
        </div>
      </div>
    </section>
    {deleteDialog}
    </>
  );
}

function ScheduledRow({ onDelete, onRunNow, onSelect, onToggle, task }: {
  onDelete: () => void;
  onRunNow: () => void;
  onSelect: () => void;
  onToggle: () => void;
  task: ScheduledTask;
}) {
  const running = task.runs[0]?.status === "in_progress";
  const paused = task.status === "paused";
  return (
    <AutomationRow
      actions={[
        { glyph: "play", label: "Run now", disabled: running, onSelect: onRunNow },
        { glyph: paused ? "play" : "pause", label: paused ? "Resume" : "Pause", onSelect: onToggle },
        { glyph: "trash", label: "Delete", onSelect: onDelete, tone: "danger" },
      ]}
      ariaLabel={task.name}
      dimmed={paused}
      onSelect={onSelect}
      segments={[task.schedule, running ? "In progress" : paused ? "Paused" : `Next run ${relativeTime(task.nextRunAtMs)}`]}
      statusIcon={
        running ? <span className="composer-spinner shrink-0" />
          : paused ? <AutomationStatusToggle glyph="pauseCircle" hoverGlyph="playCircle" label="Resume" onToggle={onToggle} />
            : <AutomationStatusToggle glyph="scheduled" hoverGlyph="pauseCircle" label="Pause" onToggle={onToggle} />
      }
      title={task.name}
    />
  );
}

function SuggestionRow({ onSelect, suggestion }: { onSelect: () => void; suggestion: (typeof suggestions)[number] }) {
  return (
    <AutomationRow
      ariaLabel={suggestion.name}
      onSelect={onSelect}
      secondaryTitle={suggestion.schedule}
      segments={[suggestion.description]}
      statusIcon={
        <span className={`relative inline-flex ${suggestion.color}`}>
          <Glyph className="icon-sm group-focus-within:opacity-0 group-hover:opacity-0" name={suggestion.glyph} size={18} />
          <Glyph className="icon-sm absolute inset-0 text-token-description-foreground opacity-0 group-focus-within:opacity-100 group-hover:opacity-100" name="plus" size={18} />
        </span>
      }
      title={suggestion.name}
    />
  );
}

function ScheduledDetail({ onBack, onDelete, onEdit, onRunNow, onStatusChange, task }: {
  onBack: () => void;
  onDelete: () => void;
  onEdit: () => void;
  onRunNow: () => void;
  onStatusChange: (status: ScheduledStatus) => void;
  task: ScheduledTask;
}) {
  const running = task.runs[0]?.status === "in_progress";
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="surface-titlebar-spacer flex h-toolbar shrink-0 items-center px-panel">
        <button className="flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground" onClick={onBack} type="button">
          <Glyph name="arrowLeft" size={14} />
          <span>Scheduled</span><span className="text-token-input-placeholder-foreground">/</span><span className="truncate text-token-foreground">{task.name}</span>
        </button>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-toolbar pt-panel pb-panel">
        <div className="mx-auto flex w-full max-w-[var(--thread-content-max-width)] flex-col gap-7">
          <header className="flex flex-wrap items-start justify-between gap-4 px-2">
            <div className="min-w-0 flex-1">
              <h1 className="heading-xl font-normal text-token-foreground">{task.name}</h1>
              <p className="mt-1 text-base text-token-text-secondary">{task.description}</p>
            </div>
            <div className="flex items-center gap-2">
              <OcButton leadingIcon="edit" onClick={onEdit} size="sm" variant="secondary">Edit</OcButton>
              <OcButton disabled={running} leadingIcon="play" onClick={onRunNow} size="sm">Run now</OcButton>
              <OcButton leadingIcon={task.status === "paused" ? "play" : "pause"} onClick={() => onStatusChange(task.status === "paused" ? "active" : "paused")} size="sm" variant="secondary">
                {task.status === "paused" ? "Resume" : "Pause"}
              </OcButton>
              <OcIconButton glyph="trash" label="Delete scheduled task" onClick={onDelete} tone="danger" variant="ghost" />
            </div>
          </header>
          <ScheduledSection title="Schedule">
            <dl className="grid grid-cols-[9rem_minmax(0,1fr)] gap-x-5 gap-y-3 px-2 text-sm">
              <dt className="text-token-text-secondary">Status</dt><dd className="m-0 text-token-foreground">{running ? "In progress" : task.status === "paused" ? "Paused" : "Active"}</dd>
              <dt className="text-token-text-secondary">Runs</dt><dd className="m-0 text-token-foreground">{task.schedule}</dd>
              <dt className="text-token-text-secondary">Next run</dt><dd className="m-0 text-token-foreground">{task.status === "paused" ? "None" : new Date(task.nextRunAtMs).toLocaleString()}</dd>
            </dl>
          </ScheduledSection>
          <ScheduledSection title="Instructions">
            <p className="m-0 px-2 text-base leading-6 whitespace-pre-wrap text-default select-text">{task.prompt}</p>
          </ScheduledSection>
          <ScheduledSection title="Previous runs">
            {task.runs.length ? (
              <div className="flex flex-col gap-1">
                {task.runs.map((run) => (
                  <div className="flex min-h-10 items-center gap-3 rounded-lg px-2 text-sm hover:bg-token-list-hover-background" key={run.id}>
                    <span className={`size-1.5 rounded-full ${run.status === "completed" ? "bg-token-charts-green" : "bg-token-description-foreground"}`} />
                    <span className="min-w-0 flex-1 text-token-foreground">{run.status === "completed" ? "Completed" : "In progress"}</span>
                    <span className="tabular-nums text-token-description-foreground">{new Date(run.createdAtMs).toLocaleString()}</span>
                  </div>
                ))}
              </div>
            ) : <p className="m-0 px-2 py-5 text-sm text-token-description-foreground">No previous runs</p>}
          </ScheduledSection>
        </div>
      </div>
    </section>
  );
}

function ScheduledEditor({ draft, editing, onCancel, onSave }: {
  draft: ScheduledDraft;
  editing: boolean;
  onCancel: () => void;
  onSave: (draft: ScheduledDraft) => void;
}) {
  const [value, setValue] = useState(draft);
  const valid = value.name.trim() && value.prompt.trim() && value.schedule.trim();
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="surface-titlebar-spacer flex h-toolbar shrink-0 items-center px-panel">
        <button className="flex items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground" onClick={onCancel} type="button">
          <Glyph name="arrowLeft" size={14} /> Back to scheduled tasks
        </button>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-toolbar pt-panel pb-panel">
        <form className="mx-auto flex w-full max-w-2xl flex-col gap-6" onSubmit={(event) => { event.preventDefault(); if (valid) onSave(value); }}>
          <header><h1 className="heading-xl font-normal text-token-foreground">{editing ? "Edit scheduled task" : "New scheduled task"}</h1><p className="mt-1 text-base text-token-text-secondary">Configure an isolated scheduled task for the Test profile.</p></header>
          <label className="flex flex-col gap-2 text-sm text-token-text-secondary">Name<OcTextField autoFocus label="Name" onChange={(event) => setValue({ ...value, name: event.target.value })} placeholder="Scheduled task name" showLabel={false} value={value.name} /></label>
          <label className="flex flex-col gap-2 text-sm text-token-text-secondary">Description<OcTextField label="Description" onChange={(event) => setValue({ ...value, description: event.target.value })} placeholder="What this task does" showLabel={false} value={value.description} /></label>
          <OcTextArea label="Instructions" onChange={(event) => setValue({ ...value, prompt: event.target.value })} placeholder="What should the agent do?" showLabel textareaClassName="min-h-32" value={value.prompt} />
          <label className="flex flex-col gap-2 text-sm text-token-text-secondary">Schedule<OcTextField label="Schedule" onChange={(event) => setValue({ ...value, schedule: event.target.value })} placeholder="Weekdays at 8:00 AM" showLabel={false} value={value.schedule} /></label>
          <div className="flex justify-end gap-2"><OcButton onClick={onCancel} type="button" variant="secondary">Cancel</OcButton><OcButton disabled={!valid} type="submit">{editing ? "Save changes" : "Create scheduled task"}</OcButton></div>
        </form>
      </div>
    </section>
  );
}

function ScheduledSection({ children, title }: { children: React.ReactNode; title: string }) {
  return <section className="flex flex-col gap-3"><h2 className="px-2 text-lg font-medium text-token-foreground">{title}</h2>{children}</section>;
}

function readTasks(): ScheduledTask[] {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(STORAGE_KEY) || "null");
    if (Array.isArray(parsed)) return parsed.filter(isScheduledTask);
  } catch {
    return seedTasks();
  }
  return seedTasks();
}

function isScheduledTask(value: unknown): value is ScheduledTask {
  if (!value || typeof value !== "object") return false;
  const task = value as Partial<ScheduledTask>;
  return typeof task.id === "string" && typeof task.name === "string" && typeof task.prompt === "string" && Array.isArray(task.runs);
}

function seedTasks(): ScheduledTask[] {
  const now = Date.now();
  return [
    {
      id: "test_daily_projection_brief",
      name: "Daily projection brief",
      description: "Summarize active execution environments and recent evidence",
      prompt: "Review the Test projection and summarize active runs, worker environments, blockers, and recent evidence.",
      schedule: "Weekdays at 8:00 AM",
      status: "active",
      nextRunAtMs: now + 43_200_000,
      runs: [{ id: "test_run_1", createdAtMs: now - 86_400_000, status: "completed" }],
    },
    {
      id: "test_registry_drift_review",
      name: "Registry drift review",
      description: "Flag changes in projected interfaces and bindings",
      prompt: "Compare the current Test registry projection with the previous snapshot and report meaningful drift.",
      schedule: "Mondays at 10:00 AM",
      status: "paused",
      nextRunAtMs: now + 259_200_000,
      runs: [],
    },
  ];
}

function emptyDraft(): ScheduledDraft {
  return { name: "", description: "", prompt: "", schedule: "" };
}

function nextOccurrence(): number {
  return Date.now() + 86_400_000;
}

function relativeTime(timestamp: number): string {
  const delta = Math.max(0, timestamp - Date.now());
  const hours = Math.max(1, Math.round(delta / 3_600_000));
  return hours < 24 ? `in ${hours}h` : `in ${Math.round(hours / 24)}d`;
}
