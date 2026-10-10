use std::any::TypeId;
use std::path::PathBuf;
use std::sync::Arc;

use crate::app::App;
use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::chatwidget::UserMessage;
#[cfg(test)]
use crate::history_cell::AgentMessageCell;
use crate::history_cell::SessionInfoCell;
use crate::history_cell::UserHistoryCell;
use crate::pager_overlay::Overlay;
use crate::tui;
use crate::tui::TuiEvent;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use octopus_world_terminal_client::TextElement;

const NO_PREVIOUS_MESSAGE_TO_EDIT: &str = "No previous message to edit.";
pub(crate) const SIDE_EDIT_PREVIOUS_UNAVAILABLE_MESSAGE: &str =
    "Editing previous prompts is unavailable in side conversations.";

#[derive(Default)]
pub(crate) struct BacktrackState {
    pub(crate) primed: bool,
    pub(crate) base_id: Option<String>,
    pub(crate) nth_user_message: usize,
    pub(crate) overlay_preview_active: bool,
    pub(crate) pending_rollback: Option<PendingBacktrackRollback>,
}

#[derive(Debug, Clone)]
pub(crate) struct BacktrackSelection {
    pub(crate) nth_user_message: usize,
    pub(crate) prefill: String,
    pub(crate) text_elements: Vec<TextElement>,
    pub(crate) local_image_paths: Vec<PathBuf>,
    pub(crate) remote_image_urls: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingBacktrackRollback {
    pub(crate) selection: BacktrackSelection,
    pub(crate) thread_id: Option<String>,
}

impl App {
    pub(crate) async fn handle_backtrack_overlay_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<bool> {
        if self.backtrack.overlay_preview_active {
            match event {
                TuiEvent::Key(KeyEvent {
                    code: KeyCode::Esc,
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                }) => {
                    self.overlay_step_backtrack(tui, event)?;
                    Ok(true)
                }
                TuiEvent::Key(KeyEvent {
                    code: KeyCode::Left,
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                }) => {
                    self.overlay_step_backtrack(tui, event)?;
                    Ok(true)
                }
                TuiEvent::Key(KeyEvent {
                    code: KeyCode::Right,
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                }) => {
                    self.overlay_step_backtrack_forward(tui, event)?;
                    Ok(true)
                }
                TuiEvent::Key(KeyEvent {
                    code: KeyCode::Enter,
                    kind: KeyEventKind::Press,
                    ..
                }) => {
                    self.overlay_confirm_backtrack(tui);
                    Ok(true)
                }
                _ => {
                    self.overlay_forward_event(tui, event)?;
                    Ok(true)
                }
            }
        } else if let TuiEvent::Key(KeyEvent {
            code: KeyCode::Esc,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        {
            self.begin_overlay_backtrack_preview(tui);
            Ok(true)
        } else {
            self.overlay_forward_event(tui, event)?;
            Ok(true)
        }
    }

    pub(crate) fn handle_backtrack_esc_key(&mut self, tui: &mut tui::Tui) {
        if !self.chat_widget.composer_is_empty() {
            return;
        }

        if !self.backtrack.primed {
            self.prime_backtrack();
        } else if self.overlay.is_none() {
            self.open_backtrack_preview(tui);
        } else if self.backtrack.overlay_preview_active {
            self.step_backtrack_and_highlight(tui);
        }
    }

    pub(crate) fn apply_backtrack_rollback(&mut self, selection: BacktrackSelection) {
        if self.chat_widget.side_conversation_active() {
            self.reset_backtrack_state();
            self.chat_widget
                .add_error_message(SIDE_EDIT_PREVIOUS_UNAVAILABLE_MESSAGE.to_string());
            return;
        }

        let user_total = user_count(&self.transcript_cells);
        if user_total == 0 {
            return;
        }

        if self.backtrack.pending_rollback.is_some() {
            self.chat_widget
                .add_error_message("Backtrack rollback already in progress.".to_string());
            return;
        }

        let num_turns = user_total.saturating_sub(selection.nth_user_message);
        let num_turns = u32::try_from(num_turns).unwrap_or(u32::MAX);
        if num_turns == 0 {
            return;
        }

        let prefill = selection.prefill.clone();
        let text_elements = selection.text_elements.clone();
        let local_image_paths = selection.local_image_paths.clone();
        let remote_image_urls = selection.remote_image_urls.clone();
        let has_remote_image_urls = !remote_image_urls.is_empty();
        self.backtrack.pending_rollback = Some(PendingBacktrackRollback {
            selection,
            thread_id: self.chat_widget.thread_id(),
        });
        self.chat_widget
            .submit_op(AppCommand::thread_rollback(num_turns));
        self.chat_widget.set_remote_image_urls(remote_image_urls);
        if !prefill.is_empty()
            || !text_elements.is_empty()
            || !local_image_paths.is_empty()
            || has_remote_image_urls
        {
            self.chat_widget
                .set_composer_text(prefill, text_elements, local_image_paths);
        }
    }

    pub(crate) fn apply_cancelled_turn_edit(&mut self, prompt: UserMessage) {
        let user_total = user_count(&self.transcript_cells);
        let selection = BacktrackSelection {
            nth_user_message: user_total.saturating_sub(1),
            prefill: prompt.text.clone(),
            text_elements: prompt.text_elements.clone(),
            local_image_paths: prompt
                .local_images
                .iter()
                .map(|image| image.path.clone())
                .collect(),
            remote_image_urls: prompt.remote_image_urls.clone(),
        };
        if user_total == 0 {
            if self.backtrack.pending_rollback.is_some() {
                self.chat_widget
                    .add_error_message("Backtrack rollback already in progress.".to_string());
                return;
            }
            self.backtrack.pending_rollback = Some(PendingBacktrackRollback {
                selection,
                thread_id: self.chat_widget.thread_id(),
            });
            self.chat_widget.submit_op(AppCommand::thread_rollback(1));
            self.chat_widget.restore_user_message_to_composer(prompt);
            return;
        }
        self.apply_backtrack_rollback(selection);
        self.chat_widget.restore_user_message_to_composer(prompt);
    }

    pub(crate) fn open_transcript_overlay(&mut self, tui: &mut tui::Tui) {
        let _ = tui.enter_alt_screen();
        self.overlay = Some(Overlay::new_transcript(
            self.transcript_cells.clone(),
            self.keymap.pager.clone(),
        ));
        tui.frame_requester().schedule_frame();
    }

    pub(crate) fn close_transcript_overlay(&mut self, tui: &mut tui::Tui) {
        let _ = tui.leave_alt_screen();
        let was_backtrack = self.backtrack.overlay_preview_active;
        if !self.deferred_history_lines.is_empty() {
            let lines = std::mem::take(&mut self.deferred_history_lines);
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                lines,
                self.history_line_wrap_policy(),
            );
        }
        self.overlay = None;
        self.backtrack.overlay_preview_active = false;
        tui.frame_requester().schedule_frame();
        if was_backtrack {
            self.reset_backtrack_state();
        }
    }

    fn prime_backtrack(&mut self) {
        self.backtrack.primed = true;
        self.backtrack.nth_user_message = usize::MAX;
        self.backtrack.base_id = self.chat_widget.thread_id();
        if has_backtrack_target(&self.transcript_cells) {
            self.chat_widget.show_esc_backtrack_hint();
        }
    }

    fn open_backtrack_preview(&mut self, tui: &mut tui::Tui) {
        if !has_backtrack_target(&self.transcript_cells) {
            self.reset_backtrack_state();
            self.chat_widget
                .add_info_message(NO_PREVIOUS_MESSAGE_TO_EDIT.to_string(), None);
            tui.frame_requester().schedule_frame();
            return;
        }

        self.open_transcript_overlay(tui);
        self.backtrack.overlay_preview_active = true;
        self.chat_widget.clear_esc_backtrack_hint();
        self.step_backtrack_and_highlight(tui);
    }

    fn begin_overlay_backtrack_preview(&mut self, tui: &mut tui::Tui) {
        if !has_backtrack_target(&self.transcript_cells) {
            self.close_transcript_overlay(tui);
            self.chat_widget
                .add_info_message(NO_PREVIOUS_MESSAGE_TO_EDIT.to_string(), None);
            tui.frame_requester().schedule_frame();
            return;
        }

        self.backtrack.primed = true;
        self.backtrack.base_id = self.chat_widget.thread_id();
        self.backtrack.overlay_preview_active = true;
        let count = user_count(&self.transcript_cells);
        if let Some(last) = count.checked_sub(1) {
            self.apply_backtrack_selection_internal(last);
        }
        tui.frame_requester().schedule_frame();
    }

    fn step_backtrack_and_highlight(&mut self, tui: &mut tui::Tui) {
        let count = user_count(&self.transcript_cells);
        if count == 0 {
            return;
        }

        let last_index = count.saturating_sub(1);
        let next_selection = if self.backtrack.nth_user_message == usize::MAX {
            last_index
        } else if self.backtrack.nth_user_message == 0 {
            0
        } else {
            self.backtrack
                .nth_user_message
                .saturating_sub(1)
                .min(last_index)
        };

        self.apply_backtrack_selection_internal(next_selection);
        tui.frame_requester().schedule_frame();
    }

    fn step_forward_backtrack_and_highlight(&mut self, tui: &mut tui::Tui) {
        let count = user_count(&self.transcript_cells);
        if count == 0 {
            return;
        }

        let last_index = count.saturating_sub(1);
        let next_selection = if self.backtrack.nth_user_message == usize::MAX {
            last_index
        } else {
            self.backtrack
                .nth_user_message
                .saturating_add(1)
                .min(last_index)
        };

        self.apply_backtrack_selection_internal(next_selection);
        tui.frame_requester().schedule_frame();
    }

    fn apply_backtrack_selection_internal(&mut self, nth_user_message: usize) {
        if let Some(cell_idx) = nth_user_position(&self.transcript_cells, nth_user_message) {
            self.backtrack.nth_user_message = nth_user_message;
            if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                t.set_highlight_cell(Some(cell_idx));
            }
        } else {
            self.backtrack.nth_user_message = usize::MAX;
            if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                t.set_highlight_cell(None);
            }
        }
    }

