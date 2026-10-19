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
    <div className="@container h-full min-h-0 overflow-y-auto" data-scheduled-landing="">
      <div className="flex min-h-full w-full flex-col items-center justify-center px-3 py-6 pb-11 select-none">
        <div className="flex w-full max-w-[46.125rem] flex-col items-center justify-center gap-11 text-center">
          <div className="flex flex-col items-center gap-2">
            <div className="flex flex-col items-center gap-3">
              <span aria-hidden="true" className="text-5xl leading-none select-none">🕗</span>
              <div className="flex flex-col items-center gap-0.5">
                <h1 className="m-0 text-[18px] leading-[26px] font-normal tracking-[-0.025em] text-default">Create a trigger</h1>
                <p className="m-0 text-[14px] leading-5 font-normal tracking-[-0.01em] text-tertiary">Octopus can take care of ongoing tasks so you don’t have to.</p>
              </div>
            </div>
          </div>
          <div className="flex w-full flex-wrap items-center justify-center gap-2">
            {suggestions.length ? (
              <ul aria-label="Trigger suggestions" className="m-0 grid w-full grid-cols-1 gap-5 p-0 @min-[39rem]:grid-cols-2">
                {suggestions.map(({ suggestion, reason }) => (
                  <li className="list-none" data-automation-card="" key={suggestion.id}>
                    <button
                      className="flex h-full min-h-27 w-full cursor-interaction items-center gap-4 rounded-[24px] border border-dashed border-default bg-transparent p-5 text-start not-disabled:hover:bg-primary-ghost-hover disabled:cursor-default focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                      disabled={Boolean(reason)}
                      onClick={() => onCreate(suggestion)}
                      title={reason || undefined}
                      type="button"
                    >
                      <span className="flex size-6 shrink-0 items-center justify-center text-tertiary">
                        <span aria-hidden="true" className="text-2xl leading-none text-default">{suggestion.emoji}</span>
                      </span>
                      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                        <span className="line-clamp-1 text-[16px] leading-[26px] font-normal tracking-[-0.02em] text-default">{suggestion.title}</span>
                        <span className="line-clamp-2 text-[14px] leading-5 font-normal text-tertiary">{suggestion.description}</span>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            ) : (
              <OcButton className="shrink-0 rounded-full! px-4" disabled={Boolean(disabledReason)} onClick={() => onCreate()} size="md" variant="primary">New trigger</OcButton>
            )}
            {disabledReason ? <p className="m-0 w-full text-[14px] leading-5 text-tertiary" role="status">{disabledReason}</p> : null}
          </div>
        </div>
      </div>
    </div>
  );
}
