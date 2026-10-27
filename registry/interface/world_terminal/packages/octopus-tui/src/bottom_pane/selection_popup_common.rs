use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Widget;
use std::borrow::Cow;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

use crate::key_hint::KeyBinding;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::style::accent_style;
use crate::style::user_message_style;

use super::scroll_state::ScrollState;

#[derive(Default)]
pub(crate) struct GenericDisplayRow {
    pub name: String,
    pub name_prefix_spans: Vec<Span<'static>>,
    pub display_shortcut: Option<KeyBinding>,
    pub match_indices: Option<Vec<usize>>,
    pub description: Option<String>,
    pub category_tag: Option<String>,
    pub disabled_reason: Option<String>,
    pub is_disabled: bool,
    pub wrap_indent: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ColumnWidthMode {
    #[default]
    AutoVisible,
    AutoAllRows,
    Fixed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ColumnWidthConfig {
    pub mode: ColumnWidthMode,
    pub name_column_width: Option<usize>,
}

impl ColumnWidthConfig {
    pub(crate) const fn new(mode: ColumnWidthMode, name_column_width: Option<usize>) -> Self {
        Self {
            mode,
            name_column_width,
        }
    }
}

const FIXED_LEFT_COLUMN_NUMERATOR: usize = 3;
const FIXED_LEFT_COLUMN_DENOMINATOR: usize = 10;

const MENU_SURFACE_INSET_V: u16 = 1;
const MENU_SURFACE_INSET_H: u16 = 2;

pub(crate) fn menu_surface_inset(area: Rect) -> Rect {
    area.inset(Insets::vh(MENU_SURFACE_INSET_V, MENU_SURFACE_INSET_H))
}

pub(crate) const fn menu_surface_padding_height() -> u16 {
    MENU_SURFACE_INSET_V * 2
}

pub(crate) fn render_menu_surface(area: Rect, buf: &mut Buffer) -> Rect {
    if area.is_empty() {
        return area;
    }
    Block::default()
        .style(user_message_style())
        .render(area, buf);
    menu_surface_inset(area)
}

pub(crate) fn wrap_styled_line<'a>(line: &'a Line<'a>, width: u16) -> Vec<Line<'a>> {
    use crate::wrapping::RtOptions;
    use crate::wrapping::word_wrap_line;

    let width = width.max(1) as usize;
    let opts = RtOptions::new(width)
        .initial_indent(Line::from(""))
        .subsequent_indent(Line::from(""));
    word_wrap_line(line, opts)
}

fn line_to_owned(line: Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .into_iter()
            .map(|span| Span {
                style: span.style,
                content: Cow::Owned(span.content.into_owned()),
            })
            .collect(),
    }
}

fn compute_desc_col(
    rows_all: &[GenericDisplayRow],
    start_idx: usize,
    visible_items: usize,
    content_width: u16,
    column_width: ColumnWidthConfig,
) -> usize {
    if content_width <= 1 {
        return 0;
    }

    let max_desc_col = content_width.saturating_sub(1) as usize;
    let max_auto_desc_col = max_desc_col.min(
        ((content_width as usize * (FIXED_LEFT_COLUMN_DENOMINATOR - FIXED_LEFT_COLUMN_NUMERATOR))
            / FIXED_LEFT_COLUMN_DENOMINATOR)
            .max(1),
    );
    match column_width.mode {
        ColumnWidthMode::Fixed => ((content_width as usize * FIXED_LEFT_COLUMN_NUMERATOR)
            / FIXED_LEFT_COLUMN_DENOMINATOR)
            .clamp(1, max_desc_col),
        ColumnWidthMode::AutoVisible | ColumnWidthMode::AutoAllRows => {
            let max_name_width = match column_width.mode {
                ColumnWidthMode::AutoVisible => rows_all
                    .iter()
                    .enumerate()
                    .skip(start_idx)
                    .take(visible_items)
                    .map(|(_, row)| {
                        let mut spans = row.name_prefix_spans.clone();
                        spans.push(row.name.clone().into());
                        if row.disabled_reason.is_some() {
                            spans.push(" (disabled)".dim());
                        }
                        Line::from(spans).width()
                    })
                    .max()
                    .unwrap_or(0),
                ColumnWidthMode::AutoAllRows => rows_all
                    .iter()
                    .map(|row| {
                        let mut spans = row.name_prefix_spans.clone();
                        spans.push(row.name.clone().into());
                        if row.disabled_reason.is_some() {
                            spans.push(" (disabled)".dim());
                        }
                        Line::from(spans).width()
                    })
                    .max()
                    .unwrap_or(0),
                ColumnWidthMode::Fixed => 0,
            };

            column_width
                .name_column_width
                .map(|width| width.max(max_name_width))
                .unwrap_or(max_name_width)
                .saturating_add(2)
                .min(max_auto_desc_col)
        }
    }
}

fn wrap_indent(row: &GenericDisplayRow, desc_col: usize, max_width: u16) -> usize {
    let max_indent = max_width.saturating_sub(1) as usize;
    let indent = row.wrap_indent.unwrap_or_else(|| {
        if row.description.is_some() || row.disabled_reason.is_some() {
            desc_col
        } else {
            0
        }
    });
    indent.min(max_indent)
}

fn should_wrap_name_in_column(row: &GenericDisplayRow) -> bool {
    row.wrap_indent.is_some()
        && row.description.is_some()
        && row.disabled_reason.is_none()
        && row.match_indices.is_none()
        && row.display_shortcut.is_none()
        && row.category_tag.is_none()
        && row.name_prefix_spans.is_empty()
}

fn wrap_two_column_row(row: &GenericDisplayRow, desc_col: usize, width: u16) -> Vec<Line<'static>> {
    let Some(description) = row.description.as_deref() else {
        return Vec::new();
    };

