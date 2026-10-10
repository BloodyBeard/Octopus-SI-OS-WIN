use super::*;
use crate::app_backtrack::SIDE_EDIT_PREVIOUS_UNAVAILABLE_MESSAGE;
use crate::key_hint::KeyBindingListExt;

impl App {
    pub(super) async fn launch_external_editor(&mut self, tui: &mut tui::Tui) {
        let editor_cmd = match external_editor::resolve_editor_command() {
            Ok(cmd) => cmd,
            Err(external_editor::EditorError::MissingEditor) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(
                    "Cannot open external editor: set $VISUAL or $EDITOR before starting Codex."
                        .to_string(),
                ));
                self.reset_external_editor_state(tui);
                return;
            }
            Err(err) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
                self.reset_external_editor_state(tui);
                return;
            }
        };

        let seed = self.chat_widget.composer_text_with_pending();
        let editor_result = tui
            .with_restored(tui::RestoreMode::KeepRaw, || async {
                external_editor::run_editor(&seed, &editor_cmd).await
            })
            .await;
        self.reset_external_editor_state(tui);

        match editor_result {
            Ok(new_text) => {
                let cleaned = new_text.trim_end().to_string();
                self.chat_widget.apply_external_edit(cleaned);
            }
            Err(err) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
            }
        }
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn request_external_editor_launch(&mut self, tui: &mut tui::Tui) {
        self.chat_widget
            .set_external_editor_state(ExternalEditorState::Requested);
        self.chat_widget.set_footer_hint_override(Some(vec![(
            EXTERNAL_EDITOR_HINT.to_string(),
            String::new(),
        )]));
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn reset_external_editor_state(&mut self, tui: &mut tui::Tui) {
        self.chat_widget
            .set_external_editor_state(ExternalEditorState::Closed);
        self.chat_widget.set_footer_hint_override(None);
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn apply_raw_output_mode(
        &mut self,
        tui: &mut tui::Tui,
        enabled: bool,
        notify: bool,
    ) {
        if notify {
            self.chat_widget.set_raw_output_mode_and_notify(enabled);
        } else {
            self.chat_widget.set_raw_output_mode(enabled);
        }
        if let Err(err) = self.reflow_transcript_now(tui) {
            tracing::warn!(error = %err, "failed to reflow transcript after raw output mode toggle");
            self.chat_widget
                .add_error_message(format!("Failed to redraw transcript: {err}"));
        }
        tui.frame_requester().schedule_frame();
    }

    pub(super) async fn handle_key_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        key_event: KeyEvent,
    ) {
        let app_keymap_shortcuts_available = self.app_keymap_shortcuts_available();

        if self.chat_widget.side_conversation_active() && side_return_shortcut_matches(key_event) {
            let Some(parent) = self.side_parent_run.clone() else {
                self.chat_widget.add_error_message(
                    "The side question has no recorded parent task; use /tasks to switch."
                        .to_string(),
                );
                return;
            };
            let target = SessionTarget {
                run: parent.clone(),
                record_path: app_server.run_record_path(&parent).unwrap_or_default(),
                cwd: Some(self.config.cwd.to_path_buf()),
            };
            match self.resume_target_session(tui, app_server, target).await {
                Ok(AppRunControl::Continue)
                    if app_server.active_run_id().as_ref() == Some(&parent) =>
                {
                    self.side_parent_run = None;
                    self.side_parent_was_running = false;
                    self.chat_widget.set_side_conversation_active(false);
                    self.chat_widget.set_side_conversation_context_label(None);
                    self.chat_widget
                        .add_info_message(format!("Returned to task {}.", parent.as_str()), None);
                }
                Ok(_) => {}
                Err(error) => self
                    .chat_widget
                    .add_error_message(format!("Failed to return to the main task: {error}")),
            }
            return;
        }

        if app_keymap_shortcuts_available
            && !self.chat_widget.side_conversation_active()
            && side_start_shortcut_matches(key_event)
        {
            if let Some(parent_thread_id) = self.chat_widget.thread_id() {
                self.app_event_tx.send(AppEvent::StartSide {
                    parent_thread_id,
                    user_message: None,
                });
            } else {
                self.chat_widget
                    .add_error_message("Side question requires an active task.".to_string());
            }
            return;
        }

        if app_keymap_shortcuts_available && self.keymap.app.toggle_vim_mode.is_pressed(key_event) {
            self.chat_widget.toggle_vim_mode_and_notify();
            return;
        }

        if app_keymap_shortcuts_available
            && self.keymap.app.toggle_fast_mode.is_pressed(key_event)
            && self.chat_widget.can_toggle_fast_mode_from_keybinding()
        {
            self.chat_widget.toggle_fast_mode_from_ui();
            return;
        }

        if app_keymap_shortcuts_available && self.keymap.app.toggle_raw_output.is_pressed(key_event)
        {
            let enabled = !self.chat_widget.raw_output_mode();
            self.apply_raw_output_mode(tui, enabled, false);
            return;
        }

        if app_keymap_shortcuts_available && self.keymap.app.open_transcript.is_pressed(key_event) {
            let _ = tui.enter_alt_screen();
            self.overlay = Some(match app_server.history_pager() {
                Ok(pager) => Overlay::new_history(
                    pager,
                    self.transcript_cells.clone(),
                    self.keymap.pager.clone(),
                )
                .unwrap_or_else(|error| {
                    self.chat_widget
                        .add_error_message(format!("Failed to open paged history: {error}"));
                    Overlay::new_transcript(
                        self.transcript_cells.clone(),
                        self.keymap.pager.clone(),
                    )
                }),
                Err(error) => {
                    self.chat_widget
                        .add_error_message(format!("Failed to open paged history: {error}"));
                    Overlay::new_transcript(
                        self.transcript_cells.clone(),
                        self.keymap.pager.clone(),
                    )
                }
            });
            tui.frame_requester().schedule_frame();
            return;
        }

        if app_keymap_shortcuts_available
            && matches!(key_event.code, KeyCode::F(2))
            && matches!(key_event.kind, KeyEventKind::Press)
        {
            let _ = tui.enter_alt_screen();
            let reads = app_server.registry_tab_reads();
            self.overlay = Some(Overlay::new_registry(reads));
            tui.frame_requester().schedule_frame();
            return;
        }

        if app_keymap_shortcuts_available
            && matches!(key_event.code, KeyCode::F(3))
            && matches!(key_event.kind, KeyEventKind::Press)
        {
            let _ = tui.enter_alt_screen();
            let movements = app_server.dashboard_movements_snapshot();
            let spine_wired = app_server.drives_embedded_spine();
            self.overlay = Some(Overlay::new_dashboard(movements, spine_wired));
            tui.frame_requester().schedule_frame();
            return;
        }

        if app_keymap_shortcuts_available
            && matches!(key_event.code, KeyCode::F(4))
            && matches!(key_event.kind, KeyEventKind::Press)
        {
            let _ = tui.enter_alt_screen();
            let movements = app_server.dashboard_movements_snapshot();
            let spine_wired = app_server.drives_embedded_spine();
            let tokens = self.chat_widget.token_usage();
            self.overlay = Some(Overlay::new_metrics(movements, tokens, spine_wired));
            tui.frame_requester().schedule_frame();
            return;
        }

        if app_keymap_shortcuts_available
            && self.keymap.app.open_external_editor.is_pressed(key_event)
        {
            if self.overlay.is_none()
                && self.chat_widget.can_launch_external_editor()
                && self.chat_widget.external_editor_state() == ExternalEditorState::Closed
            {
                self.request_external_editor_launch(tui);
            }
            return;
        }

        if matches!(key_event.code, KeyCode::Esc)
            && matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            if self.should_handle_backtrack_esc(key_event) {
                self.handle_backtrack_esc_key(tui);
            } else if self.should_reject_side_backtrack_esc(key_event) {
                self.reject_side_backtrack_esc();
            } else {
                self.chat_widget.handle_key_event(key_event);
            }
            return;
        }

        match key_event {
            _ if app_keymap_shortcuts_available
                && self.keymap.app.clear_terminal.is_pressed(key_event) =>
            {
                if !self.chat_widget.can_run_ctrl_l_clear_now() {
                    return;
                }
                if let Err(err) = self.clear_terminal_ui(tui, false) {
                    tracing::warn!(error = %err, "failed to clear terminal UI");
                    self.chat_widget
                        .add_error_message(format!("Failed to clear terminal UI: {err}"));
                } else {
                    self.reset_app_ui_state_after_clear();
                    self.queue_clear_ui_header(tui);
                    tui.frame_requester().schedule_frame();
                }
            }
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.nth_user_message != usize::MAX
                && self.chat_widget.composer_is_empty() =>
            {
                if let Some(selection) = self.confirm_backtrack_from_main() {
                    self.apply_backtrack_selection(tui, selection);
                }
            }
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_widget.handle_key_event(key_event);
            }
            _ => {
                self.chat_widget.handle_key_event(key_event);
            }
        };
    }

    pub(super) fn should_handle_backtrack_esc(&self, key_event: KeyEvent) -> bool {
        !self.chat_widget.side_conversation_active()
            && self.chat_widget.is_normal_backtrack_mode()
            && self.chat_widget.composer_is_empty()
            && !self.chat_widget.should_handle_vim_insert_escape(key_event)
    }

    pub(super) fn should_reject_side_backtrack_esc(&self, key_event: KeyEvent) -> bool {
        self.chat_widget.side_conversation_active()
            && self.chat_widget.is_normal_backtrack_mode()
            && self.chat_widget.composer_is_empty()
            && !self.chat_widget.should_handle_vim_insert_escape(key_event)
    }

    pub(super) fn reject_side_backtrack_esc(&mut self) {
        self.reset_backtrack_state();
        self.chat_widget
            .add_error_message(SIDE_EDIT_PREVIOUS_UNAVAILABLE_MESSAGE.to_string());
    }

    fn app_keymap_shortcuts_available(&self) -> bool {
        self.overlay.is_none() && self.chat_widget.no_modal_or_popup_active()
    }

    pub(super) fn refresh_status_line(&mut self) {
        self.chat_widget.refresh_status_line();
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_test_app;

    #[tokio::test]
    async fn app_keymap_shortcuts_are_disabled_while_keymap_view_is_active() {
        let mut app = make_test_app().await;
        assert!(app.app_keymap_shortcuts_available());

        let keymap = app.keymap.clone();
        app.chat_widget.open_keymap_debug(&keymap);

        assert!(!app.app_keymap_shortcuts_available());
    }
}
