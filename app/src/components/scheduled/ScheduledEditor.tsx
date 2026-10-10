import { useMemo, useState } from "react";
import { OcButton, OcSegmented, OcSelect, OcSwitch, OcTextArea, OcTextField } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { draftFromTemplate, formatSchedule, scheduledDraftError } from "./scheduleModel";
import type { ScheduledEditorDraft, ScheduledEditorMode, ScheduledInterfaceProjection } from "./types";

export function ScheduledEditor({
  draft: initialDraft,
  mode,
  pending,
  templates,
  onCancel,
  onSave,
}: {
  draft: ScheduledEditorDraft;
  mode: ScheduledEditorMode;
  pending: boolean;
  templates: ScheduledInterfaceProjection[];
  onCancel: () => void;
  onSave: (draft: ScheduledEditorDraft, source: ScheduledInterfaceProjection) => void;
}) {
  const [draft, setDraft] = useState(initialDraft);
  const creating = mode.kind === "create";
  const source = templates.find((template) => template.interfaceId === draft.sourceInterfaceId)
    ?? (mode.kind === "create" ? mode.source : mode.task);
  const error = scheduledDraftError(draft, creating);
  const sourceOptions = useMemo(() => templates.map((template) => ({
    value: template.interfaceId,
    label: template.displayName,
    description: `${template.runTemplate?.eeId ?? "Unknown EE"} · ${template.runTemplate?.workerInterfaceId ?? "Unknown worker"}`,
  })), [templates]);

  function chooseSource(interfaceId: string) {
    const selected = templates.find((template) => template.interfaceId === interfaceId);
    if (!selected) return;
    const replacement = draftFromTemplate(selected);
    setDraft({
      ...replacement,
      interfaceId: draft.interfaceId,
      instructions: draft.instructions || replacement.instructions,
    });
  }

  return (
    <section aria-label={creating ? "Create scheduled interface" : "Edit scheduled interface"} className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="flex h-toolbar shrink-0 items-center justify-between gap-3 px-panel">
        <button className="flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground" onClick={onCancel} type="button">
          <Glyph name="arrowLeft" size={14} />
          <span>Scheduled</span>
        </button>
        <div className="flex items-center gap-2">
          <OcButton disabled={pending} onClick={onCancel} size="sm" variant="secondary">Cancel</OcButton>
          <OcButton disabled={Boolean(error) || pending || !source.cloneEligible} onClick={() => onSave(draft, source)} size="sm">
            {pending ? "Saving…" : creating ? "Create" : "Save"}
          </OcButton>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-toolbar pt-panel pb-panel">
        <div className="mx-auto flex w-full max-w-2xl flex-col gap-7">
          <header className="flex flex-col gap-1">
            <h1 className="heading-xl font-normal text-token-foreground">{creating ? "New scheduled task" : "Edit scheduled task"}</h1>
            <p className="m-0 text-base text-token-text-secondary">
              Choose instructions and timing for this task.
            </p>
          </header>

          {creating ? (
            <EditorSection title="Task template" detail="Choose the registered template this task will use.">
              <OcSelect
                label="Scheduled interface template"
                onChange={chooseSource}
                options={sourceOptions}
                showLabel
                value={source.interfaceId}
              />
              {!source.cloneEligible ? (
                <InlineNotice tone="warning">This template has no outbound interface binding and cannot create useful work.</InlineNotice>
              ) : null}
              <details className="scheduled-interface-details"><summary>Template details<Glyph name="chevronDown" size={14} /></summary><TopologyCard task={source} /></details>
            </EditorSection>
          ) : null}

          <EditorSection title="Identity" detail={creating ? "The interface folder is the stable route identity." : "Interface identity cannot be renamed in place."}>
            <OcTextField
              autoFocus={creating}
              disabled={!creating}
              label="Interface id"
              onChange={(event) => setDraft({ ...draft, interfaceId: event.target.value.trim().toLowerCase() })}
              placeholder="world_daily_brief"
              showLabel
              value={draft.interfaceId}
            />
          </EditorSection>

          <EditorSection title="Instructions">
            <OcTextArea
              label="Task instructions"
              onChange={(event) => setDraft({ ...draft, instructions: event.target.value })}
              placeholder="What should Octopus do when this task runs?"
              showLabel
              textareaClassName="min-h-36"
              value={draft.instructions}
            />
          </EditorSection>

          <EditorSection title="Schedule">
            <OcSegmented
              label="Schedule type"
              onChange={(type) => setDraft({ ...draft, type, fireOnStart: type === "once" ? false : draft.fireOnStart })}
              options={[
                { value: "once", label: "Once" },
                { value: "interval", label: "Interval" },
                { value: "cron", label: "Cron" },
              ]}
              value={draft.type}
            />
            {draft.type === "interval" ? (
              <OcTextField
                inputMode="decimal"
                label="Interval in seconds"
                min="0.001"
                onChange={(event) => setDraft({ ...draft, intervalSeconds: Number(event.target.value) })}
                showLabel
                step="any"
                type="number"
                value={Number.isFinite(draft.intervalSeconds) ? String(draft.intervalSeconds) : ""}
              />
            ) : draft.type === "once" ? (
              <OcTextField
                description={`Your time zone: ${Intl.DateTimeFormat().resolvedOptions().timeZone}`}
                label="Date and time"
                onChange={(event) => setDraft({ ...draft, dueAt: event.target.value ? new Date(event.target.value).toISOString() : "" })}
                showLabel
                type="datetime-local"
                value={localDateTime(draft.dueAt)}
              />
            ) : (
              <OcTextField
                description="Five fields: minute hour day-of-month month day-of-week"
                label="Cron expression"
                onChange={(event) => setDraft({ ...draft, cron: event.target.value })}
                placeholder="0 9 * * 1-5"
                showLabel
                value={draft.cron ?? ""}
              />
            )}
            {draft.type !== "once" ? <><OcTextField
              description="Use local, UTC, or an IANA timezone such as Europe/Kyiv"
              label="Timezone"
              onChange={(event) => setDraft({ ...draft, timezone: event.target.value })}
              showLabel
              value={draft.timezone}
            />
            <OcSwitch
              checked={draft.fireOnStart}
              description="Emit once when this activation is first raised, before the recurring schedule."
              label="Fire on start"
              onCheckedChange={(fireOnStart) => setDraft({ ...draft, fireOnStart })}
              showLabel
            /></> : null}
            <div className="rounded-lg border border-token-border px-3 py-2.5 text-sm text-token-text-secondary">
              <span className="text-token-foreground">Preview</span>
              <span className="mx-2 text-token-input-placeholder-foreground">·</span>
              {formatSchedule({
                schedule: draft.type === "interval"
                  ? { type: "interval", intervalSeconds: draft.intervalSeconds ?? 0 }
                  : draft.type === "once" ? { type: "once", dueAt: draft.dueAt ?? "" }
                    : { type: "cron", cron: draft.cron ?? "" },
                timezone: draft.timezone,
              })}
            </div>
          </EditorSection>

          {error ? <InlineNotice tone="danger">{error}</InlineNotice> : null}
        </div>
      </div>
    </section>
  );
}

