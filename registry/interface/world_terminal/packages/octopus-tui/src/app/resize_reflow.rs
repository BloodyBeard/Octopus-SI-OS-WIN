use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use color_eyre::eyre::Result;
use ratatui::text::Line;

use super::App;
use super::InitialHistoryReplayBuffer;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::insert_history::HistoryLineWrapPolicy;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::transcript_reflow::TRANSCRIPT_REFLOW_DEBOUNCE;
use crate::tui;

struct ReflowCellDisplay {
    lines: Vec<HyperlinkLine>,
    is_stream_continuation: bool,
}

pub(super) struct ReflowRenderResult {
    pub(super) lines: Vec<HyperlinkLine>,
}

pub(super) fn trailing_run_start<T: 'static>(transcript_cells: &[Arc<dyn HistoryCell>]) -> usize {
    let end = transcript_cells.len();
    let mut start = end;

    while start > 0
        && transcript_cells[start - 1].is_stream_continuation()
        && transcript_cells[start - 1].as_any().is::<T>()
    {
        start -= 1;
    }

    if start > 0
        && transcript_cells[start - 1].as_any().is::<T>()
        && !transcript_cells[start - 1].is_stream_continuation()
    {
        start -= 1;
    }

    start
}

impl App {
    pub(super) fn reset_history_emission_state(&mut self) {
        self.has_emitted_history_lines = false;
        self.deferred_history_lines.clear();
    }

    fn display_lines_for_history_insert(
        &mut self,
        cell: &dyn HistoryCell,
        width: u16,
    ) -> Vec<HyperlinkLine> {
        let mut display =
            cell.display_hyperlink_lines_for_mode(width, self.chat_widget.history_render_mode());
        if !display.is_empty() && !cell.is_stream_continuation() {
            if self.has_emitted_history_lines {
                display.insert(0, HyperlinkLine::new(Line::from("")));
            } else {
                self.has_emitted_history_lines = true;
            }
        }
        display
    }