    let width = width.max(1);
    let max_desc_col = width.saturating_sub(1) as usize;
    if max_desc_col == 0 {
        return Vec::new();
    }

    let desc_col = desc_col.clamp(1, max_desc_col);
    let left_width = desc_col.saturating_sub(2).max(1);
    let right_width = width.saturating_sub(desc_col as u16).max(1) as usize;
    let name_wrap_indent = row
        .wrap_indent
        .unwrap_or(0)
        .min(left_width.saturating_sub(1));

    let name_subsequent_indent = " ".repeat(name_wrap_indent);
    let name_options = textwrap::Options::new(left_width)
        .initial_indent("")
        .subsequent_indent(name_subsequent_indent.as_str());
    let name_lines = textwrap::wrap(row.name.as_str(), name_options);

    let desc_options = textwrap::Options::new(right_width).initial_indent("");
    let desc_lines = textwrap::wrap(description, desc_options);

    let rows = name_lines.len().max(desc_lines.len()).max(1);
    let mut out = Vec::with_capacity(rows);
    for idx in 0..rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if let Some(name) = name_lines.get(idx) {
            spans.push(name.to_string().into());
        }

        if let Some(desc) = desc_lines.get(idx) {
            let left_used = spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum::<usize>();
            let gap = if left_used == 0 {
                desc_col
            } else {
                desc_col.saturating_sub(left_used).max(2)
            };
            if gap > 0 {
                spans.push(" ".repeat(gap).into());
            }
            spans.push(desc.to_string().dim());
        }

        out.push(Line::from(spans));
    }

    out
}

fn wrap_standard_row(row: &GenericDisplayRow, desc_col: usize, width: u16) -> Vec<Line<'static>> {
    use crate::wrapping::RtOptions;
    use crate::wrapping::word_wrap_line;

    let full_line = build_full_line(row, desc_col);
    let continuation_indent = wrap_indent(row, desc_col, width);
    let options = RtOptions::new(width.max(1) as usize)
        .initial_indent(Line::from(""))
        .subsequent_indent(Line::from(" ".repeat(continuation_indent)));
    word_wrap_line(&full_line, options)
        .into_iter()
        .map(line_to_owned)
        .collect()
}

fn wrap_row_lines(row: &GenericDisplayRow, desc_col: usize, width: u16) -> Vec<Line<'static>> {
    if should_wrap_name_in_column(row) {
        let wrapped = wrap_two_column_row(row, desc_col, width);
        if !wrapped.is_empty() {
            return wrapped;
        }
    }

    wrap_standard_row(row, desc_col, width)
}

