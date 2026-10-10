import { OcButton } from "../../design-system/components";
import { scheduledSuggestions, type ScheduledSuggestion } from "./scheduledSuggestions";

export function ScheduledLanding({ disabledReason, onCreate }: {
  disabledReason: string;
  onCreate: (suggestion?: ScheduledSuggestion) => void;
}) {
  return (
    <div className="scheduled-landing">
      <div className="scheduled-landing-content">
        <header className="scheduled-landing-title">
          <span aria-hidden="true" className="scheduled-landing-emoji">🕗</span>
          <h1>Schedule a task</h1>
          <p>Let Octopus take care of ongoing work and keep you up to date.</p>
        </header>
        <ul aria-label="Scheduled task suggestions" className="scheduled-suggestions">
          {scheduledSuggestions.map((suggestion) => (
            <li key={suggestion.id}>
              <button className="scheduled-suggestion" disabled={Boolean(disabledReason)} onClick={() => onCreate(suggestion)} title={disabledReason || undefined} type="button">
                <span aria-hidden="true" className="scheduled-suggestion-emoji">{suggestion.emoji}</span>
                <span className="scheduled-suggestion-copy"><strong>{suggestion.title}</strong><span>{suggestion.description}</span></span>
              </button>
            </li>
          ))}
        </ul>
        {disabledReason ? <p className="scheduled-create-note" role="status">{disabledReason}</p> : (
          <OcButton className="scheduled-new-custom" leadingIcon="plus" onClick={() => onCreate()} size="sm" variant="ghost">Create your own task</OcButton>
        )}
      </div>
    </div>
  );
}