    fn overlay_forward_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        if matches!(&event, TuiEvent::Draw | TuiEvent::Resize)
            && let Some(Overlay::Transcript(t)) = &mut self.overlay
        {
            let active_key = self.chat_widget.active_cell_transcript_key();
            let chat_widget = &self.chat_widget;
            tui.draw(u16::MAX, |frame| {
                let width = frame.area().width.max(1);
                t.sync_live_tail(width, active_key, |w| {
                    chat_widget.active_cell_transcript_hyperlink_lines(w)
                });
                t.render(frame.area(), frame.buffer);
            })?;
            let close_overlay = t.is_done();
            if !close_overlay
                && active_key.is_some_and(|key| key.animation_tick.is_some())
                && t.is_scrolled_to_bottom()
            {
                tui.frame_requester()
                    .schedule_frame_in(std::time::Duration::from_millis(50));
            }
            if close_overlay {
                self.close_transcript_overlay(tui);
                tui.frame_requester().schedule_frame();
            }
            return Ok(());
        }

        if let Some(overlay) = &mut self.overlay {
            overlay.handle_event(tui, event)?;
            if overlay.is_done() {
                self.close_transcript_overlay(tui);
                tui.frame_requester().schedule_frame();
            }
        }
        Ok(())
    }

    fn overlay_confirm_backtrack(&mut self, tui: &mut tui::Tui) {
        let nth_user_message = self.backtrack.nth_user_message;
        let selection = self.backtrack_selection(nth_user_message);
        self.close_transcript_overlay(tui);
        if let Some(selection) = selection {
            self.apply_backtrack_rollback(selection);
            tui.frame_requester().schedule_frame();
        }
    }

    fn overlay_step_backtrack(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        if self.backtrack.base_id.is_some() {
            self.step_backtrack_and_highlight(tui);
        } else {
            self.overlay_forward_event(tui, event)?;
        }
        Ok(())
    }

    fn overlay_step_backtrack_forward(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<()> {
        if self.backtrack.base_id.is_some() {
            self.step_forward_backtrack_and_highlight(tui);
        } else {
            self.overlay_forward_event(tui, event)?;
        }
        Ok(())
    }

    pub(crate) fn confirm_backtrack_from_main(&mut self) -> Option<BacktrackSelection> {
        let selection = self.backtrack_selection(self.backtrack.nth_user_message);
        self.reset_backtrack_state();
        selection
    }

    pub(crate) fn reset_backtrack_state(&mut self) {
        self.backtrack.primed = false;
        self.backtrack.base_id = None;
        self.backtrack.nth_user_message = usize::MAX;
        self.chat_widget.clear_esc_backtrack_hint();
    }

    pub(crate) fn apply_backtrack_selection(
        &mut self,
        tui: &mut tui::Tui,
        selection: BacktrackSelection,
    ) {
        self.apply_backtrack_rollback(selection);
        tui.frame_requester().schedule_frame();
    }

    pub(crate) fn handle_backtrack_rollback_succeeded(&mut self, num_turns: u32) {
        if self.backtrack.pending_rollback.is_some() {
            self.finish_pending_backtrack();
        } else {
            self.app_event_tx
                .send(AppEvent::ApplyThreadRollback { num_turns });
        }
    }

    pub(crate) fn handle_backtrack_rollback_failed(&mut self) {
        self.backtrack.pending_rollback = None;
    }

    pub(crate) fn apply_non_pending_thread_rollback(&mut self, num_turns: u32) -> bool {
        if !trim_transcript_cells_drop_last_n_user_turns(&mut self.transcript_cells, num_turns) {
            return false;
        }
        self.chat_widget.clear_pending_token_activity_refreshes();
        self.chat_widget.clear_pending_rate_limit_reset_hint();
        self.chat_widget
            .truncate_agent_copy_history_to_user_turn_count(user_count(&self.transcript_cells));
        self.sync_overlay_after_transcript_trim();
        self.backtrack_render_pending = true;
        true
    }

    fn finish_pending_backtrack(&mut self) {
        let Some(pending) = self.backtrack.pending_rollback.take() else {
            return;
        };
        if pending.thread_id != self.chat_widget.thread_id() {
            return;
        }
        if trim_transcript_cells_to_nth_user(
            &mut self.transcript_cells,
            pending.selection.nth_user_message,
        ) {
            self.chat_widget.clear_pending_token_activity_refreshes();
            self.chat_widget.clear_pending_rate_limit_reset_hint();
            self.chat_widget
                .truncate_agent_copy_history_to_user_turn_count(user_count(&self.transcript_cells));
            self.sync_overlay_after_transcript_trim();
            self.backtrack_render_pending = true;
        }
    }

    fn backtrack_selection(&self, nth_user_message: usize) -> Option<BacktrackSelection> {
        let base_id = self.backtrack.base_id.clone()?;
        if self.chat_widget.thread_id() != Some(base_id) {
            return None;
        }

        let (prefill, text_elements, local_image_paths, remote_image_urls) =
            nth_user_position(&self.transcript_cells, nth_user_message)
                .and_then(|idx| self.transcript_cells.get(idx))
                .and_then(|cell| cell.as_any().downcast_ref::<UserHistoryCell>())
                .map(|cell| {
                    (
                        cell.message.clone(),
                        cell.text_elements.clone(),
                        cell.local_image_paths.clone(),
                        cell.remote_image_urls.clone(),
                    )
                })
                .unwrap_or_else(|| (String::new(), Vec::new(), Vec::new(), Vec::new()));

        Some(BacktrackSelection {
            nth_user_message,
            prefill,
            text_elements,
            local_image_paths,
            remote_image_urls,
        })
    }

    fn sync_overlay_after_transcript_trim(&mut self) {
        if let Some(Overlay::Transcript(t)) = &mut self.overlay {
            t.replace_cells(self.transcript_cells.clone());
        }
        if self.backtrack.overlay_preview_active {
            let total_users = user_count(&self.transcript_cells);
            let next_selection = if total_users == 0 {
                usize::MAX
            } else {
                self.backtrack
                    .nth_user_message
                    .min(total_users.saturating_sub(1))
            };
            self.apply_backtrack_selection_internal(next_selection);
        }
        self.deferred_history_lines.clear();
    }
}

