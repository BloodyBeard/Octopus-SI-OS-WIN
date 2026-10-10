use std::collections::BTreeMap;
use std::io::Result;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use octopus_ledger::Movement;
use octopus_ledger::MovementKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use crate::token_usage::TokenUsage;

#[derive(Clone, Debug, Default)]
struct MetricsProjection {
    total_movements: usize,
    kind_counts: Vec<(String, usize, u64)>,
    arrivals: usize,
    outbound: usize,
    artifact_moves: usize,
    admitted_mark_outcomes: usize,
    runs: usize,
    ees: usize,
    tokens: TokenUsage,
}

impl MetricsProjection {
    fn project(movements: &[Movement], tokens: TokenUsage) -> Self {
        let mut runs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut ees: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut arrivals = 0usize;
        let mut outbound = 0usize;
        let mut artifact_moves = 0usize;
        let mut admitted_mark_outcomes = 0usize;

        let touch_run = |runs: &mut std::collections::BTreeSet<String>, id: &str| {
            runs.insert(id.to_string());
        };

        for m in movements {
            *kind_counts.entry(movement_kind_label(&m.kind)).or_insert(0) += 1;

            if let Some(run) = m.run() {
                touch_run(&mut runs, run.as_str());
            }
            if let Some(runtime) = m.runtime() {
                ees.insert(runtime.ee.as_str().to_string());
            }

            match &m.kind {
                MovementKind::ArtifactMoved => artifact_moves += 1,
                MovementKind::InterfaceArrival => arrivals += 1,
                MovementKind::InterfaceOutbound => outbound += 1,
                MovementKind::MarkOutcomeAdmitted(_) => admitted_mark_outcomes += 1,
                MovementKind::VerificationFailed => {}
                MovementKind::DeliveryRefused => {}
            }
        }

        let total = movements.len();
        let mut kind_counts: Vec<(String, usize, u64)> = kind_counts
            .into_iter()
            .map(|(label, count)| {
                let permille = if total == 0 {
                    0
                } else {
                    ((count as u64) * 1000) / (total as u64)
                };
                (label, count, permille)
            })
            .collect();
        kind_counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        Self {
            total_movements: total,
            kind_counts,
            arrivals,
            outbound,
            artifact_moves,
            admitted_mark_outcomes,
            runs: runs.len(),
            ees: ees.len(),
            tokens,
        }
    }

    fn ledger_is_empty(&self) -> bool {
        self.total_movements == 0
    }
}

