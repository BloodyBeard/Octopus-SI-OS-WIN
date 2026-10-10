use super::*;

impl ChatWidget {
    pub(super) fn update_task_running_state(&mut self) {
        self.bottom_pane.set_task_running(
            self.turn_lifecycle.agent_turn_running || self.tool_interface_startup_status.is_some(),
        );
        self.refresh_plan_mode_nudge();
        self.refresh_status_surfaces();
    }

    pub(super) fn collect_runtime_metrics_delta(&mut self) {
        if let Some(delta) = self.session_telemetry.runtime_metrics_summary() {
            self.apply_runtime_metrics_delta(delta);
        }
    }

    pub(super) fn apply_runtime_metrics_delta(&mut self, delta: RuntimeMetricsSummary) {
        let should_log_timing = has_websocket_timing_metrics(delta);
        self.turn_runtime_metrics.merge(delta);
        if should_log_timing {
            self.log_websocket_timing_totals(delta);
        }
    }

    pub(super) fn log_websocket_timing_totals(&mut self, delta: RuntimeMetricsSummary) {
        if let Some(label) = history_cell::runtime_metrics_label(delta.responses_api_summary()) {
            self.add_plain_history_lines(vec![
                vec!["• ".dim(), format!("WebSocket timing: {label}").dark_gray()].into(),
            ]);
        }
    }

    pub(super) fn refresh_runtime_metrics(&mut self) {
        self.collect_runtime_metrics_delta();
    }

