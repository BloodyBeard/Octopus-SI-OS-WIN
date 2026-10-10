use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use octopus_world_terminal_client::HookCatalog;
use octopus_world_terminal_client::HookCatalogItem;
use octopus_world_terminal_client::HookTrustState;
use octopus_world_terminal_client::hook_event_description;
use octopus_world_terminal_client::hook_event_names;
use octopus_world_terminal_client::hook_source_label;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Styled;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::render_menu_surface;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::hooks_surface::HookTrustUpdate;
use crate::key_hint;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ListKeymap;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::renderable::Renderable;
use crate::status::format_directory_display;
use crate::style::accent_style;

const EVENT_COLUMN_WIDTH: usize = 22;
const COUNT_COLUMN_WIDTH: usize = 12;
const MAX_COMMAND_DETAIL_LINES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
enum HooksBrowserPage {
    Events,
    Handlers(String),
}

pub(crate) struct HooksBrowserView {
    catalog: HookCatalog,
    page: HooksBrowserPage,
    state: ScrollState,
    complete: bool,
    app_event_tx: AppEventSender,
    keymap: ListKeymap,
}

impl HooksBrowserView {
    #[cfg(test)]
    pub(crate) fn new(
        hooks: Vec<HookCatalogItem>,
        warnings: Vec<String>,
        errors: Vec<octopus_world_terminal_client::HookCatalogIssue>,
        app_event_tx: AppEventSender,
    ) -> Self {
        Self::from_catalog(
            HookCatalog {
                hooks,
                warnings,
                errors,
            },
            app_event_tx,
            crate::keymap::RuntimeKeymap::defaults().list,
        )
    }

    pub(crate) fn from_catalog(
        mut catalog: HookCatalog,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        catalog.hooks.sort_by_key(|hook| hook.display_order);
        let mut view = Self {
            catalog,
            page: HooksBrowserPage::Events,
            state: ScrollState::new(),
            complete: false,
            app_event_tx,
            keymap,
        };
        if view.page_len() > 0 {
            view.state.selected_idx = Some(
                view.event_rows()
                    .iter()
                    .position(|row| row.needs_review > 0)
                    .unwrap_or(0),
            );
        }
        view
    }

    fn event_rows(&self) -> Vec<EventRow> {
        hook_event_names(&self.catalog)
            .into_iter()
            .map(|event| {
                let installed = self
                    .catalog
                    .hooks
                    .iter()
                    .filter(|hook| hook.event == event)
                    .count();
                let active = self
                    .catalog
                    .hooks
                    .iter()
                    .filter(|hook| hook.event == event && hook.is_active())
                    .count();
                let needs_review = self
                    .catalog
                    .hooks
                    .iter()
                    .filter(|hook| hook.event == event && hook.needs_review())
                    .count();
                EventRow {
                    event,
                    installed,
                    active,
                    needs_review,
                }
            })
            .collect()
    }

