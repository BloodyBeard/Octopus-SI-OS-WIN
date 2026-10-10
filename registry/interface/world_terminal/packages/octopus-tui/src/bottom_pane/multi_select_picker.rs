use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use octopus_utils_fuzzy_match::fuzzy_match;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Widget;

use super::selection_popup_common::GenericDisplayRow;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::popup_consts::MAX_POPUP_ROWS;
use crate::bottom_pane::scroll_state::ScrollState;
use crate::bottom_pane::selection_popup_common::render_rows_single_line;
use crate::key_hint;
use crate::key_hint::KeyBindingListExt;
use crate::key_hint::is_plain_text_key_event;
use crate::keymap::ListKeymap;
use crate::keymap::RuntimeKeymap;
use crate::keymap::primary_binding;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::Insets;
use crate::render::RectExt;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;
use crate::text_formatting::truncate_text;

const ITEM_NAME_TRUNCATE_LEN: usize = 21;

const SEARCH_PLACEHOLDER: &str = "Type to search";

const SEARCH_PROMPT_PREFIX: &str = "> ";

const SECTION_BREAK_ROW: &str = "  ───────────────────────";

enum Direction {
    Up,
    Down,
}

pub type ChangeCallBack = Box<dyn Fn(&[MultiSelectItem], &AppEventSender) + Send + Sync>;

pub type ConfirmCallback = Box<dyn Fn(&[String], &AppEventSender) + Send + Sync>;

pub type CancelCallback = Box<dyn Fn(&AppEventSender) + Send + Sync>;

pub type PreviewCallback = Box<dyn Fn(&[MultiSelectItem]) -> Option<Line<'static>> + Send + Sync>;

pub(crate) struct MultiSelectItem {
    pub id: String,

    pub name: String,

    pub description: Option<String>,

    pub enabled: bool,

    pub orderable: bool,

    pub section_break_after: bool,
}

impl Default for MultiSelectItem {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: None,
            enabled: false,
            orderable: true,
            section_break_after: false,
        }
    }
}

struct BuiltRows {
    rows: Vec<GenericDisplayRow>,
    state: ScrollState,
}

pub(crate) struct MultiSelectPicker {
    items: Vec<MultiSelectItem>,

    state: ScrollState,

    pub(crate) complete: bool,

    app_event_tx: AppEventSender,

    header: Box<dyn Renderable>,

    footer_hint: Line<'static>,

    search_query: String,

    filtered_indices: Vec<usize>,

    ordering_enabled: bool,

    keymap: ListKeymap,

    preview_builder: Option<PreviewCallback>,

    preview_line: Option<Line<'static>>,

    on_change: Option<ChangeCallBack>,

    on_confirm: Option<ConfirmCallback>,

    on_cancel: Option<CancelCallback>,
}

impl MultiSelectPicker {
    pub fn builder(
        title: String,
        subtitle: Option<String>,
        app_event_tx: AppEventSender,
    ) -> MultiSelectPickerBuilder {
        MultiSelectPickerBuilder::new(title, subtitle, app_event_tx)
    }

    fn apply_filter(&mut self) {
        let previously_selected = self
            .state
            .selected_idx
            .and_then(|visible_idx| self.filtered_indices.get(visible_idx).copied());

        let filter = self.search_query.trim();
        if filter.is_empty() {
            self.filtered_indices = (0..self.items.len()).collect();
        } else {
            let mut matches: Vec<(usize, i32)> = Vec::new();
            for (idx, item) in self.items.iter().enumerate() {
                let display_name = item.name.as_str();
                if let Some((_indices, score)) = match_item(filter, display_name, &item.name) {
                    matches.push((idx, score));
                }
            }

            matches.sort_by(|a, b| {
                a.1.cmp(&b.1).then_with(|| {
                    let an = self.items[a.0].name.as_str();
                    let bn = self.items[b.0].name.as_str();
                    an.cmp(bn)
                })
            });

            self.filtered_indices = matches.into_iter().map(|(idx, _score)| idx).collect();
        }

        let len = self.filtered_indices.len();
        self.state.selected_idx = previously_selected
            .and_then(|actual_idx| {
                self.filtered_indices
                    .iter()
                    .position(|idx| *idx == actual_idx)
            })
            .or_else(|| (len > 0).then_some(0));

        let visible = Self::max_visible_rows(len);
        self.state.clamp_selection(len);
        self.state.ensure_visible(len, visible);
    }

    fn visible_len(&self) -> usize {
        self.filtered_indices.len()
    }

    fn max_visible_rows(len: usize) -> usize {
        MAX_POPUP_ROWS.min(len.max(1))
    }

