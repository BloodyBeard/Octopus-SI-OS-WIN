export type ScheduledSuggestion = {
  id: string;
  emoji: string;
  title: string;
  description: string;
  instructions: string;
  schedule: string;
  templateInterfaceId?: string;
};

export const scheduledSuggestions: readonly ScheduledSuggestion[] = [
  {
    id: "daily-brief", emoji: "☀️", title: "Daily brief",
    description: "Start the day with recent progress and what needs your attention.",
    instructions: "Prepare a concise daily brief from the projects and evidence available to this run. Summarize recent progress, outstanding decisions, and priorities for today.",
    schedule: "Weekdays at 9:00 AM in my local timezone",
  },
  {
    id: "project-check-in", emoji: "📋", title: "Project check-in",
    description: "Keep track of open work and report meaningful changes.",
    instructions: "Check the project work available to this run. Report meaningful progress, unresolved blockers, and decisions that need my attention. Cite the evidence for changes.",
    schedule: "Weekdays at 4:00 PM in my local timezone",
  },
  {
    id: "weekly-review", emoji: "📖", title: "Weekly review",
    description: "Bring together the week's results and unfinished work every Friday.",
    instructions: "Review this week's work using the evidence available to this run. Summarize completed outcomes, unfinished work, and useful next steps.",
    schedule: "Fridays at 4:00 PM in my local timezone",
  },
  {
    id: "research-update", emoji: "🔎", title: "Research update",
    description: "Review new findings in your ongoing research each week.",
    instructions: "Review the research goals and sources available to this run. Summarize new findings, material changes, and open questions, with source references.",
    schedule: "Wednesdays at 10:00 AM in my local timezone",
  },
  {
    id: "follow-up", emoji: "🔔", title: "Follow up",
    description: "Check pending decisions and anything waiting for a response.",
    instructions: "Review the pending decisions and follow-ups available to this run. Identify what changed and what still needs attention. Do not send external messages without authorization.",
    schedule: "Weekdays at 10:00 AM in my local timezone",
  },
  {
    id: "plan-week", emoji: "🗓️", title: "Plan the week",
    description: "Review priorities and prepare a focused plan every Monday.",
    instructions: "Review the goals, deadlines, and outstanding work available to this run. Prepare a focused weekly plan, explaining the priorities and decisions that need my input.",
    schedule: "Mondays at 8:00 AM in my local timezone",
  },
];