    fn handlers_for_event<'a>(
        &'a self,
        event: &'a str,
    ) -> impl Iterator<Item = &'a HookCatalogItem> {
        self.catalog
            .hooks
            .iter()
            .filter(move |hook| hook.event == event)
    }

    fn selected_event(&self) -> Option<String> {
        self.state
            .selected_idx
            .and_then(|idx| hook_event_names(&self.catalog).into_iter().nth(idx))
    }

    fn selected_hook_index(&self, event: &str) -> Option<usize> {
        let selected_visible_idx = self.state.selected_idx?;
        self.catalog
            .hooks
            .iter()
            .enumerate()
            .filter(|(_, hook)| hook.event == event)
            .nth(selected_visible_idx)
            .map(|(idx, _)| idx)
    }

    fn selected_hook(&self, event: &str) -> Option<&HookCatalogItem> {
        self.selected_hook_index(event)
            .and_then(|idx| self.catalog.hooks.get(idx))
    }

    fn move_up(&mut self) {
        let len = self.page_len();
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, self.max_visible_rows());
    }

    fn move_down(&mut self) {
        let len = self.page_len();
        self.state.move_down_wrap(len);
        self.state.ensure_visible(len, self.max_visible_rows());
    }

    fn page_up(&mut self) {
        let len = self.page_len();
        self.state.page_up_clamped(len, self.max_visible_rows());
    }

    fn page_down(&mut self) {
        let len = self.page_len();
        self.state.page_down_clamped(len, self.max_visible_rows());
    }

    fn jump_top(&mut self) {
        let len = self.page_len();
        self.state.jump_top(len, self.max_visible_rows());
    }

    fn jump_bottom(&mut self) {
        let len = self.page_len();
        self.state.jump_bottom(len, self.max_visible_rows());
    }

    fn page_len(&self) -> usize {
        match &self.page {
            HooksBrowserPage::Events => hook_event_names(&self.catalog).len(),
            HooksBrowserPage::Handlers(event) => self.handlers_for_event(event).count(),
        }
    }

    fn max_visible_rows(&self) -> usize {
        MAX_POPUP_ROWS.min(self.page_len().max(1))
    }

    fn open_selected_event(&mut self) {
        let Some(event) = self.selected_event() else {
            return;
        };
        self.page = HooksBrowserPage::Handlers(event);
        self.state = ScrollState::new();
        if self.page_len() > 0 {
            self.state.selected_idx = Some(0);
        }
    }

    fn toggle_selected_hook(&mut self, event: &str) {
        let Some(idx) = self.selected_hook_index(event) else {
            return;
        };
        let Some(hook) = self.catalog.hooks.get_mut(idx) else {
            return;
        };
        if hook.is_managed {
            return;
        }
        if hook.needs_review() {
            return;
        }

        hook.enabled = !hook.enabled;
        self.app_event_tx.send(AppEvent::SetHookEnabled {
            key: hook.key.clone(),
            enabled: hook.enabled,
        });
    }

    fn trust_selected_hook(&mut self, event: &str) {
        let Some(idx) = self.selected_hook_index(event) else {
            return;
        };
        let Some(hook) = self.catalog.hooks.get_mut(idx) else {
            return;
        };
        if !hook.needs_review() {
            return;
        }

        hook.trust = HookTrustState::Trusted;
        self.app_event_tx.send(AppEvent::TrustHook {
            key: hook.key.clone(),
            current_hash: hook.current_hash.clone(),
        });
    }

    fn trust_all_hooks(&mut self) {
        let mut updates = Vec::new();
        for hook in &mut self.catalog.hooks {
            if !hook.needs_review() {
                continue;
            }

            hook.trust = HookTrustState::Trusted;
            updates.push(HookTrustUpdate {
                key: hook.key.clone(),
                current_hash: hook.current_hash.clone(),
            });
        }
        if !updates.is_empty() {
            self.app_event_tx.send(AppEvent::TrustHooks { updates });
        }
    }

    fn close(&mut self) {
        self.complete = true;
    }

    fn return_to_events(&mut self) {
        let selected_event = match &self.page {
            HooksBrowserPage::Events => None,
            HooksBrowserPage::Handlers(event) => Some(event.clone()),
        };
        self.page = HooksBrowserPage::Events;
        self.state = ScrollState::new();
        self.state.selected_idx = selected_event
            .and_then(|event| {
                hook_event_names(&self.catalog)
                    .iter()
                    .position(|candidate| *candidate == event)
            })
            .or_else(|| (self.page_len() > 0).then_some(0));
    }

    fn event_header_lines() -> Vec<Line<'static>> {
        vec![
            "Hooks".bold().into(),
            "Lifecycle hooks from active configuration.".dim().into(),
        ]
    }

    fn review_needed_total_count(&self) -> usize {
        self.catalog
            .hooks
            .iter()
            .filter(|hook| HookCatalogItem::needs_review(hook))
            .count()
    }

    #[allow(clippy::disallowed_methods)]
    fn handler_header_lines(event: &str, review_needed_count: usize) -> Vec<Line<'static>> {
        let mut lines = vec![format!("{event} hooks").bold().into()];
        match review_needed_message(review_needed_count) {
            None => lines.push(
                "Turn hooks on or off. Your changes are saved automatically."
                    .dim()
                    .into(),
            ),
            Some(message) => lines.push(message.yellow().into()),
        }
        lines
    }

    fn review_needed_count(&self, event: &str) -> usize {
        self.handlers_for_event(event)
            .filter(|hook| HookCatalogItem::needs_review(hook))
            .count()
    }

    #[allow(clippy::disallowed_methods)]
    fn event_table_lines(&self) -> Vec<Line<'static>> {
        let rows = self.event_rows();
        let show_review = rows.iter().any(|row| row.needs_review > 0);
        let mut lines = Vec::new();
        let mut header = vec![
            format!("{:<EVENT_COLUMN_WIDTH$}", "Event").into(),
            format!("{:<COUNT_COLUMN_WIDTH$}", "Installed").into(),
            format!("{:<COUNT_COLUMN_WIDTH$}", "Active").into(),
        ];
        if show_review {
            header.push(format!("{:<COUNT_COLUMN_WIDTH$}", "Review").into());
        }
        header.push("Description".into());
        lines.push(Line::from(header));
        for (idx, row) in rows.into_iter().enumerate() {
            let selected = self.state.selected_idx == Some(idx);
            let needs_review = row.needs_review > 0;
            let mut row_line = vec![
                Span::from(format!("{:<EVENT_COLUMN_WIDTH$}", row.event)),
                Span::from(format!("{:<COUNT_COLUMN_WIDTH$}", row.installed)),
                Span::from(format!("{:<COUNT_COLUMN_WIDTH$}", row.active)),
            ];
            if show_review {
                let review_count = Span::from(format!("{:<COUNT_COLUMN_WIDTH$}", row.needs_review));
                row_line.push(if needs_review {
                    review_count.yellow()
                } else {
                    review_count
                });
            }
            row_line.push(Span::from(hook_event_description(&row.event)));

            if selected {
                let style = accent_style();
                for span in &mut row_line {
                    *span = span.clone().set_style(style);
                }
            } else {
                row_line[1] = row_line[1].clone().dim();
                row_line[2] = row_line[2].clone().dim();
                if show_review && !needs_review {
                    row_line[3] = row_line[3].clone().dim();
                }
                let description_idx = row_line.len() - 1;
                row_line[description_idx] = row_line[description_idx].clone().dim();
            }
            lines.push(Line::from(row_line));
        }
        lines
    }

    fn event_issue_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if self.catalog.warnings.is_empty() && self.catalog.errors.is_empty() {
            return lines;
        }

        lines.push("Issues".bold().into());
        lines.extend(
            self.catalog
                .warnings
                .iter()
                .map(|warning| format!("⚠ {warning}").into()),
        );
        lines.extend(
            self.catalog
                .errors
                .iter()
                .map(|error| format!("■ {}: {}", error.path, error.message).red().into()),
        );
        lines
    }

    #[allow(clippy::disallowed_methods)]
    fn event_page_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Self::event_header_lines();
        lines.push(Line::default());

        if let Some(message) = review_needed_message(self.review_needed_total_count()) {
            lines.push(format!("⚠ {message}").yellow().into());
            lines.push(Line::default());
        }

        let issue_lines = self.event_issue_lines();
        if !issue_lines.is_empty() {
            lines.extend(issue_lines);
            lines.push(Line::default());
        }

        lines.extend(self.event_table_lines());
        lines
    }

    #[allow(clippy::disallowed_methods)]
    fn handler_row_lines(&self, event: &str, width: usize) -> Vec<Line<'static>> {
        self.handlers_for_event(event)
            .enumerate()
            .map(|(idx, hook)| {
                let marker = if hook.needs_review() {
                    '!'
                } else if hook.is_active() {
                    'x'
                } else {
                    ' '
                };
                let row = match hook.trust {
                    HookTrustState::Modified => {
                        format!("[{marker}] {} · modified", hook_title(idx))
                    }
                    HookTrustState::Untrusted => format!("[{marker}] {} · new", hook_title(idx)),
                    HookTrustState::Managed | HookTrustState::Trusted => {
                        format!("[{marker}] {}", hook_title(idx))
                    }
                };
                let mut line = Line::from(row);
                line = truncate_line_with_ellipsis_if_overflow(line, width);
                let needs_review = hook.needs_review();
                if self.state.selected_idx == Some(idx) {
                    if needs_review {
                        line = line.yellow().bold();
                    } else {
                        line = line.patch_style(accent_style());
                    }
                } else if needs_review {
                    line = line.yellow();
                } else if hook.is_managed {
                    line = line.dim();
                }
                line
            })
            .collect()
    }

    fn detail_lines(&self, event: &str, width: usize) -> Vec<Line<'static>> {
        let Some(hook) = self.selected_hook(event) else {
            return vec!["No hooks installed for this event.".dim().into()];
        };

        let mut lines = vec![detail_line("Event", event)];
        if let Some(matcher) = hook.matcher.as_deref() {
            lines.extend(detail_wrapped_lines("Matcher", matcher, width, None));
        }
        lines.extend(detail_wrapped_lines(
            "Source",
            &detail_source_value(hook),
            width,
            None,
        ));
        lines.extend(detail_wrapped_lines(
            "Command",
            hook.command.as_deref().unwrap_or("-"),
            width,
            Some(MAX_COMMAND_DETAIL_LINES),
        ));
        lines.push(detail_line("Timeout", &format!("{}s", hook.timeout_sec)));
        lines.push(detail_line("Trust", hook_trust_label(hook.trust)));
        lines
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let hint_area = Rect {
            x: area.x + 2,
            y: area.y,
            width: area.width.saturating_sub(2),
            height: area.height,
        };
        let footer = match &self.page {
            HooksBrowserPage::Events if self.review_needed_total_count() > 0 => Line::from(vec![
                "Press ".into(),
                key_hint::plain(KeyCode::Char('t')).into(),
                " to trust all; ".into(),
                key_hint::plain(KeyCode::Enter).into(),
                " to review hooks; ".into(),
                key_hint::plain(KeyCode::Esc).into(),
                " to close".into(),
            ]),
            HooksBrowserPage::Events => Line::from(vec![
                "Press ".into(),
                key_hint::plain(KeyCode::Enter).into(),
                " to view hooks; ".into(),
                key_hint::plain(KeyCode::Esc).into(),
                " to close".into(),
            ]),
            HooksBrowserPage::Handlers(event) => {
                let selected_hook = self.selected_hook(event);
                if selected_hook.is_none() {
                    Line::from(vec![
                        "Press ".into(),
                        key_hint::plain(KeyCode::Esc).into(),
                        " to go back".into(),
                    ])
                } else if selected_hook.is_some_and(|hook| hook.is_managed) {
                    Line::from(vec![
                        "Managed hooks are always on; press ".into(),
                        key_hint::plain(KeyCode::Esc).into(),
                        " to go back".into(),
                    ])
                } else if selected_hook.is_some_and(HookCatalogItem::needs_review) {
                    Line::from(vec![
                        "Press ".into(),
                        key_hint::plain(KeyCode::Char('t')).into(),
                        " to trust; ".into(),
                        key_hint::plain(KeyCode::Esc).into(),
                        " to go back".into(),
                    ])
                } else {
                    Line::from(vec![
                        "Press ".into(),
                        key_hint::plain(KeyCode::Char(' ')).into(),
                        " or ".into(),
                        key_hint::plain(KeyCode::Enter).into(),
                        " to toggle; ".into(),
                        key_hint::plain(KeyCode::Esc).into(),
                        " to go back".into(),
                    ])
                }
            }
        };
        footer.dim().render(hint_area, buf);
    }
}

