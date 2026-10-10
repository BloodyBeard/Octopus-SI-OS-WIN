use super::HistoryCell;
use super::plain_lines;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::motion::shimmer_text;
use crate::render::renderable::Renderable;
use octopus_world_terminal_client::HookItemStatus;
use octopus_world_terminal_client::HookOutputItem;
use octopus_world_terminal_client::HookOutputKind;
use octopus_world_terminal_client::HookRunItem;
use ratatui::prelude::*;
use ratatui::style::Stylize;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::time::Duration;
use std::time::Instant;

#[derive(Debug)]
pub(crate) struct HookCell {
    runs: Vec<HookRunCell>,
    animations_enabled: bool,
}

const HOOK_RUN_REVEAL_DELAY: Duration = Duration::from_millis(300);

const QUIET_HOOK_MIN_VISIBLE: Duration = Duration::from_millis(600);

const HOOK_OUTPUT_INDENT: &str = "  ";
const HOOK_OUTPUT_BODY_INDENT: &str = "    ";

#[derive(Debug)]
struct HookRunCell {
    id: String,
    event: String,
    status_message: Option<String>,
    state: HookRunState,
}

#[derive(Debug)]
enum HookRunState {
    PendingReveal {
        start_time: Instant,
        reveal_deadline: Instant,
    },
    VisibleRunning {
        start_time: Instant,
        visible_since: Instant,
    },
    QuietLinger {
        start_time: Instant,
        removal_deadline: Instant,
    },
    Completed {
        status: HookItemStatus,
        entries: Vec<HookOutputItem>,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct RunningHookGroupKey {
    event: String,
    status_message: Option<String>,
}

struct RunningHookGroup {
    key: RunningHookGroupKey,
    start_time: Option<Instant>,
    count: usize,
}

impl HookCell {
    fn new_active(run: HookRunItem, animations_enabled: bool) -> Self {
        let mut cell = Self {
            runs: Vec::new(),
            animations_enabled,
        };
        cell.start_run(run);
        cell
    }

    fn new_completed(run: HookRunItem, animations_enabled: bool) -> Self {
        let mut cell = Self {
            runs: Vec::new(),
            animations_enabled,
        };
        cell.add_completed_run(run);
        cell
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.runs.iter().any(|run| run.state.is_active())
    }

    pub(crate) fn should_flush(&self) -> bool {
        !self.is_active() && !self.is_empty()
    }

    pub(crate) fn should_render(&self) -> bool {
        self.runs.iter().any(|run| run.state.should_render())
    }

    pub(crate) fn take_completed_persistent_runs(&mut self) -> Option<Self> {
        let mut completed = Vec::new();
        let mut remaining = Vec::new();
        for run in self.runs.drain(..) {
            if run.state.has_persistent_output() {
                completed.push(run);
            } else {
                remaining.push(run);
            }
        }
        self.runs = remaining;
        (!completed.is_empty()).then_some(Self {
            runs: completed,
            animations_enabled: self.animations_enabled,
        })
    }

    pub(crate) fn has_visible_running_run(&self) -> bool {
        self.runs.iter().any(|run| run.state.is_running_visible())
    }

    pub(crate) fn advance_time(&mut self, now: Instant) -> bool {
        let old_len = self.runs.len();
        let mut changed = false;
        for run in &mut self.runs {
            changed |= run.state.reveal_if_due(now);
        }
        self.runs.retain(|run| !run.state.quiet_linger_expired(now));
        changed || self.runs.len() != old_len
    }

    pub(crate) fn start_run(&mut self, run: HookRunItem) {
        let now = Instant::now();
        if let Some(existing) = self.runs.iter_mut().find(|existing| existing.id == run.id) {
            existing.event = run.event;
            existing.status_message = run.status_message;
            existing.state = HookRunState::pending(now);
            return;
        }
        self.runs.push(HookRunCell {
            id: run.id,
            event: run.event,
            status_message: run.status_message,
            state: HookRunState::pending(now),
        });
    }

    pub(crate) fn complete_run(&mut self, run: HookRunItem) -> bool {
        let Some(index) = self.runs.iter().position(|existing| existing.id == run.id) else {
            return false;
        };
        if hook_run_is_quiet_success(&run) {
            if !self.runs[index]
                .state
                .complete_quiet_success(Instant::now())
            {
                self.runs.remove(index);
            }
            return true;
        }
        let HookRunItem {
            event,
            status_message,
            status,
            outputs,
            ..
        } = run;
        let existing = &mut self.runs[index];
        existing.event = event;
        existing.status_message = status_message;
        existing.state = HookRunState::completed(status, outputs);
        true
    }

    pub(crate) fn add_completed_run(&mut self, run: HookRunItem) {
        if hook_run_is_quiet_success(&run) {
            return;
        }
        let HookRunItem {
            id,
            event,
            status_message,
            status,
            outputs,
            ..
        } = run;
        self.runs.push(HookRunCell {
            id,
            event,
            status_message,
            state: HookRunState::completed(status, outputs),
        });
    }

    pub(crate) fn next_timer_deadline(&self) -> Option<Instant> {
        self.runs
            .iter()
            .filter_map(|run| run.state.next_timer_deadline())
            .min()
    }

    #[cfg(test)]
    pub(crate) fn expire_quiet_runs_now_for_test(&mut self) {
        for run in &mut self.runs {
            run.expire_quiet_linger_now_for_test();
        }
    }

    #[cfg(test)]
    pub(crate) fn reveal_running_runs_now_for_test(&mut self) {
        let now = Instant::now();
        for run in &mut self.runs {
            run.reveal_running_now_for_test(now);
        }
    }

    #[cfg(test)]
    pub(crate) fn reveal_running_runs_after_delayed_redraw_for_test(&mut self) {
        let now = Instant::now();
        for run in &mut self.runs {
            run.reveal_running_after_delayed_redraw_for_test(now);
        }
    }
}

impl HistoryCell for HookCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut running_group: Option<RunningHookGroup> = None;
        for run in &self.runs {
            if !run.state.should_render() {
                continue;
            }

            let Some(key) = run.running_group_key() else {
                if let Some(group) = running_group.take() {
                    push_running_hook_group(&mut lines, &group, self.animations_enabled);
                }
                push_hook_line_separator(&mut lines);
                run.push_display_lines(&mut lines, self.animations_enabled);
                continue;
            };

            if let Some(group) = running_group.as_mut()
                && group.key == key
            {
                group.count += 1;
                group.start_time = earliest_instant(group.start_time, run.state.start_time());
                continue;
            }

            if let Some(group) =
                running_group.replace(RunningHookGroup::new(key, run.state.start_time()))
            {
                push_running_hook_group(&mut lines, &group, self.animations_enabled);
            }
        }
        if let Some(group) = running_group {
            push_running_hook_group(&mut lines, &group, self.animations_enabled);
        }
        lines
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if !self.animations_enabled {
            return None;
        }
        let elapsed = self
            .runs
            .iter()
            .filter(|run| run.state.is_running_visible())
            .find_map(|run| run.state.start_time())?
            .elapsed();
        Some(elapsed.as_millis() as u64 / 600)
    }
}

