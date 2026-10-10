use super::App;
use crate::app_event::AppEvent;
use crate::app_event::ThreadGoalSetMode;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::goal_display::GOAL_USAGE;
use crate::goal_display::goal_status_label;
use crate::goal_display::goal_usage_summary;
use crate::goal_files;
use crate::session_state::ThreadSessionState;
use crate::session_vocab::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crate::text_formatting::truncate_text;
use octopus_world_terminal_client::GoalState;
use octopus_world_terminal_client::GoalStatusItem;

const EPHEMERAL_THREAD_GOAL_ERROR_MESSAGE: &str = concat!(
    "Goals need a saved session. This session is temporary.\n",
    "Run `octopus` to start a saved chat, or `octopus resume` / `/resume` to reopen one.",
);

impl App {
    pub(super) fn should_prompt_for_paused_goal_after_startup_resume(
        session_selection: &crate::resume_picker::SessionSelection,
        initial_prompt: &Option<String>,
        initial_images: &[std::path::PathBuf],
    ) -> bool {
        matches!(
            session_selection,
            crate::resume_picker::SessionSelection::Resume(_)
        ) && initial_prompt.is_none()
            && initial_images.is_empty()
    }

    pub(super) async fn open_thread_goal_menu(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: String,
    ) {
        let result = app_server.thread_goal_get(&thread_id).await;
        if self.chat_widget.thread_id().as_deref() != Some(thread_id.as_str()) {
            return;
        }

        let goal = match result {
            Ok(goal) => goal,
            Err(err) => {
                self.chat_widget
                    .add_error_message(thread_goal_error_message("read", &err));
                return;
            }
        };

        let Some(goal) = goal else {
            self.chat_widget.add_info_message(
                GOAL_USAGE.to_string(),
                Some("No goal is currently set.".to_string()),
            );
            return;
        };

        self.chat_widget.show_goal_summary(goal);
    }

    pub(super) async fn maybe_prompt_resume_paused_goal_after_resume(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        let Some(thread_id) = self.chat_widget.thread_id() else {
            return;
        };
        let result = app_server.thread_goal_get(&thread_id).await;
        let goal = match result {
            Ok(goal) => goal,
            Err(err) => {
                tracing::warn!("failed to read thread goal after resume: {err}");
                return;
            }
        };

        let Some(goal) = goal else {
            return;
        };
        if matches!(
            goal.status,
            GoalState::Paused | GoalState::Blocked | GoalState::UsageLimited
        ) {
            self.chat_widget
                .show_resume_paused_goal_prompt(thread_id, goal.objective);
        }
    }

    pub(super) async fn open_thread_goal_editor(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: Option<String>,
    ) {
        let Some(thread_id) = thread_id else {
            self.show_no_thread_goal_to_edit();
            return;
        };

        let result = app_server.thread_goal_get(&thread_id).await;
        if self.chat_widget.thread_id().as_deref() != Some(thread_id.as_str()) {
            return;
        }

        let goal = match result {
            Ok(goal) => goal,
            Err(err) => {
                self.chat_widget
                    .add_error_message(thread_goal_error_message("read", &err));
                return;
            }
        };

        let Some(mut goal) = goal else {
            self.show_no_thread_goal_to_edit();
            return;
        };

        let codex_home = app_server.codex_home_path(&self.config.codex_home);
        match goal_files::objective_text_for_edit(app_server, codex_home.as_ref(), &goal.objective)
            .await
        {
            Ok(objective) => goal.objective = objective,
            Err(err) => {
                self.chat_widget.add_error_message(err.to_string());
            }
        }
        if self.chat_widget.thread_id().as_deref() != Some(thread_id.as_str()) {
            return;
        }
        self.chat_widget.show_goal_edit_prompt(thread_id, goal);
    }

