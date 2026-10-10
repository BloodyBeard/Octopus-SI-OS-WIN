export type ScheduledSuggestion = {
  id: string;
  emoji: string;
  title: string;
  description: string;
  instructions: string;
  cron: string;
};

export const scheduledSuggestions: readonly ScheduledSuggestion[] = [
  {
    id: "daily-brief", emoji: "☀️", title: "Daily brief",
    description: "Start the day with recent progress and what needs your attention.",
    instructions: "Prepare a concise daily brief from the projects and evidence available to this run. Summarize recent progress, outstanding decisions, and priorities for today.",
    cron: "0 9 * * 1-5",
  },
  {
    id: "project-check-in", emoji: "📋", title: "Project check-in",
    description: "Keep track of open work and report meaningful changes.",
    instructions: "Check the project work available to this run. Report meaningful progress, unresolved blockers, and decisions that need my attention. Cite the evidence for changes.",
    cron: "0 16 * * 1-5",
  },
  {
    id: "weekly-review", emoji: "📖", title: "Weekly review",
    description: "Bring together the week's results and unfinished work every Friday.",
    instructions: "Review this week's work using the evidence available to this run. Summarize completed outcomes, unfinished work, and useful next steps.",
    cron: "0 16 * * 5",
  },
  {
    id: "research-update", emoji: "🔎", title: "Research update",
    description: "Review new findings in your ongoing research each week.",
    instructions: "Review the research goals and sources available to this run. Summarize new findings, material changes, and open questions, with source references.",
    cron: "0 10 * * 3",
  },
  {
    id: "follow-up", emoji: "🔔", title: "Follow up",
    description: "Check pending decisions and anything waiting for a response.",
    instructions: "Review the pending decisions and follow-ups available to this run. Identify what changed and what still needs attention. Do not send external messages without authorization.",
    cron: "0 10 * * 1-5",
  },
  {
    id: "plan-week", emoji: "🗓️", title: "Plan the week",
    description: "Review priorities and prepare a focused plan every Monday.",
    instructions: "Review the goals, deadlines, and outstanding work available to this run. Prepare a focused weekly plan, explaining the priorities and decisions that need my input.",
    cron: "0 8 * * 1",
  },
];