fn movement_kind_label(kind: &MovementKind) -> String {
    match kind {
        MovementKind::ArtifactMoved => "artifact_moved".to_string(),
        MovementKind::MarkOutcomeAdmitted(m) => format!("mark:{}", m.as_str()),
        MovementKind::VerificationFailed => "verification_failed".to_string(),
        MovementKind::DeliveryRefused => "delivery_refused".to_string(),
        MovementKind::InterfaceArrival => "interface_arrival".to_string(),
        MovementKind::InterfaceOutbound => "interface_outbound".to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetricsFocus {
    Summary,
    Kinds,
}

pub(crate) struct MetricsOverlay {
    projection: MetricsProjection,
    spine_wired: bool,
    focus: MetricsFocus,
    summary_scroll: u16,
    kinds_scroll: u16,
    is_done: bool,
}

impl MetricsOverlay {
    pub(crate) fn open(movements: Vec<Movement>, tokens: TokenUsage, spine_wired: bool) -> Self {
        Self {
            projection: MetricsProjection::project(&movements, tokens),
            spine_wired,
            focus: MetricsFocus::Summary,
            summary_scroll: 0,
            kinds_scroll: 0,
            is_done: false,
        }
    }

    pub(crate) fn refresh(
        &mut self,
        movements: Vec<Movement>,
        tokens: TokenUsage,
        spine_wired: bool,
    ) {
        self.projection = MetricsProjection::project(&movements, tokens);
        self.spine_wired = spine_wired;
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
    }

    pub(crate) fn wants_refresh(&self) -> bool {
        !self.is_done
    }

    pub(crate) fn handle_event(
        &mut self,
        tui: &mut crate::tui::Tui,
        event: crate::tui::TuiEvent,
    ) -> Result<()> {
        match event {
            crate::tui::TuiEvent::Key(key_event) => {
                self.handle_key(key_event);
                tui.frame_requester().schedule_frame();
                Ok(())
            }
            crate::tui::TuiEvent::Draw | crate::tui::TuiEvent::Resize => {
                tui.draw(u16::MAX, |frame| {
                    self.render(frame.area(), frame.buffer);
                })?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.is_done = true,
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                MetricsFocus::Summary => {
                    self.summary_scroll = self.summary_scroll.saturating_add(1)
                }
                MetricsFocus::Kinds => self.kinds_scroll = self.kinds_scroll.saturating_add(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                MetricsFocus::Summary => {
                    self.summary_scroll = self.summary_scroll.saturating_sub(1)
                }
                MetricsFocus::Kinds => self.kinds_scroll = self.kinds_scroll.saturating_sub(1),
            },
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                self.focus = match self.focus {
                    MetricsFocus::Summary => MetricsFocus::Kinds,
                    MetricsFocus::Kinds => MetricsFocus::Summary,
                };
            }
            _ => {}
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(area);
        self.render_header(chunks[0], buf);

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);
        self.render_summary(cols[0], buf);
        self.render_kinds(cols[1], buf);
        self.render_footer(chunks[2], buf);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let line = Line::from(vec![
            Span::from(" metrics — telemetry projection ").bold().cyan(),
            Span::from("· ").dim(),
            Span::from(format!("{} movements", self.projection.total_movements)),
            Span::from("  ·  ").dim(),
            Span::from(format!("{} marks", self.projection.admitted_mark_outcomes)),
            Span::from("  ·  ").dim(),
            Span::from(format!("{} runs", self.projection.runs)),
        ]);
        Paragraph::new(line).render(area, buf);
    }

    fn render_summary(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focus == MetricsFocus::Summary;
        let block = Block::default()
            .title(" counters · tokens ")
            .borders(Borders::ALL)
            .border_style(if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().dim()
            });
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from("LEDGER COUNTERS (pure read)".bold().cyan()));
        if self.projection.ledger_is_empty() {
            if self.spine_wired {
                lines.push(Line::from(
                    "  embedded spine wired, but no movement has crossed the ledger yet.".dim(),
                ));
                lines.push(Line::from(
                    "  submit a turn (chat) and the counters will populate live.".dim(),
                ));
            } else {
                lines.push(Line::from(
                    "  embedded spine not wired (the held Runtime + ledger are composed by the"
                        .dim(),
                ));
                lines.push(Line::from(
                    "  binary lane); no ledger metrics to project (we do NOT fabricate counts)."
                        .dim(),
                ));
            }
        } else {
            lines.push(metric_row(
                "interface arrivals (inbound)",
                self.projection.arrivals,
            ));
            lines.push(metric_row(
                "interface outbound (egress)",
                self.projection.outbound,
            ));
            lines.push(metric_row("artifact moves", self.projection.artifact_moves));
        }

        lines.push(Line::from(""));
        lines.push(Line::from("ADMITTED MARK OUTCOMES".bold().cyan()));
        lines.push(metric_row(
            "verified artifacts admitted",
            self.projection.admitted_mark_outcomes,
        ));

        lines.push(Line::from(""));
        lines.push(Line::from("RUNS · EXISTENCE".bold().cyan()));
        lines.push(metric_row("runs in the ledger", self.projection.runs));
        lines.push(metric_row("EE instances", self.projection.ees));

        lines.push(Line::from(""));
        lines.push(Line::from(
            "TOKEN USAGE (in-process, pure read)".bold().cyan(),
        ));
        let t = &self.projection.tokens;
        if t.is_zero() {
            lines.push(Line::from(
                "  no tokens yet this session (no model turn has completed).".dim(),
            ));
        } else {
            lines.push(metric_row_i64("total tokens", t.total_tokens));
            lines.push(metric_row_i64("input tokens", t.input_tokens));
            lines.push(metric_row_i64("  of which cached", t.cached_input_tokens));
            lines.push(metric_row_i64("output tokens", t.output_tokens));
            lines.push(metric_row_i64(
                "  of which reasoning",
                t.reasoning_output_tokens,
            ));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(
            "OTEL METER (latency / durations)".bold().yellow(),
        ));
        lines.push(Line::from("  not wired as a pure read.".yellow().bold()));
        lines.push(Line::from(
            "  the only in-process read-handle (SessionTelemetry::runtime_metrics_summary)".dim(),
        ));
        lines.push(Line::from(
            "  COLLECTS+RESETS the delta accumulators — a side effect, not a projection —".dim(),
        ));
        lines.push(Line::from(
            "  so tool/api latency + ttft are NOT read here (would steal the turn deltas).".dim(),
        ));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.summary_scroll, 0))
            .render(inner, buf);
    }

    fn render_kinds(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focus == MetricsFocus::Kinds;
        let block = Block::default()
            .title(format!(
                " movement kinds (of {} rows) ",
                self.projection.total_movements
            ))
            .borders(Borders::ALL)
            .border_style(if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().dim()
            });
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![
            Span::from(format!("{:>7} ", "count")).dim(),
            Span::from(format!("{:>6} ", "share")).dim(),
            Span::from("kind").dim(),
        ]));
        for (label, count, permille) in &self.projection.kind_counts {
            let share = format!("{}.{}%", permille / 10, permille % 10);
            lines.push(Line::from(vec![
                Span::from(format!("{count:>7} ")),
                Span::from(format!("{share:>6} ")).dim(),
                kind_span(label),
            ]));
        }
        if self.projection.kind_counts.is_empty() {
            lines.push(Line::from("(no movements)".dim()));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.kinds_scroll, 0))
            .render(inner, buf);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let line = Line::from(
            "telemetry projection — read-only (no run/mark/mutate, no otel reset) · j/k scroll · Tab switch pane · r refresh · q/Esc close"
                .dim(),
        );
        Paragraph::new(line).render(area, buf);
    }
}