impl Renderable for HookCell {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.display_lines(area.width);
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        paragraph.render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        HistoryCell::desired_height(self, width)
    }
}

impl HookRunCell {
    #[cfg(test)]
    fn expire_quiet_linger_now_for_test(&mut self) {
        if let HookRunState::QuietLinger {
            removal_deadline, ..
        } = &mut self.state
        {
            *removal_deadline = Instant::now();
        }
    }

    #[cfg(test)]
    fn reveal_running_now_for_test(&mut self, now: Instant) {
        if let HookRunState::PendingReveal {
            reveal_deadline, ..
        } = &mut self.state
        {
            *reveal_deadline = now;
        }
    }

    #[cfg(test)]
    fn reveal_running_after_delayed_redraw_for_test(&mut self, now: Instant) {
        if let HookRunState::PendingReveal {
            reveal_deadline, ..
        } = &mut self.state
        {
            let delayed_deadline = now
                .checked_sub(QUIET_HOOK_MIN_VISIBLE + Duration::from_millis(100))
                .unwrap_or(now);
            *reveal_deadline = delayed_deadline;
        }
    }

    fn running_group_key(&self) -> Option<RunningHookGroupKey> {
        self.state
            .is_running_visible()
            .then(|| RunningHookGroupKey {
                event: self.event.clone(),
                status_message: self.status_message.clone(),
            })
    }

