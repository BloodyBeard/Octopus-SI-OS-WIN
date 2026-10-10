use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use crate::markdown_stream::MarkdownStreamCollector;
use crate::terminal_hyperlinks::HyperlinkLine;
pub(crate) mod chunking;
pub(crate) mod commit_tick;
pub(crate) mod controller;
mod table_holdback;

struct QueuedLine {
    line: HyperlinkLine,
    enqueued_at: Instant,
}

pub(crate) struct StreamState {
    pub(crate) collector: MarkdownStreamCollector,
    queued_lines: VecDeque<QueuedLine>,
    pub(crate) has_seen_delta: bool,
}

impl StreamState {
    pub(crate) fn new(width: Option<usize>, cwd: &Path) -> Self {
        Self {
            collector: MarkdownStreamCollector::new(width, cwd),
            queued_lines: VecDeque::new(),
            has_seen_delta: false,
        }
    }
    pub(crate) fn clear(&mut self) {
        self.collector.clear();
        self.queued_lines.clear();
        self.has_seen_delta = false;
    }
    pub(crate) fn step(&mut self) -> Vec<HyperlinkLine> {
        self.queued_lines
            .pop_front()
            .map(|queued| queued.line)
            .into_iter()
            .collect()
    }
    pub(crate) fn drain_n(&mut self, max_lines: usize) -> Vec<HyperlinkLine> {
        let end = max_lines.min(self.queued_lines.len());
        self.queued_lines
            .drain(..end)
            .map(|queued| queued.line)
            .collect()
    }
    pub(crate) fn clear_queue(&mut self) {
        self.queued_lines.clear();
    }
    pub(crate) fn is_idle(&self) -> bool {
        self.queued_lines.is_empty()
    }
    pub(crate) fn queued_len(&self) -> usize {
        self.queued_lines.len()
    }
    pub(crate) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.queued_lines
            .front()
            .map(|queued| now.saturating_duration_since(queued.enqueued_at))
    }
    pub(crate) fn enqueue(&mut self, lines: Vec<HyperlinkLine>) {
        let now = Instant::now();
        self.queued_lines
            .extend(lines.into_iter().map(|line| QueuedLine {
                line,
                enqueued_at: now,
            }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::text::Line;
    use std::path::PathBuf;

    fn test_cwd() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn drain_n_clamps_to_available_lines() {
        let mut state = StreamState::new(None, &test_cwd());
        state.enqueue(vec![HyperlinkLine::new(Line::from("one"))]);

        let drained = state.drain_n(8);
        assert_eq!(drained, vec![HyperlinkLine::new(Line::from("one"))]);
        assert!(state.is_idle());
    }
}
