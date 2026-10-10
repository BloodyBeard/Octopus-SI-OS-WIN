import { useEffect, useRef, useState } from "react";
import { OcButton, OcEmptyState, OcIconButton } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { ScheduledDetail } from "./ScheduledDetail";
import { ScheduledEditor } from "./ScheduledEditor";
import { ScheduledLanding } from "./ScheduledLanding";
import { ScheduledSidebar } from "./ScheduledSidebar";
import { useScheduledWorkspace } from "./ScheduledWorkspace";
import "./scheduled.css";

export function LiveScheduledView({ searchFocusTick, navigationInPane = false }: { searchFocusTick: number; navigationInPane?: boolean }) {
  const workspace = useScheduledWorkspace();
  const { selected, selectedId, editor, loading, error, mutationError, pending, canEdit, requestSearch } = workspace;
  const [navigationOpen, setNavigationOpen] = useState(false);
  const navigationRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (searchFocusTick > 0) {
      if (navigationInPane) setNavigationOpen(true);
      requestSearch();
    }
  }, [searchFocusTick, requestSearch, navigationInPane]);

  return (
    <section aria-label="Scheduled task details" className="scheduled-content">
      {navigationInPane ? <div className="scheduled-pane-navigation">
        <OcButton buttonRef={navigationRef} leadingIcon="scheduled" onClick={() => setNavigationOpen(!navigationOpen)} trailingIcon="chevronDown" variant="ghost">Scheduled tasks</OcButton>
        <OcDropdown anchorRef={navigationRef} aria-label="Scheduled tasks" className="scheduled-pane-navigation-list" onOpenChange={setNavigationOpen} open={navigationOpen} role="dialog" surface="opaque">
          <ScheduledSidebar onNavigate={() => setNavigationOpen(false)} />
        </OcDropdown>
      </div> : null}
      {mutationError ? (
        <div className="scheduled-error" role="alert">
          <Glyph name="warning" size={16} />
          <span>{mutationError}</span>
          <OcIconButton glyph="close" label="Dismiss error" onClick={workspace.dismissError} size="xs" variant="ghost" />
        </div>
      ) : null}
      {editor ? (
        <ScheduledEditor
          draft={editor.draft}
          key={`${editor.mode.kind}:${editor.draft.interfaceId}:${editor.revision}`}
          mode={editor.mode}
          onCancel={workspace.cancelEdit}
          onSave={workspace.save}
          pending={pending}
          templates={workspace.templates}
        />
      ) : error ? (
        <OcEmptyState
          action={<OcButton onClick={() => void workspace.reload()} size="sm" variant="secondary">Retry</OcButton>}
          detail={error}
          glyph="warning"
          title="Scheduled tasks could not be loaded"
          tone="danger"
        />
      ) : selected ? (
        <ScheduledDetail
          canEdit={canEdit}
          key={selected.interfaceId}
          onBack={() => workspace.selectTask("")}
          onDelete={() => workspace.deleteTask(selected)}
          onEdit={() => workspace.startEdit(selected)}
          onToggle={() => workspace.toggleTask(selected)}
          pending={pending}
          task={selected}
        />
      ) : loading ? (
        <div className="scheduled-content-status" role="status">Loading scheduled tasks…</div>
      ) : selectedId ? (
        <OcEmptyState
          action={<OcButton onClick={() => workspace.selectTask("")} size="sm" variant="secondary">Back to Scheduled</OcButton>}
          detail="This scheduled interface is no longer available."
          glyph="scheduled"
          title="Scheduled task unavailable"
        />
      ) : (
        <ScheduledLanding disabledReason={workspace.createUnavailableReason} onCreate={workspace.startCreate} />
      )}
    </section>
  );
}