impl BottomPaneView for HooksBrowserView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            _ if self.keymap.move_up.is_pressed(key_event) => self.move_up(),
            _ if self.keymap.move_down.is_pressed(key_event) => self.move_down(),
            _ if self.keymap.page_up.is_pressed(key_event) => self.page_up(),
            _ if self.keymap.page_down.is_pressed(key_event) => self.page_down(),
            _ if self.keymap.jump_top.is_pressed(key_event) => self.jump_top(),
            _ if self.keymap.jump_bottom.is_pressed(key_event) => self.jump_bottom(),
            _ if self.keymap.accept.is_pressed(key_event)
                && self.page == HooksBrowserPage::Events =>
            {
                self.open_selected_event()
            }
            _ if self.keymap.accept.is_pressed(key_event) => {
                if let HooksBrowserPage::Handlers(event) = self.page.clone() {
                    self.toggle_selected_hook(&event);
                }
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let HooksBrowserPage::Handlers(event) = self.page.clone() {
                    self.toggle_selected_hook(&event);
                }
            }
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: KeyModifiers::NONE,
                ..
            } => match self.page.clone() {
                HooksBrowserPage::Events => self.trust_all_hooks(),
                HooksBrowserPage::Handlers(event) => self.trust_selected_hook(&event),
            },
            _ if self.keymap.cancel.is_pressed(key_event) => match self.page {
                HooksBrowserPage::Events => self.close(),
                HooksBrowserPage::Handlers(_) => self.return_to_events(),
            },
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.close();
        CancellationEvent::Handled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
}