fn apply_row_state_style(lines: &mut [Line<'static>], selected: bool, is_disabled: bool) {
    if selected {
        for line in lines.iter_mut() {
            line.spans.iter_mut().for_each(|span| {
                span.style = accent_style();
            });
        }
    }
    if is_disabled {
        for line in lines.iter_mut() {
            line.spans.iter_mut().for_each(|span| {
                span.style = span.style.dim();
            });
        }
    }
}

fn compute_item_window_start(
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_items: usize,
) -> usize {
    if rows_all.is_empty() || max_items == 0 {
        return 0;
    }

    let mut start_idx = state.scroll_top.min(rows_all.len().saturating_sub(1));
    if let Some(sel) = state.selected_idx {
        if sel < start_idx {
            start_idx = sel;
        } else {
            let bottom = start_idx.saturating_add(max_items.saturating_sub(1));
            if sel > bottom {
                start_idx = sel + 1 - max_items;
            }
        }
    }
    start_idx
}

fn is_selected_visible_in_wrapped_viewport(
    rows_all: &[GenericDisplayRow],
    start_idx: usize,
    max_items: usize,
    selected_idx: usize,
    desc_col: usize,
    width: u16,
    viewport_height: u16,
) -> bool {
    if viewport_height == 0 {
        return false;
    }

    let mut used_lines = 0usize;
    let viewport_height = viewport_height as usize;
    for (idx, row) in rows_all.iter().enumerate().skip(start_idx).take(max_items) {
        let row_lines = wrap_row_lines(row, desc_col, width).len().max(1);
        if used_lines > 0 && used_lines.saturating_add(row_lines) > viewport_height {
            break;
        }
        if idx == selected_idx {
            return true;
        }
        used_lines = used_lines.saturating_add(row_lines);
        if used_lines >= viewport_height {
            break;
        }
    }
    false
}

fn adjust_start_for_wrapped_selection_visibility(
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_items: usize,
    desc_measure_items: usize,
    width: u16,
    viewport_height: u16,
    column_width: ColumnWidthConfig,
) -> usize {
    let mut start_idx = compute_item_window_start(rows_all, state, max_items);
    let Some(sel) = state.selected_idx else {
        return start_idx;
    };
    if viewport_height == 0 {
        return start_idx;
    }

    while start_idx < sel {
        let desc_col =
            compute_desc_col(rows_all, start_idx, desc_measure_items, width, column_width);
        if is_selected_visible_in_wrapped_viewport(
            rows_all,
            start_idx,
            max_items,
            sel,
            desc_col,
            width,
            viewport_height,
        ) {
            break;
        }
        start_idx = start_idx.saturating_add(1);
    }
    start_idx
}

fn build_full_line(row: &GenericDisplayRow, desc_col: usize) -> Line<'static> {
    let combined_description = match (&row.description, &row.disabled_reason) {
        (Some(desc), Some(reason)) => Some(format!("{desc} (disabled: {reason})")),
        (Some(desc), None) => Some(desc.clone()),
        (None, Some(reason)) => Some(format!("disabled: {reason}")),
        (None, None) => None,
    };

    let name_prefix_width = Line::from(row.name_prefix_spans.clone()).width();
    let name_limit = combined_description
        .as_ref()
        .map(|_| desc_col.saturating_sub(2).saturating_sub(name_prefix_width))
        .unwrap_or(usize::MAX);

    let mut name_spans: Vec<Span> = Vec::with_capacity(row.name.len());
    let mut used_width = 0usize;
    let mut truncated = false;

    if let Some(idxs) = row.match_indices.as_ref() {
        let mut idx_iter = idxs.iter().peekable();
        for (char_idx, ch) in row.name.chars().enumerate() {
            let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
            let next_width = used_width.saturating_add(ch_w);
            if next_width > name_limit {
                truncated = true;
                break;
            }
            used_width = next_width;

            if idx_iter.peek().is_some_and(|next| **next == char_idx) {
                idx_iter.next();
                name_spans.push(ch.to_string().bold());
            } else {
                name_spans.push(ch.to_string().into());
            }
        }
    } else {
        for ch in row.name.chars() {
            let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
            let next_width = used_width.saturating_add(ch_w);
            if next_width > name_limit {
                truncated = true;
                break;
            }
            used_width = next_width;
            name_spans.push(ch.to_string().into());
        }
    }

    if truncated {
        name_spans.push("…".into());
    }

    if row.disabled_reason.is_some() {
        name_spans.push(" (disabled)".dim());
    }

    let this_name_width = name_prefix_width + Line::from(name_spans.clone()).width();
    let mut full_spans: Vec<Span> = row.name_prefix_spans.clone();
    full_spans.extend(name_spans);
    if let Some(display_shortcut) = row.display_shortcut {
        full_spans.push(" (".into());
        full_spans.push(display_shortcut.into());
        full_spans.push(")".into());
    }
    if let Some(desc) = combined_description.as_ref() {
        let gap = desc_col.saturating_sub(this_name_width);
        if gap > 0 {
            full_spans.push(" ".repeat(gap).into());
        }
        full_spans.push(desc.clone().dim());
    }
    if let Some(tag) = row.category_tag.as_deref().filter(|tag| !tag.is_empty()) {
        full_spans.push("  ".into());
        full_spans.push(tag.to_string().dim());
    }
    Line::from(full_spans)
}