function TopologyCard({ task }: { task: ScheduledInterfaceProjection }) {
  const topology = task.runTemplate;
  if (!topology) return <InlineNotice tone="danger">The run template is not projectable.</InlineNotice>;
  return (
    <dl className="grid grid-cols-[8.5rem_minmax(0,1fr)] gap-x-4 gap-y-2 rounded-lg border border-token-border px-3 py-3 text-sm">
      <dt className="text-token-text-secondary">Run</dt><dd className="m-0 truncate text-token-foreground" title={topology.runId}>{topology.runId}</dd>
      <dt className="text-token-text-secondary">EE</dt><dd className="m-0 truncate text-token-foreground" title={topology.eeId}>{topology.eeId}</dd>
      <dt className="text-token-text-secondary">Worker binding</dt><dd className="m-0 truncate text-token-foreground" title={topology.workerInterfaceId}>{topology.workerInterfaceId}</dd>
      <dt className="text-token-text-secondary">Outbounds</dt><dd className="m-0 text-token-foreground">{topology.outboundInterfaceIds.length ? topology.outboundInterfaceIds.join(", ") : "None"}</dd>
      <dt className="text-token-text-secondary">Bindings</dt><dd className="m-0 text-token-foreground">{topology.artifactBindingCount} artifacts · {topology.checkpointCount} checkpoints</dd>
    </dl>
  );
}

function EditorSection({ children, detail, title }: { children: React.ReactNode; detail?: string; title: string }) {
  return (
    <section className="flex flex-col gap-3">
      <div className="flex flex-col gap-0.5">
        <h2 className="m-0 text-lg font-medium text-token-foreground">{title}</h2>
        {detail ? <p className="m-0 text-sm leading-5 text-token-text-secondary">{detail}</p> : null}
      </div>
      {children}
    </section>
  );
}

function InlineNotice({ children, tone }: { children: React.ReactNode; tone: "warning" | "danger" }) {
  return (
    <div className={`flex items-start gap-2 rounded-lg border px-3 py-2.5 text-sm ${tone === "danger" ? "border-token-error-border bg-token-error-background text-token-error-foreground" : "border-token-border bg-token-list-hover-background text-token-text-secondary"}`}>
      <Glyph className="mt-0.5 shrink-0" name="warning" size={14} />
      <span>{children}</span>
    </div>
  );
}

function localDateTime(value: string | undefined): string {
  if (!value) return "";
  const at = new Date(value);
  if (!Number.isFinite(at.getTime())) return "";
  const local = new Date(at.getTime() - at.getTimezoneOffset() * 60_000);
  return local.toISOString().slice(0, 16);
}