    fn push_display_lines(&self, lines: &mut Vec<Line<'static>>, animations_enabled: bool) {
        let label = self.event.as_str();
        match &self.state {
            HookRunState::VisibleRunning { start_time, .. }
            | HookRunState::QuietLinger { start_time, .. } => {
                let hook_text = format!("Running {label} hook");
                push_running_hook_header(
                    lines,
                    &hook_text,
                    Some(*start_time),
                    self.status_message.as_deref(),
                    animations_enabled,
                );
            }
            HookRunState::Completed { status, entries } => {
                let status_text = status.label();
                let bullet = hook_completed_bullet(*status, entries);
                lines.push(
                    vec![
                        bullet,
                        " ".into(),
                        format!("{label} hook ({status_text})").into(),
                    ]
                    .into(),
                );
                for entry in entries {
                    let prefix = hook_output_prefix(entry.kind);
                    let mut output_lines = entry.text.split('\n');
                    if let Some(first_line) = output_lines.next() {
                        lines.push(format!("{HOOK_OUTPUT_INDENT}{prefix}{first_line}").into());
                    }
                    for line in output_lines {
                        if line.is_empty() {
                            lines.push("".into());
                        } else {
                            lines.push(format!("{HOOK_OUTPUT_BODY_INDENT}{line}").into());
                        }
                    }
                }
            }
            HookRunState::PendingReveal { .. } => {}
        }
    }
}

impl HookRunState {
    fn pending(start_time: Instant) -> Self {
        Self::PendingReveal {
            start_time,
            reveal_deadline: start_time + HOOK_RUN_REVEAL_DELAY,
        }
    }

    fn completed(status: HookItemStatus, entries: Vec<HookOutputItem>) -> Self {
        Self::Completed { status, entries }
    }

    fn is_active(&self) -> bool {
        match self {
            HookRunState::PendingReveal { .. }
            | HookRunState::VisibleRunning { .. }
            | HookRunState::QuietLinger { .. } => true,
            HookRunState::Completed { .. } => false,
        }
    }

    fn should_render(&self) -> bool {
        match self {
            HookRunState::VisibleRunning { .. }
            | HookRunState::QuietLinger { .. }
            | HookRunState::Completed { .. } => true,
            HookRunState::PendingReveal { .. } => false,
        }
    }

    fn has_persistent_output(&self) -> bool {
        match self {
            HookRunState::Completed { status, entries } => {
                *status != HookItemStatus::Completed || !entries.is_empty()
            }
            HookRunState::PendingReveal { .. }
            | HookRunState::VisibleRunning { .. }
            | HookRunState::QuietLinger { .. } => false,
        }
    }

    fn start_time(&self) -> Option<Instant> {
        match self {
            HookRunState::PendingReveal { start_time, .. }
            | HookRunState::VisibleRunning { start_time, .. }
            | HookRunState::QuietLinger { start_time, .. } => Some(*start_time),
            HookRunState::Completed { .. } => None,
        }
    }

    fn is_running_visible(&self) -> bool {
        matches!(
            self,
            HookRunState::VisibleRunning { .. } | HookRunState::QuietLinger { .. }
        )
    }

    fn reveal_if_due(&mut self, now: Instant) -> bool {
        let HookRunState::PendingReveal {
            start_time,
            reveal_deadline,
        } = self
        else {
            return false;
        };
        if now < *reveal_deadline {
            return false;
        }
        *self = HookRunState::VisibleRunning {
            start_time: *start_time,
            visible_since: now,
        };
        true
    }

