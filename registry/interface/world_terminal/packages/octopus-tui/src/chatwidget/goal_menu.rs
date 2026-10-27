use super::*;
use crate::goal_display::format_goal_elapsed_seconds;
use crate::goal_files;
use crate::status::format_tokens_compact;
use octopus_world_terminal_client::GoalStatusItem;

impl ChatWidget {
    pub(crate) fn show_goal_summary(&mut self, goal: GoalStatusItem) {
        self.add_plain_history_lines(goal_summary_lines(&goal));
    }

    pub(crate) fn show_goal_edit_prompt(&mut self, thread_id: String, goal: GoalStatusItem) {
        let tx = self.app_event_tx.clone();
        let status = edited_goal_status(goal.status);
        let token_budget = goal.token_budget;
        let view = CustomPromptView::new(
            "Edit goal".to_string(),
            "Type a goal objective and press Enter".to_string(),
            goal.objective,
            None,
            Box::new(move |objective: String| {
                tx.send(AppEvent::SetThreadGoalDraft {
                    thread_id: thread_id.clone(),
                    draft: goal_files::GoalDraft {
                        objective,
                        ..Default::default()
                    },
                    mode: crate::app_event::ThreadGoalSetMode::UpdateExisting {
                        status,
                        token_budget,
                    },
                });
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    pub(crate) fn show_resume_paused_goal_prompt(&mut self, thread_id: String, objective: String) {
        let resume_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::SetThreadGoalStatus {
                thread_id: thread_id.clone(),
                status: GoalState::Active,
            });
        })];
        self.show_selection_view(SelectionViewParams {
            title: Some("Resume paused goal?".to_string()),
            subtitle: Some(format!("Goal: {objective}")),
            footer_hint: Some(standard_popup_hint_line()),
            initial_selected_idx: Some(0),
            items: vec![
                SelectionItem {
                    name: "Resume goal".to_string(),
                    description: Some("Mark it active and continue when idle".to_string()),
                    actions: resume_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Leave paused".to_string(),
                    description: Some("Keep it paused; use /goal resume later".to_string()),
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
    }
}

fn goal_summary_lines(goal: &GoalStatusItem) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("Goal".bold()),
        Line::from(vec![
            "Status: ".dim(),
            goal_status_label(goal.status).to_string().into(),
        ]),
        Line::from(vec!["Objective: ".dim(), goal.objective.clone().into()]),
        Line::from(vec![
            "Time used: ".dim(),
            format_goal_elapsed_seconds(goal.time_used_seconds).into(),
        ]),
        Line::from(vec![
            "Tokens used: ".dim(),
            format_tokens_compact(goal.tokens_used).into(),
        ]),
    ];
    if let Some(token_budget) = goal.token_budget {
        lines.push(Line::from(vec![
            "Token budget: ".dim(),
            format_tokens_compact(token_budget).into(),
        ]));
    }
    let command_hint = match goal.status {
        GoalState::Active => "Commands: /goal edit, /goal pause, /goal clear",
        GoalState::Paused | GoalState::Blocked | GoalState::UsageLimited => {
            "Commands: /goal edit, /goal resume, /goal clear"
        }
        GoalState::BudgetLimited | GoalState::Complete | GoalState::Unknown => {
            "Commands: /goal edit, /goal clear"
        }
    };
    lines.push(Line::default());
    lines.push(Line::from(command_hint.dim()));
    lines
}

fn goal_status_label(status: GoalState) -> &'static str {
    match status {
        GoalState::Active => "active",
        GoalState::Paused => "paused",
        GoalState::Blocked => "blocked",
        GoalState::UsageLimited => "usage limited",
        GoalState::BudgetLimited => "limited by budget",
        GoalState::Complete => "complete",
        GoalState::Unknown => "unknown",
    }
}

fn edited_goal_status(status: GoalState) -> GoalState {
    match status {
        GoalState::Active => GoalState::Active,
        GoalState::Paused | GoalState::Blocked | GoalState::UsageLimited => status,
        GoalState::BudgetLimited | GoalState::Complete | GoalState::Unknown => GoalState::Active,
    }
}
