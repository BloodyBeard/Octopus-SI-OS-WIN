use std::collections::BTreeMap;
use std::io::Result;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use octopus_ledger::Endpoint;
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

use crate::tui;
use crate::tui::TuiEvent;

const LEDGER_TAIL_LIMIT: usize = 200;

#[derive(Clone, Debug)]
struct RunNode {
    run: String,
    movement_count: usize,
}

#[derive(Clone, Debug)]
struct EeNode {
    ee: String,
    runs: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct LiveTopology {
    runs: Vec<RunNode>,
    ees: Vec<EeNode>,
    tail: Vec<Movement>,
    kind_counts: Vec<(String, usize)>,
    total_movements: usize,
}

impl LiveTopology {
    fn project(movements: &[Movement]) -> Self {
        struct RunAcc {
            count: usize,
        }
        let mut runs: BTreeMap<String, RunAcc> = BTreeMap::new();
        struct EeAcc {
            runs: std::collections::BTreeSet<String>,
        }
        let mut ees: BTreeMap<String, EeAcc> = BTreeMap::new();
        let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();

        let touch_run = |runs: &mut BTreeMap<String, RunAcc>, id: &str| {
            runs.entry(id.to_string())
                .or_insert(RunAcc { count: 0 })
                .count += 1;
        };

        for m in movements {
            *kind_counts.entry(movement_kind_label(&m.kind)).or_insert(0) += 1;

            if let Some(run) = m.run() {
                touch_run(&mut runs, run.as_str());
            }
            if let Some(runtime) = m.runtime() {
                ees.entry(runtime.ee.as_str().to_string())
                    .or_insert(EeAcc { runs: std::collections::BTreeSet::new() })
                    .runs.insert(runtime.run.as_str().to_string());
            }
        }

        let runs = runs
            .into_iter()
            .map(|(run, acc)| RunNode {
                run,
                movement_count: acc.count,
            })
            .collect();
        let ees = ees
            .into_iter()
            .map(|(ee, acc)| EeNode {
                ee,
                runs: acc.runs.into_iter().collect(),
            })
            .collect();
        let kind_counts = kind_counts.into_iter().collect();

        let total = movements.len();
        let tail_start = total.saturating_sub(LEDGER_TAIL_LIMIT);
        let tail = movements[tail_start..].to_vec();

        Self {
            runs,
            ees,
            tail,
            kind_counts,
            total_movements: total,
        }
    }