    fn rows_width(total_width: u16) -> u16 {
        total_width.saturating_sub(2)
    }

    fn rows_height(&self, rows: &BuiltRows) -> u16 {
        rows.rows
            .len()
            .clamp(1, MAX_POPUP_ROWS)
            .try_into()
            .unwrap_or(1)
    }

    fn build_rows(&self) -> BuiltRows {
        let mut rows = Vec::new();
        let mut visible_to_row = Vec::with_capacity(self.filtered_indices.len());
        for (visible_idx, actual_idx) in self.filtered_indices.iter().enumerate() {
            let Some(item) = self.items.get(*actual_idx) else {
                continue;
            };
            visible_to_row.push(rows.len());
            let is_selected = self.state.selected_idx == Some(visible_idx);
            let prefix = if is_selected { '›' } else { ' ' };
            let marker = if item.enabled { 'x' } else { ' ' };
            let item_name = truncate_text(&item.name, ITEM_NAME_TRUNCATE_LEN);
            let name = format!("{prefix} [{marker}] {item_name}");
            rows.push(GenericDisplayRow {
                name,
                description: item.description.clone(),
                ..Default::default()
            });

            if item.section_break_after && visible_idx + 1 < self.filtered_indices.len() {
                rows.push(GenericDisplayRow {
                    name: SECTION_BREAK_ROW.to_string(),
                    is_disabled: true,
                    ..Default::default()
                });
            }
        }

        let selected_idx = self
            .state
            .selected_idx
            .and_then(|visible_idx| visible_to_row.get(visible_idx).copied());
        let scroll_top = visible_to_row
            .get(self.state.scroll_top)
            .copied()
            .unwrap_or(0);
        BuiltRows {
            rows,
            state: ScrollState {
                selected_idx,
                scroll_top,
            },
        }
    }

    fn move_up(&mut self) {
        let len = self.visible_len();
        self.state.move_up_wrap(len);
        let visible = Self::max_visible_rows(len);
        self.state.ensure_visible(len, visible);
    }

    fn move_down(&mut self) {
        let len = self.visible_len();
        self.state.move_down_wrap(len);
        let visible = Self::max_visible_rows(len);
        self.state.ensure_visible(len, visible);
    }

    fn page_up(&mut self) {
        let len = self.visible_len();
        let visible = Self::max_visible_rows(len);
        self.state.page_up_clamped(len, visible);
    }

    fn page_down(&mut self) {
        let len = self.visible_len();
        let visible = Self::max_visible_rows(len);
        self.state.page_down_clamped(len, visible);
    }

    fn jump_top(&mut self) {
        let len = self.visible_len();
        let visible = Self::max_visible_rows(len);
        self.state.jump_top(len, visible);
    }

    fn jump_bottom(&mut self) {
        let len = self.visible_len();
        let visible = Self::max_visible_rows(len);
        self.state.jump_bottom(len, visible);
    }

    fn toggle_selected(&mut self) {
        let Some(idx) = self.state.selected_idx else {
            return;
        };
        let Some(actual_idx) = self.filtered_indices.get(idx).copied() else {
            return;
        };
        let Some(item) = self.items.get_mut(actual_idx) else {
            return;
        };

        item.enabled = !item.enabled;
        self.update_preview_line();
        if let Some(on_change) = &self.on_change {
            on_change(&self.items, &self.app_event_tx);
        }
    }

    fn confirm_selection(&mut self) {
        if self.complete {
            return;
        }
        self.complete = true;

        if let Some(on_confirm) = &self.on_confirm {
            let selected_ids: Vec<String> = self
                .items
                .iter()
                .filter(|item| item.enabled)
                .map(|item| item.id.clone())
                .collect();
            on_confirm(&selected_ids, &self.app_event_tx);
        }
    }

    fn move_selected_item(&mut self, direction: Direction) {
        if !self.search_query.is_empty() {
            return;
        }

        let Some(visible_idx) = self.state.selected_idx else {
            return;
        };
        let Some(actual_idx) = self.filtered_indices.get(visible_idx).copied() else {
            return;
        };

        let len = self.items.len();
        if len == 0 {
            return;
        }

        if !self
            .items
            .get(actual_idx)
            .is_some_and(|item| item.orderable)
        {
            return;
        }

        let new_idx = match direction {
            Direction::Up if actual_idx > 0 => actual_idx - 1,
            Direction::Down if actual_idx + 1 < len => actual_idx + 1,
            _ => return,
        };

        if !self.items.get(new_idx).is_some_and(|item| item.orderable) {
            return;
        }

        self.items.swap(actual_idx, new_idx);

        self.update_preview_line();
        if let Some(on_change) = &self.on_change {
            on_change(&self.items, &self.app_event_tx);
        }

        self.apply_filter();

        let moved_idx = new_idx;
        if let Some(new_visible_idx) = self
            .filtered_indices
            .iter()
            .position(|idx| *idx == moved_idx)
        {
            self.state.selected_idx = Some(new_visible_idx);
        }
    }

