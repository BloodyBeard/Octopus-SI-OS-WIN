use std::time::Duration;
use std::time::Instant;

pub const TRANSCRIPT_REFLOW_DEBOUNCE: Duration = Duration::from_millis(75);

#[derive(Debug, Default)]
pub struct TranscriptReflowState {
    last_observed_width: Option<u16>,
    last_reflow_width: Option<u16>,
    pending_reflow_width: Option<u16>,
    pending_until: Option<Instant>,
    ran_during_stream: bool,
    resize_requested_during_stream: bool,
}

impl TranscriptReflowState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn note_width(&mut self, width: u16) -> TranscriptWidthChange {
        let previous_width = self.last_observed_width.replace(width);
        if previous_width.is_none() {
            self.last_reflow_width = Some(width);
        }
        TranscriptWidthChange {
            changed: previous_width.is_some_and(|previous| previous != width),
            initialized: previous_width.is_none(),
        }
    }

    pub fn reflow_needed_for_width(&self, width: u16) -> bool {
        self.last_reflow_width != Some(width) && self.pending_reflow_width != Some(width)
    }

    pub fn schedule_debounced(&mut self, target_width: Option<u16>) -> bool {
        let now = Instant::now();
        if let Some(target_width) = target_width {
            self.pending_reflow_width = Some(target_width);
        }
        self.pending_until = Some(now + TRANSCRIPT_REFLOW_DEBOUNCE);
        false
    }

    pub fn schedule_immediate(&mut self) {
        self.pending_reflow_width = None;
        self.pending_until = Some(Instant::now());
    }

    #[cfg(test)]
    pub fn set_due_for_test(&mut self) {
        self.pending_until = Some(Instant::now() - Duration::from_millis(1));
    }

    pub fn pending_is_due(&self, now: Instant) -> bool {
        self.pending_until.is_some_and(|deadline| now >= deadline)
    }

    pub fn pending_until(&self) -> Option<Instant> {
        self.pending_until
    }

    pub fn has_pending_reflow(&self) -> bool {
        self.pending_until.is_some()
    }

    pub fn clear_pending_reflow(&mut self) {
        self.pending_until = None;
        self.pending_reflow_width = None;
    }

    pub fn mark_reflowed_width(&mut self, width: u16) -> bool {
        self.last_reflow_width.replace(width) != Some(width)
    }

    pub fn mark_ran_during_stream(&mut self) {
        self.ran_during_stream = true;
    }

    pub fn mark_resize_requested_during_stream(&mut self) {
        self.resize_requested_during_stream = true;
    }

    pub fn take_stream_finish_reflow_needed(&mut self) -> bool {
        let needed = self.ran_during_stream || self.resize_requested_during_stream;
        self.ran_during_stream = false;
        self.resize_requested_during_stream = false;
        needed
    }

    pub fn clear_stream_flags(&mut self) {
        self.ran_during_stream = false;
        self.resize_requested_during_stream = false;
    }
}

pub struct TranscriptWidthChange {
    pub changed: bool,
    pub initialized: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_debounced_postpones_existing_reflow() {
        let mut state = TranscriptReflowState::default();

        assert!(!state.schedule_debounced(None));
        let first_deadline = state.pending_until().expect("pending reflow");

        std::thread::sleep(Duration::from_millis(1));
        assert!(!state.schedule_debounced(None));

        assert!(
            state.pending_until().expect("pending reflow") > first_deadline,
            "a later resize should push the debounce deadline out"
        );
    }

    #[test]
    fn schedule_debounced_postpones_due_existing_reflow() {
        let mut state = TranscriptReflowState::default();
        state.set_due_for_test();
        let before_reschedule = Instant::now();

        assert!(!state.schedule_debounced(None));
        assert!(
            state.pending_until().expect("pending reflow") > before_reschedule,
            "a resize after the old deadline should start a fresh quiet period"
        );
    }

    #[test]
    fn first_observed_width_marks_reflow_baseline() {
        let mut state = TranscriptReflowState::default();

        let width = state.note_width(80);

        assert!(width.initialized);
        assert_eq!(state.last_observed_width, Some(80));
        assert_eq!(state.last_reflow_width, Some(80));
        assert!(!state.reflow_needed_for_width(80));
    }

    #[test]
    fn mark_reflowed_width_records_actual_rebuild_width() {
        let mut state = TranscriptReflowState::default();
        state.note_width(80);

        assert!(state.mark_reflowed_width(100));

        assert_eq!(state.last_observed_width, Some(80));
        assert_eq!(state.last_reflow_width, Some(100));
    }

    #[test]
    fn reflow_needed_compares_against_actual_rebuild_width() {
        let mut state = TranscriptReflowState::default();
        state.note_width(80);
        state.mark_reflowed_width(90);
        state.note_width(100);

        assert!(state.reflow_needed_for_width(100));
    }

    #[test]
    fn pending_reflow_target_prevents_repeated_reschedule() {
        let mut state = TranscriptReflowState::default();
        state.note_width(80);

        assert!(state.reflow_needed_for_width(100));
        state.schedule_debounced(Some(100));

        assert!(!state.reflow_needed_for_width(100));
    }

    #[test]
    fn clear_pending_reflow_allows_same_width_to_be_rescheduled() {
        let mut state = TranscriptReflowState::default();
        state.note_width(80);
        state.schedule_debounced(Some(100));

        state.clear_pending_reflow();

        assert!(state.reflow_needed_for_width(100));
    }

    #[test]
    fn mark_reflowed_width_reports_unchanged_width() {
        let mut state = TranscriptReflowState::default();
        assert!(state.mark_reflowed_width(100));

        assert!(!state.mark_reflowed_width(100));
        assert_eq!(state.last_reflow_width, Some(100));
    }

    #[test]
    fn take_stream_finish_reflow_needed_drains_resize_request() {
        let mut state = TranscriptReflowState::default();
        state.mark_resize_requested_during_stream();

        assert!(state.take_stream_finish_reflow_needed());
        assert!(!state.take_stream_finish_reflow_needed());
    }

    #[test]
    fn take_stream_finish_reflow_needed_drains_ran_during_stream() {
        let mut state = TranscriptReflowState::default();
        state.mark_ran_during_stream();

        assert!(state.take_stream_finish_reflow_needed());
        assert!(!state.take_stream_finish_reflow_needed());
    }

    #[test]
    fn clear_resets_stream_reflow_flags() {
        let mut state = TranscriptReflowState::default();
        state.mark_ran_during_stream();
        state.mark_resize_requested_during_stream();

        state.clear();

        assert!(!state.take_stream_finish_reflow_needed());
    }
}