fn render_rows_inner(
    area: Rect,
    buf: &mut Buffer,
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    empty_message: &str,
    column_width: ColumnWidthConfig,
) -> u16 {
    if rows_all.is_empty() {
        if area.height > 0 {
            Line::from(empty_message.dim().italic()).render(area, buf);
        }
        return u16::from(area.height > 0);
    }

    let max_items = max_results.min(rows_all.len());
    if max_items == 0 {
        return 0;
    }
    let desc_measure_items = max_items.min(area.height.max(1) as usize);

    let start_idx = adjust_start_for_wrapped_selection_visibility(
        rows_all,
        state,
        max_items,
        desc_measure_items,
        area.width,
        area.height,
        column_width,
    );

    let desc_col = compute_desc_col(
        rows_all,
        start_idx,
        desc_measure_items,
        area.width,
        column_width,
    );

    let mut cur_y = area.y;
    let mut rendered_lines: u16 = 0;
    for (i, row) in rows_all.iter().enumerate().skip(start_idx).take(max_items) {
        if cur_y >= area.y + area.height {
            break;
        }

        let mut wrapped = wrap_row_lines(row, desc_col, area.width);
        apply_row_state_style(
            &mut wrapped,
            Some(i) == state.selected_idx && !row.is_disabled,
            row.is_disabled,
        );

        for line in wrapped {
            if cur_y >= area.y + area.height {
                break;
            }
            line.render(
                Rect {
                    x: area.x,
                    y: cur_y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            cur_y = cur_y.saturating_add(1);
            rendered_lines = rendered_lines.saturating_add(1);
        }
    }

    rendered_lines
}

pub(crate) fn render_rows(
    area: Rect,
    buf: &mut Buffer,
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    empty_message: &str,
) -> u16 {
    render_rows_inner(
        area,
        buf,
        rows_all,
        state,
        max_results,
        empty_message,
        ColumnWidthConfig::default(),
    )
}

pub(crate) fn render_rows_with_col_width_mode(
    area: Rect,
    buf: &mut Buffer,
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    empty_message: &str,
    column_width: ColumnWidthConfig,
) -> u16 {
    render_rows_inner(
        area,
        buf,
        rows_all,
        state,
        max_results,
        empty_message,
        column_width,
    )
}

pub(crate) fn render_rows_single_line(
    area: Rect,
    buf: &mut Buffer,
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    empty_message: &str,
) -> u16 {
    render_rows_single_line_with_col_width_mode(
        area,
        buf,
        rows_all,
        state,
        max_results,
        empty_message,
        ColumnWidthConfig::default(),
    )
}

pub(crate) fn render_rows_single_line_with_col_width_mode(
    area: Rect,
    buf: &mut Buffer,
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    empty_message: &str,
    column_width: ColumnWidthConfig,
) -> u16 {
    if rows_all.is_empty() {
        if area.height > 0 {
            Line::from(empty_message.dim().italic()).render(area, buf);
        }
        return u16::from(area.height > 0);
    }

    let visible_items = max_results
        .min(rows_all.len())
        .min(area.height.max(1) as usize);

    let mut start_idx = state.scroll_top.min(rows_all.len().saturating_sub(1));
    if let Some(sel) = state.selected_idx {
        if sel < start_idx {
            start_idx = sel;
        } else if visible_items > 0 {
            let bottom = start_idx + visible_items - 1;
            if sel > bottom {
                start_idx = sel + 1 - visible_items;
            }
        }
    }

    let desc_col = compute_desc_col(rows_all, start_idx, visible_items, area.width, column_width);

    let mut cur_y = area.y;
    let mut rendered_lines: u16 = 0;
    for (i, row) in rows_all
        .iter()
        .enumerate()
        .skip(start_idx)
        .take(visible_items)
    {
        if cur_y >= area.y + area.height {
            break;
        }

        let mut full_line = build_full_line(row, desc_col);
        if Some(i) == state.selected_idx && !row.is_disabled {
            full_line.spans.iter_mut().for_each(|span| {
                span.style = accent_style();
            });
        }
        if row.is_disabled {
            full_line.spans.iter_mut().for_each(|span| {
                span.style = span.style.dim();
            });
        }

        let full_line = truncate_line_with_ellipsis_if_overflow(full_line, area.width as usize);
        full_line.render(
            Rect {
                x: area.x,
                y: cur_y,
                width: area.width,
                height: 1,
            },
            buf,
        );
        cur_y = cur_y.saturating_add(1);
        rendered_lines = rendered_lines.saturating_add(1);
    }

    rendered_lines
}

pub(crate) fn measure_rows_height(
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    width: u16,
) -> u16 {
    measure_rows_height_inner(
        rows_all,
        state,
        max_results,
        width,
        ColumnWidthConfig::default(),
    )
}

pub(crate) fn measure_rows_height_with_col_width_mode(
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    width: u16,
    column_width: ColumnWidthConfig,
) -> u16 {
    measure_rows_height_inner(rows_all, state, max_results, width, column_width)
}

fn measure_rows_height_inner(
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    width: u16,
    column_width: ColumnWidthConfig,
) -> u16 {
    if rows_all.is_empty() {
        return 1;
    }

    let content_width = width.saturating_sub(1).max(1);

    let visible_items = max_results.min(rows_all.len());
    let mut start_idx = state.scroll_top.min(rows_all.len().saturating_sub(1));
    if let Some(sel) = state.selected_idx {
        if sel < start_idx {
            start_idx = sel;
        } else if visible_items > 0 {
            let bottom = start_idx + visible_items - 1;
            if sel > bottom {
                start_idx = sel + 1 - visible_items;
            }
        }
    }

    let desc_col = compute_desc_col(
        rows_all,
        start_idx,
        visible_items,
        content_width,
        column_width,
    );

    let mut total: u16 = 0;
    for row in rows_all
        .iter()
        .enumerate()
        .skip(start_idx)
        .take(visible_items)
        .map(|(_, r)| r)
    {
        let wrapped_lines = wrap_row_lines(row, desc_col, content_width).len();
        total = total.saturating_add(wrapped_lines as u16);
    }
    total.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    #[test]
    fn one_cell_width_falls_back_without_panic_for_wrapped_two_column_rows() {
        let row = GenericDisplayRow {
            name: "1. Very long option label".to_string(),
            description: Some("Very long description".to_string()),
            wrap_indent: Some(4),
            ..Default::default()
        };

        let two_col = wrap_two_column_row(&row, 0, 1);
        assert_eq!(two_col.len(), 0);
    }

    #[test]
    fn selected_rows_use_the_shared_accent_style() {
        let rows = vec![GenericDisplayRow {
            name: "selected".to_string(),
            ..Default::default()
        }];
        let state = ScrollState {
            selected_idx: Some(0),
            ..Default::default()
        };
        let area = Rect::new(0, 0, 16, 1);
        let mut buf = Buffer::empty(area);

        render_rows(area, &mut buf, &rows, &state, 1, "no rows");

        let style = buf[(0, 0)].style();
        let expected = accent_style();
        assert_eq!(style.fg, expected.fg);
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }
}