fn trim_transcript_cells_to_nth_user(
    transcript_cells: &mut Vec<Arc<dyn crate::history_cell::HistoryCell>>,
    nth_user_message: usize,
) -> bool {
    if nth_user_message == usize::MAX {
        return false;
    }

    if let Some(cut_idx) = nth_user_position(transcript_cells, nth_user_message) {
        let original_len = transcript_cells.len();
        transcript_cells.truncate(cut_idx);
        return transcript_cells.len() != original_len;
    }
    false
}

pub(crate) fn trim_transcript_cells_drop_last_n_user_turns(
    transcript_cells: &mut Vec<Arc<dyn crate::history_cell::HistoryCell>>,
    num_turns: u32,
) -> bool {
    if num_turns == 0 {
        return false;
    }

    let user_positions: Vec<usize> = user_positions_iter(transcript_cells).collect();
    let Some(&first_user_idx) = user_positions.first() else {
        return false;
    };

    let turns_from_end = usize::try_from(num_turns).unwrap_or(usize::MAX);
    let cut_idx = if turns_from_end >= user_positions.len() {
        first_user_idx
    } else {
        user_positions[user_positions.len() - turns_from_end]
    };
    let original_len = transcript_cells.len();
    transcript_cells.truncate(cut_idx);
    transcript_cells.len() != original_len
}

