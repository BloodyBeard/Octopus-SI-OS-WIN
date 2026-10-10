use super::HistoryCell;
use super::MAX_AGENT_COPY_HISTORY;

#[derive(Debug)]
pub(super) struct AgentTurnMarkdown {
    pub(super) user_turn_count: usize,
    pub(super) markdown: String,
}

#[derive(Default)]
pub(super) struct TranscriptState {
    pub(super) active_cell: Option<Box<dyn HistoryCell>>,
    pub(super) active_cell_revision: u64,
    pub(super) last_agent_markdown: Option<String>,
    pub(super) agent_turn_markdowns: Vec<AgentTurnMarkdown>,
    pub(super) visible_user_turn_count: usize,
    pub(super) copy_history_evicted_by_rollback: bool,
    pub(super) latest_proposed_plan_markdown: Option<String>,
    pub(super) saw_copy_source_this_turn: bool,
    pub(super) needs_final_message_separator: bool,
    pub(super) had_work_activity: bool,
    pub(super) saw_plan_update_this_turn: bool,
    pub(super) saw_plan_item_this_turn: bool,
    pub(super) last_plan_progress: Option<(usize, usize)>,
    pub(super) plan_delta_buffer: String,
    pub(super) plan_item_active: bool,
}

impl TranscriptState {
    pub(super) fn new(active_cell: Option<Box<dyn HistoryCell>>) -> Self {
        Self {
            active_cell,
            ..Self::default()
        }
    }

    pub(super) fn bump_active_cell_revision(&mut self) {
        self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
    }

    pub(super) fn record_agent_markdown(&mut self, markdown: String) {
        match self.agent_turn_markdowns.last_mut() {
            Some(entry) if entry.user_turn_count == self.visible_user_turn_count => {
                entry.markdown = markdown.clone();
            }
            _ => {
                self.agent_turn_markdowns.push(AgentTurnMarkdown {
                    user_turn_count: self.visible_user_turn_count,
                    markdown: markdown.clone(),
                });
                if self.agent_turn_markdowns.len() > MAX_AGENT_COPY_HISTORY {
                    self.agent_turn_markdowns.remove(0);
                }
            }
        }
        self.last_agent_markdown = Some(markdown);
        self.copy_history_evicted_by_rollback = false;
        self.saw_copy_source_this_turn = true;
    }

    pub(super) fn record_visible_user_turn(&mut self) {
        self.visible_user_turn_count = self.visible_user_turn_count.saturating_add(1);
    }

    pub(super) fn reset_copy_history(&mut self) {
        self.last_agent_markdown = None;
        self.agent_turn_markdowns.clear();
        self.visible_user_turn_count = 0;
        self.copy_history_evicted_by_rollback = false;
        self.saw_copy_source_this_turn = false;
    }

    pub(super) fn truncate_copy_history_to_user_turn_count(&mut self, user_turn_count: usize) {
        self.visible_user_turn_count = user_turn_count;
        let had_copy_history = !self.agent_turn_markdowns.is_empty();
        self.agent_turn_markdowns
            .retain(|entry| entry.user_turn_count <= user_turn_count);
        self.last_agent_markdown = self
            .agent_turn_markdowns
            .last()
            .map(|entry| entry.markdown.clone());
        self.copy_history_evicted_by_rollback =
            user_turn_count > 0 && had_copy_history && self.last_agent_markdown.is_none();
        self.saw_copy_source_this_turn = false;
    }

    pub(super) fn reset_turn_flags(&mut self) {
        self.saw_copy_source_this_turn = false;
        self.saw_plan_update_this_turn = false;
        self.saw_plan_item_this_turn = false;
        self.had_work_activity = false;
        self.latest_proposed_plan_markdown = None;
        self.plan_delta_buffer.clear();
        self.plan_item_active = false;
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn active_cell_revision_wraps() {
        let mut state = TranscriptState {
            active_cell_revision: u64::MAX,
            ..TranscriptState::default()
        };

        state.bump_active_cell_revision();

        assert_eq!(state.active_cell_revision, 0);
    }

    #[test]
    fn copy_history_tracks_latest_visible_turn() {
        let mut state = TranscriptState::default();
        state.record_visible_user_turn();
        state.record_agent_markdown("first".to_string());
        state.record_visible_user_turn();
        state.record_agent_markdown("second".to_string());

        state.truncate_copy_history_to_user_turn_count(1);

        assert_eq!(state.last_agent_markdown.as_deref(), Some("first"));
        assert!(!state.copy_history_evicted_by_rollback);
    }
}
