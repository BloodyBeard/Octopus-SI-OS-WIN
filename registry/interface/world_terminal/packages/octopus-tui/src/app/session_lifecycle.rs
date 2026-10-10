use super::*;
use crate::resume_picker::SessionTarget;
use crate::tui_config::TuiConfig;

impl App {
    pub(super) fn replace_chat_widget(&mut self, mut chat_widget: ChatWidget) {
        let previous_terminal_title = self.chat_widget.last_terminal_title.take();
        if chat_widget.last_terminal_title.is_none() {
            chat_widget.last_terminal_title = previous_terminal_title;
        }
        chat_widget.remote_connection = self.chat_widget.remote_connection.clone();
        self.chat_widget = chat_widget;
        if let Some(history) = self.prompt_history.as_deref() {
            self.chat_widget.set_prompt_history_metadata(
                crate::app::PROMPT_HISTORY_SNAPSHOT_ID,
                history.entry_count(),
            );
        }
    }

    pub(super) fn reset_for_thread_switch(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.reset_transcript_state_after_clear();
        tui.clear_pending_history_lines();
        Self::clear_terminal_for_thread_switch(&mut tui.terminal)?;
        Ok(())
    }

    pub(super) fn clear_terminal_for_thread_switch<B>(
        terminal: &mut crate::custom_terminal::Terminal<B>,
    ) -> Result<()>
    where
        B: Backend + Write,
    {
        terminal.clear_scrollback_and_visible_screen_ansi()?;
        let mut area = terminal.viewport_area;
        if area.y > 0 {
            area.y = 0;
            terminal.set_viewport_area(area);
        }
        Ok(())
    }

    pub(super) async fn start_fresh_session_with_summary_hint(
        &mut self,
        tui: &mut tui::Tui,
        _app_server: &mut AppServerSession,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
    ) {
        self.refresh_in_memory_config_from_disk_best_effort("starting a new thread")
            .await;
        let config = self.fresh_session_config();
        let summary = session_summary(
            self.chat_widget.token_usage(),
            self.chat_widget.thread_id(),
            self.chat_widget.thread_name(),
            self.chat_widget.rollout_path().as_deref(),
        );
        self.config = config.clone();
        let init =
            self.chatwidget_init_for_forked_or_resumed_thread(tui, config, initial_user_message);
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        if let Some(summary) = summary {
            let mut lines: Vec<Line<'static>> = Vec::new();
            if let Some(usage_line) = summary.usage_line {
                lines.push(usage_line.into());
            }
            if let Some(command) = summary.resume_hint {
                let spans = vec!["To continue this chat, run ".into(), command.cyan()];
                lines.push(spans.into());
            }
            self.chat_widget.add_plain_history_lines(lines);
        }
        self.chat_widget.submit_initial_user_message_if_pending();
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn fresh_session_config(&self) -> TuiConfig {
        let mut config = self.config.clone();
        config.service_tier = self.chat_widget.configured_service_tier();
        config
    }

    pub(super) fn replay_run_transcript_as_history(&mut self, run: &RunId) {
        match crate::resume_picker::run_transcript(&self.config, run) {
            Ok(entries) => {
                if entries.is_empty() {
                    self.chat_widget.add_info_message(
                        format!("Resumed run {} (no prior transcript).", run.as_str()),
                        None,
                    );
                } else {
                    self.chat_widget
                        .add_info_message(format!("Resumed run {}.", run.as_str()), None);
                    self.chat_widget.replay_transcript_entries(&entries);
                }
            }
            Err(err) => {
                tracing::warn!(run = run.as_str(), %err, "failed to render resumed run transcript");
                self.chat_widget.add_info_message(
                    format!("Resumed run {} (transcript unavailable).", run.as_str()),
                    None,
                );
            }
        }
    }

    pub(super) async fn resume_target_session(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        target_session: SessionTarget,
    ) -> Result<AppRunControl> {
        let current_cwd = self.config.cwd.to_path_buf();
        let resume_cwd = if self.app_server_target.uses_remote_workspace() {
            current_cwd.clone()
        } else {
            target_session
                .cwd
                .clone()
                .filter(|cwd| cwd.exists())
                .unwrap_or_else(|| current_cwd.clone())
        };

        let mut resume_config = match self
            .rebuild_config_for_resume_or_fallback(&current_cwd, resume_cwd)
            .await
        {
            Ok(cfg) => cfg,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to rebuild configuration for resume: {err}"
                ));
                return Ok(AppRunControl::Continue);
            }
        };
        self.apply_runtime_policy_overrides(&mut resume_config);

        if let Err(err) = app_server.attach_existing_run(&target_session.run).await {
            self.chat_widget.add_error_message(format!(
                "Failed to switch to {}: {err}",
                target_session.display_label()
            ));
            return Ok(AppRunControl::Continue);
        }

        self.reset_for_thread_switch(tui)?;
        self.config = resume_config;
        tui.set_notification_settings(
            self.config.terminal.notifications.method,
            self.config.terminal.notifications.condition,
        );
        self.file_search
            .update_search_dir(self.config.cwd.to_path_buf());
        let init =
            self.chatwidget_init_for_forked_or_resumed_thread(tui, self.config.clone(), None);
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        self.sync_chat_to_active_topology(app_server)?;

        match crate::resume_picker::run_transcript(&self.config, &target_session.run) {
            Ok(entries) => {
                let header = if entries.is_empty() {
                    format!(
                        "Resumed run {} (no prior transcript).",
                        target_session.run.as_str()
                    )
                } else {
                    format!("Resumed run {}.", target_session.run.as_str())
                };
                self.chat_widget.add_info_message(header, None);
                if !entries.is_empty() {
                    self.chat_widget.replay_transcript_entries(&entries);
                }
            }
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to resume session from {}: {err}",
                    target_session.display_label()
                ));
            }
        }

        tui.frame_requester().schedule_frame();
        Ok(AppRunControl::Continue)
    }
}