pub(crate) fn user_count(cells: &[Arc<dyn crate::history_cell::HistoryCell>]) -> usize {
    user_positions_iter(cells).count()
}

fn has_backtrack_target(cells: &[Arc<dyn crate::history_cell::HistoryCell>]) -> bool {
    user_count(cells) > 0
}

fn nth_user_position(
    cells: &[Arc<dyn crate::history_cell::HistoryCell>],
    nth: usize,
) -> Option<usize> {
    user_positions_iter(cells)
        .enumerate()
        .find_map(|(i, idx)| (i == nth).then_some(idx))
}

fn user_positions_iter(
    cells: &[Arc<dyn crate::history_cell::HistoryCell>],
) -> impl Iterator<Item = usize> + '_ {
    let session_start_type = TypeId::of::<SessionInfoCell>();
    let user_type = TypeId::of::<UserHistoryCell>();
    let type_of = |cell: &Arc<dyn crate::history_cell::HistoryCell>| cell.as_any().type_id();

    let start = cells
        .iter()
        .rposition(|cell| type_of(cell) == session_start_type)
        .map_or(0, |idx| idx + 1);

    cells
        .iter()
        .enumerate()
        .skip(start)
        .filter_map(move |(idx, cell)| (type_of(cell) == user_type).then_some(idx))
}