impl Renderable for HooksBrowserView {
    fn desired_height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(4) as usize;
        let height = match &self.page {
            HooksBrowserPage::Events => self.event_page_lines().len(),
            HooksBrowserPage::Handlers(event) => {
                let row_count = self.handler_row_lines(event, content_width).len();
                let header_line_count =
                    Self::handler_header_lines(event, self.review_needed_count(event)).len();
                if row_count == 0 {
                    header_line_count + 2
                } else {
                    let visible_row_count = row_count.min(MAX_POPUP_ROWS);
                    header_line_count
                        + 1
                        + visible_row_count
                        + 1
                        + self.detail_lines(event, content_width).len()
                }
            }
        };
        (height + 3).try_into().unwrap_or(u16::MAX)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        let content_area = render_menu_surface(content_area, buf);
        let width = content_area.width as usize;
        let lines = match &self.page {
            HooksBrowserPage::Events => self.event_page_lines(),
            HooksBrowserPage::Handlers(event) => {
                let mut lines = Self::handler_header_lines(event, self.review_needed_count(event));
                let rows = self.handler_row_lines(event, width);
                if rows.is_empty() {
                    lines.push(Line::default());
                    lines.push(Line::from(
                        "No hooks installed for this event.".dim().italic(),
                    ));
                    lines.push(Line::default());
                    Paragraph::new(lines).render(content_area, buf);
                    self.render_footer(footer_area, buf);
                    return;
                }
                let list_height = rows.len().clamp(1, MAX_POPUP_ROWS) as u16;
                lines.push(Line::default());
                let header_height = lines.len() as u16;
                let [header_area, list_area, detail_area] = Layout::vertical([
                    Constraint::Length(header_height),
                    Constraint::Length(list_height),
                    Constraint::Fill(1),
                ])
                .areas(content_area);
                Paragraph::new(lines.clone()).render(header_area, buf);
                let visible_rows = rows
                    .into_iter()
                    .skip(self.state.scroll_top)
                    .take(list_height as usize)
                    .collect::<Vec<_>>();
                Paragraph::new(visible_rows).render(list_area, buf);
                let mut detail_lines = vec![Line::default()];
                detail_lines.extend(self.detail_lines(event, width));
                Paragraph::new(detail_lines).render(detail_area, buf);
                self.render_footer(footer_area, buf);
                return;
            }
        };
        Paragraph::new(lines).render(content_area, buf);
        self.render_footer(footer_area, buf);
    }
}