    pub(super) fn on_task_started(&mut self) {
        self.input_queue.user_turn_pending_start = false;
        self.turn_lifecycle.start(Instant::now());
        self.transcript.reset_turn_flags();
        self.adaptive_chunking.reset();
        if self.plan_stream_controller.take().is_some() {
            self.request_pending_usage_output_insertion_after_stream_shutdown();
        }
        self.turn_runtime_metrics = RuntimeMetricsSummary::default();
        self.session_telemetry.reset_runtime_metrics();
        self.bottom_pane.clear_quit_shortcut_hint();
        self.quit_shortcut_expires_at = None;
        self.quit_shortcut_key = None;
        self.update_task_running_state();
        self.status_state.retry_status_header = None;
        self.clear_active_hook_cell();
        self.status_state.pending_status_indicator_restore = false;
        self.bottom_pane.set_interrupt_hint_visible(true);
        self.status_state.terminal_title_status_kind = TerminalTitleStatusKind::Working;
        if self.tool_interface_startup_status.is_none()
            || !self.status_header_is_tool_interface_startup_owned()
        {
            self.set_status_header(String::from("Working"));
        }
        self.full_reasoning_buffer.clear();
        self.reasoning_buffer.clear();
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Running, None);
        self.request_redraw();
    }

    pub(crate) fn on_task_complete(
        &mut self,
        last_agent_message: Option<String>,
        duration_ms: Option<i64>,
        from_replay: bool,
    ) {
        self.input_queue.submit_pending_steers_after_interrupt = false;
        let sanitized_last_agent_message = last_agent_message.as_deref().map(|message| {
            parse_assistant_markdown(message, self.config.cwd.as_path()).visible_markdown
        });
        if let Some(message) = sanitized_last_agent_message
            .as_ref()
            .filter(|message| !message.is_empty())
            && !self.transcript.saw_copy_source_this_turn
        {
            self.record_agent_markdown(message);
        }
        let notification_response = sanitized_last_agent_message
            .as_ref()
            .filter(|message| !message.is_empty())
            .cloned()
            .or_else(|| {
                if self.transcript.saw_copy_source_this_turn {
                    self.transcript.last_agent_markdown.clone()
                } else {
                    None
                }
            })
            .unwrap_or_default();
        self.transcript.saw_copy_source_this_turn = false;
        self.flush_answer_stream_with_separator();
        if let Some(mut controller) = self.plan_stream_controller.take() {
            let had_live_tail = controller.has_live_tail();
            self.clear_active_stream_tail();
            let (cell, source) = controller.finalize();
            if !had_live_tail && let Some(cell) = cell {
                self.add_boxed_history(cell);
            }
            if let Some(source) = source {
                self.note_stream_consolidation_queued();
                self.app_event_tx
                    .send(AppEvent::ConsolidateProposedPlan(source));
            }
            self.request_pending_usage_output_insertion_after_stream_shutdown();
        }
        self.flush_unified_exec_wait_streak();
        if !from_replay {
            self.collect_runtime_metrics_delta();
            let runtime_metrics =
                (!self.turn_runtime_metrics.is_empty()).then_some(self.turn_runtime_metrics);
            let show_work_separator = self.transcript.had_work_activity
                && (self.transcript.needs_final_message_separator || runtime_metrics.is_some());
            if show_work_separator || runtime_metrics.is_some() {
                let elapsed_seconds = if show_work_separator {
                    duration_ms
                        .and_then(|duration_ms| u64::try_from(duration_ms).ok())
                        .map(|duration_ms| duration_ms / 1_000)
                        .or_else(|| {
                            self.bottom_pane
                                .status_widget()
                                .map(crate::status_indicator_widget::StatusIndicatorWidget::elapsed_seconds)
                        })
                } else {
                    None
                };
                self.add_to_history(history_cell::FinalMessageSeparator::new(
                    elapsed_seconds,
                    runtime_metrics,
                ));
            }
            self.turn_runtime_metrics = RuntimeMetricsSummary::default();
            self.transcript.needs_final_message_separator = false;
            self.transcript.had_work_activity = false;
            self.request_status_line_branch_refresh();
            self.request_status_line_git_summary_refresh();
        }
        self.status_state.pending_status_indicator_restore = false;
        self.input_queue.user_turn_pending_start = false;
        self.clear_active_hook_cell();
        self.turn_lifecycle.finish();
        self.update_task_running_state();
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.unified_exec_wait_streak = None;
        if !from_replay {
            let body = Notification::agent_turn_preview(&notification_response);
            self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Review, body);
        }
        self.request_redraw();

        let had_pending_steers = !self.input_queue.pending_steers.is_empty();
        self.refresh_pending_input_preview();

        if !from_replay && !self.has_queued_follow_up_messages() && !had_pending_steers {
            self.maybe_prompt_plan_implementation();
        }
        if !from_replay {
            self.transcript.saw_plan_item_this_turn = false;
        }
        let follow_up_started = self.maybe_send_next_queued_input();
        let active_goal_continuing = self
            .current_goal_status
            .as_ref()
            .is_some_and(GoalStatusState::is_active);
        if !follow_up_started && !active_goal_continuing {
            self.notify(Notification::AgentTurnComplete {
                response: notification_response,
            });
        }

        self.maybe_show_pending_rate_limit_prompt();
    }

    pub(super) fn maybe_prompt_plan_implementation(&mut self) {
        if !self.collaboration_modes_enabled() {
            return;
        }
        if self.has_queued_follow_up_messages() {
            return;
        }
        if self.active_mode_kind() != ModeKind::Plan {
            return;
        }
        if !self.transcript.saw_plan_item_this_turn {
            return;
        }
        if !self.bottom_pane.no_modal_or_popup_active() {
            return;
        }

        if matches!(
            self.rate_limit_switch_prompt,
            RateLimitSwitchPromptState::Pending
        ) {
            return;
        }

        self.open_plan_implementation_prompt();
    }

    pub(super) fn open_plan_implementation_prompt(&mut self) {
        let default_mask = collaboration_modes::default_mode_mask(self.model_catalog.as_ref());
        let context_usage_label = self.plan_implementation_context_usage_label();

        self.bottom_pane
            .show_selection_view(plan_implementation::selection_view_params(
                default_mask,
                self.transcript.latest_proposed_plan_markdown.as_deref(),
                context_usage_label.as_deref(),
            ));
        self.notify(Notification::PlanModePrompt {
            title: PLAN_IMPLEMENTATION_TITLE.to_string(),
        });
    }

    pub(super) fn plan_implementation_context_usage_label(&self) -> Option<String> {
        let info = self.token_info.as_ref()?;
        let percent = self.context_remaining_percent(info);

        let used_tokens = self.context_used_tokens(info, percent.is_some());
        if let Some(percent) = percent {
            let used_percent = 100 - percent.clamp(0, 100);
            if used_percent <= 0 {
                return None;
            }
            return Some(format!("{used_percent}% used"));
        }

        if let Some(tokens) = used_tokens
            && tokens > 0
        {
            return Some(format!("{} used", format_tokens_compact(tokens)));
        }

        None
    }

    pub(super) fn has_queued_follow_up_messages(&self) -> bool {
        self.input_queue.has_queued_follow_up_messages()
    }

    pub(super) fn finalize_turn(&mut self) {
        self.clear_active_stream_tail();
        self.finalize_active_cell_as_failed();
        self.clear_active_hook_cell();
        self.input_queue.user_turn_pending_start = false;
        self.turn_lifecycle.finish();
        self.update_task_running_state();
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.unified_exec_wait_streak = None;
        self.adaptive_chunking.reset();
        self.stream_controller = None;
        self.plan_stream_controller = None;
        self.request_pending_usage_output_insertion_after_stream_shutdown();
        self.status_state.pending_status_indicator_restore = false;
        self.clear_cancel_edit();
        self.request_status_line_branch_refresh();
        self.request_status_line_git_summary_refresh();
        self.maybe_show_pending_rate_limit_prompt();
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn on_server_overloaded_error(&mut self, message: String) {
        self.input_queue.submit_pending_steers_after_interrupt = false;
        self.finalize_turn();

        let message = if message.trim().is_empty() {
            "Codex is currently experiencing high load.".to_string()
        } else {
            message
        };

        self.add_to_history(history_cell::new_warning_event(message));
        self.request_redraw();
        self.maybe_send_next_queued_input();
    }

    pub(super) fn on_error(&mut self, message: String) {
        self.input_queue.submit_pending_steers_after_interrupt = false;
        self.flush_answer_stream_with_separator();
        self.finalize_turn();
        self.add_to_history(history_cell::new_error_event(message));
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Failed, None);
        self.request_redraw();

        self.maybe_send_next_queued_input();
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn on_cyber_policy_error(&mut self) {
        self.input_queue.submit_pending_steers_after_interrupt = false;
        self.finalize_turn();
        self.add_to_history(history_cell::new_cyber_policy_error_event());
        self.request_redraw();

        self.maybe_send_next_queued_input();
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn on_rate_limit_error(&mut self, error_kind: RateLimitErrorKind, message: String) {
        let usage_limit_error = matches!(error_kind, RateLimitErrorKind::UsageLimit);
        let rate_limit_reached_type = self.codex_rate_limit_reached_type.map(|kind| {
            if usage_limit_error {
                match kind {
                    RateLimitReachedKind::WorkspaceOwnerCreditsDepleted => {
                        RateLimitReachedKind::WorkspaceOwnerUsageLimitReached
                    }
                    RateLimitReachedKind::WorkspaceMemberCreditsDepleted => {
                        RateLimitReachedKind::WorkspaceMemberUsageLimitReached
                    }
                    other => other,
                }
            } else {
                kind
            }
        });
        self.codex_rate_limit_reached_type = rate_limit_reached_type;
        match rate_limit_reached_type {
            Some(RateLimitReachedKind::WorkspaceOwnerCreditsDepleted) => {
                self.on_error(
                    "You're out of credits. Your workspace is out of credits. Add credits to continue using Codex."
                        .to_string(),
                );
            }
            Some(RateLimitReachedKind::WorkspaceOwnerUsageLimitReached) => {
                self.on_error(
                    "Usage limit reached. You've reached your usage limit. Increase your limits to continue using codex."
                        .to_string(),
                );
            }
            Some(RateLimitReachedKind::WorkspaceMemberCreditsDepleted) => {
                self.on_error(message);
                self.open_workspace_owner_nudge_prompt("credits");
            }
            Some(RateLimitReachedKind::WorkspaceMemberUsageLimitReached) => {
                self.on_error(message);
                self.open_workspace_owner_nudge_prompt("usage_limit");
            }
            Some(RateLimitReachedKind::RateLimitReached) | None => {
                self.on_error(message);
            }
        }
    }

    pub(super) fn on_warning(&mut self, message: impl Into<String>) {
        let message = message.into();
        if !self.warning_display_state.should_display(&message) {
            return;
        }
        self.add_to_history(history_cell::new_warning_event(message));
        self.request_redraw();
    }

    pub(super) fn on_plan_update(&mut self, update: UpdatePlanArgs) {
        self.transcript.saw_plan_update_this_turn = true;
        let total = update.plan.len();
        let completed = update
            .plan
            .iter()
            .filter(|item| match &item.status {
                StepStatus::Completed => true,
                StepStatus::Pending | StepStatus::InProgress => false,
            })
            .count();
        self.transcript.last_plan_progress = (total > 0).then_some((completed, total));
        self.refresh_status_surfaces();
        self.add_to_history(history_cell::new_plan_update(update));
    }

    pub(super) fn interrupted_turn_message(&self, reason: TurnAbortReason) -> String {
        if reason == TurnAbortReason::BudgetLimited {
            return "Goal budget reached - the turn was stopped.".to_string();
        }

        "Conversation interrupted - tell the model what to do differently. Something went wrong? Hit `/feedback` to report the issue.".to_string()
    }
}