    fn next_timer_deadline(&self) -> Option<Instant> {
        match self {
            HookRunState::PendingReveal {
                reveal_deadline, ..
            } => Some(*reveal_deadline),
            HookRunState::QuietLinger {
                removal_deadline, ..
            } => Some(*removal_deadline),
            HookRunState::VisibleRunning { .. } | HookRunState::Completed { .. } => None,
        }
    }

    fn quiet_linger_expired(&self, now: Instant) -> bool {
        match self {
            HookRunState::QuietLinger {
                removal_deadline, ..
            } => now >= *removal_deadline,
            HookRunState::PendingReveal { .. }
            | HookRunState::VisibleRunning { .. }
            | HookRunState::Completed { .. } => false,
        }
    }

    fn complete_quiet_success(&mut self, now: Instant) -> bool {
        let HookRunState::VisibleRunning {
            start_time,
            visible_since,
            ..
        } = self
        else {
            return false;
        };
        let start_time = *start_time;
        let minimum_deadline = *visible_since + QUIET_HOOK_MIN_VISIBLE;
        if now >= minimum_deadline {
            return false;
        }
        *self = HookRunState::QuietLinger {
            start_time,
            removal_deadline: minimum_deadline,
        };
        true
    }
}

impl RunningHookGroup {
    fn new(key: RunningHookGroupKey, start_time: Option<Instant>) -> Self {
        Self {
            key,
            start_time,
            count: 1,
        }
    }
}

fn push_running_hook_group(
    lines: &mut Vec<Line<'static>>,
    group: &RunningHookGroup,
    animations_enabled: bool,
) {
    push_hook_line_separator(lines);
    let label = group.key.event.as_str();
    let hook_text = if group.count == 1 {
        format!("Running {label} hook")
    } else {
        format!("Running {} {label} hooks", group.count)
    };
    push_running_hook_header(
        lines,
        &hook_text,
        group.start_time,
        group.key.status_message.as_deref(),
        animations_enabled,
    );
}

fn push_running_hook_header(
    lines: &mut Vec<Line<'static>>,
    hook_text: &str,
    start_time: Option<Instant>,
    status_message: Option<&str>,
    animations_enabled: bool,
) {
    let mut header = Vec::new();
    let motion_mode = MotionMode::from_animations_enabled(animations_enabled);
    if let Some(indicator) =
        activity_indicator(start_time, motion_mode, ReducedMotionIndicator::Hidden)
    {
        header.push(indicator);
        header.push(" ".into());
    }
    header.extend(shimmer_text(hook_text, motion_mode));
    if !animations_enabled && let Some(span) = header.last_mut() {
        span.style = span.style.patch(Style::default().bold());
    }
    if let Some(status_message) = status_message
        && !status_message.is_empty()
    {
        header.push(": ".into());
        header.push(status_message.to_string().dim());
    }
    lines.push(header.into());
}

fn push_hook_line_separator(lines: &mut Vec<Line<'static>>) {
    if !lines.is_empty() {
        lines.push("".into());
    }
}

fn earliest_instant(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

pub(crate) fn new_active_hook_cell(run: HookRunItem, animations_enabled: bool) -> HookCell {
    HookCell::new_active(run, animations_enabled)
}

pub(crate) fn new_completed_hook_cell(run: HookRunItem, animations_enabled: bool) -> HookCell {
    HookCell::new_completed(run, animations_enabled)
}

fn hook_run_is_quiet_success(run: &HookRunItem) -> bool {
    run.status == HookItemStatus::Completed && run.outputs.is_empty()
}

fn hook_completed_bullet(status: HookItemStatus, entries: &[HookOutputItem]) -> Span<'static> {
    match status {
        HookItemStatus::Completed => {
            if entries
                .iter()
                .any(|entry| entry.kind == HookOutputKind::Warning)
            {
                "•".bold()
            } else {
                "•".green().bold()
            }
        }
        HookItemStatus::Blocked | HookItemStatus::Failed | HookItemStatus::Stopped => {
            "•".red().bold()
        }
        HookItemStatus::Running => "•".into(),
    }
}