fn review_needed_message(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some("1 hook needs review before it can run.".to_string()),
        count => Some(format!("{count} hooks need review before they can run.")),
    }
}

struct EventRow {
    event: String,
    installed: usize,
    active: usize,
    needs_review: usize,
}

fn hook_trust_label(trust: HookTrustState) -> &'static str {
    match trust {
        HookTrustState::Managed => "Managed",
        HookTrustState::Trusted => "Trusted",
        HookTrustState::Untrusted => "New hook - review required",
        HookTrustState::Modified => "Modified since last trusted - review required",
    }
}

fn hook_title(idx: usize) -> String {
    format!("Hook {}", idx + 1)
}

fn detail_source_value(hook: &HookCatalogItem) -> String {
    match hook.source.as_str() {
        "plugin" => hook
            .plugin_id
            .as_deref()
            .map(|plugin_id| format!("Configured hook source - {plugin_id}"))
            .unwrap_or_else(|| "Configured hook source".to_string()),
        "system"
        | "mdm"
        | "cloudRequirements"
        | "cloudManagedConfig"
        | "legacyManagedConfigFile"
        | "legacyManagedConfigMdm" => hook_source_label(&hook.source).to_string(),
        _ => format!(
            "{} - {}",
            hook_source_label(&hook.source),
            format_directory_display(std::path::Path::new(&hook.source_path), None)
        ),
    }
}

fn detail_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![format!("{label:<10}").into(), value.to_string().dim()])
}