    fn update_preview_line(&mut self) {
        self.preview_line = self
            .preview_builder
            .as_ref()
            .and_then(|builder| builder(&self.items));
    }

    pub fn close(&mut self) {
        if self.complete {
            return;
        }
        self.complete = true;
        if let Some(on_cancel) = &self.on_cancel {
            on_cancel(&self.app_event_tx);
        }
    }
}

impl BottomPaneView for MultiSelectPicker {
    fn is_complete(&self) -> bool {
        self.complete
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.close();
        CancellationEvent::Handled
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        let allow_plain_char_navigation = !is_plain_text_key_event(key_event);

        match key_event {
            _ if allow_plain_char_navigation
                && self.ordering_enabled
                && self.keymap.move_left.is_pressed(key_event) =>
            {
                self.move_selected_item(Direction::Up);
            }
            _ if allow_plain_char_navigation
                && self.ordering_enabled
                && self.keymap.move_right.is_pressed(key_event) =>
            {
                self.move_selected_item(Direction::Down);
            }
            _ if allow_plain_char_navigation && self.keymap.move_up.is_pressed(key_event) => {
                self.move_up()
            }
            _ if allow_plain_char_navigation && self.keymap.move_down.is_pressed(key_event) => {
                self.move_down()
            }
            _ if allow_plain_char_navigation && self.keymap.page_up.is_pressed(key_event) => {
                self.page_up()
            }
            _ if allow_plain_char_navigation && self.keymap.page_down.is_pressed(key_event) => {
                self.page_down()
            }
            _ if allow_plain_char_navigation && self.keymap.jump_top.is_pressed(key_event) => {
                self.jump_top()
            }
            _ if allow_plain_char_navigation && self.keymap.jump_bottom.is_pressed(key_event) => {
                self.jump_bottom()
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                self.search_query.pop();
                self.apply_filter();
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.toggle_selected(),
            _ if self.keymap.accept.is_pressed(key_event) => self.confirm_selection(),
            _ if self.keymap.cancel.is_pressed(key_event) => self.close(),
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.search_query.push(c);
                self.apply_filter();
            }
            _ => {}
        }
    }
}

impl Renderable for MultiSelectPicker {
    fn desired_height(&self, width: u16) -> u16 {
        let rows = self.build_rows();
        let rows_height = self.rows_height(&rows);
        let preview_height = if self.preview_line.is_some() { 1 } else { 0 };

        let mut height = self.header.desired_height(width.saturating_sub(4));
        height = height.saturating_add(rows_height + 3);
        height = height.saturating_add(2);
        height.saturating_add(1 + preview_height)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let preview_height = if self.preview_line.is_some() { 1 } else { 0 };
        let footer_height = 1 + preview_height;
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(footer_height)]).areas(area);

        Block::default()
            .style(user_message_style())
            .render(content_area, buf);

        let header_height = self
            .header
            .desired_height(content_area.width.saturating_sub(4));
        let rows = self.build_rows();
        let rows_width = Self::rows_width(content_area.width);
        let rows_height = self.rows_height(&rows);
        let [header_area, _, search_area, list_area] = Layout::vertical([
            Constraint::Max(header_height),
            Constraint::Max(1),
            Constraint::Length(2),
            Constraint::Length(rows_height),
        ])
        .areas(content_area.inset(Insets::vh(1, 2)));

        self.header.render(header_area, buf);

        if search_area.height >= 2 {
            let [placeholder_area, input_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(search_area);
            Line::from(SEARCH_PLACEHOLDER.dim()).render(placeholder_area, buf);
            let line = if self.search_query.is_empty() {
                Line::from(vec![SEARCH_PROMPT_PREFIX.dim()])
            } else {
                Line::from(vec![
                    SEARCH_PROMPT_PREFIX.dim(),
                    self.search_query.clone().into(),
                ])
            };
            line.render(input_area, buf);
        } else if search_area.height > 0 {
            let query_span = if self.search_query.is_empty() {
                SEARCH_PLACEHOLDER.dim()
            } else {
                self.search_query.clone().into()
            };
            Line::from(query_span).render(search_area, buf);
        }

        if list_area.height > 0 {
            let render_area = Rect {
                x: list_area.x.saturating_sub(2),
                y: list_area.y,
                width: rows_width.max(1),
                height: list_area.height,
            };
            render_rows_single_line(
                render_area,
                buf,
                &rows.rows,
                &rows.state,
                render_area.height as usize,
                "no matches",
            );
        }

        let hint_area = if let Some(preview_line) = &self.preview_line {
            let [preview_area, hint_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(footer_area);
            let preview_area = Rect {
                x: preview_area.x + 2,
                y: preview_area.y,
                width: preview_area.width.saturating_sub(2),
                height: preview_area.height,
            };
            let max_preview_width = preview_area.width.saturating_sub(2) as usize;
            let preview_line =
                truncate_line_with_ellipsis_if_overflow(preview_line.clone(), max_preview_width);
            preview_line.render(preview_area, buf);
            hint_area
        } else {
            footer_area
        };
        let hint_area = Rect {
            x: hint_area.x + 2,
            y: hint_area.y,
            width: hint_area.width.saturating_sub(2),
            height: hint_area.height,
        };
        self.footer_hint.clone().dim().render(hint_area, buf);
    }
}