    fn is_empty(&self) -> bool {
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

fn endpoint_label(ep: &Endpoint) -> String {
    match ep {
        Endpoint::Interface(i) => format!("iface:{}", i.as_str()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DashFocus {
    Topology,
    Ledger,
}

pub(crate) struct DashboardOverlay {
    topology: LiveTopology,
    spine_wired: bool,
    focus: DashFocus,
    topo_scroll: u16,
    ledger_scroll: u16,
    is_done: bool,
}

impl DashboardOverlay {
    pub(crate) fn open(movements: Vec<Movement>, spine_wired: bool) -> Self {
        Self {
            topology: LiveTopology::project(&movements),
            spine_wired,
            focus: DashFocus::Topology,
            topo_scroll: 0,
            ledger_scroll: 0,
            is_done: false,
        }
    }

    pub(crate) fn refresh(&mut self, movements: Vec<Movement>, spine_wired: bool) {
        self.topology = LiveTopology::project(&movements);
        self.spine_wired = spine_wired;
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
    }

    pub(crate) fn wants_refresh(&self) -> bool {
        !self.is_done
    }

    pub(crate) fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(key_event) => {
                self.handle_key(key_event);
                tui.frame_requester().schedule_frame();
                Ok(())
            }
            TuiEvent::Draw | TuiEvent::Resize => {
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
                DashFocus::Topology => self.topo_scroll = self.topo_scroll.saturating_add(1),
                DashFocus::Ledger => self.ledger_scroll = self.ledger_scroll.saturating_add(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                DashFocus::Topology => self.topo_scroll = self.topo_scroll.saturating_sub(1),
                DashFocus::Ledger => self.ledger_scroll = self.ledger_scroll.saturating_sub(1),
            },
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                self.focus = match self.focus {
                    DashFocus::Topology => DashFocus::Ledger,
                    DashFocus::Ledger => DashFocus::Topology,
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

        if self.topology.is_empty() {
            self.render_no_live_data(chunks[1], buf);
            self.render_footer(chunks[2], buf);
            return;
        }

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);
        self.render_topology(cols[0], buf);
        self.render_ledger(cols[1], buf);
        self.render_footer(chunks[2], buf);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let line = Line::from(vec![
            Span::from(" dashboard — live topology ").bold().cyan(),
            Span::from("· ").dim(),
            Span::from(format!("{} runs", self.topology.runs.len())),
            Span::from("  ·  ").dim(),
            Span::from(format!("{} EEs", self.topology.ees.len())),
            Span::from("  ·  ").dim(),
            Span::from(format!("{} movements", self.topology.total_movements)),
        ]);
        Paragraph::new(line).render(area, buf);
    }

    fn render_no_live_data(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(" live topology ")
            .borders(Borders::ALL)
            .border_style(Style::default().dim());
        let inner = block.inner(area);
        block.render(area, buf);
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.spine_wired {
            lines.push(Line::from("no live data yet".yellow().bold()));
            lines.push(Line::from(
                "the embedded spine is wired, but no movement has crossed the ledger yet.".dim(),
            ));
            lines.push(Line::from(
                "submit a turn (chat) and the runs / EE tree / ledger tail will appear here live."
                    .dim(),
            ));
        } else {
            lines.push(Line::from("embedded spine not wired".yellow().bold()));
            lines.push(Line::from(
                "the held Runtime + ledger are composed by the binary lane; until they attach,"
                    .dim(),
            ));
            lines.push(Line::from(
                "there is no live topology to project (and we do NOT fabricate one from policy descriptors)."
                    .dim(),
            ));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }

    fn render_topology(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focus == DashFocus::Topology;
        let block = Block::default()
            .title(" runs · EE tree ")
            .borders(Borders::ALL)
            .border_style(if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().dim()
            });
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from("RUNS (ledger projection)".bold().cyan()));
        for r in &self.topology.runs {
            lines.push(Line::from(vec![
                Span::from("  • ").dim(),
                Span::from(r.run.clone()).bold(),
                Span::from(format!("  recorded · {} movements", r.movement_count)).dim(),
            ]));
        }
        if self.topology.runs.is_empty() {
            lines.push(Line::from("  (no runs in the ledger)".dim()));
        }

        lines.push(Line::from(""));
        lines.push(Line::from("EE TREE (existence → runs)".bold().cyan()));
        for ee in &self.topology.ees {
            lines.push(Line::from(vec![
                Span::from("  ○ ").magenta(),
                Span::from(ee.ee.clone()).magenta().bold(),
            ]));
            for run in &ee.runs {
                lines.push(Line::from(Span::from(format!("      └─ run:{run}")).dim()));
            }
        }
        if self.topology.ees.is_empty() {
            lines.push(Line::from("  (no EE instances in the ledger)".dim()));
        }

        lines.push(Line::from(""));
        lines.push(Line::from("SUMMARY (movement kinds)".bold().cyan()));
        for (label, count) in &self.topology.kind_counts {
            lines.push(Line::from(vec![
                Span::from(format!("  {count:>4} ")).dim(),
                Span::from(label.clone()),
            ]));
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.topo_scroll, 0))
            .render(inner, buf);
    }

    fn render_ledger(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focus == DashFocus::Ledger;
        let block = Block::default()
            .title(format!(
                " movement ledger (last {} of {}) ",
                self.topology.tail.len(),
                self.topology.total_movements
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
        for m in self.topology.tail.iter().rev() {
            let kind = movement_kind_label(&m.kind);
            let kind_span = match &m.kind {
                MovementKind::MarkOutcomeAdmitted(_) => Span::from(kind).green(),
                MovementKind::InterfaceArrival => Span::from(kind).cyan(),
                MovementKind::InterfaceOutbound => Span::from(kind).blue(),
                _ => Span::from(kind).dim(),
            };
            lines.push(Line::from(vec![
                Span::from(format!("#{:<4} ", m.seq.0)).dim(),
                kind_span,
            ]));
            lines.push(Line::from(vec![
                Span::from("      ").dim(),
                Span::from(endpoint_label(&m.from)).dim(),
                Span::from(" → ").dim(),
                Span::from(endpoint_label(&m.to)).dim(),
            ]));
        }
        if self.topology.tail.is_empty() {
            lines.push(Line::from("(no movements)".dim()));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.ledger_scroll, 0))
            .render(inner, buf);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let line = Line::from(
            "live topology — read-only projection (no run/mark/mutate) · j/k scroll · Tab switch pane · r refresh · q/Esc close"
                .dim(),
        );
        Paragraph::new(line).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, EeId, RunId};
    use octopus_ledger::{ArtifactRef, ContentHash, Lamport, Seq, TraceId};

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
    fn projects_runs_from_real_movements() {
        let movements = vec![
            mv(1, MovementKind::InterfaceArrival, "run_a", "ee_root"),
            mv(2, MovementKind::ArtifactMoved, "run_b", "ee_root"),
        ];
        let topo = LiveTopology::project(&movements);
        assert_eq!(
            topo.runs.iter().map(|r| r.run.as_str()).collect::<Vec<_>>(),
            vec!["run_a", "run_b"]
        );
        let run_a = topo.runs.iter().find(|r| r.run == "run_a").unwrap();
        assert_eq!(run_a.movement_count, 1);
    }

    #[test]
    fn projects_ee_tree_from_real_movement_metadata() {
        let moved = mv(1, MovementKind::ArtifactMoved, "run_a", "ee_root");
        let topo = LiveTopology::project(&[moved]);
        assert_eq!(topo.ees.len(), 1);
        assert_eq!(topo.ees[0].ee, "ee_root");
        assert_eq!(topo.ees[0].runs, vec!["run_a".to_string()]);
    }

    #[test]
    fn empty_ledger_is_no_live_data_not_a_fabricated_graph() {
        let topo = LiveTopology::project(&[]);
        assert!(topo.is_empty());
        assert!(topo.runs.is_empty());
        assert!(topo.ees.is_empty());
        let _ = ArtifactId::new("unused");
        let _ = ArtifactRef {
            id: ArtifactId::new("x"),
            content: ContentHash::of_bytes(b"x"),
        };
    }

    #[test]
    fn summary_counts_kinds_and_tail_windows() {
        let mut movements = Vec::new();
        for i in 0..(LEDGER_TAIL_LIMIT + 50) {
            movements.push(mv((i + 1) as u64, MovementKind::ArtifactMoved, "r", "ee_root"));
        }
        let topo = LiveTopology::project(&movements);
        assert_eq!(topo.total_movements, LEDGER_TAIL_LIMIT + 50);
        assert_eq!(topo.tail.len(), LEDGER_TAIL_LIMIT);
        assert!(
            topo.kind_counts
                .iter()
                .any(|(k, c)| k == "artifact_moved" && *c == LEDGER_TAIL_LIMIT + 50)
        );
    }
}
