use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;
use std::cell::Cell;

use crate::ascii_animation::AsciiAnimation;
use crate::key_hint::KeyBindingListExt;
use crate::onboarding::keys;
use crate::onboarding::onboarding_screen::KeyboardHandler;
use crate::onboarding::onboarding_screen::StepStateProvider;
use crate::tui::FrameRequester;

use super::onboarding_screen::StepState;

const MIN_ANIMATION_HEIGHT: u16 = 24;
const MIN_ANIMATION_WIDTH: u16 = 78;
const COMPACT_BOOT_HEIGHT: u16 = 6;
const COMPACT_BOOT_WIDTH: u16 = 48;
const OCTOPUS_ORANGE: Color = Color::Rgb(255, 104, 48);
const OCTOPUS_AMBER: Color = Color::Rgb(255, 164, 88);
const OCTOPUS_DIM: Color = Color::Rgb(116, 108, 103);

pub(crate) struct WelcomeWidget {
    pub is_logged_in: bool,
    animation: AsciiAnimation,
    animations_enabled: bool,
    animations_suppressed: Cell<bool>,
    layout_area: Cell<Option<Rect>>,
}

impl KeyboardHandler for WelcomeWidget {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if !self.animations_enabled {
            return;
        }
        if key_event.kind == KeyEventKind::Press && keys::TOGGLE_ANIMATION.is_pressed(key_event) {
            tracing::warn!("Welcome background to press '.'");
            let _ = self.animation.pick_random_variant();
        }
    }
}

impl WelcomeWidget {
    pub(crate) fn new(
        is_logged_in: bool,
        request_frame: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            is_logged_in,
            animation: AsciiAnimation::new(request_frame),
            animations_enabled,
            animations_suppressed: Cell::new(false),
            layout_area: Cell::new(None),
        }
    }

    pub(crate) fn update_layout_area(&self, area: Rect) {
        self.layout_area.set(Some(area));
    }

    pub(crate) fn set_animations_suppressed(&self, suppressed: bool) {
        self.animations_suppressed.set(suppressed);
    }
}

impl WidgetRef for &WelcomeWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        if self.animations_enabled && !self.animations_suppressed.get() {
            self.animation.schedule_next_frame();
        }

        let layout_area = self.layout_area.get().unwrap_or(area);
        let show_animation = self.animations_enabled
            && !self.animations_suppressed.get()
            && layout_area.height >= MIN_ANIMATION_HEIGHT
            && layout_area.width >= MIN_ANIMATION_WIDTH;
        let show_compact_boot = self.animations_enabled
            && !self.animations_suppressed.get()
            && !show_animation
            && layout_area.height >= COMPACT_BOOT_HEIGHT
            && layout_area.width >= COMPACT_BOOT_WIDTH;

        let mut lines: Vec<Line> = Vec::new();
        if show_animation {
            let frame = self.animation.current_frame();
            lines.extend(frame.lines().map(styled_boot_line));
            lines.push("".into());
        } else if show_compact_boot {
            lines.extend(compact_boot_lines(layout_area.width));
            lines.push("".into());
        }
        lines.push(Line::from(vec![
            "  ".into(),
            "Welcome to ".into(),
            "Octopus".fg(OCTOPUS_ORANGE).bold(),
            " OS".fg(OCTOPUS_ORANGE).bold(),
        ]));
        lines.push(Line::from(vec![
            "  ".into(),
            "interfaces".fg(OCTOPUS_AMBER),
            " / ".fg(OCTOPUS_DIM),
            "artifacts".fg(OCTOPUS_AMBER),
            " / ".fg(OCTOPUS_DIM),
            "policy".fg(OCTOPUS_AMBER),
            " / ".fg(OCTOPUS_DIM),
            "runs".fg(OCTOPUS_AMBER),
            " / ".fg(OCTOPUS_DIM),
            "marks".fg(OCTOPUS_AMBER),
        ]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

impl StepStateProvider for WelcomeWidget {
    fn get_step_state(&self) -> StepState {
        match self.is_logged_in {
            true => StepState::Hidden,
            false => StepState::Complete,
        }
    }
}

fn styled_boot_line(line: &str) -> Line<'static> {
    let style = if line.contains("OCTOPUS OS") {
        Style::default().fg(OCTOPUS_ORANGE).bold()
    } else if line.contains("[*]") {
        Style::default().fg(OCTOPUS_AMBER).bold()
    } else if line.contains("interface shell") || line.contains("policy") || line.contains("runs") {
        Style::default().fg(OCTOPUS_AMBER)
    } else if line.contains("interfaces")
        || line.contains("artifacts")
        || line.contains("marks")
        || line.contains("evidence")
    {
        Style::default().fg(OCTOPUS_DIM)
    } else {
        Style::default().fg(Color::Gray)
    };
    Line::from(Span::styled(line.to_owned(), style))
}

fn compact_boot_lines(width: u16) -> Vec<Line<'static>> {
    let inner_width = usize::from(width.min(78).saturating_sub(4).max(32));
    let rule = "-".repeat(inner_width);
    let title = center_text("OCTOPUS OS", inner_width);
    let shell_line = center_text("interface shell", inner_width);
    let flow_line = center_text("interfaces -> artifacts -> policy", inner_width);
    let run_line = center_text("runs -> marks -> replay ledger", inner_width);

    vec![
        boot_line(format!("  .{rule}."), OCTOPUS_DIM, false),
        boot_line(format!("  |{title}|"), OCTOPUS_ORANGE, true),
        boot_line(format!("  |{shell_line}|"), Color::Gray, false),
        boot_line(format!("  |{flow_line}|"), OCTOPUS_AMBER, false),
        boot_line(format!("  |{run_line}|"), OCTOPUS_DIM, false),
        boot_line(format!("  '{rule}'"), OCTOPUS_DIM, false),
    ]
}

