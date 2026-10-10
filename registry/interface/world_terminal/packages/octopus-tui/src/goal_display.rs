use crate::status::format_tokens_compact;
use octopus_world_terminal_client::GoalState;
use octopus_world_terminal_client::GoalStatusItem;

pub(crate) const GOAL_USAGE: &str = "Usage: /goal [<objective>|clear|edit|pause|resume]";

pub(crate) fn format_goal_elapsed_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0) as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }

    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    if hours >= 24 {
        let days = hours / 24;
        let remaining_hours = hours % 24;
        return format!("{days}d {remaining_hours}h {remaining_minutes}m");
    }

    if remaining_minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {remaining_minutes}m")
    }
}

pub(crate) fn goal_status_label(status: GoalState) -> &'static str {
    match status {
        GoalState::Active => "active",
        GoalState::Paused => "paused",
        GoalState::Blocked => "blocked",
        GoalState::UsageLimited => "usage limited",
        GoalState::BudgetLimited => "limited by budget",
        GoalState::Complete => "complete",
        GoalState::Unknown => "recorded",
    }
}

pub(crate) fn goal_usage_summary(goal: &GoalStatusItem) -> String {
    let mut parts = vec![format!("Objective: {}", goal.objective)];
    if goal.time_used_seconds > 0 {
        parts.push(format!(
            "Time: {}.",
            format_goal_elapsed_seconds(goal.time_used_seconds)
        ));
    }
    if let Some(token_budget) = goal.token_budget {
        parts.push(format!(
            "Tokens: {}/{}.",
            format_tokens_compact(goal.tokens_used),
            format_tokens_compact(token_budget)
        ));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_goal_elapsed_seconds_is_compact() {
        assert_eq!(format_goal_elapsed_seconds(0), "0s");
        assert_eq!(format_goal_elapsed_seconds(59), "59s");
        assert_eq!(format_goal_elapsed_seconds(60), "1m");
        assert_eq!(format_goal_elapsed_seconds(30 * 60), "30m");
        assert_eq!(format_goal_elapsed_seconds(90 * 60), "1h 30m");
        assert_eq!(format_goal_elapsed_seconds(2 * 60 * 60), "2h");
        let just_before_one_day = 24 * 60 * 60 - 1;
        assert_eq!(format_goal_elapsed_seconds(just_before_one_day), "23h 59m");

        let one_day = 24 * 60 * 60;
        assert_eq!(format_goal_elapsed_seconds(one_day), "1d 0h 0m");

        let almost_three_days = 2 * 24 * 60 * 60 + 23 * 60 * 60 + 42 * 60;
        assert_eq!(format_goal_elapsed_seconds(almost_three_days), "2d 23h 42m");
    }

    fn test_thread_goal(token_budget: Option<i64>, tokens_used: i64) -> GoalStatusItem {
        GoalStatusItem {
            objective: "Complete the task described in ../gameboy-long-running-prompt5.txt"
                .to_string(),
            status: GoalState::BudgetLimited,
            token_budget,
            tokens_used,
            time_used_seconds: 120,
        }
    }

    #[test]
    fn goal_usage_summary_formats_time_and_budgeted_tokens() {
        assert_eq!(
            goal_usage_summary(&test_thread_goal(Some(50_000), 63_876,)),
            "Objective: Complete the task described in ../gameboy-long-running-prompt5.txt Time: 2m. Tokens: 63.9K/50K."
        );
    }
}