fn detail_wrapped_lines(
    label: &str,
    value: &str,
    width: usize,
    max_lines: Option<usize>,
) -> Vec<Line<'static>> {
    let prefix = format!("{label:<10}");
    let available = width.saturating_sub(prefix.width()).max(1);
    let mut wrapped = textwrap::wrap(value, available).into_iter();
    let first = wrapped.next().unwrap_or_default().into_owned();
    let mut lines = vec![Line::from(vec![prefix.into(), first.dim()])];
    lines
        .extend(wrapped.map(|line| Line::from(vec!["          ".into(), line.into_owned().dim()])));
    let Some(max_lines) = max_lines else {
        return lines;
    };
    if lines.len() <= max_lines {
        return lines;
    }

    lines.truncate(max_lines);
    if let Some(last_line) = lines.last_mut() {
        let prefix_width = last_line.spans[..last_line.spans.len().saturating_sub(1)]
            .iter()
            .map(ratatui::prelude::Span::width)
            .sum::<usize>();
        let max_width = width.saturating_sub(prefix_width);
        let Some(last_span) = last_line.spans.last_mut() else {
            return lines;
        };
        let truncated = truncate_line_with_ellipsis_if_overflow(
            Line::from(format!("{}…", last_span.content)),
            max_width,
        );
        let content = truncated
            .spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect::<String>();
        last_span.content = content.into();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use crate::bottom_pane::bottom_pane_view::BottomPaneView;
    use crate::render::renderable::Renderable;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use insta::assert_snapshot;
    use octopus_world_terminal_client::HookCatalogIssue;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::style::Modifier;
    use tokio::sync::mpsc::unbounded_channel;

    fn render_lines(view: &HooksBrowserView, width: u16) -> String {
        let height = view.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        (0..area.height)
            .map(|row| {
                let rendered = (0..area.width)
                    .map(|col| {
                        let symbol = buf[(area.x + col, area.y + row)].symbol();
                        if symbol.is_empty() {
                            " ".to_string()
                        } else {
                            symbol.to_string()
                        }
                    })
                    .collect::<String>();
                format!("{rendered:width$}", width = area.width as usize)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_buffer(view: &HooksBrowserView, width: u16) -> Buffer {
        let height = view.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        buf
    }

    #[allow(clippy::too_many_arguments)]
    fn hook(
        key: &str,
        event: &str,
        source: &str,
        plugin_id: Option<&str>,
        command: &str,
        enabled: bool,
        is_managed: bool,
        display_order: i64,
    ) -> HookCatalogItem {
        HookCatalogItem {
            key: key.to_string(),
            event: event.to_string(),
            matcher: Some("Bash".to_string()),
            command: Some(command.to_string()),
            timeout_sec: 30,
            source: source.to_string(),
            source_path: "/tmp/hooks.json".to_string(),
            plugin_id: plugin_id.map(str::to_string),
            display_order,
            enabled,
            is_managed,
            current_hash: "sha256:current".to_string(),
            trust: if is_managed {
                HookTrustState::Managed
            } else {
                HookTrustState::Trusted
            },
        }
    }

    fn view() -> HooksBrowserView {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        HooksBrowserView::new(
            vec![
                hook(
                    "plugin:superpowers",
                    "PreToolUse",
                    "plugin",
                    Some("superpowers@openai-curated"),
                    "${CODEX_PLUGIN_ROOT}/hooks/pre-tool-use-check.sh",
                    true,
                    false,
                    0,
                ),
                hook(
                    "path:user-config",
                    "PreToolUse",
                    "user",
                    None,
                    "~/bin/check-shell-with-a-command-that-is-way-too-long-for-the-summary-column.sh",
                    false,
                    false,
                    1,
                ),
                hook(
                    "path:managed",
                    "PermissionRequest",
                    "system",
                    None,
                    "/enterprise/hooks/permission-check.sh",
                    true,
                    true,
                    2,
                ),
            ],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        )
    }

    #[test]
    fn renders_event_browser() {
        let view = view();
        assert_snapshot!("hooks_browser_events", render_lines(&view, 112));
    }

    #[test]
    fn selected_event_rows_use_the_shared_accent_style() {
        let view = view();
        let buf = render_buffer(&view, 112);
        let expected = accent_style();

        let selected_cell = buf
            .content
            .iter()
            .find(|cell| {
                let style = cell.style();
                cell.symbol() == "P"
                    && style.fg == expected.fg
                    && style.add_modifier.contains(Modifier::BOLD)
            })
            .expect("selected event row should use the shared accent style");

        assert_eq!(selected_cell.style().fg, expected.fg);
    }

    #[test]
    fn renders_event_browser_with_review_column_when_needed() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut untrusted_hook = hook(
            "path:untrusted",
            "PreToolUse",
            "user",
            None,
            "/tmp/pre-tool-use-check.sh",
            false,
            false,
            0,
        );
        untrusted_hook.trust = HookTrustState::Untrusted;
        let view = HooksBrowserView::new(
            vec![untrusted_hook],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );

        assert_snapshot!(
            "hooks_browser_events_with_review_column",
            render_lines(&view, 112)
        );
        assert_eq!(
            view.event_table_lines()[1].spans[3].style.fg,
            Some(Color::Cyan)
        );
        assert!(
            view.event_table_lines()[1].spans[3]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn renders_event_browser_with_issues() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let view = HooksBrowserView::new(
            Vec::new(),
            vec!["skipped invalid matcher for PreToolUse".to_string()],
            vec![HookCatalogIssue {
                path: "/tmp/hooks.json".to_string(),
                message: "failed to parse hooks config".to_string(),
            }],
            AppEventSender::new(tx_raw),
        );

        assert_snapshot!("hooks_browser_events_with_issues", render_lines(&view, 112));
    }

    #[test]
    fn renders_handler_browser_with_details() {
        let mut view = view();
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_snapshot!("hooks_browser_handlers", render_lines(&view, 112));
    }

    #[test]
    fn renders_untrusted_enabled_handler_as_inactive() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut untrusted_hook = hook(
            "path:untrusted",
            "PreToolUse",
            "user",
            None,
            "~/bin/untrusted.sh",
            true,
            false,
            0,
        );
        untrusted_hook.trust = HookTrustState::Untrusted;
        let mut view = HooksBrowserView::new(
            vec![untrusted_hook],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));

        assert_snapshot!(
            "hooks_browser_untrusted_enabled_handler",
            render_lines(&view, 112)
        );
    }

    #[test]
    fn review_needed_handler_rows_use_warning_color() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut untrusted_hook = hook(
            "path:untrusted",
            "PreToolUse",
            "user",
            None,
            "~/bin/untrusted.sh",
            false,
            false,
            1,
        );
        untrusted_hook.trust = HookTrustState::Untrusted;
        let mut view = HooksBrowserView::new(
            vec![
                hook(
                    "path:trusted",
                    "PreToolUse",
                    "user",
                    None,
                    "~/bin/trusted.sh",
                    true,
                    false,
                    0,
                ),
                untrusted_hook,
            ],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));

        assert_eq!(
            view.handler_row_lines("PreToolUse", 112)[1].style.fg,
            Some(Color::Yellow)
        );
    }

    #[test]
    fn review_needed_handler_header_uses_warning_color() {
        assert_eq!(
            HooksBrowserView::handler_header_lines("PreToolUse", 1,)[1].spans[0]
                .style
                .fg,
            Some(Color::Yellow)
        );
    }

    #[test]
    fn renders_managed_handler_without_toggle_hint() {
        let mut view = view();
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_snapshot!("hooks_browser_managed_handler", render_lines(&view, 112));
    }

    #[test]
    fn renders_selected_managed_handler() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut view = HooksBrowserView::new(
            vec![
                hook(
                    "path:managed-1",
                    "PreToolUse",
                    "system",
                    None,
                    "/enterprise/hooks/pre-tool-use-1.sh",
                    true,
                    true,
                    0,
                ),
                hook(
                    "path:managed-2",
                    "PreToolUse",
                    "system",
                    None,
                    "/enterprise/hooks/pre-tool-use-2.sh",
                    true,
                    true,
                    1,
                ),
            ],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
        assert_snapshot!(
            "hooks_browser_selected_managed_handler",
            render_lines(&view, 112)
        );
    }

    #[test]
    fn renders_scrolled_handler_window() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let hooks = (0..=MAX_POPUP_ROWS)
            .map(|idx| {
                hook(
                    &format!("path:hook-{idx}"),
                    "PreToolUse",
                    "user",
                    None,
                    &format!("/tmp/hook-{idx}.sh"),
                    true,
                    false,
                    idx as i64,
                )
            })
            .collect();
        let mut view =
            HooksBrowserView::new(hooks, Vec::new(), Vec::new(), AppEventSender::new(tx_raw));
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        for _ in 0..MAX_POPUP_ROWS {
            view.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        assert_snapshot!("hooks_browser_scrolled_handlers", render_lines(&view, 112));
    }

    #[test]
    fn renders_command_details_with_three_line_cap() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut capped_command_hook = hook(
            "path:long-command",
            "PreToolUse",
            "user",
            None,
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty",
            true,
            false,
            0,
        );
        capped_command_hook.source_path = "/tmp/h.json".to_string();
        let mut view = HooksBrowserView::new(
            vec![capped_command_hook],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_snapshot!(
            "hooks_browser_capped_command_details",
            render_lines(&view, 44)
        );
    }

    #[test]
    fn renders_empty_handler_browser_message() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut view = HooksBrowserView::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_snapshot!("hooks_browser_empty_handlers", render_lines(&view, 112));
    }

    #[test]
    fn managed_hooks_count_as_active() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let view = HooksBrowserView::new(
            vec![hook(
                "path:managed",
                "PreToolUse",
                "system",
                None,
                "/enterprise/hooks/pre-tool-use-check.sh",
                true,
                true,
                0,
            )],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );

        let rows = view.event_rows();
        let pre_tool_use = rows
            .into_iter()
            .find(|row| row.event == "PreToolUse")
            .expect("pre tool use row");

        assert_eq!(pre_tool_use.installed, 1);
        assert_eq!(pre_tool_use.active, 1);
    }

    #[test]
    fn review_needed_hooks_are_not_active() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut untrusted_hook = hook(
            "path:untrusted",
            "PreToolUse",
            "user",
            None,
            "/tmp/pre-tool-use-check.sh",
            true,
            false,
            0,
        );
        untrusted_hook.trust = HookTrustState::Untrusted;
        let view = HooksBrowserView::new(
            vec![untrusted_hook],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );

        let rows = view.event_rows();
        let pre_tool_use = rows
            .into_iter()
            .find(|row| row.event == "PreToolUse")
            .expect("pre tool use row");

        assert_eq!(pre_tool_use.installed, 1);
        assert_eq!(pre_tool_use.active, 0);
        assert_eq!(pre_tool_use.needs_review, 1);
    }

    #[test]
    fn review_needed_event_is_selected_by_default() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut untrusted_hook = hook(
            "path:untrusted",
            "PermissionRequest",
            "user",
            None,
            "/tmp/permission-request-check.sh",
            false,
            false,
            0,
        );
        untrusted_hook.trust = HookTrustState::Untrusted;
        let view = HooksBrowserView::new(
            vec![untrusted_hook],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );

        assert_eq!(view.selected_event(), Some("PermissionRequest".to_string()));
    }

    #[test]
    fn renders_review_needed_handler() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut untrusted_hook = hook(
            "path:untrusted",
            "PreToolUse",
            "user",
            None,
            "/tmp/pre-tool-use-check.sh",
            false,
            false,
            0,
        );
        untrusted_hook.trust = HookTrustState::Untrusted;
        let mut view = HooksBrowserView::new(
            vec![untrusted_hook],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));

        assert_snapshot!(
            "hooks_browser_review_needed_handler",
            render_lines(&view, 112)
        );
    }

    fn assert_unmanaged_toggle_key(key_code: KeyCode) {
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let mut view = HooksBrowserView::new(
            vec![hook(
                "plugin:superpowers",
                "PreToolUse",
                "plugin",
                Some("superpowers@openai-curated"),
                "hooks/pre-tool-use-check.sh",
                true,
                false,
                0,
            )],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        view.handle_key_event(KeyEvent::from(key_code));

        match rx.try_recv().expect("toggle event") {
            AppEvent::SetHookEnabled { key, enabled } => {
                assert_eq!(key, "plugin:superpowers");
                assert!(!enabled);
            }
            other => panic!("expected hook toggle event, got {other:?}"),
        }
    }

    #[test]
    fn toggle_keys_toggle_unmanaged_handler() {
        for key_code in [KeyCode::Char(' '), KeyCode::Enter] {
            assert_unmanaged_toggle_key(key_code);
        }
    }

    #[test]
    fn space_does_not_toggle_managed_handler() {
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let mut view = HooksBrowserView::new(
            vec![hook(
                "path:managed",
                "PreToolUse",
                "system",
                None,
                "/enterprise/hooks/pre-tool-use-check.sh",
                true,
                true,
                0,
            )],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn trust_key_trusts_review_needed_handler_without_changing_enablement() {
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let mut untrusted_hook = hook(
            "path:untrusted",
            "PreToolUse",
            "user",
            None,
            "/tmp/pre-tool-use-check.sh",
            false,
            false,
            0,
        );
        untrusted_hook.trust = HookTrustState::Untrusted;
        let current_hash = untrusted_hook.current_hash.clone();
        let mut view = HooksBrowserView::new(
            vec![untrusted_hook],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        view.handle_key_event(KeyEvent::from(KeyCode::Char('t')));

        match rx.try_recv().expect("trust event") {
            AppEvent::TrustHook {
                key,
                current_hash: hash_to_trust,
            } => {
                assert_eq!(key, "path:untrusted");
                assert_eq!(hash_to_trust, current_hash);
            }
            other => panic!("expected hook trust event, got {other:?}"),
        }
    }

    #[test]
    fn trust_key_preserves_disabled_modified_handler() {
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let mut modified_hook = hook(
            "path:modified",
            "PreToolUse",
            "user",
            None,
            "/tmp/pre-tool-use-check.sh",
            false,
            false,
            0,
        );
        modified_hook.trust = HookTrustState::Modified;
        let current_hash = modified_hook.current_hash.clone();
        let mut view = HooksBrowserView::new(
            vec![modified_hook],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        view.handle_key_event(KeyEvent::from(KeyCode::Char('t')));

        let hook = view.catalog.hooks.first().expect("trusted hook");
        assert!(!hook.enabled);
        assert_eq!(hook.trust, HookTrustState::Trusted);
        match rx.try_recv().expect("trust event") {
            AppEvent::TrustHook {
                key,
                current_hash: hash_to_trust,
            } => {
                assert_eq!(key, "path:modified");
                assert_eq!(hash_to_trust, current_hash);
            }
            other => panic!("expected hook trust event, got {other:?}"),
        }
    }

    #[test]
    fn trust_key_on_event_page_trusts_all_review_needed_hooks() {
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let mut untrusted_hook = hook(
            "path:untrusted",
            "PreToolUse",
            "user",
            None,
            "/tmp/pre-tool-use-check.sh",
            false,
            false,
            0,
        );
        untrusted_hook.trust = HookTrustState::Untrusted;
        let mut modified_hook = hook(
            "path:modified",
            "Stop",
            "user",
            None,
            "/tmp/stop-check.sh",
            false,
            false,
            1,
        );
        modified_hook.trust = HookTrustState::Modified;
        let mut view = HooksBrowserView::new(
            vec![
                untrusted_hook,
                modified_hook,
                hook(
                    "path:trusted",
                    "PreToolUse",
                    "user",
                    None,
                    "/tmp/trusted.sh",
                    true,
                    false,
                    2,
                ),
            ],
            Vec::new(),
            Vec::new(),
            AppEventSender::new(tx_raw),
        );

        view.handle_key_event(KeyEvent::from(KeyCode::Char('t')));

        assert_eq!(
            view.catalog
                .hooks
                .iter()
                .map(|hook| hook.trust)
                .collect::<Vec<_>>(),
            vec![
                HookTrustState::Trusted,
                HookTrustState::Trusted,
                HookTrustState::Trusted,
            ]
        );
        match rx.try_recv().expect("trust event") {
            AppEvent::TrustHooks { updates } => assert_eq!(
                updates,
                vec![
                    HookTrustUpdate {
                        key: "path:untrusted".to_string(),
                        current_hash: "sha256:current".to_string(),
                    },
                    HookTrustUpdate {
                        key: "path:modified".to_string(),
                        current_hash: "sha256:current".to_string(),
                    },
                ]
            ),
            other => panic!("expected hook trust event, got {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn escape_returns_to_the_selected_event() {
        let mut view = view();
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        view.handle_key_event(KeyEvent::from(KeyCode::Esc));

        assert_eq!(view.page, HooksBrowserPage::Events);
        assert_eq!(view.selected_event(), Some("PermissionRequest".to_string()));
    }

    #[test]
    fn esc_routes_through_the_view() {
        assert!(view().prefer_esc_to_handle_key_event());
    }
}
