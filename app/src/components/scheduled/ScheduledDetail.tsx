import { useRef, useState } from "react";
import { OcButton, OcDialog, OcDialogFooter, OcDialogHeader, OcIconButton, OcMenuItem } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { MessageTooltip } from "../chat/MessageTooltip";
import { describeSchedule, formatEvidenceTime, nextScheduledRunAtMs, scheduleExpression } from "./scheduleModel";
import type { ScheduledInterfaceProjection } from "./types";

export function ScheduledDetail({ task, pending, canEdit, onBack, onDelete, onEdit, onToggle }: {
  task: ScheduledInterfaceProjection;
  pending: boolean;
  canEdit: boolean;
  onBack: () => void;
  onDelete: () => void;
  onEdit: () => void;
  onToggle: () => void;
}) {
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const nextDueAtMs = task.status === "enabled" ? nextScheduledRunAtMs(task) : null;
  const estimated = nextDueAtMs !== task.nextRunAtMs || task.nextRunSource === "";
  const triggered = task.schedule?.type === "once" && task.lastRunAtMs !== null && task.nextRunAtMs === null;
  return (
    <section aria-label={task.displayName} className="scheduled-detail">
      <div className="scheduled-detail-content">
        <div className="scheduled-detail-actions">
          <MessageTooltip label="Back to Triggers"><OcIconButton appearance="compact" disabled={pending} glyph="arrowLeft" label="Back to Triggers" onClick={onBack} title="" /></MessageTooltip>
          <span className="scheduled-detail-state">{task.status === "enabled" ? triggered ? "Triggered" : "Active" : task.status === "disabled" ? "Paused" : "Needs attention"}</span>
          <OcButton disabled={!canEdit || !task.valid || pending} onClick={onEdit} size="sm" variant="secondary">Edit</OcButton>
          <MessageTooltip label="Trigger actions"><OcIconButton appearance="compact" buttonRef={menuRef} glyph="more" label="Trigger actions" onClick={() => setMenuOpen(!menuOpen)} title="" /></MessageTooltip>
          <OcDropdown anchorRef={menuRef} align="end" aria-label="Trigger actions" onOpenChange={setMenuOpen} open={menuOpen}>
            <OcMenuItem disabled={!canEdit || !task.valid || pending} label="Edit" leadingIcon="edit" onClick={() => { setMenuOpen(false); onEdit(); }} role="menuitem" />
            <OcMenuItem disabled={!canEdit || !task.valid || pending || triggered} label={task.status === "disabled" ? "Resume" : "Pause"} leadingIcon={task.status === "disabled" ? "play" : "pause"} onClick={() => { setMenuOpen(false); onToggle(); }} role="menuitem" />
            <OcMenuItem disabled={!canEdit || pending} label="Delete" leadingIcon="trash" onClick={() => { setMenuOpen(false); setConfirmDelete(true); }} role="menuitem" tone="danger" />
          </OcDropdown>
        </div>
        <header className="scheduled-detail-heading">
          <h1>{task.displayName}</h1>
          <p><Glyph name="scheduled" size={16} />{describeSchedule(task)}{task.timezone && task.schedule?.type !== "once" && task.schedule?.type !== "inactivity" ? ` · ${task.timezone}` : ""}</p>
        </header>
        {!task.valid ? <div className="scheduled-detail-error" role="alert"><Glyph name="warning" size={16} /><span>{task.error}</span></div> : null}
        <section className="scheduled-detail-section">
          <h2>Instructions</h2>
          <p className="scheduled-detail-instructions">{task.instructions || "No default instructions. This task may use its configured environment override."}</p>
        </section>
        <section className="scheduled-detail-section">
          <h2>Trigger</h2>
          <DetailGrid rows={[
            ["When", describeSchedule(task)],
            ...(task.schedule?.type === "inactivity" ? [] : [["Timezone", task.timezone || "Not configured"] as [string, string]]),
            ["Next run", task.status === "disabled" ? "Paused" : nextDueAtMs ? `${formatEvidenceTime(nextDueAtMs)}${estimated ? " · estimated" : ""}` : triggered ? "Does not repeat" : task.schedule?.type === "inactivity" ? "Waiting for new activity" : "Not recorded"],
            ["Last triggered", task.lastRunAtMs ? formatEvidenceTime(task.lastRunAtMs) : "Not triggered yet"],
          ]} />
        </section>
        <details className="scheduled-interface-details">
          <summary>Interface details<Glyph name="chevronDown" size={14} /></summary>
          <DetailGrid rows={[
            ["Interface", task.interfaceId],
            ["Expression", scheduleExpression(task)],
            ["Fire on start", task.fireOnStart ? "Yes" : "No"],
            ["Run", task.runTemplate?.runId ?? "Not available"],
            ["EE", task.runTemplate?.eeId ?? "Not available"],
            ["Worker binding", task.runTemplate?.workerInterfaceId ?? "Not available"],
            ["Outbounds", task.runTemplate?.outboundInterfaceIds.join(", ") || "None"],
            ["Checkpoints", String(task.runTemplate?.checkpointCount ?? 0)],
            ["Artifact bindings", String(task.runTemplate?.artifactBindingCount ?? 0)],
            ["Movement", task.lastMovementSeq == null ? "Not recorded" : `seq ${task.lastMovementSeq}`],
            ...(task.reminderEnv ? [["Environment override", task.reminderEnv] as [string, string]] : []),
          ]} />
        </details>
      </div>
      <OcDialog ariaLabelledBy="scheduled-delete-title" busy={pending} onClose={() => setConfirmDelete(false)} open={confirmDelete} panelClassName="max-w-md">
        <OcDialogHeader closeDisabled={pending} description="This removes the trigger activation and its owned run record. Ledger evidence is kept." onClose={() => setConfirmDelete(false)} title={`Delete ${task.displayName}?`} titleId="scheduled-delete-title" />
        <OcDialogFooter>
          <OcButton disabled={pending} onClick={() => setConfirmDelete(false)} variant="ghost">Cancel</OcButton>
          <OcButton disabled={!canEdit || pending} onClick={onDelete} tone="danger" variant="primary">{pending ? "Deleting…" : "Delete"}</OcButton>
        </OcDialogFooter>
      </OcDialog>
    </section>
  );
}

function DetailGrid({ rows }: { rows: Array<[string, string]> }) {
  return <dl className="scheduled-detail-grid">{rows.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>;
}