pub(crate) struct MultiSelectPickerBuilder {
    title: String,
    subtitle: Option<String>,
    instructions: Vec<Span<'static>>,
    items: Vec<MultiSelectItem>,
    ordering_enabled: bool,
    app_event_tx: AppEventSender,
    keymap: ListKeymap,
    preview_builder: Option<PreviewCallback>,
    on_change: Option<ChangeCallBack>,
    on_confirm: Option<ConfirmCallback>,
    on_cancel: Option<CancelCallback>,
}

impl MultiSelectPickerBuilder {
    pub fn new(title: String, subtitle: Option<String>, app_event_tx: AppEventSender) -> Self {
        Self {
            title,
            subtitle,
            instructions: Vec::new(),
            items: Vec::new(),
            ordering_enabled: false,
            app_event_tx,
            keymap: RuntimeKeymap::defaults().list,
            preview_builder: None,
            on_change: None,
            on_confirm: None,
            on_cancel: None,
        }
    }

    pub fn items(mut self, items: Vec<MultiSelectItem>) -> Self {
        self.items = items;
        self
    }

    pub fn enable_ordering(mut self) -> Self {
        self.ordering_enabled = true;
        self
    }

    pub fn list_keymap(mut self, keymap: ListKeymap) -> Self {
        self.keymap = keymap;
        self
    }

    pub fn on_preview<F>(mut self, callback: F) -> Self
    where
        F: Fn(&[MultiSelectItem]) -> Option<Line<'static>> + Send + Sync + 'static,
    {
        self.preview_builder = Some(Box::new(callback));
        self
    }

    #[allow(dead_code)]
    pub fn on_change<F>(mut self, callback: F) -> Self
    where
        F: Fn(&[MultiSelectItem], &AppEventSender) + Send + Sync + 'static,
    {
        self.on_change = Some(Box::new(callback));
        self
    }

    pub fn on_confirm<F>(mut self, callback: F) -> Self
    where
        F: Fn(&[String], &AppEventSender) + Send + Sync + 'static,
    {
        self.on_confirm = Some(Box::new(callback));
        self
    }

    pub fn on_cancel<F>(mut self, callback: F) -> Self
    where
        F: Fn(&AppEventSender) + Send + Sync + 'static,
    {
        self.on_cancel = Some(Box::new(callback));
        self
    }

    pub fn build(self) -> MultiSelectPicker {
        let mut header = ColumnRenderable::new();
        header.push(Line::from(self.title.bold()));

        if let Some(subtitle) = self.subtitle {
            header.push(Line::from(subtitle.dim()));
        }

        let instructions = if self.instructions.is_empty() {
            let mut spans = vec![
                "Press ".into(),
                key_hint::plain(KeyCode::Char(' ')).into(),
                " to toggle".into(),
            ];
            if self.ordering_enabled
                && let (Some(move_left), Some(move_right)) = (
                    primary_binding(&self.keymap.move_left),
                    primary_binding(&self.keymap.move_right),
                )
            {
                spans.push("; ".into());
                spans.push(move_left.into());
                spans.push("/".into());
                spans.push(move_right.into());
                spans.push(" to move".into());
            }
            if let Some(accept) = primary_binding(&self.keymap.accept) {
                spans.push("; ".into());
                spans.push(accept.into());
                spans.push(" to confirm and close".into());
            }
            if let Some(cancel) = primary_binding(&self.keymap.cancel) {
                spans.push("; ".into());
                spans.push(cancel.into());
                spans.push(" to close".into());
            }
            spans
        } else {
            self.instructions
        };

        let mut view = MultiSelectPicker {
            items: self.items,
            state: ScrollState::new(),
            complete: false,
            app_event_tx: self.app_event_tx,
            header: Box::new(header),
            footer_hint: Line::from(instructions),
            ordering_enabled: self.ordering_enabled,
            keymap: self.keymap,
            search_query: String::new(),
            filtered_indices: Vec::new(),
            preview_builder: self.preview_builder,
            preview_line: None,
            on_change: self.on_change,
            on_confirm: self.on_confirm,
            on_cancel: self.on_cancel,
        };
        view.apply_filter();
        view.update_preview_line();
        view
    }
}

