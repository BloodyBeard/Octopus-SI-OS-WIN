import { OcButton } from "../../design-system/components";
import { describeSchedule } from "./scheduleModel";
import { scheduledSuggestions, type ScheduledSuggestion } from "./scheduledSuggestions";
import type { ScheduledInterfaceProjection } from "./types";

export function ScheduledLanding({ disabledReason, onCreate, templates, timeDisabledReason }: {
  disabledReason: string;
  onCreate: (suggestion?: ScheduledSuggestion) => void;
  templates: ScheduledInterfaceProjection[];
  timeDisabledReason: string;
}) {
  const installed = templates.filter((template) => template.template).map((template): ScheduledSuggestion => ({
    id: `template:${template.interfaceId}`,
    emoji: template.schedule?.type === "inactivity" ? "✨" : "🕗",
    title: template.displayName,
    description: describeSchedule(template),
    instructions: template.instructions,
    cron: "",
    templateInterfaceId: template.interfaceId,
  }));
  const suggestions = [...installed.map((suggestion) => ({ suggestion, reason: disabledReason })), ...scheduledSuggestions.map((suggestion) => ({ suggestion, reason: timeDisabledReason }))];
  return (
    <div className="scheduled-landing">
      <div className="scheduled-landing-content">
        <header className="scheduled-landing-title">
          <span aria-hidden="true" className="scheduled-landing-emoji">🕗</span>
          <h1>Create a trigger</h1>
          <p>Let Octopus take care of ongoing work and keep you up to date.</p>
        </header>
        <ul aria-label="Trigger suggestions" className="scheduled-suggestions">
          {suggestions.map(({ suggestion, reason }) => (
            <li key={suggestion.id}>
              <button className="scheduled-suggestion" disabled={Boolean(reason)} onClick={() => onCreate(suggestion)} title={reason || undefined} type="button">
                <span aria-hidden="true" className="scheduled-suggestion-emoji">{suggestion.emoji}</span>
                <span className="scheduled-suggestion-copy"><strong>{suggestion.title}</strong><span>{suggestion.description}</span></span>
              </button>
            </li>
          ))}
        </ul>
        {disabledReason ? <p className="scheduled-create-note" role="status">{disabledReason}</p> : (
          <OcButton className="scheduled-new-custom" leadingIcon="plus" onClick={() => onCreate()} size="sm" variant="ghost">Create your own trigger</OcButton>
        )}
      </div>
    </div>
  );
}