    pub(super) async fn set_thread_goal_draft(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: String,
        draft: goal_files::GoalDraft,
        mode: ThreadGoalSetMode,
    ) {
        let codex_home = app_server.codex_home_path(&self.config.codex_home);
        let mode = if matches!(mode, ThreadGoalSetMode::ConfirmIfExists) {
            let result = app_server.thread_goal_get(&thread_id).await;
            if self.chat_widget.thread_id().as_deref() != Some(thread_id.as_str()) {
                return;
            }

            match result {
                Ok(goal) => match goal.as_ref() {
                    Some(goal) if should_confirm_before_replacing_goal(goal) => {
                        self.show_replace_thread_goal_confirmation(thread_id, draft);
                        return;
                    }
                    Some(_) => ThreadGoalSetMode::ReplaceExisting,
                    None => mode,
                },
                Err(err) => {
                    self.chat_widget
                        .add_error_message(thread_goal_error_message("read", &err));
                    return;
                }
            }
        } else {
            mode
        };

        let (objective, output_dir) = match goal_files::materialize_goal_draft(
            app_server,
            codex_home.as_ref(),
            draft,
        )
        .await
        {
            Ok(materialized) => materialized,
            Err(err) => {
                if self.chat_widget.thread_id().as_deref() == Some(thread_id.as_str()) {
                    self.chat_widget.add_error_message(err.to_string());
                }
                return;
            }
        };

        let replacing_goal = matches!(mode, ThreadGoalSetMode::ReplaceExisting);

        let (status, token_budget) = match mode {
            ThreadGoalSetMode::ConfirmIfExists | ThreadGoalSetMode::ReplaceExisting => {
                (GoalState::Active, None)
            }
            ThreadGoalSetMode::UpdateExisting {
                status,
                token_budget,
            } => (status, Some(token_budget)),
        };

        let result = app_server
            .thread_goal_set(&thread_id, Some(objective), Some(status), token_budget)
            .await;

        match result {
            Ok(goal) => {
                if let Err(err) = self.sync_chat_to_active_topology(app_server) {
                    self.chat_widget
                        .add_error_message(thread_goal_error_message("attach", &err));
                    return;
                }
                self.chat_widget.add_info_message(
                    format!("Goal {}", goal_status_label(goal.status)),
                    Some(goal_usage_summary(&goal)),
                );
                self.chat_widget.maybe_send_next_queued_input();
            }
            Err(err) => {
                cleanup_materialized_goal_files(app_server, output_dir).await;
                if self.chat_widget.thread_id().as_deref() != Some(thread_id.as_str()) {
                    return;
                }
                let action = if replacing_goal { "replace" } else { "set" };
                self.chat_widget
                    .add_error_message(thread_goal_error_message(action, &err));
            }
        }
    }

    pub(super) fn sync_chat_to_active_topology(
        &mut self,
        app_server: &AppServerSession,
    ) -> color_eyre::Result<()> {
        let topology = app_server
            .active_topology()
            .cloned()
            .ok_or_else(|| color_eyre::eyre::eyre!("terminal has no active private topology"))?;
        let reasoning_effort = topology
            .reasoning_effort
            .parse::<ReasoningEffortConfig>()
            .map_err(|error| color_eyre::eyre::eyre!(error))?;
        self.config.model = Some(topology.model.clone());
        self.config.model_reasoning_effort = Some(reasoning_effort.clone());
        self.chat_widget
            .handle_thread_session_quiet(ThreadSessionState {
                thread_id: topology.run.as_str().to_string(),
                forked_from_id: None,
                fork_parent_title: None,
                thread_name: None,
                model: topology.model,
                model_provider_id: self.config.model_provider_id.clone(),
                service_tier: self.config.service_tier.clone(),
                approval_policy: self.config.permissions.approval_policy.value(),
                approvals_reviewer: self.config.approvals_reviewer,
                permission_profile: self.config.permissions.permission_profile().clone(),
                active_permission_profile: self.config.permissions.active_permission_profile(),
                cwd: self.config.cwd.clone(),
                runtime_workspace_roots: self.config.effective_workspace_roots(),
                instruction_source_paths: Vec::new(),
                reasoning_effort: Some(reasoning_effort),
                collaboration_mode: None,
                personality: self.config.personality.clone(),
                network_proxy: None,
                rollout_path: app_server.active_run_record_path(),
            });
        self.refresh_task_mentions(app_server);
        Ok(())
    }

    pub(super) fn refresh_task_mentions(&mut self, app_server: &AppServerSession) {
        let Some(current_run) = app_server.active_run_id() else {
            self.chat_widget.set_task_mentions(Vec::new());
            return;
        };
        let current_root = app_server
            .working_roots()
            .ok()
            .and_then(|roots| {
                roots
                    .iter()
                    .find(|(slot, _)| slot == "worktree_root" || slot.ends_with(".worktree_root"))
                    .or_else(|| {
                        roots
                            .iter()
                            .find(|(slot, _)| slot == "repo_root" || slot.ends_with(".repo_root"))
                    })
                    .map(|(_, root)| root.clone())
            })
            .unwrap_or_else(|| self.config.cwd.to_path_buf());
        match app_server.task_projection() {
            Ok(tasks) => self.chat_widget.set_task_mentions(
                octopus_world_terminal_client::task_mention_candidates(
                    &tasks,
                    &current_run,
                    &current_root,
                ),
            ),
            Err(error) => {
                tracing::warn!(%error, "failed to refresh exact task mention projection");
                self.chat_widget.set_task_mentions(Vec::new());
            }
        }
    }

    pub(super) async fn set_thread_goal_status(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: String,
        status: GoalState,
    ) {
        let result = app_server
            .thread_goal_set(&thread_id, None, Some(status), None)
            .await;
        if self.chat_widget.thread_id().as_deref() != Some(thread_id.as_str()) {
            return;
        }

        match result {
            Ok(goal) => self.chat_widget.add_info_message(
                format!("Goal {}", goal_status_label(goal.status)),
                Some(goal_usage_summary(&goal)),
            ),
            Err(err) => self
                .chat_widget
                .add_error_message(thread_goal_error_message("update", &err)),
        }
    }

