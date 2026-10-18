import { useRef, useState } from "react";
import { OcButton, OcDialog, OcDialogFooter, OcDialogHeader, OcIconButton, OcMenuItem } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { MessageTooltip } from "../chat/MessageTooltip";
import { describeInactivityState, describeSchedule, formatEvidenceTime, nextScheduledRunAtMs, scheduleExpression } from "./scheduleModel";
import type { ScheduledDelivery, ScheduledInterfaceProjection, ScheduledScopeOption } from "./types";

export function ScheduledDetail({ task, pending, canEdit, scopeOptions, onBack, onDelete, onEdit, onToggle, onUndo }: {
  task: ScheduledInterfaceProjection;
  pending: boolean;
  canEdit: boolean;
  scopeOptions: ScheduledScopeOption[];
  onBack: () => void;
  onDelete: () => void;
  onEdit: () => void;
  onToggle: () => void;
  onUndo: (delivery: ScheduledDelivery) => void;
}) {
  const [confirmUndo, setConfirmUndo] = useState<ScheduledDelivery | null>(null);
  const inactivity = task.schedule?.type === "inactivity" ? task.schedule : null;
  const scopeName = inactivity?.withinEe ? scopeOptions.find((option) => option.eeId === inactivity.withinEe)?.name ?? inactivity.withinEe : "All work";
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
          <DetailGrid rows={inactivity ? [
            ["When", describeSchedule(task)],
            ["Scope", scopeName],
            ["Now", describeInactivityState(task)],
            ["Last work", task.inactivityState?.lastActivityAtMs ? formatEvidenceTime(task.inactivityState.lastActivityAtMs) : "Not recorded"],
            ["Last triggered", task.lastRunAtMs ? formatEvidenceTime(task.lastRunAtMs) : "Not triggered yet"],
          ] : [
            ["When", describeSchedule(task)],
            ["Timezone", task.timezone || "Not configured"],
            ["Next run", task.status === "disabled" ? "Paused" : nextDueAtMs ? `${formatEvidenceTime(nextDueAtMs)}${estimated ? " · estimated" : ""}` : triggered ? "Does not repeat" : "Not recorded"],
            ["Last triggered", task.lastRunAtMs ? formatEvidenceTime(task.lastRunAtMs) : "Not triggered yet"],
          ]} />
        </section>
        {task.deliveries.length ? (
          <section className="scheduled-detail-section">
            <h2>Runs</h2>
            <ul className="scheduled-runs" aria-label="Runs started by this trigger">
              {task.deliveries.map((delivery) => <DeliveryRow canEdit={canEdit} delivery={delivery} key={delivery.seq} onUndo={() => setConfirmUndo(delivery)} pending={pending} />)}
            </ul>
          </section>
        ) : null}
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
      <OcDialog ariaLabelledBy="scheduled-undo-title" busy={pending} onClose={() => setConfirmUndo(null)} open={confirmUndo !== null} panelClassName="max-w-md">
        <OcDialogHeader closeDisabled={pending} description="Each recorded file returns to its exact earlier bytes. Undo stops if a file has changed since this run." onClose={() => setConfirmUndo(null)} title="Undo this run's changes?" titleId="scheduled-undo-title" />
        <OcDialogFooter>
          <OcButton disabled={pending} onClick={() => setConfirmUndo(null)} variant="ghost">Cancel</OcButton>
          <OcButton disabled={!canEdit || pending} onClick={() => { if (confirmUndo) onUndo(confirmUndo); setConfirmUndo(null); }} variant="primary">{pending ? "Restoring…" : "Undo changes"}</OcButton>
        </OcDialogFooter>
      </OcDialog>
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

function DeliveryRow({ canEdit, delivery, onUndo, pending }: { canEdit: boolean; delivery: ScheduledDelivery; onUndo: () => void; pending: boolean }) {
  const journal = delivery.journal;
  const files = journal?.changes.filter((change) => change.kind === "file") ?? [];
  const status = !journal ? "No change journal yet"
    : journal.error ? `Journal unreadable: ${journal.error}`
      : journal.outcome === "unchanged" ? "No change"
        : journal.undone ? "Undone"
          : `${files.length} ${files.length === 1 ? "file" : "files"} changed`;
  return (
    <li className="scheduled-run">
      <div className="scheduled-run-heading">
        <span className="scheduled-run-time">{formatEvidenceTime(delivery.atMs)}</span>
        <span className="scheduled-run-status">{status}</span>
        {journal?.undoable ? <OcButton disabled={!canEdit || pending} onClick={onUndo} size="sm" variant="secondary">Undo</OcButton> : null}
      </div>
      {journal?.summary ? <p className="scheduled-run-summary">{journal.summary}</p> : null}
      {journal && (journal.changes.length || journal.checks.length) ? (
        <details className="scheduled-interface-details">
          <summary>Changes and checks<Glyph name="chevronDown" size={14} /></summary>
          <ul className="scheduled-run-changes">
            {journal.changes.map((change, index) => change.kind === "file" ? (
              <li key={`${change.path}:${index}`} title={change.reason}>
                <span>{change.action} · {change.path}</span>
                <span>{change.status === "applied" ? change.restorable ? "in place" : `in place · ${change.unrestorableReason}` : change.status === "undone" ? "restored" : "changed since"}</span>
              </li>
            ) : (
              <li key={`other:${index}`} title={change.undo}><span>{change.description}</span><span>{change.undo ? `undo: ${change.undo}` : ""}</span></li>
            ))}
            {journal.checks.map((check, index) => <li key={`check:${index}`}><span>{check.check}</span><span>{check.result}</span></li>)}
          </ul>
        </details>
      ) : null}
      <span className="scheduled-run-ref">{delivery.runId}</span>
    </li>
  );
}

function DetailGrid({ rows }: { rows: Array<[string, string]> }) {
  return <dl className="scheduled-detail-grid">{rows.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>;
}