pub(crate) fn match_item(
    filter: &str,
    display_name: &str,
    name: &str,
) -> Option<(Option<Vec<usize>>, i32)> {
    if let Some((indices, score)) = fuzzy_match(display_name, filter) {
        return Some((Some(indices), score));
    }
    if display_name != name
        && let Some((_indices, score)) = fuzzy_match(name, filter)
    {
        return Some((None, score));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc::unbounded_channel;

    fn test_picker(items: Vec<MultiSelectItem>) -> MultiSelectPicker {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        MultiSelectPicker::builder("Test".to_string(), None, AppEventSender::new(tx))
            .items(items)
            .enable_ordering()
            .build()
    }

    fn item(id: &str, orderable: bool, section_break_after: bool) -> MultiSelectItem {
        MultiSelectItem {
            id: id.to_string(),
            name: id.to_string(),
            orderable,
            section_break_after,
            ..Default::default()
        }
    }

    #[test]
    fn non_orderable_items_cannot_move_or_be_crossed() {
        let mut picker = test_picker(vec![
            item("theme-colors", false, true),
            item("model", true, false),
            item("branch", true, false),
        ]);

        picker.move_selected_item(Direction::Down);
        assert_eq!(
            picker
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["theme-colors", "model", "branch"]
        );

        picker.move_down();
        picker.move_selected_item(Direction::Up);
        assert_eq!(
            picker
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["theme-colors", "model", "branch"]
        );
    }

    #[test]
    fn horizontal_list_keys_reorder_orderable_items() {
        let mut picker = test_picker(vec![
            item("model", true, false),
            item("branch", true, false),
        ]);

        picker.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(
            picker
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["branch", "model"]
        );

        picker.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(
            picker
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["model", "branch"]
        );
    }

    #[test]
    fn section_break_after_item_renders_separator_row() {
        let picker = test_picker(vec![
            item("theme-colors", false, true),
            item("model", true, false),
        ]);

        let rows = picker.build_rows();

        assert_eq!(
            rows.rows
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            vec!["› [ ] theme-colors", SECTION_BREAK_ROW, "  [ ] model"]
        );
        assert_eq!(rows.state.selected_idx, Some(0));
    }

    #[test]
    fn searchable_plain_j_updates_query_instead_of_navigating() {
        let mut picker = test_picker(vec![
            item("alpha", true, false),
            item("jupiter", true, false),
        ]);

        picker.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));

        assert_eq!(picker.search_query, "j");
        assert_eq!(picker.filtered_indices, vec![1]);
        assert_eq!(picker.state.selected_idx, Some(0));
    }

    #[test]
    fn page_and_jump_navigation_use_list_keymap() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let mut keymap = RuntimeKeymap::defaults().list;
        keymap.page_down = vec![key_hint::ctrl(KeyCode::Char('d'))];
        keymap.page_up = vec![key_hint::ctrl(KeyCode::Char('u'))];
        keymap.jump_bottom = vec![key_hint::ctrl(KeyCode::Char('e'))];
        keymap.jump_top = vec![key_hint::ctrl(KeyCode::Char('a'))];
        let mut picker =
            MultiSelectPicker::builder("Test".to_string(), None, AppEventSender::new(tx))
                .items(
                    (0..12)
                        .map(|idx| item(&format!("item-{idx}"), true, false))
                        .collect(),
                )
                .list_keymap(keymap)
                .build();

        picker.handle_key_event(KeyEvent::from(KeyCode::PageDown));
        assert_eq!(picker.state.selected_idx, Some(0));

        picker.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(picker.state.selected_idx, Some(8));

        picker.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(picker.state.selected_idx, Some(0));

        picker.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(picker.state.selected_idx, Some(11));

        picker.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(picker.state.selected_idx, Some(0));
    }
}