    pub(super) fn insert_history_cell_lines(
        &mut self,
        tui: &mut tui::Tui,
        cell: &dyn HistoryCell,
        width: u16,
    ) {
        let display = self.display_lines_for_history_insert(cell, width);
        if display.is_empty() {
            return;
        }
        if self.overlay.is_some() {
            self.deferred_history_lines.extend(display);
        } else {
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                display,
                self.history_line_wrap_policy(),
            );
        }
    }

    pub(super) fn begin_initial_history_replay_buffer(&mut self) {
        if self.overlay.is_none() {
            self.initial_history_replay_buffer = Some(Default::default());
        }
    }

    pub(super) fn begin_thread_switch_history_replay_buffer(&mut self) {
        if self.resize_reflow_max_rows().is_some() && self.overlay.is_none() {
            self.initial_history_replay_buffer = Some(InitialHistoryReplayBuffer {
                retained_lines: VecDeque::new(),
                render_from_transcript_tail: true,
            });
        }
    }

    pub(super) fn finish_initial_history_replay_buffer(&mut self, tui: &mut tui::Tui) {
        let Some(buffer) = self.initial_history_replay_buffer.take() else {
            return;
        };

        if buffer.retained_lines.is_empty() {
            if buffer.render_from_transcript_tail {
                let width = tui.terminal.last_known_screen_size.width;
                let reflowed_lines = self.render_transcript_lines_for_reflow(width).lines;
                if !reflowed_lines.is_empty() {
                    tui.insert_history_hyperlink_lines_with_wrap_policy(
                        reflowed_lines,
                        self.history_line_wrap_policy(),
                    );
                }
            }
            return;
        }

        let retained_lines = buffer.retained_lines.into_iter().collect::<Vec<_>>();
        tui.insert_history_hyperlink_lines_with_wrap_policy(
            retained_lines,
            self.history_line_wrap_policy(),
        );
    }

    pub(super) fn insert_history_cell_lines_with_initial_replay_buffer(
        &mut self,
        tui: &mut tui::Tui,
        cell: &dyn HistoryCell,
        width: u16,
    ) {
        if self
            .initial_history_replay_buffer
            .as_ref()
            .is_some_and(|buffer| buffer.render_from_transcript_tail)
        {
            return;
        }

        let display = self.display_lines_for_history_insert(cell, width);

        if display.is_empty() {
            return;
        }

        let max_rows = self.resize_reflow_max_rows();
        if let Some(buffer) = &mut self.initial_history_replay_buffer {
            if let Some(max_rows) = max_rows {
                Self::buffer_initial_history_replay_display_lines(buffer, display, max_rows);
            } else if self.overlay.is_some() {
                self.deferred_history_lines.extend(display);
            } else {
                tui.insert_history_hyperlink_lines_with_wrap_policy(
                    display,
                    self.history_line_wrap_policy(),
                );
            }
        }
    }

    pub(crate) fn history_line_wrap_policy(&self) -> HistoryLineWrapPolicy {
        if self.chat_widget.raw_output_mode() {
            HistoryLineWrapPolicy::Terminal
        } else {
            HistoryLineWrapPolicy::PreWrap
        }
    }

    pub(super) fn buffer_initial_history_replay_display_lines(
        buffer: &mut InitialHistoryReplayBuffer,
        display: Vec<HyperlinkLine>,
        max_rows: usize,
    ) {
        buffer.retained_lines.extend(display);
        while buffer.retained_lines.len() > max_rows {
            buffer.retained_lines.pop_front();
        }
    }

    fn schedule_resize_reflow(&mut self, target_width: Option<u16>) -> bool {
        self.transcript_reflow.schedule_debounced(target_width)
    }

    fn resize_reflow_max_rows(&self) -> Option<usize> {
        crate::resize_reflow_cap::resize_reflow_max_rows(self.config.terminal.resize_reflow)
    }

    fn clear_terminal_for_resize_replay(&mut self, tui: &mut tui::Tui) -> Result<()> {
        if tui.is_alt_screen_active() {
            tui.terminal.clear_visible_screen()?;
        } else {
            tui.terminal.clear_scrollback_and_visible_screen_ansi()?;
        }
        let mut area = tui.terminal.viewport_area;
        if area.y > 0 {
            area.y = 0;
            tui.terminal.set_viewport_area(area);
        }
        Ok(())
    }

    pub(super) fn maybe_finish_stream_reflow(&mut self, tui: &mut tui::Tui) -> Result<()> {
        if self.transcript_reflow.take_stream_finish_reflow_needed() {
            self.schedule_immediate_resize_reflow(tui);
            self.maybe_run_resize_reflow(tui)?;
        } else if self.transcript_reflow.pending_is_due(Instant::now()) {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    fn schedule_immediate_resize_reflow(&mut self, tui: &mut tui::Tui) {
        self.transcript_reflow.schedule_immediate();
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn finish_required_stream_reflow(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.schedule_immediate_resize_reflow(tui);
        self.maybe_run_resize_reflow(tui)?;
        if !self.transcript_reflow.has_pending_reflow() {
            self.transcript_reflow.clear_stream_flags();
        }
        Ok(())
    }

    pub(super) fn handle_draw_size_change(
        &mut self,
        size: ratatui::layout::Size,
        last_known_screen_size: ratatui::layout::Size,
        frame_requester: &tui::FrameRequester,
    ) -> bool {
        let width = self.transcript_reflow.note_width(size.width);
        let reflow_needed = self.transcript_reflow.reflow_needed_for_width(size.width);
        let height_changed = size.height != last_known_screen_size.height;
        let should_rebuild_transcript = reflow_needed || height_changed;
        if width.changed || width.initialized {
            self.chat_widget.on_terminal_resize(size.width);
        }
        if should_rebuild_transcript {
            if reflow_needed && self.should_mark_reflow_as_stream_time() {
                self.transcript_reflow.mark_resize_requested_during_stream();
            }
            let target_width = reflow_needed.then_some(size.width);
            if self.schedule_resize_reflow(target_width) {
                frame_requester.schedule_frame();
            } else {
                frame_requester.schedule_frame_in(TRANSCRIPT_REFLOW_DEBOUNCE);
            }
        }
        if size != last_known_screen_size {
            self.refresh_status_line();
        }
        self.maybe_clear_resize_reflow_without_terminal();
        should_rebuild_transcript
    }

    fn maybe_clear_resize_reflow_without_terminal(&mut self) {
        let Some(deadline) = self.transcript_reflow.pending_until() else {
            return;
        };
        if Instant::now() < deadline || self.overlay.is_some() || !self.transcript_cells.is_empty()
        {
            return;
        }

        self.transcript_reflow.clear_pending_reflow();
        self.reset_history_emission_state();
    }

    pub(super) fn handle_draw_pre_render(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let size = tui.terminal.size()?;
        let should_rebuild_transcript = self.handle_draw_size_change(
            size,
            tui.terminal.last_known_screen_size,
            &tui.frame_requester(),
        );
        if should_rebuild_transcript {
            tui.clear_pending_history_lines();
        }
        self.maybe_run_resize_reflow(tui)?;
        Ok(())
    }

    pub(super) fn maybe_run_resize_reflow(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let Some(deadline) = self.transcript_reflow.pending_until() else {
            return Ok(());
        };
        let now = Instant::now();
        if now < deadline {
            tui.frame_requester().schedule_frame_in(deadline - now);
            return Ok(());
        }
        if self.overlay.is_some() {
            return Ok(());
        }

        self.transcript_reflow.clear_pending_reflow();

        let reflow_ran_during_stream =
            !self.transcript_cells.is_empty() && self.should_mark_reflow_as_stream_time();

        let width = self.reflow_transcript_now(tui)?;
        self.transcript_reflow.mark_reflowed_width(width);

        if reflow_ran_during_stream {
            self.transcript_reflow.mark_ran_during_stream();
        }
        tui.frame_requester()
            .schedule_frame_in(TRANSCRIPT_REFLOW_DEBOUNCE);

        Ok(())
    }

    pub(super) fn reflow_transcript_now(&mut self, tui: &mut tui::Tui) -> Result<u16> {
        let terminal_width = tui.terminal.size()?.width;
        let width = self.chat_widget.history_wrap_width(terminal_width);
        if self.transcript_cells.is_empty() {
            tui.clear_pending_history_lines();
            self.reset_history_emission_state();
            return Ok(terminal_width);
        }

        let reflow_result = self.render_transcript_lines_for_reflow(width);
        let reflowed_lines = reflow_result.lines;

        tui.clear_pending_history_lines();
        self.clear_terminal_for_resize_replay(tui)?;

        self.deferred_history_lines.clear();
        if !reflowed_lines.is_empty() {
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                reflowed_lines,
                self.history_line_wrap_policy(),
            );
        }

        Ok(terminal_width)
    }

    pub(super) fn rebuild_transcript_after_backtrack(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let terminal_width = tui.terminal.size()?.width;
        let width = self.chat_widget.history_wrap_width(terminal_width);
        let reflowed_lines = if self.transcript_cells.is_empty() {
            self.reset_history_emission_state();
            Vec::new()
        } else {
            self.render_transcript_lines_for_reflow(width).lines
        };

        tui.clear_pending_history_lines();
        self.clear_terminal_for_resize_replay(tui)?;

        self.deferred_history_lines.clear();
        if !reflowed_lines.is_empty() {
            tui.insert_history_hyperlink_lines_with_wrap_policy(
                reflowed_lines,
                self.history_line_wrap_policy(),
            );
        }

        Ok(())
    }

    pub(super) fn render_transcript_lines_for_reflow(&mut self, width: u16) -> ReflowRenderResult {
        let row_cap = self.resize_reflow_max_rows();
        let mut cell_displays = VecDeque::new();
        let mut rendered_rows = 0usize;
        let mut start = self.transcript_cells.len();

        while start > 0 {
            start -= 1;
            let cell = self.transcript_cells[start].clone();
            let lines = cell
                .display_hyperlink_lines_for_mode(width, self.chat_widget.history_render_mode());
            rendered_rows += lines.len();
            cell_displays.push_front(ReflowCellDisplay {
                lines,
                is_stream_continuation: cell.is_stream_continuation(),
            });

            if row_cap.is_some_and(|max_rows| rendered_rows > max_rows) {
                break;
            }
        }

        while start > 0
            && cell_displays
                .front()
                .is_some_and(|display| display.is_stream_continuation)
        {
            start -= 1;
            let cell = self.transcript_cells[start].clone();
            cell_displays.push_front(ReflowCellDisplay {
                lines: cell.display_hyperlink_lines_for_mode(
                    width,
                    self.chat_widget.history_render_mode(),
                ),
                is_stream_continuation: cell.is_stream_continuation(),
            });
        }

        let mut has_emitted_history_lines = false;
        let mut reflowed_lines = Vec::new();
        for display in cell_displays {
            if !display.lines.is_empty() && !display.is_stream_continuation {
                if has_emitted_history_lines {
                    reflowed_lines.push(HyperlinkLine::new(Line::from("")));
                } else {
                    has_emitted_history_lines = true;
                }
            }
            reflowed_lines.extend(display.lines);
        }
        if let Some(max_rows) = row_cap
            && reflowed_lines.len() > max_rows
        {
            let trimmed_line_count = reflowed_lines.len() - max_rows;
            reflowed_lines = reflowed_lines.split_off(trimmed_line_count);
        }
        self.has_emitted_history_lines = !reflowed_lines.is_empty();

        ReflowRenderResult {
            lines: reflowed_lines,
        }
    }

    pub(super) fn should_mark_reflow_as_stream_time(&self) -> bool {
        self.chat_widget.has_active_agent_stream()
            || self.chat_widget.has_active_plan_stream()
            || trailing_run_start::<history_cell::AgentMessageCell>(&self.transcript_cells)
                < self.transcript_cells.len()
            || trailing_run_start::<history_cell::ProposedPlanStreamCell>(&self.transcript_cells)
                < self.transcript_cells.len()
    }
}
