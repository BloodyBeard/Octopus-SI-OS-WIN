import { Glyph } from "../IconGlyphs";
import { MessageTooltip } from "../chat/MessageTooltip";

export type TasksView = "tasks" | "scheduled" | "archived";

export function TasksViewSwitcher({ value, onChange }: {
  value: TasksView;
  onChange: (view: TasksView) => void;
}) {
  return <div className="tasks-view-switcher" role="group" aria-label="Tasks views">
    <button
      aria-pressed={value === "tasks"}
      className="tasks-view-filter"
      onClick={() => onChange("tasks")}
      type="button"
    >All tasks</button>
    <div className="tasks-view-icon-controls">
      <MessageTooltip label="Triggers">
        <button
          aria-label="Triggers"
          aria-pressed={value === "scheduled"}
          className="tasks-view-filter is-icon"
          onClick={() => onChange("scheduled")}
          type="button"
        ><Glyph name="scheduled" size={16} /></button>
      </MessageTooltip>
      <MessageTooltip label="Archived tasks">
        <button
          aria-label="Archived tasks"
          aria-pressed={value === "archived"}
          className="tasks-view-filter is-icon"
          onClick={() => onChange("archived")}
          type="button"
        ><Glyph name="archive" size={16} /></button>
      </MessageTooltip>
    </div>
  </div>;
}