#[cfg(test)]
fn agent_group_count(cells: &[Arc<dyn crate::history_cell::HistoryCell>]) -> usize {
    agent_group_positions_iter(cells).count()
}

#[cfg(test)]
fn agent_group_positions_iter(
    cells: &[Arc<dyn crate::history_cell::HistoryCell>],
) -> impl Iterator<Item = usize> + '_ {
    let session_start_type = TypeId::of::<SessionInfoCell>();
    let type_of = |cell: &Arc<dyn crate::history_cell::HistoryCell>| cell.as_any().type_id();

    let start = cells
        .iter()
        .rposition(|cell| type_of(cell) == session_start_type)
        .map_or(0, |idx| idx + 1);

    cells
        .iter()
        .enumerate()
        .skip(start)
        .filter_map(move |(idx, cell)| {
            let is_agent = cell.as_any().downcast_ref::<AgentMessageCell>().is_some();
            let is_copy_source_group = is_agent && !cell.is_stream_continuation();
            is_copy_source_group.then_some(idx)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use std::sync::Arc;

    fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn trim_transcript_for_first_user_drops_user_and_newer_cells() {
        let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(UserHistoryCell {
                message: "first user".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(vec![Line::from("assistant")], true))
                as Arc<dyn HistoryCell>,
        ];
        trim_transcript_cells_to_nth_user(&mut cells, 0);

        assert!(cells.is_empty());
    }

    #[test]
    fn trim_transcript_preserves_cells_before_selected_user() {
        let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(AgentMessageCell::new(vec![Line::from("intro")], true))
                as Arc<dyn HistoryCell>,
            Arc::new(UserHistoryCell {
                message: "first".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(vec![Line::from("after")], false))
                as Arc<dyn HistoryCell>,
        ];
        trim_transcript_cells_to_nth_user(&mut cells, 0);

        assert_eq!(cells.len(), 1);
        let agent = cells[0]
            .as_any()
            .downcast_ref::<AgentMessageCell>()
            .expect("agent cell");
        let agent_lines = agent.display_lines(u16::MAX);
        assert_eq!(agent_lines.len(), 1);
        let intro_text: String = agent_lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(intro_text, "• intro");
    }

    #[test]
    fn trim_transcript_for_later_user_keeps_prior_history() {
        let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(AgentMessageCell::new(vec![Line::from("intro")], true))
                as Arc<dyn HistoryCell>,
            Arc::new(UserHistoryCell {
                message: "first".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(vec![Line::from("between")], false))
                as Arc<dyn HistoryCell>,
            Arc::new(UserHistoryCell {
                message: "second".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(vec![Line::from("tail")], false))
                as Arc<dyn HistoryCell>,
        ];
        trim_transcript_cells_to_nth_user(&mut cells, 1);

        assert_eq!(cells.len(), 3);
        let agent_intro = cells[0]
            .as_any()
            .downcast_ref::<AgentMessageCell>()
            .expect("intro agent");
        let intro_lines = agent_intro.display_lines(u16::MAX);
        let intro_text: String = intro_lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(intro_text, "• intro");

        let user_first = cells[1]
            .as_any()
            .downcast_ref::<UserHistoryCell>()
            .expect("first user");
        assert_eq!(user_first.message, "first");

        let agent_between = cells[2]
            .as_any()
            .downcast_ref::<AgentMessageCell>()
            .expect("between agent");
        let between_lines = agent_between.display_lines(u16::MAX);
        let between_text: String = between_lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(between_text, "  between");
    }

    #[test]
    fn trim_drop_last_n_user_turns_applies_rollback_semantics() {
        let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(UserHistoryCell {
                message: "first".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(
                vec![Line::from("after first")],
                false,
            )) as Arc<dyn HistoryCell>,
            Arc::new(UserHistoryCell {
                message: "second".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(
                vec![Line::from("after second")],
                false,
            )) as Arc<dyn HistoryCell>,
        ];

        let changed = trim_transcript_cells_drop_last_n_user_turns(&mut cells, 1);

        assert!(changed);
        assert_eq!(cells.len(), 2);
        let first_user = cells[0]
            .as_any()
            .downcast_ref::<UserHistoryCell>()
            .expect("first user");
        assert_eq!(first_user.message, "first");
    }

    #[test]
    fn trim_drop_last_n_user_turns_allows_overflow() {
        let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(AgentMessageCell::new(vec![Line::from("intro")], true))
                as Arc<dyn HistoryCell>,
            Arc::new(UserHistoryCell {
                message: "first".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(vec![Line::from("after")], false))
                as Arc<dyn HistoryCell>,
        ];

        let changed = trim_transcript_cells_drop_last_n_user_turns(&mut cells, u32::MAX);

        assert!(changed);
        assert_eq!(cells.len(), 1);
        let intro = cells[0]
            .as_any()
            .downcast_ref::<AgentMessageCell>()
            .expect("intro agent");
        let intro_lines = intro.display_lines(u16::MAX);
        let intro_text: String = intro_lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(intro_text, "• intro");
    }

    #[test]
    fn agent_group_count_ignores_context_compacted_marker() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(AgentMessageCell::new(vec![Line::from("first")], true))
                as Arc<dyn HistoryCell>,
            Arc::new(crate::history_cell::new_info_event(
                "Context compacted".to_string(),
                None,
            )) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(vec![Line::from("second")], true))
                as Arc<dyn HistoryCell>,
        ];

        assert_eq!(agent_group_count(&cells), 2);
    }

    #[test]
    fn backtrack_target_requires_user_message() {
        let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(AgentMessageCell::new(vec![Line::from("assistant")], true))
                as Arc<dyn HistoryCell>,
            Arc::new(crate::history_cell::new_info_event(
                "Context compacted".to_string(),
                None,
            )) as Arc<dyn HistoryCell>,
        ];

        assert!(!has_backtrack_target(&cells));

        cells.push(Arc::new(UserHistoryCell {
            message: "hello".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>);

        assert!(has_backtrack_target(&cells));
    }

    #[test]
    fn backtrack_unavailable_info_message_snapshot() {
        let cell =
            crate::history_cell::new_info_event(NO_PREVIOUS_MESSAGE_TO_EDIT.to_string(), None);
        let rendered = render_lines(&cell.display_lines(80)).join("\n");

        insta::assert_snapshot!(rendered);
    }
}