fn hook_output_prefix(kind: HookOutputKind) -> &'static str {
    match kind {
        HookOutputKind::Warning => "warning: ",
        HookOutputKind::Stop => "stop: ",
        HookOutputKind::Feedback => "feedback: ",
        HookOutputKind::Context => "hook context: ",
        HookOutputKind::Error => "error: ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::style::Modifier;

    #[test]
    fn completed_hook_with_warning_uses_default_bold_bullet() {
        let entries = vec![HookOutputItem {
            kind: HookOutputKind::Warning,
            text: "Heads up from the hook".to_string(),
        }];

        let bullet = hook_completed_bullet(HookItemStatus::Completed, &entries);

        assert_eq!(bullet.content.as_ref(), "•");
        assert_eq!(bullet.style.fg, None);
        assert!(bullet.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn completed_hook_multiline_context_preserves_display_and_raw_lines() {
        let cell = completed_hook_cell(
            "SessionStart",
            HookItemStatus::Completed,
            vec![HookOutputItem {
                kind: HookOutputKind::Context,
                text: "## Working Memory Recall\n\nSource: Codex compaction\nScope: Durable workspace memory"
                    .to_string(),
            }],
        );
        let expected = vec![
            "• SessionStart hook (completed)".to_string(),
            "  hook context: ## Working Memory Recall".to_string(),
            "".to_string(),
            "    Source: Codex compaction".to_string(),
            "    Scope: Durable workspace memory".to_string(),
        ];

        assert_eq!(line_texts(&cell.display_lines(80)), expected);
        assert_eq!(line_texts(&cell.raw_lines()), expected);
    }

    #[test]
    fn completed_hook_multiline_warning_prefixes_first_line_only() {
        let cell = completed_hook_cell(
            "PostToolUse",
            HookItemStatus::Completed,
            vec![HookOutputItem {
                kind: HookOutputKind::Warning,
                text: "Heads up\nReview generated files".to_string(),
            }],
        );

        assert_eq!(
            line_texts(&cell.display_lines(80)),
            vec![
                "• PostToolUse hook (completed)".to_string(),
                "  warning: Heads up".to_string(),
                "    Review generated files".to_string(),
            ]
        );
    }

    #[test]
    fn pending_hook_does_not_animate_transcript() {
        let cell = HookCell::new_active(hook_run_summary("hook-1"), true);

        assert_eq!(cell.transcript_animation_tick(), None);
    }

    #[test]
    fn visible_hook_animates_transcript_when_animations_enabled() {
        let mut cell = HookCell::new_active(hook_run_summary("hook-1"), true);
        cell.reveal_running_runs_now_for_test();
        cell.advance_time(Instant::now());

        assert_eq!(cell.transcript_animation_tick(), Some(0));
    }

    #[test]
    fn visible_hook_does_not_animate_transcript_when_animations_disabled() {
        let mut cell = HookCell::new_active(hook_run_summary("hook-1"), false);
        cell.reveal_running_runs_now_for_test();
        cell.advance_time(Instant::now());

        assert_eq!(cell.transcript_animation_tick(), None);
    }

    #[test]
    fn visible_hook_without_animations_omits_spinner() {
        let mut cell = HookCell::new_active(hook_run_summary("hook-1"), false);
        cell.reveal_running_runs_now_for_test();
        cell.advance_time(Instant::now());

        let rendered: Vec<String> = cell.display_lines(80).iter().map(line_text).collect();

        assert_eq!(
            rendered,
            vec!["Running PostToolUse hook: checking output policy".to_string()]
        );
    }

    fn completed_hook_cell(
        event: &str,
        status: HookItemStatus,
        outputs: Vec<HookOutputItem>,
    ) -> HookCell {
        let mut run = hook_run_summary("hook-1");
        run.event = event.to_string();
        run.status = status;
        run.status_message = None;
        run.outputs = outputs;
        HookCell::new_completed(run, false)
    }

    fn line_texts(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(line_text).collect()
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn hook_run_summary(id: &str) -> HookRunItem {
        HookRunItem {
            id: id.to_string(),
            event: "PostToolUse".to_string(),
            status: HookItemStatus::Running,
            status_message: Some("checking output policy".to_string()),
            outputs: Vec::new(),
        }
    }
}