    pub(super) async fn clear_thread_goal(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: String,
    ) {
        let result = app_server.thread_goal_clear(&thread_id).await;
        if self.chat_widget.thread_id().as_deref() != Some(thread_id.as_str()) {
            return;
        }

        match result {
            Ok(cleared) => {
                if cleared {
                    self.chat_widget
                        .add_info_message("Goal cleared".to_string(), None);
                } else {
                    self.chat_widget.add_info_message(
                        "No goal to clear".to_string(),
                        Some("This thread does not currently have a goal.".to_string()),
                    );
                }
            }
            Err(err) => self
                .chat_widget
                .add_error_message(thread_goal_error_message("clear", &err)),
        }
    }

    pub(super) fn show_replace_thread_goal_confirmation(
        &mut self,
        thread_id: String,
        draft: goal_files::GoalDraft,
    ) {
        let objective = draft.objective.clone();
        let replace_draft = draft;
        let replace_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::SetThreadGoalDraft {
                thread_id: thread_id.clone(),
                draft: replace_draft.clone(),
                mode: ThreadGoalSetMode::ReplaceExisting,
            });
        })];
        let items = vec![
            SelectionItem {
                name: "Replace current goal".to_string(),
                description: Some("Set the new objective and start it now".to_string()),
                actions: replace_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                description: Some("Keep the current goal".to_string()),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Replace goal?".to_string()),
            subtitle: Some(format!("New objective: {}", truncate_text(&objective, 200))),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    fn show_no_thread_goal_to_edit(&mut self) {
        self.chat_widget
            .add_error_message("No goal is currently set.".to_string());
        self.chat_widget.add_info_message(
            GOAL_USAGE.to_string(),
            Some("Create a goal before editing it.".to_string()),
        );
    }
}

async fn cleanup_materialized_goal_files(
    app_server: &mut AppServerSession,
    output_dir: Option<goal_files::GoalFilePath>,
) {
    if let Some(output_dir) = output_dir
        && let Err(err) = app_server.fs_remove_path(&output_dir).await
    {
        tracing::warn!("failed to clean up materialized goal files at {output_dir}: {err}");
    }
}

fn thread_goal_error_message(action: &str, err: &color_eyre::Report) -> String {
    if is_ephemeral_thread_goal_error(err) {
        EPHEMERAL_THREAD_GOAL_ERROR_MESSAGE.to_string()
    } else {
        format!("Failed to {action} thread goal: {err}")
    }
}

fn is_ephemeral_thread_goal_error(err: &color_eyre::Report) -> bool {
    err.chain().any(|cause| {
        let message = cause.to_string();
        message.contains("ephemeral thread does not support goals")
            || message.contains("thread goals require a persisted thread; this thread is ephemeral")
    })
}

fn should_confirm_before_replacing_goal(goal: &GoalStatusItem) -> bool {
    !matches!(goal.status, GoalState::Complete | GoalState::Unknown)
}

#[cfg(test)]
mod tests {
    use crate::history_cell::HistoryCell;
    use pretty_assertions::assert_eq;
    use ratatui::layout::Rect;

    use super::*;

    #[test]
    fn thread_goal_error_message_explains_temporary_session() {
        let err = color_eyre::eyre::eyre!(
            "thread/goal/get failed: ephemeral thread does not support goals: thread-1"
        )
        .wrap_err("thread/goal/get failed in TUI");

        assert_eq!(
            thread_goal_error_message("read", &err),
            EPHEMERAL_THREAD_GOAL_ERROR_MESSAGE
        );
    }

    #[test]
    fn thread_goal_ephemeral_error_message_renders_snapshot() {
        let err = color_eyre::eyre::eyre!(
            "thread/goal/get failed: ephemeral thread does not support goals: thread-1"
        )
        .wrap_err("thread/goal/get failed in TUI");
        let cell = crate::history_cell::new_error_event(thread_goal_error_message("read", &err));
        let width = 72;
        let height = 6;
        let backend = crate::test_backend::VT100Backend::new(width, height);
        let mut terminal =
            crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, height - 1, width, 1));

        crate::insert_history::insert_history_lines(&mut terminal, cell.display_lines(width))
            .expect("insert history lines");

        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn thread_goal_error_message_preserves_generic_failure_context() {
        let err =
            color_eyre::eyre::eyre!("server disappeared").wrap_err("thread/goal/get failed in TUI");

        assert_eq!(
            thread_goal_error_message("read", &err),
            "Failed to read thread goal: thread/goal/get failed in TUI"
        );
    }

    #[test]
    fn completed_goal_does_not_require_replace_confirmation() {
        assert!(!should_confirm_before_replacing_goal(&test_goal(
            GoalState::Complete
        )));
    }

    #[test]
    fn unfinished_goals_require_replace_confirmation() {
        for status in [
            GoalState::Active,
            GoalState::Paused,
            GoalState::Blocked,
            GoalState::UsageLimited,
            GoalState::BudgetLimited,
        ] {
            assert!(should_confirm_before_replacing_goal(&test_goal(status)));
        }
    }

    fn test_goal(status: GoalState) -> GoalStatusItem {
        GoalStatusItem {
            objective: "Finish the thing.".to_string(),
            status,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
        }
    }
}
