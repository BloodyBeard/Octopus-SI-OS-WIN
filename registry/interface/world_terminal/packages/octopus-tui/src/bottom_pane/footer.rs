use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::render::line_utils::prefix_lines;
use crate::status::format_tokens_compact;
use crate::ui_consts::FOOTER_INDENT_COLS;
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

#[derive(Clone, Debug)]
pub(crate) struct FooterProps {
    pub(crate) mode: FooterMode,
    pub(crate) esc_backtrack_hint: bool,
    pub(crate) use_shift_enter_hint: bool,
    pub(crate) is_task_running: bool,
    pub(crate) queue_submissions: bool,
    pub(crate) collaboration_modes_enabled: bool,
    pub(crate) is_wsl: bool,
    pub(crate) quit_shortcut_key: KeyBinding,
    pub(crate) status_line_value: Option<Line<'static>>,
    pub(crate) status_line_enabled: bool,
    pub(crate) key_hints: FooterKeyHints,
    pub(crate) active_agent_label: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CollaborationModeIndicator {
    Plan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GoalStatusIndicator {
    Active { usage: Option<String> },
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited { usage: Option<String> },
    Complete { usage: Option<String> },
}

const MODE_CYCLE_HINT: &str = "shift+tab to cycle";
const FOOTER_CONTEXT_GAP_COLS: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FooterKeyHints {
    pub(crate) toggle_shortcuts: Option<KeyBinding>,
    pub(crate) queue: Option<KeyBinding>,
    pub(crate) insert_newline: Option<KeyBinding>,
    pub(crate) external_editor: Option<KeyBinding>,
    pub(crate) edit_previous: Option<KeyBinding>,
    pub(crate) show_transcript: Option<KeyBinding>,
    pub(crate) history_search: Option<KeyBinding>,
    pub(crate) reasoning_down: Option<KeyBinding>,
    pub(crate) reasoning_up: Option<KeyBinding>,
}

impl FooterKeyHints {
    #[cfg(test)]
    pub(crate) fn default_bindings() -> Self {
        Self {
            toggle_shortcuts: Some(key_hint::plain(KeyCode::Char('?'))),
            queue: Some(key_hint::plain(KeyCode::Tab)),
            insert_newline: Some(key_hint::ctrl(KeyCode::Char('j'))),
            external_editor: Some(key_hint::ctrl(KeyCode::Char('g'))),
            edit_previous: Some(key_hint::plain(KeyCode::Esc)),
            show_transcript: Some(key_hint::ctrl(KeyCode::Char('t'))),
            history_search: Some(key_hint::ctrl(KeyCode::Char('r'))),
            reasoning_down: Some(key_hint::alt(KeyCode::Char(','))),
            reasoning_up: Some(key_hint::alt(KeyCode::Char('.'))),
        }
    }
}

impl CollaborationModeIndicator {
    fn label(self, show_cycle_hint: bool) -> String {
        let suffix = if show_cycle_hint {
            format!(" ({MODE_CYCLE_HINT})")
        } else {
            String::new()
        };
        match self {
            CollaborationModeIndicator::Plan => format!("Plan mode{suffix}"),
        }
    }

    fn styled_span(self, show_cycle_hint: bool) -> Span<'static> {
        let label = self.label(show_cycle_hint);
        match self {
            CollaborationModeIndicator::Plan => Span::from(label).magenta(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FooterMode {
    HistorySearch,
    QuitShortcutReminder,
    ShortcutOverlay,
    EscHint,
    ComposerEmpty,
    ComposerHasDraft,
}

pub(crate) fn toggle_shortcut_mode(
    current: FooterMode,
    ctrl_c_hint: bool,
    is_empty: bool,
) -> FooterMode {
    if ctrl_c_hint && matches!(current, FooterMode::QuitShortcutReminder) {
        return current;
    }

    let base_mode = if is_empty {
        FooterMode::ComposerEmpty
    } else {
        FooterMode::ComposerHasDraft
    };

    match current {
        FooterMode::ShortcutOverlay | FooterMode::QuitShortcutReminder => base_mode,
        _ => FooterMode::ShortcutOverlay,
    }
}

pub(crate) fn esc_hint_mode(current: FooterMode, is_task_running: bool) -> FooterMode {
    if is_task_running {
        current
    } else {
        FooterMode::EscHint
    }
}

pub(crate) fn reset_mode_after_activity(current: FooterMode) -> FooterMode {
    match current {
        FooterMode::EscHint
        | FooterMode::ShortcutOverlay
        | FooterMode::QuitShortcutReminder
        | FooterMode::HistorySearch
        | FooterMode::ComposerHasDraft => FooterMode::ComposerEmpty,
        other => other,
    }
}

pub(crate) fn footer_height(props: &FooterProps) -> u16 {
    let show_shortcuts_hint = match props.mode {
        FooterMode::ComposerEmpty => true,
        FooterMode::ComposerHasDraft => false,
        FooterMode::HistorySearch
        | FooterMode::QuitShortcutReminder
        | FooterMode::ShortcutOverlay
        | FooterMode::EscHint => false,
    };
    let show_queue_hint = match props.mode {
        FooterMode::ComposerHasDraft => props.is_task_running,
        FooterMode::QuitShortcutReminder
        | FooterMode::HistorySearch
        | FooterMode::ComposerEmpty
        | FooterMode::ShortcutOverlay
        | FooterMode::EscHint => false,
    };
    footer_from_props_lines(props, None, false, show_shortcuts_hint, show_queue_hint).len() as u16
}

pub(crate) fn render_footer_line(area: Rect, buf: &mut Buffer, line: Line<'static>) {
    Paragraph::new(prefix_lines(
        vec![line],
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

pub(crate) fn render_footer_from_props(
    area: Rect,
    buf: &mut Buffer,
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) {
    Paragraph::new(prefix_lines(
        footer_from_props_lines(
            props,
            collaboration_mode_indicator,
            show_cycle_hint,
            show_shortcuts_hint,
            show_queue_hint,
        ),
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

pub(crate) fn left_fits(area: Rect, left_width: u16) -> bool {
    let max_width = area.width.saturating_sub(FOOTER_INDENT_COLS as u16);
    left_width <= max_width
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummaryHintKind {
    None,
    Shortcuts,
    QueueMessage,
    QueueShort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeftSideState {
    hint: SummaryHintKind,
    show_cycle_hint: bool,
}

fn left_side_line(
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    state: LeftSideState,
    key_hints: FooterKeyHints,
) -> Line<'static> {
    let mut line = Line::from("");
    match state.hint {
        SummaryHintKind::None => {}
        SummaryHintKind::Shortcuts => {
            if let Some(key) = key_hints.toggle_shortcuts {
                line.push_span(key);
                line.push_span(" for shortcuts".dim());
            }
        }
        SummaryHintKind::QueueMessage => {
            if let Some(key) = key_hints.queue {
                line.push_span(key);
                line.push_span(" to queue message".dim());
            }
        }
        SummaryHintKind::QueueShort => {
            if let Some(key) = key_hints.queue {
                line.push_span(key);
                line.push_span(" to queue".dim());
            }
        }
    };

    if let Some(collaboration_mode_indicator) = collaboration_mode_indicator {
        if !matches!(state.hint, SummaryHintKind::None) {
            line.push_span(" · ".dim());
        }
        line.push_span(collaboration_mode_indicator.styled_span(state.show_cycle_hint));
    }

    line
}

pub(crate) enum SummaryLeft {
    Default,
    Custom(Line<'static>),
    None,
}

pub(crate) fn single_line_footer_layout(
    area: Rect,
    context_width: u16,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
    key_hints: FooterKeyHints,
) -> (SummaryLeft, bool) {
    let hint_kind = if show_queue_hint {
        SummaryHintKind::QueueMessage
    } else if show_shortcuts_hint {
        SummaryHintKind::Shortcuts
    } else {
        SummaryHintKind::None
    };
    let default_state = LeftSideState {
        hint: hint_kind,
        show_cycle_hint,
    };
    let default_line = left_side_line(collaboration_mode_indicator, default_state, key_hints);
    let default_width = default_line.width() as u16;
    if default_width > 0 && can_show_left_with_context(area, default_width, context_width) {
        return (SummaryLeft::Default, true);
    }

    let state_line = |state: LeftSideState| -> Line<'static> {
        if state == default_state {
            default_line.clone()
        } else {
            left_side_line(collaboration_mode_indicator, state, key_hints)
        }
    };
    let state_width = |state: LeftSideState| -> u16 { state_line(state).width() as u16 };
    let context_requires_cycle_hint = show_cycle_hint && !show_queue_hint;

    if show_queue_hint {
        let queue_states = [
            default_state,
            LeftSideState {
                hint: SummaryHintKind::QueueMessage,
                show_cycle_hint: false,
            },
            LeftSideState {
                hint: SummaryHintKind::QueueShort,
                show_cycle_hint: false,
            },
        ];

        let mut previous_state: Option<LeftSideState> = None;
        for state in queue_states {
            if previous_state == Some(state) {
                continue;
            }
            previous_state = Some(state);
            let width = state_width(state);
            if width > 0 && can_show_left_with_context(area, width, context_width) {
                if state == default_state {
                    return (SummaryLeft::Default, true);
                }
                return (SummaryLeft::Custom(state_line(state)), true);
            }
        }

        let mut previous_state: Option<LeftSideState> = None;
        for state in queue_states {
            if previous_state == Some(state) {
                continue;
            }
            previous_state = Some(state);
            let width = state_width(state);
            if width > 0 && left_fits(area, width) {
                if state == default_state {
                    return (SummaryLeft::Default, false);
                }
                return (SummaryLeft::Custom(state_line(state)), false);
            }
        }
    } else if collaboration_mode_indicator.is_some() {
        if show_cycle_hint {
            let cycle_state = LeftSideState {
                hint: SummaryHintKind::None,
                show_cycle_hint: true,
            };
            let cycle_width = state_width(cycle_state);
            if cycle_width > 0 && can_show_left_with_context(area, cycle_width, context_width) {
                return (SummaryLeft::Custom(state_line(cycle_state)), true);
            }
            if cycle_width > 0 && left_fits(area, cycle_width) {
                return (SummaryLeft::Custom(state_line(cycle_state)), false);
            }
        }

        let mode_only_state = LeftSideState {
            hint: SummaryHintKind::None,
            show_cycle_hint: false,
        };
        let mode_only_width = state_width(mode_only_state);
        if !context_requires_cycle_hint
            && mode_only_width > 0
            && can_show_left_with_context(area, mode_only_width, context_width)
        {
            return (SummaryLeft::Custom(state_line(mode_only_state)), true);
        }
        if mode_only_width > 0 && left_fits(area, mode_only_width) {
            return (SummaryLeft::Custom(state_line(mode_only_state)), false);
        }
    }

    if let Some(collaboration_mode_indicator) = collaboration_mode_indicator {
        let mode_only_state = LeftSideState {
            hint: SummaryHintKind::None,
            show_cycle_hint: false,
        };
        let mode_only_width = left_side_line(
            Some(collaboration_mode_indicator),
            mode_only_state,
            key_hints,
        )
        .width() as u16;
        if !context_requires_cycle_hint
            && can_show_left_with_context(area, mode_only_width, context_width)
        {
            return (
                SummaryLeft::Custom(left_side_line(
                    Some(collaboration_mode_indicator),
                    mode_only_state,
                    key_hints,
                )),
                true,
            );
        }
        if left_fits(area, mode_only_width) {
            return (
                SummaryLeft::Custom(left_side_line(
                    Some(collaboration_mode_indicator),
                    mode_only_state,
                    key_hints,
                )),
                false,
            );
        }
    }

    (SummaryLeft::None, true)
}

pub(crate) fn mode_indicator_line(
    indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
) -> Option<Line<'static>> {
    indicator.map(|indicator| Line::from(vec![indicator.styled_span(show_cycle_hint)]))
}

pub(crate) fn goal_status_indicator_line(
    indicator: Option<&GoalStatusIndicator>,
) -> Option<Line<'static>> {
    let indicator = indicator?;
    let label = match indicator {
        GoalStatusIndicator::Active { usage } => {
            if let Some(usage) = usage {
                format!("Pursuing goal ({usage})")
            } else {
                "Pursuing goal".to_string()
            }
        }
        GoalStatusIndicator::Paused => "Goal paused (/goal resume)".to_string(),
        GoalStatusIndicator::Blocked => "Goal blocked (/goal resume)".to_string(),
        GoalStatusIndicator::UsageLimited => "Goal hit usage limits (/goal resume)".to_string(),
        GoalStatusIndicator::BudgetLimited { usage } => {
            if let Some(usage) = usage {
                format!("Goal unmet ({usage})")
            } else {
                "Goal abandoned".to_string()
            }
        }
        GoalStatusIndicator::Complete { usage } => {
            if let Some(usage) = usage {
                format!("Goal achieved ({usage})")
            } else {
                "Goal achieved".to_string()
            }
        }
    };

    Some(Line::from(vec![Span::from(label).magenta()]))
}

pub(crate) fn status_line_right_indicator_line(
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    goal_status_indicator: Option<&GoalStatusIndicator>,
    ide_context_active: bool,
    show_cycle_hint: bool,
) -> Option<Line<'static>> {
    let primary_indicator = mode_indicator_line(collaboration_mode_indicator, show_cycle_hint)
        .or_else(|| goal_status_indicator_line(goal_status_indicator));
    let ide_context_indicator = ide_context_active.then(|| Line::from(vec!["IDE context".cyan()]));
    let mut line: Option<Line<'static>> = None;

    for indicator in [primary_indicator, ide_context_indicator]
        .into_iter()
        .flatten()
    {
        if let Some(line) = line.as_mut() {
            line.push_span(" · ".dim());
            for span in indicator.spans {
                line.push_span(span);
            }
        } else {
            line = Some(indicator);
        }
    }

    line
}

pub(crate) fn side_conversation_context_line(label: &str) -> Line<'static> {
    if let Some(rest) = label.strip_prefix("Side ") {
        Line::from(vec!["Side".magenta().bold(), format!(" {rest}").magenta()])
    } else {
        Line::from(label.to_string()).magenta()
    }
}

fn right_aligned_x(area: Rect, content_width: u16) -> Option<u16> {
    if area.is_empty() {
        return None;
    }

    let right_padding = FOOTER_INDENT_COLS as u16;
    let max_width = area.width.saturating_sub(right_padding);
    if content_width == 0 || max_width == 0 {
        return None;
    }

    if content_width >= max_width {
        return Some(area.x.saturating_add(right_padding));
    }

    Some(
        area.x
            .saturating_add(area.width)
            .saturating_sub(content_width)
            .saturating_sub(right_padding),
    )
}

pub(crate) fn max_left_width_for_right(area: Rect, right_width: u16) -> Option<u16> {
    let context_x = right_aligned_x(area, right_width)?;
    let left_start = area.x + FOOTER_INDENT_COLS as u16;

    let gap = FOOTER_CONTEXT_GAP_COLS;

    if context_x <= left_start + gap {
        return Some(0);
    }

    Some(context_x.saturating_sub(left_start + gap))
}

pub(crate) fn can_show_left_with_context(area: Rect, left_width: u16, context_width: u16) -> bool {
    let Some(context_x) = right_aligned_x(area, context_width) else {
        return true;
    };
    if left_width == 0 {
        return true;
    }
    let left_extent = FOOTER_INDENT_COLS as u16 + left_width + FOOTER_CONTEXT_GAP_COLS;
    left_extent <= context_x.saturating_sub(area.x)
}

pub(crate) fn render_context_right(area: Rect, buf: &mut Buffer, line: &Line<'static>) {
    if area.is_empty() {
        return;
    }

    let context_width = line.width() as u16;
    let Some(mut x) = right_aligned_x(area, context_width) else {
        return;
    };
    let y = area.y + area.height.saturating_sub(1);
    let max_x = area.x.saturating_add(area.width);

    for span in &line.spans {
        if x >= max_x {
            break;
        }
        let span_width = span.width() as u16;
        if span_width == 0 {
            continue;
        }
        let remaining = max_x.saturating_sub(x);
        let draw_width = span_width.min(remaining);
        buf.set_span(x, y, span, draw_width);
        x = x.saturating_add(span_width);
    }
}

pub(crate) fn inset_footer_hint_area(mut area: Rect) -> Rect {
    if area.width > 2 {
        area.x += 2;
        area.width = area.width.saturating_sub(2);
    }
    area
}

pub(crate) fn render_footer_hint_items(area: Rect, buf: &mut Buffer, items: &[(String, String)]) {
    if items.is_empty() {
        return;
    }

    footer_hint_items_line(items).render(inset_footer_hint_area(area), buf);
}

fn footer_from_props_lines(
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> Vec<Line<'static>> {
    let key_hints = props.key_hints;
    if let Some(status_line) = passive_footer_status_line(props) {
        return vec![status_line];
    }
    match props.mode {
        FooterMode::QuitShortcutReminder => {
            vec![quit_shortcut_reminder_line(props.quit_shortcut_key)]
        }
        FooterMode::HistorySearch => vec![Line::from("reverse-i-search: ").dim()],
        FooterMode::ComposerEmpty => {
            let state = LeftSideState {
                hint: if show_shortcuts_hint {
                    SummaryHintKind::Shortcuts
                } else {
                    SummaryHintKind::None
                },
                show_cycle_hint,
            };
            vec![left_side_line(
                collaboration_mode_indicator,
                state,
                key_hints,
            )]
        }
        FooterMode::ShortcutOverlay => {
            let state = ShortcutsState {
                use_shift_enter_hint: props.use_shift_enter_hint,
                esc_backtrack_hint: props.esc_backtrack_hint,
                is_task_running: props.is_task_running,
                queue_submissions: props.queue_submissions,
                is_wsl: props.is_wsl,
                collaboration_modes_enabled: props.collaboration_modes_enabled,
                key_hints,
            };
            shortcut_overlay_lines(state)
        }
        FooterMode::EscHint => vec![esc_hint_line(props.esc_backtrack_hint)],
        FooterMode::ComposerHasDraft => {
            let state = LeftSideState {
                hint: if show_queue_hint {
                    SummaryHintKind::QueueMessage
                } else if show_shortcuts_hint {
                    SummaryHintKind::Shortcuts
                } else {
                    SummaryHintKind::None
                },
                show_cycle_hint,
            };
            vec![left_side_line(
                collaboration_mode_indicator,
                state,
                key_hints,
            )]
        }
    }
}

pub(crate) fn passive_footer_status_line(props: &FooterProps) -> Option<Line<'static>> {
    if !shows_passive_footer_line(props) {
        return None;
    }

    let mut line = if props.status_line_enabled {
        props.status_line_value.clone()
    } else {
        None
    };

    if let Some(active_agent_label) = props.active_agent_label.as_ref() {
        if let Some(existing) = line.as_mut() {
            existing.spans.push(" · ".dim());
            existing.spans.push(active_agent_label.clone().dim());
        } else {
            line = Some(Line::from(active_agent_label.clone()).dim());
        }
    }

    line
}

pub(crate) fn shows_passive_footer_line(props: &FooterProps) -> bool {
    match props.mode {
        FooterMode::ComposerEmpty => true,
        FooterMode::ComposerHasDraft => !props.is_task_running,
        FooterMode::HistorySearch
        | FooterMode::QuitShortcutReminder
        | FooterMode::ShortcutOverlay
        | FooterMode::EscHint => false,
    }
}

pub(crate) fn uses_passive_footer_status_layout(props: &FooterProps) -> bool {
    props.status_line_enabled && shows_passive_footer_line(props)
}

pub(crate) fn footer_line_width(
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> u16 {
    footer_from_props_lines(
        props,
        collaboration_mode_indicator,
        show_cycle_hint,
        show_shortcuts_hint,
        show_queue_hint,
    )
    .last()
    .map(|line| line.width() as u16)
    .unwrap_or(0)
}

pub(crate) fn footer_hint_items_width(items: &[(String, String)]) -> u16 {
    if items.is_empty() {
        return 0;
    }
    footer_hint_items_line(items).width() as u16
}

fn footer_hint_items_line(items: &[(String, String)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(items.len() * 4);
    for (idx, (key, label)) in items.iter().enumerate() {
        spans.push(" ".into());
        spans.push(key.clone().bold());
        spans.push(format!(" {label}").into());
        if idx + 1 != items.len() {
            spans.push("   ".into());
        }
    }
    Line::from(spans)
}

#[derive(Clone, Copy, Debug)]
struct ShortcutsState {
    use_shift_enter_hint: bool,
    esc_backtrack_hint: bool,
    is_task_running: bool,
    queue_submissions: bool,
    is_wsl: bool,
    collaboration_modes_enabled: bool,
    key_hints: FooterKeyHints,
}

fn quit_shortcut_reminder_line(key: KeyBinding) -> Line<'static> {
    Line::from(vec![key.into(), " again to quit".into()]).dim()
}

fn esc_hint_line(esc_backtrack_hint: bool) -> Line<'static> {
    let esc = key_hint::plain(KeyCode::Esc);
    if esc_backtrack_hint {
        Line::from(vec![esc.into(), " again to edit previous message".into()]).dim()
    } else {
        Line::from(vec![
            esc.into(),
            " ".into(),
            esc.into(),
            " to edit previous message".into(),
        ])
        .dim()
    }
}

fn shortcut_overlay_lines(state: ShortcutsState) -> Vec<Line<'static>> {
    let mut commands = Line::from("");
    let mut shell_commands = Line::from("");
    let mut newline = Line::from("");
    let mut queue_message_tab = Line::from("");
    let mut file_paths = Line::from("");
    let mut paste_image = Line::from("");
    let mut external_editor = Line::from("");
    let mut edit_previous = Line::from("");
    let mut history_search = Line::from("");
    let mut quit = Line::from("");
    let mut show_transcript = Line::from("");
    let mut change_mode = Line::from("");
    let mut reasoning_down = Line::from("");
    let mut reasoning_up = Line::from("");

    for descriptor in SHORTCUTS {
        if let Some(text) = descriptor.overlay_entry(state) {
            match descriptor.id {
                ShortcutId::Commands => commands = text,
                ShortcutId::ShellCommands => shell_commands = text,
                ShortcutId::InsertNewline => newline = text,
                ShortcutId::QueueMessageTab => queue_message_tab = text,
                ShortcutId::FilePaths => file_paths = text,
                ShortcutId::PasteImage => paste_image = text,
                ShortcutId::ExternalEditor => external_editor = text,
                ShortcutId::EditPrevious => edit_previous = text,
                ShortcutId::HistorySearch => history_search = text,
                ShortcutId::Quit => quit = text,
                ShortcutId::ShowTranscript => show_transcript = text,
                ShortcutId::ChangeMode => change_mode = text,
                ShortcutId::ReasoningDown => reasoning_down = text,
                ShortcutId::ReasoningUp => reasoning_up = text,
            }
        }
    }

    let mut ordered = vec![
        commands,
        shell_commands,
        newline,
        queue_message_tab,
        file_paths,
        paste_image,
        external_editor,
        edit_previous,
        history_search,
        quit,
        reasoning_down,
        reasoning_up,
    ];
    if change_mode.width() > 0 {
        ordered.push(change_mode);
    }
    ordered.push(show_transcript);

    let mut lines = build_columns(ordered);
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        "customize shortcuts with ".into(),
        "/keymap".cyan(),
    ]));
    lines
}

fn build_columns(entries: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if entries.is_empty() {
        return Vec::new();
    }

    const COLUMNS: usize = 2;
    const COLUMN_PADDING: [usize; COLUMNS] = [4, 4];
    const COLUMN_GAP: usize = 4;

    let rows = entries.len().div_ceil(COLUMNS);
    let target_len = rows * COLUMNS;
    let mut entries = entries;
    if entries.len() < target_len {
        entries.extend(std::iter::repeat_n(
            Line::from(""),
            target_len - entries.len(),
        ));
    }

    let mut column_widths = [0usize; COLUMNS];

    for (idx, entry) in entries.iter().enumerate() {
        let column = idx % COLUMNS;
        column_widths[column] = column_widths[column].max(entry.width());
    }

    for (idx, width) in column_widths.iter_mut().enumerate() {
        *width += COLUMN_PADDING[idx];
    }

    entries
        .chunks(COLUMNS)
        .map(|chunk| {
            let mut line = Line::from("");
            for (col, entry) in chunk.iter().enumerate() {
                line.extend(entry.spans.clone());
                if col < COLUMNS - 1 {
                    let target_width = column_widths[col];
                    let padding = target_width.saturating_sub(entry.width()) + COLUMN_GAP;
                    line.push_span(Span::from(" ".repeat(padding)));
                }
            }
            line.dim()
        })
        .collect()
}

pub(crate) fn context_window_line(percent: Option<i64>, used_tokens: Option<i64>) -> Line<'static> {
    if let Some(percent) = percent {
        let percent = percent.clamp(0, 100);
        return Line::from(vec![Span::from(format!("{percent}% context left")).dim()]);
    }

    if let Some(tokens) = used_tokens {
        let used_fmt = format_tokens_compact(tokens);
        return Line::from(vec![Span::from(format!("{used_fmt} used")).dim()]);
    }

    Line::from(vec![Span::from("100% context left").dim()])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShortcutId {
    Commands,
    ShellCommands,
    InsertNewline,
    QueueMessageTab,
    FilePaths,
    PasteImage,
    ExternalEditor,
    EditPrevious,
    HistorySearch,
    Quit,
    ShowTranscript,
    ChangeMode,
    ReasoningDown,
    ReasoningUp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShortcutBinding {
    key: KeyBinding,
    condition: DisplayCondition,
}

impl ShortcutBinding {
    fn matches(&self, state: ShortcutsState) -> bool {
        self.condition.matches(state)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayCondition {
    Always,
    WhenShiftEnterHint,
    WhenNotShiftEnterHint,
    WhenUnderWSL,
    WhenCollaborationModesEnabled,
}

impl DisplayCondition {
    fn matches(self, state: ShortcutsState) -> bool {
        match self {
            DisplayCondition::Always => true,
            DisplayCondition::WhenShiftEnterHint => state.use_shift_enter_hint,
            DisplayCondition::WhenNotShiftEnterHint => !state.use_shift_enter_hint,
            DisplayCondition::WhenUnderWSL => state.is_wsl,
            DisplayCondition::WhenCollaborationModesEnabled => state.collaboration_modes_enabled,
        }
    }
}

struct ShortcutDescriptor {
    id: ShortcutId,
    bindings: &'static [ShortcutBinding],
    prefix: &'static str,
    label: &'static str,
}

impl ShortcutDescriptor {
    fn binding_for(&self, state: ShortcutsState) -> Option<&'static ShortcutBinding> {
        self.bindings.iter().find(|binding| binding.matches(state))
    }

    fn overlay_entry(&self, state: ShortcutsState) -> Option<Line<'static>> {
        let key = match self.id {
            ShortcutId::InsertNewline => state.key_hints.insert_newline,
            ShortcutId::QueueMessageTab => state.key_hints.queue,
            ShortcutId::ExternalEditor => state.key_hints.external_editor,
            ShortcutId::EditPrevious => state.key_hints.edit_previous,
            ShortcutId::ShowTranscript => state.key_hints.show_transcript,
            ShortcutId::HistorySearch => state.key_hints.history_search,
            ShortcutId::ReasoningDown => state.key_hints.reasoning_down,
            ShortcutId::ReasoningUp => state.key_hints.reasoning_up,
            ShortcutId::Commands
            | ShortcutId::ShellCommands
            | ShortcutId::FilePaths
            | ShortcutId::PasteImage
            | ShortcutId::Quit
            | ShortcutId::ChangeMode => self.binding_for(state).map(|binding| binding.key),
        }?;
        let mut line = Line::from(vec![self.prefix.into(), key.into()]);
        match self.id {
            ShortcutId::QueueMessageTab => {
                if state.is_task_running || state.queue_submissions {
                    line.push_span(" to queue message");
                } else {
                    line.push_span(" to submit message");
                }
            }
            ShortcutId::EditPrevious => {
                if state.esc_backtrack_hint {
                    line.push_span(" again to edit previous message");
                } else {
                    line.extend(vec![
                        " ".into(),
                        key.into(),
                        " to edit previous message".into(),
                    ]);
                }
            }
            ShortcutId::Quit => {
                if state.is_task_running {
                    line.push_span(" to interrupt");
                } else {
                    line.push_span(" to exit");
                }
            }
            _ => line.push_span(self.label),
        };
        Some(line)
    }
}

const SHORTCUTS: &[ShortcutDescriptor] = &[
    ShortcutDescriptor {
        id: ShortcutId::Commands,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('/')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for commands",
    },
    ShortcutDescriptor {
        id: ShortcutId::ShellCommands,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('!')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for shell commands",
    },
    ShortcutDescriptor {
        id: ShortcutId::InsertNewline,
        bindings: &[
            ShortcutBinding {
                key: key_hint::shift(KeyCode::Enter),
                condition: DisplayCondition::WhenShiftEnterHint,
            },
            ShortcutBinding {
                key: key_hint::ctrl(KeyCode::Char('j')),
                condition: DisplayCondition::WhenNotShiftEnterHint,
            },
        ],
        prefix: "",
        label: " for newline",
    },
    ShortcutDescriptor {
        id: ShortcutId::QueueMessageTab,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Tab),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to queue message",
    },
    ShortcutDescriptor {
        id: ShortcutId::FilePaths,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('@')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for file paths",
    },
    ShortcutDescriptor {
        id: ShortcutId::PasteImage,
        bindings: &[
            ShortcutBinding {
                key: key_hint::ctrl_alt(KeyCode::Char('v')),
                condition: DisplayCondition::WhenUnderWSL,
            },
            ShortcutBinding {
                key: key_hint::ctrl(KeyCode::Char('v')),
                condition: DisplayCondition::Always,
            },
        ],
        prefix: "",
        label: " to paste images",
    },
    ShortcutDescriptor {
        id: ShortcutId::ExternalEditor,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('g')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to edit in external editor",
    },
    ShortcutDescriptor {
        id: ShortcutId::EditPrevious,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Esc),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: "",
    },
    ShortcutDescriptor {
        id: ShortcutId::HistorySearch,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('r')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " search history",
    },
    ShortcutDescriptor {
        id: ShortcutId::Quit,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('c')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to exit",
    },
    ShortcutDescriptor {
        id: ShortcutId::ShowTranscript,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('t')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to view transcript",
    },
    ShortcutDescriptor {
        id: ShortcutId::ChangeMode,
        bindings: &[ShortcutBinding {
            key: key_hint::shift(KeyCode::Tab),
            condition: DisplayCondition::WhenCollaborationModesEnabled,
        }],
        prefix: "",
        label: " to change mode",
    },
    ShortcutDescriptor {
        id: ShortcutId::ReasoningDown,
        bindings: &[ShortcutBinding {
            key: key_hint::alt(KeyCode::Char(',')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " reasoning down",
    },
    ShortcutDescriptor {
        id: ShortcutId::ReasoningUp,
        bindings: &[ShortcutBinding {
            key: key_hint::alt(KeyCode::Char('.')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " reasoning up",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
    use crate::test_backend::VT100Backend;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::Backend;
    use ratatui::backend::TestBackend;

    fn snapshot_footer(name: &str, props: FooterProps) {
        snapshot_footer_with_mode_indicator(name, 80, &props, None);
    }

    fn snapshot_footer_with_context(
        name: &str,
        props: FooterProps,
        percent: Option<i64>,
        used_tokens: Option<i64>,
    ) {
        snapshot_footer_with_mode_indicator_and_context(
            name,
            80,
            &props,
            None,
            context_window_line(percent, used_tokens),
        );
    }

    fn draw_footer_frame<B: Backend>(
        terminal: &mut Terminal<B>,
        height: u16,
        props: &FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
        ide_context_active: bool,
        context_line: Line<'static>,
    ) {
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, f.area().width, height);
                let show_cycle_hint = !props.is_task_running;
                let show_shortcuts_hint = match props.mode {
                    FooterMode::ComposerEmpty => true,
                    FooterMode::ComposerHasDraft => false,
                    FooterMode::HistorySearch
                    | FooterMode::QuitShortcutReminder
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let show_queue_hint = match props.mode {
                    FooterMode::ComposerHasDraft => props.is_task_running,
                    FooterMode::HistorySearch
                    | FooterMode::QuitShortcutReminder
                    | FooterMode::ComposerEmpty
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let status_line_active = uses_passive_footer_status_layout(props);
                let passive_status_line = if status_line_active {
                    passive_footer_status_line(props)
                } else {
                    None
                };
                let left_mode_indicator = if status_line_active {
                    None
                } else {
                    collaboration_mode_indicator
                };
                let available_width = area.width.saturating_sub(FOOTER_INDENT_COLS as u16) as usize;
                let mut truncated_status_line = if status_line_active
                    && matches!(
                        props.mode,
                        FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                    ) {
                    passive_status_line.as_ref().map(|line| {
                        truncate_line_with_ellipsis_if_overflow(line.clone(), available_width)
                    })
                } else {
                    None
                };
                let mut left_width = if status_line_active {
                    truncated_status_line
                        .as_ref()
                        .map(|line| line.width() as u16)
                        .unwrap_or(0)
                } else {
                    footer_line_width(
                        props,
                        left_mode_indicator,
                        show_cycle_hint,
                        show_shortcuts_hint,
                        show_queue_hint,
                    )
                };
                let right_line = if status_line_active {
                    let full = status_line_right_indicator_line(
                        collaboration_mode_indicator,
                        None,
                        ide_context_active,
                        show_cycle_hint,
                    );
                    let compact = status_line_right_indicator_line(
                        collaboration_mode_indicator,
                        None,
                        ide_context_active,
                        false,
                    );
                    let full_width = full.as_ref().map(|line| line.width() as u16).unwrap_or(0);
                    if can_show_left_with_context(area, left_width, full_width) {
                        full
                    } else {
                        compact
                    }
                } else {
                    Some(context_line.clone())
                };
                let right_width = right_line
                    .as_ref()
                    .map(|line| line.width() as u16)
                    .unwrap_or(0);
                if status_line_active
                    && let Some(max_left) = max_left_width_for_right(area, right_width)
                    && left_width > max_left
                    && let Some(line) = passive_status_line.as_ref().map(|line| {
                        truncate_line_with_ellipsis_if_overflow(line.clone(), max_left as usize)
                    })
                {
                    left_width = line.width() as u16;
                    truncated_status_line = Some(line);
                }
                let can_show_left_and_context =
                    can_show_left_with_context(area, left_width, right_width);
                if matches!(
                    props.mode,
                    FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                ) {
                    if status_line_active {
                        if let Some(line) = truncated_status_line.clone() {
                            render_footer_line(area, f.buffer_mut(), line);
                        }
                        if can_show_left_and_context && let Some(line) = &right_line {
                            render_context_right(area, f.buffer_mut(), line);
                        }
                    } else {
                        let (summary_left, show_context) = single_line_footer_layout(
                            area,
                            right_width,
                            left_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                            props.key_hints,
                        );
                        match summary_left {
                            SummaryLeft::Default => {
                                render_footer_from_props(
                                    area,
                                    f.buffer_mut(),
                                    props,
                                    left_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                );
                            }
                            SummaryLeft::Custom(line) => {
                                render_footer_line(area, f.buffer_mut(), line);
                            }
                            SummaryLeft::None => {}
                        }
                        if show_context && let Some(line) = &right_line {
                            render_context_right(area, f.buffer_mut(), line);
                        }
                    }
                } else {
                    render_footer_from_props(
                        area,
                        f.buffer_mut(),
                        props,
                        left_mode_indicator,
                        show_cycle_hint,
                        show_shortcuts_hint,
                        show_queue_hint,
                    );
                    let show_context = can_show_left_and_context
                        && !matches!(
                            props.mode,
                            FooterMode::EscHint
                                | FooterMode::HistorySearch
                                | FooterMode::QuitShortcutReminder
                                | FooterMode::ShortcutOverlay
                        );
                    if show_context && let Some(line) = &right_line {
                        render_context_right(area, f.buffer_mut(), line);
                    }
                }
            })
            .unwrap();
    }

    fn snapshot_footer_with_mode_indicator(
        name: &str,
        width: u16,
        props: &FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    ) {
        snapshot_footer_with_mode_indicator_and_context(
            name,
            width,
            props,
            collaboration_mode_indicator,
            context_window_line(None, None),
        );
    }

    fn snapshot_footer_with_mode_indicator_and_context(
        name: &str,
        width: u16,
        props: &FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
        context_line: Line<'static>,
    ) {
        let height = footer_height(props).max(1);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        draw_footer_frame(
            &mut terminal,
            height,
            props,
            collaboration_mode_indicator,
            false,
            context_line,
        );
        assert_snapshot!(name, terminal.backend());
    }

    fn render_footer_with_mode_indicator_and_context(
        width: u16,
        props: &FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
        context_line: Line<'static>,
    ) -> String {
        let height = footer_height(props).max(1);
        let mut terminal = Terminal::new(VT100Backend::new(width, height)).expect("terminal");
        draw_footer_frame(
            &mut terminal,
            height,
            props,
            collaboration_mode_indicator,
            false,
            context_line,
        );
        terminal.backend().vt100().screen().contents()
    }

    fn snapshot_footer_with_indicators(
        name: &str,
        width: u16,
        props: &FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
        ide_context_active: bool,
    ) {
        let height = footer_height(props).max(1);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        draw_footer_frame(
            &mut terminal,
            height,
            props,
            collaboration_mode_indicator,
            ide_context_active,
            context_window_line(None, None),
        );
        assert_snapshot!(name, terminal.backend());
    }

    #[test]
    fn footer_snapshots() {
        snapshot_footer(
            "footer_shortcuts_default",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_shortcuts_shift_and_esc",
            FooterProps {
                mode: FooterMode::ShortcutOverlay,
                esc_backtrack_hint: true,
                use_shift_enter_hint: true,
                is_task_running: false,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints {
                    insert_newline: Some(key_hint::shift(KeyCode::Enter)),
                    ..FooterKeyHints::default_bindings()
                },
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_shortcuts_collaboration_modes_enabled",
            FooterProps {
                mode: FooterMode::ShortcutOverlay,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                queue_submissions: false,
                collaboration_modes_enabled: true,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_shortcuts_running",
            FooterProps {
                mode: FooterMode::ShortcutOverlay,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_ctrl_c_quit_idle",
            FooterProps {
                mode: FooterMode::QuitShortcutReminder,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_ctrl_c_quit_running",
            FooterProps {
                mode: FooterMode::QuitShortcutReminder,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_esc_hint_idle",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
        );

        snapshot_footer(
            "footer_esc_hint_primed",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: true,
                use_shift_enter_hint: false,
                is_task_running: false,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
        );

        snapshot_footer_with_context(
            "footer_shortcuts_context_running",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
            Some(72),
            None,
        );

        snapshot_footer_with_context(
            "footer_context_tokens_used",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
            None,
            Some(123_456),
        );

        snapshot_footer(
            "footer_composer_has_draft_queue_hint_enabled",
            FooterProps {
                mode: FooterMode::ComposerHasDraft,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                queue_submissions: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                status_line_value: None,
                status_line_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
                active_agent_label: None,
            },
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_wide",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_narrow_overlap_hides",
            50,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: true,
            queue_submissions: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_running_hides_hint",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: Some(Line::from("Status line content".to_string())),
            status_line_enabled: true,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer("footer_status_line_overrides_shortcuts", props);

        let props = FooterProps {
            mode: FooterMode::ComposerHasDraft,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: true,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: Some(Line::from("Status line content".to_string())),
            status_line_enabled: true,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer("footer_status_line_yields_to_queue_hint", props);

        let props = FooterProps {
            mode: FooterMode::ComposerHasDraft,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: Some(Line::from("Status line content".to_string())),
            status_line_enabled: true,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer("footer_status_line_overrides_draft_idle", props);

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: true,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator_and_context(
            "footer_status_line_enabled_mode_right",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
            context_window_line(Some(50), None),
        );

        snapshot_footer_with_indicators(
            "footer_status_line_enabled_mode_and_ide_context_right",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
            true,
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator_and_context(
            "footer_status_line_disabled_context_right",
            120,
            &props,
            Some(CollaborationModeIndicator::Plan),
            context_window_line(Some(50), None),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: true,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator_and_context(
            "footer_status_line_enabled_no_mode_right",
            120,
            &props,
            None,
            context_window_line(Some(50), None),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: Some(Line::from(
                "Status line content that should truncate before the mode indicator".to_string(),
            )),
            status_line_enabled: true,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        snapshot_footer_with_mode_indicator_and_context(
            "footer_status_line_truncated_with_gap",
            40,
            &props,
            Some(CollaborationModeIndicator::Plan),
            context_window_line(Some(50), None),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: Some("Robie [explorer]".to_string()),
        };

        snapshot_footer("footer_active_agent_label", props);

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: Some(Line::from("Status line content".to_string())),
            status_line_enabled: true,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: Some("Robie [explorer]".to_string()),
        };

        snapshot_footer("footer_status_line_with_active_agent_label", props);
    }

    #[test]
    fn footer_status_line_truncates_to_keep_mode_indicator() {
        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: Some(Line::from(
                "Status line content that is definitely too long to fit alongside the mode label"
                    .to_string(),
            )),
            status_line_enabled: true,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        };

        let screen = render_footer_with_mode_indicator_and_context(
            80,
            &props,
            Some(CollaborationModeIndicator::Plan),
            context_window_line(Some(50), None),
        );
        let collapsed = screen.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            collapsed.contains("Plan mode"),
            "mode indicator should remain visible"
        );
        assert!(
            !collapsed.contains("shift+tab to cycle"),
            "compact mode indicator should be used when space is tight"
        );
        assert!(
            screen.contains('…'),
            "status line should be truncated with ellipsis to keep mode indicator"
        );
    }

    #[test]
    fn paste_image_shortcut_prefers_ctrl_alt_v_under_wsl() {
        let descriptor = SHORTCUTS
            .iter()
            .find(|descriptor| descriptor.id == ShortcutId::PasteImage)
            .expect("paste image shortcut");

        let is_wsl = {
            #[cfg(target_os = "linux")]
            {
                crate::clipboard_paste::is_probably_wsl()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        };

        let expected_key = if is_wsl {
            key_hint::ctrl_alt(KeyCode::Char('v'))
        } else {
            key_hint::ctrl(KeyCode::Char('v'))
        };

        let actual_key = descriptor
            .binding_for(ShortcutsState {
                use_shift_enter_hint: false,
                esc_backtrack_hint: false,
                is_task_running: false,
                queue_submissions: false,
                is_wsl,
                collaboration_modes_enabled: false,
                key_hints: FooterKeyHints::default_bindings(),
            })
            .expect("shortcut binding")
            .key;

        assert_eq!(actual_key, expected_key);
    }
}