fn metric_row(label: &str, count: usize) -> Line<'static> {
    Line::from(vec![
        Span::from(format!("  {count:>8} ")),
        Span::from(label.to_string()).dim(),
    ])
}

fn metric_row_i64(label: &str, count: i64) -> Line<'static> {
    Line::from(vec![
        Span::from(format!("  {count:>8} ")),
        Span::from(label.to_string()).dim(),
    ])
}

fn kind_span(label: &str) -> Span<'static> {
    if label.starts_with("mark:") {
        Span::from(label.to_string()).green()
    } else if label == "interface_arrival" {
        Span::from(label.to_string()).cyan()
    } else if label == "interface_outbound" {
        Span::from(label.to_string()).blue()
    } else if label == "run_parked" {
        Span::from(label.to_string()).yellow()
    } else {
        Span::from(label.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_ledger::Endpoint;
    use octopus_descriptors::{EeId, MarkId, RunId};
    use octopus_ledger::{Lamport, Seq, TraceId};

    fn mv(seq: u64, kind: MovementKind, run: &str, ee: &str) -> Movement {
        Movement {
            seq: Seq(seq), lamport: Lamport(seq), recorded_at_unix_ms: seq,
            trace: TraceId("t".into()), kind,
            from: Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_test")),
            to: Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_worker_test")),
            artifact: None,
            invocation_metadata: Some(octopus_ledger::InvocationMetadata::new(octopus_ledger::GrantProjection::default()).with_runtime(
                octopus_ledger::RuntimeInvocationMetadata {
                    run: RunId::new(run), ee: EeId::new(ee),
                    worker: octopus_descriptors::InterfaceId::new("system_worker_test"),
                    interface: octopus_descriptors::InterfaceId::new("world_test"),
                    checkpoint: octopus_descriptors::CheckpointId::new("c1"),
                    epoch: 0, run_record_hash: None,
                }).into()),
        }
    }

    #[test]
    fn projects_ledger_counters_and_shares() {
        let movements = vec![
            mv(1, MovementKind::InterfaceArrival, "r", "ee_root"),
            mv(2, MovementKind::ArtifactMoved, "r", "ee_root"),
            mv(3, MovementKind::InterfaceOutbound, "r", "ee_root"),
            mv(4, MovementKind::ArtifactMoved, "r", "ee_root"),
        ];
        let p = MetricsProjection::project(&movements, TokenUsage::default());
        assert_eq!(p.total_movements, 4);
        assert_eq!(p.arrivals, 1);
        assert_eq!(p.outbound, 1);
        assert_eq!(p.artifact_moves, 2);
        let top = &p.kind_counts[0];
        assert_eq!(top.0, "artifact_moved");
        assert_eq!(top.1, 2);
        assert_eq!(top.2, 500);
    }

    #[test]
    fn admitted_mark_outcomes_count_verified_admissions() {
        let movements = vec![
            mv(1, MovementKind::MarkOutcomeAdmitted(MarkId::new("coder.build.ok")), "r", "ee_root"),
            mv(2, MovementKind::MarkOutcomeAdmitted(MarkId::new("ocr.text.contains")), "r", "ee_root"),
            mv(3, MovementKind::ArtifactMoved, "r", "ee_root"),
        ];
        let p = MetricsProjection::project(&movements, TokenUsage::default());
        assert_eq!(p.admitted_mark_outcomes, 2);
    }

    #[test]
    fn run_and_ee_counts_come_from_movement_metadata() {
        let movements = vec![
            mv(1, MovementKind::ArtifactMoved, "run_a", "ee_root"),
            mv(2, MovementKind::ArtifactMoved, "run_b", "ee_root"),
        ];
        let p = MetricsProjection::project(&movements, TokenUsage::default());
        assert_eq!(p.runs, 2);
        assert_eq!(p.ees, 1);
    }

    #[test]
    fn empty_ledger_is_no_metrics_not_fabricated() {
        let p = MetricsProjection::project(&[], TokenUsage::default());
        assert!(p.ledger_is_empty());
        assert_eq!(p.total_movements, 0);
        assert_eq!(p.admitted_mark_outcomes, 0);
        assert!(p.kind_counts.is_empty());
    }

    #[test]
    fn token_usage_is_a_pure_clone() {
        let tokens = TokenUsage {
            input_tokens: 100,
            cached_input_tokens: 20,
            output_tokens: 50,
            reasoning_output_tokens: 10,
            total_tokens: 160,
        };
        let p = MetricsProjection::project(&[], tokens.clone());
        assert!(!p.tokens.is_zero());
        assert_eq!(p.tokens.total_tokens, 160);
        assert_eq!(p.tokens.cached_input_tokens, 20);
    }
}