fn boot_line(text: String, color: Color, bold: bool) -> Line<'static> {
    let style = if bold {
        Style::default().fg(color).bold()
    } else {
        Style::default().fg(color)
    };
    Line::from(Span::styled(text, style))
}

fn center_text(text: &str, width: usize) -> String {
    if text.len() >= width {
        return text.chars().take(width).collect();
    }
    let slack = width - text.len();
    let left = slack / 2;
    let right = slack - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyModifiers;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    static VARIANT_A: [&str; 1] = ["frame-a"];
    static VARIANT_B: [&str; 1] = ["frame-b"];
    static VARIANTS: [&[&str]; 2] = [&VARIANT_A, &VARIANT_B];

    fn row_containing(buf: &Buffer, needle: &str) -> Option<u16> {
        (0..buf.area.height).find(|&y| {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            row.contains(needle)
        })
    }

    fn buffer_text(buf: &Buffer) -> String {
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn welcome_renders_animation_on_first_draw() {
        let widget = WelcomeWidget::new(false, FrameRequester::test_dummy(), true);
        let area = Rect::new(0, 0, MIN_ANIMATION_WIDTH, MIN_ANIMATION_HEIGHT);
        let mut buf = Buffer::empty(area);
        let frame_lines = widget.animation.current_frame().lines().count() as u16;
        (&widget).render(area, &mut buf);

        let welcome_row = row_containing(&buf, "Welcome");
        assert_eq!(welcome_row, Some(frame_lines + 1));
    }

    #[test]
    fn welcome_uses_octopus_branding() {
        let widget = WelcomeWidget::new(false, FrameRequester::test_dummy(), true);
        let area = Rect::new(0, 0, MIN_ANIMATION_WIDTH, MIN_ANIMATION_HEIGHT);
        let mut buf = Buffer::empty(area);
        (&widget).render(area, &mut buf);

        let text = buffer_text(&buf);
        assert!(text.contains("Welcome to Octopus"), "{text}");
        assert!(!text.contains("Welcome to Codex"), "{text}");
        assert!(
            !text.contains("OpenAI's command-line coding agent"),
            "{text}"
        );
    }

    #[test]
    fn welcome_skips_animation_below_height_breakpoint() {
        let widget = WelcomeWidget::new(false, FrameRequester::test_dummy(), true);
        let area = Rect::new(0, 0, MIN_ANIMATION_WIDTH, MIN_ANIMATION_HEIGHT - 1);
        let mut buf = Buffer::empty(area);
        (&widget).render(area, &mut buf);

        let welcome_row = row_containing(&buf, "Welcome");
        assert_eq!(welcome_row, Some(COMPACT_BOOT_HEIGHT + 1));
    }

    #[test]
    fn welcome_renders_compact_boot_when_full_frame_does_not_fit() {
        let widget = WelcomeWidget::new(false, FrameRequester::test_dummy(), true);
        let area = Rect::new(0, 0, COMPACT_BOOT_WIDTH, COMPACT_BOOT_HEIGHT);
        let mut buf = Buffer::empty(area);
        (&widget).render(area, &mut buf);

        let text = buffer_text(&buf);
        assert!(text.contains("OCTOPUS OS"), "{text}");
        assert!(text.contains("interfaces -> artifacts -> policy"), "{text}");
        assert!(!text.contains("Welcome to Codex"), "{text}");
        assert!(!text.contains("( o )"), "{text}");
    }

    #[test]
    fn ctrl_dot_changes_animation_variant() {
        let mut widget = WelcomeWidget {
            is_logged_in: false,
            animation: AsciiAnimation::with_variants(FrameRequester::test_dummy(), &VARIANTS, 0),
            animations_enabled: true,
            animations_suppressed: Cell::new(false),
            layout_area: Cell::new(None),
        };

        let before = widget.animation.current_frame();
        widget.handle_key_event(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL));
        let after = widget.animation.current_frame();

        assert_ne!(
            before, after,
            "expected ctrl+. to switch welcome animation variant"
        );
    }

    #[test]
    fn ctrl_shift_dot_changes_animation_variant() {
        let mut widget = WelcomeWidget {
            is_logged_in: false,
            animation: AsciiAnimation::with_variants(FrameRequester::test_dummy(), &VARIANTS, 0),
            animations_enabled: true,
            animations_suppressed: Cell::new(false),
            layout_area: Cell::new(None),
        };

        let before = widget.animation.current_frame();
        widget.handle_key_event(KeyEvent::new(
            KeyCode::Char('.'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        let after = widget.animation.current_frame();

        assert_ne!(
            before, after,
            "expected ctrl+shift+. to switch welcome animation variant"
        );
    }
}
