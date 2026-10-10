use diffy::Hunk;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line as RtLine;
use ratatui::text::Span as RtSpan;
use ratatui::widgets::Paragraph;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use octopus_util_abspath::AbsolutePathBuf;
use unicode_width::UnicodeWidthChar;

const TAB_WIDTH: usize = 4;

const DARK_TC_ADD_LINE_BG_RGB: (u8, u8, u8) = (33, 58, 43);
const DARK_TC_DEL_LINE_BG_RGB: (u8, u8, u8) = (74, 34, 29);
const LIGHT_TC_ADD_LINE_BG_RGB: (u8, u8, u8) = (218, 251, 225);
const LIGHT_TC_DEL_LINE_BG_RGB: (u8, u8, u8) = (255, 235, 233);
const LIGHT_TC_ADD_NUM_BG_RGB: (u8, u8, u8) = (172, 238, 187);
const LIGHT_TC_DEL_NUM_BG_RGB: (u8, u8, u8) = (255, 206, 203);
const LIGHT_TC_GUTTER_FG_RGB: (u8, u8, u8) = (31, 35, 40);

const DARK_256_ADD_LINE_BG_IDX: u8 = 22;
const DARK_256_DEL_LINE_BG_IDX: u8 = 52;
const LIGHT_256_ADD_LINE_BG_IDX: u8 = 194;
const LIGHT_256_DEL_LINE_BG_IDX: u8 = 224;
const LIGHT_256_ADD_NUM_BG_IDX: u8 = 157;
const LIGHT_256_DEL_NUM_BG_IDX: u8 = 217;
const LIGHT_256_GUTTER_FG_IDX: u8 = 236;

use crate::color::is_light;
use crate::color::perceptual_distance;
use crate::diff_model::FileChange;
use crate::exec_command::relativize_to_home;
use crate::render::Insets;
use crate::render::highlight::DiffScopeBackgroundRgbs;
use crate::render::highlight::diff_scope_background_rgbs;
use crate::render::highlight::exceeds_highlight_limits;
use crate::render::highlight::highlight_code_to_styled_spans;
use crate::render::line_utils::prefix_lines;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::InsetRenderable;
use crate::render::renderable::Renderable;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::XTERM_COLORS;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::indexed_color;
use crate::terminal_palette::rgb_color;
use crate::terminal_palette::stdout_color_level;
use octopus_git_utils::get_git_repo_root;
use octopus_terminal_detection::TerminalName;
use octopus_terminal_detection::terminal_info;

#[derive(Clone, Copy)]
pub enum DiffLineType {
    Insert,
    Delete,
    Context,
}

#[derive(Clone, Copy, Debug)]
enum DiffTheme {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiffColorLevel {
    TrueColor,
    Ansi256,
    Ansi16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RichDiffColorLevel {
    TrueColor,
    Ansi256,
}

impl RichDiffColorLevel {
    fn from_diff_color_level(level: DiffColorLevel) -> Option<Self> {
        match level {
            DiffColorLevel::TrueColor => Some(Self::TrueColor),
            DiffColorLevel::Ansi256 => Some(Self::Ansi256),
            DiffColorLevel::Ansi16 => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ResolvedDiffBackgrounds {
    add: Option<Color>,
    del: Option<Color>,
}

#[derive(Clone, Copy, Debug)]
pub struct DiffRenderStyleContext {
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
}

fn resolve_diff_backgrounds(
    theme: DiffTheme,
    color_level: DiffColorLevel,
) -> ResolvedDiffBackgrounds {
    resolve_diff_backgrounds_for(theme, color_level, diff_scope_background_rgbs())
}

pub fn current_diff_render_style_context() -> DiffRenderStyleContext {
    let theme = diff_theme();
    let color_level = diff_color_level();
    let diff_backgrounds = resolve_diff_backgrounds(theme, color_level);
    DiffRenderStyleContext {
        theme,
        color_level,
        diff_backgrounds,
    }
}

fn resolve_diff_backgrounds_for(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    scope_backgrounds: DiffScopeBackgroundRgbs,
) -> ResolvedDiffBackgrounds {
    let mut resolved = fallback_diff_backgrounds(theme, color_level);
    let Some(level) = RichDiffColorLevel::from_diff_color_level(color_level) else {
        return resolved;
    };

    if let Some(rgb) = scope_backgrounds.inserted {
        resolved.add = Some(color_from_rgb_for_level(rgb, level));
    }
    if let Some(rgb) = scope_backgrounds.deleted {
        resolved.del = Some(color_from_rgb_for_level(rgb, level));
    }
    resolved
}

fn fallback_diff_backgrounds(
    theme: DiffTheme,
    color_level: DiffColorLevel,
) -> ResolvedDiffBackgrounds {
    match RichDiffColorLevel::from_diff_color_level(color_level) {
        Some(level) => ResolvedDiffBackgrounds {
            add: Some(add_line_bg(theme, level)),
            del: Some(del_line_bg(theme, level)),
        },
        None => ResolvedDiffBackgrounds::default(),
    }
}

fn color_from_rgb_for_level(rgb: (u8, u8, u8), color_level: RichDiffColorLevel) -> Color {
    match color_level {
        RichDiffColorLevel::TrueColor => rgb_color(rgb),
        RichDiffColorLevel::Ansi256 => quantize_rgb_to_ansi256(rgb),
    }
}

fn quantize_rgb_to_ansi256(target: (u8, u8, u8)) -> Color {
    let best_index = XTERM_COLORS
        .iter()
        .enumerate()
        .skip(16)
        .min_by(|(_, a), (_, b)| {
            perceptual_distance(**a, target).total_cmp(&perceptual_distance(**b, target))
        })
        .map(|(index, _)| index as u8);
    match best_index {
        Some(index) => indexed_color(index),
        None => indexed_color(DARK_256_ADD_LINE_BG_IDX),
    }
}

pub struct DiffSummary {
    changes: HashMap<PathBuf, FileChange>,
    cwd: AbsolutePathBuf,
}

impl DiffSummary {
    pub fn new(changes: HashMap<PathBuf, FileChange>, cwd: AbsolutePathBuf) -> Self {
        Self { changes, cwd }
    }
}

impl Renderable for FileChange {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut lines = vec![];
        render_change(self, &mut lines, area.width as usize, None);
        Paragraph::new(lines).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let mut lines = vec![];
        render_change(self, &mut lines, width as usize, None);
        lines.len() as u16
    }
}

impl From<DiffSummary> for Box<dyn Renderable> {
    fn from(val: DiffSummary) -> Self {
        let mut rows: Vec<Box<dyn Renderable>> = vec![];

        for (i, row) in collect_rows(&val.changes).into_iter().enumerate() {
            if i > 0 {
                rows.push(Box::new(RtLine::from("")));
            }
            let mut path = RtLine::from(display_path_for(&row.path, val.cwd.as_path()));
            path.push_span(" ");
            path.extend(render_line_count_summary(row.added, row.removed));
            rows.push(Box::new(path));
            rows.push(Box::new(RtLine::from("")));
            rows.push(Box::new(InsetRenderable::new(
                Box::new(row.change) as Box<dyn Renderable>,
                Insets::tlbr(0, 2, 0, 0),
            )));
        }

        Box::new(ColumnRenderable::with(rows))
    }
}

pub fn create_diff_summary(
    changes: &HashMap<PathBuf, FileChange>,
    cwd: &Path,
    wrap_cols: usize,
) -> Vec<RtLine<'static>> {
    let rows = collect_rows(changes);
    render_changes_block(rows, wrap_cols, cwd)
}

#[derive(Clone)]
struct Row {
    #[allow(dead_code)]
    path: PathBuf,
    move_path: Option<PathBuf>,
    added: usize,
    removed: usize,
    change: FileChange,
}

fn collect_rows(changes: &HashMap<PathBuf, FileChange>) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (path, change) in changes.iter() {
        let (added, removed) = match change {
            FileChange::Add { content } => (content.lines().count(), 0),
            FileChange::Delete { content } => (0, content.lines().count()),
            FileChange::Update { unified_diff, .. } => calculate_add_remove_from_diff(unified_diff),
        };
        let move_path = match change {
            FileChange::Update {
                move_path: Some(new),
                ..
            } => Some(new.clone()),
            _ => None,
        };
        rows.push(Row {
            path: path.clone(),
            move_path,
            added,
            removed,
            change: change.clone(),
        });
    }
    rows.sort_by_key(|r| r.path.clone());
    rows
}

fn render_line_count_summary(added: usize, removed: usize) -> Vec<RtSpan<'static>> {
    let mut spans = Vec::new();
    spans.push("(".into());
    spans.push(format!("+{added}").green());
    spans.push(" ".into());
    spans.push(format!("-{removed}").red());
    spans.push(")".into());
    spans
}

fn render_changes_block(rows: Vec<Row>, wrap_cols: usize, cwd: &Path) -> Vec<RtLine<'static>> {
    let mut out: Vec<RtLine<'static>> = Vec::new();

    let render_path = |row: &Row| -> Vec<RtSpan<'static>> {
        let mut spans = Vec::new();
        spans.push(display_path_for(&row.path, cwd).into());
        if let Some(move_path) = &row.move_path {
            spans.push(format!(" → {}", display_path_for(move_path, cwd)).into());
        }
        spans
    };

    let total_added: usize = rows.iter().map(|r| r.added).sum();
    let total_removed: usize = rows.iter().map(|r| r.removed).sum();
    let file_count = rows.len();
    let noun = if file_count == 1 { "file" } else { "files" };
    let mut header_spans: Vec<RtSpan<'static>> = vec!["• ".dim()];
    if let [row] = &rows[..] {
        let verb = match &row.change {
            FileChange::Add { .. } => "Added",
            FileChange::Delete { .. } => "Deleted",
            _ => "Edited",
        };
        header_spans.push(verb.bold());
        header_spans.push(" ".into());
        header_spans.extend(render_path(row));
        header_spans.push(" ".into());
        header_spans.extend(render_line_count_summary(row.added, row.removed));
    } else {
        header_spans.push("Edited".bold());
        header_spans.push(format!(" {file_count} {noun} ").into());
        header_spans.extend(render_line_count_summary(total_added, total_removed));
    }
    out.push(RtLine::from(header_spans));

    for (idx, r) in rows.into_iter().enumerate() {
        if idx > 0 {
            out.push("".into());
        }
        let skip_file_header = file_count == 1;
        if !skip_file_header {
            let mut header: Vec<RtSpan<'static>> = Vec::new();
            header.push("  └ ".dim());
            header.extend(render_path(&r));
            header.push(" ".into());
            header.extend(render_line_count_summary(r.added, r.removed));
            out.push(RtLine::from(header));
        }

        let lang_path = r.move_path.as_deref().unwrap_or(&r.path);
        let lang = detect_lang_for_path(lang_path);
        let mut lines = vec![];
        render_change(&r.change, &mut lines, wrap_cols - 4, lang.as_deref());
        out.extend(prefix_lines(lines, "    ".into(), "    ".into()));
    }

    out
}

fn detect_lang_for_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    Some(ext.to_string())
}

fn render_change(
    change: &FileChange,
    out: &mut Vec<RtLine<'static>>,
    width: usize,
    lang: Option<&str>,
) {
    let style_context = current_diff_render_style_context();
    match change {
        FileChange::Add { content } => {
            let syntax_lines = lang.and_then(|l| highlight_code_to_styled_spans(content, l));
            let line_number_width = line_number_width(content.lines().count());
            for (i, raw) in content.lines().enumerate() {
                let syn = syntax_lines.as_ref().and_then(|sl| sl.get(i));
                if let Some(spans) = syn {
                    out.extend(push_wrapped_diff_line_inner_with_theme_and_color_level(
                        i + 1,
                        DiffLineType::Insert,
                        raw,
                        width,
                        line_number_width,
                        Some(spans),
                        style_context.theme,
                        style_context.color_level,
                        style_context.diff_backgrounds,
                    ));
                } else {
                    out.extend(push_wrapped_diff_line_inner_with_theme_and_color_level(
                        i + 1,
                        DiffLineType::Insert,
                        raw,
                        width,
                        line_number_width,
                        None,
                        style_context.theme,
                        style_context.color_level,
                        style_context.diff_backgrounds,
                    ));
                }
            }
        }
        FileChange::Delete { content } => {
            let syntax_lines = lang.and_then(|l| highlight_code_to_styled_spans(content, l));
            let line_number_width = line_number_width(content.lines().count());
            for (i, raw) in content.lines().enumerate() {
                let syn = syntax_lines.as_ref().and_then(|sl| sl.get(i));
                if let Some(spans) = syn {
                    out.extend(push_wrapped_diff_line_inner_with_theme_and_color_level(
                        i + 1,
                        DiffLineType::Delete,
                        raw,
                        width,
                        line_number_width,
                        Some(spans),
                        style_context.theme,
                        style_context.color_level,
                        style_context.diff_backgrounds,
                    ));
                } else {
                    out.extend(push_wrapped_diff_line_inner_with_theme_and_color_level(
                        i + 1,
                        DiffLineType::Delete,
                        raw,
                        width,
                        line_number_width,
                        None,
                        style_context.theme,
                        style_context.color_level,
                        style_context.diff_backgrounds,
                    ));
                }
            }
        }
        FileChange::Update { unified_diff, .. } => {
            if let Ok(patch) = diffy::Patch::from_str(unified_diff) {
                let mut max_line_number = 0;
                let mut total_diff_bytes: usize = 0;
                let mut total_diff_lines: usize = 0;
                for h in patch.hunks() {
                    let mut old_ln = h.old_range().start();
                    let mut new_ln = h.new_range().start();
                    for l in h.lines() {
                        let text = match l {
                            diffy::Line::Insert(t)
                            | diffy::Line::Delete(t)
                            | diffy::Line::Context(t) => t,
                        };
                        total_diff_bytes += text.len();
                        total_diff_lines += 1;
                        match l {
                            diffy::Line::Insert(_) => {
                                max_line_number = max_line_number.max(new_ln);
                                new_ln += 1;
                            }
                            diffy::Line::Delete(_) => {
                                max_line_number = max_line_number.max(old_ln);
                                old_ln += 1;
                            }
                            diffy::Line::Context(_) => {
                                max_line_number = max_line_number.max(new_ln);
                                old_ln += 1;
                                new_ln += 1;
                            }
                        }
                    }
                }

                let diff_lang = if exceeds_highlight_limits(total_diff_bytes, total_diff_lines) {
                    None
                } else {
                    lang
                };

                let line_number_width = line_number_width(max_line_number);
                let mut is_first_hunk = true;
                for h in patch.hunks() {
                    if !is_first_hunk {
                        let spacer = format!("{:width$} ", "", width = line_number_width.max(1));
                        let spacer_span = RtSpan::styled(
                            spacer,
                            style_gutter_for(
                                DiffLineType::Context,
                                style_context.theme,
                                style_context.color_level,
                            ),
                        );
                        out.push(RtLine::from(vec![spacer_span, "⋮".dim()]));
                    }
                    is_first_hunk = false;

                    let hunk_syntax_lines = diff_lang.and_then(|language| {
                        let hunk_text: String = h
                            .lines()
                            .iter()
                            .map(|line| match line {
                                diffy::Line::Insert(text)
                                | diffy::Line::Delete(text)
                                | diffy::Line::Context(text) => *text,
                            })
                            .collect();
                        let syntax_lines = highlight_code_to_styled_spans(&hunk_text, language)?;
                        (syntax_lines.len() == h.lines().len()).then_some(syntax_lines)
                    });

                    let mut old_ln = h.old_range().start();
                    let mut new_ln = h.new_range().start();
                    for (line_idx, l) in h.lines().iter().enumerate() {
                        let syntax_spans = hunk_syntax_lines
                            .as_ref()
                            .and_then(|syntax_lines| syntax_lines.get(line_idx));
                        match l {
                            diffy::Line::Insert(text) => {
                                let s = text.trim_end_matches('\n');
                                if let Some(syn) = syntax_spans {
                                    out.extend(
                                        push_wrapped_diff_line_inner_with_theme_and_color_level(
                                            new_ln,
                                            DiffLineType::Insert,
                                            s,
                                            width,
                                            line_number_width,
                                            Some(syn),
                                            style_context.theme,
                                            style_context.color_level,
                                            style_context.diff_backgrounds,
                                        ),
                                    );
                                } else {
                                    out.extend(
                                        push_wrapped_diff_line_inner_with_theme_and_color_level(
                                            new_ln,
                                            DiffLineType::Insert,
                                            s,
                                            width,
                                            line_number_width,
                                            None,
                                            style_context.theme,
                                            style_context.color_level,
                                            style_context.diff_backgrounds,
                                        ),
                                    );
                                }
                                new_ln += 1;
                            }
                            diffy::Line::Delete(text) => {
                                let s = text.trim_end_matches('\n');
                                if let Some(syn) = syntax_spans {
                                    out.extend(
                                        push_wrapped_diff_line_inner_with_theme_and_color_level(
                                            old_ln,
                                            DiffLineType::Delete,
                                            s,
                                            width,
                                            line_number_width,
                                            Some(syn),
                                            style_context.theme,
                                            style_context.color_level,
                                            style_context.diff_backgrounds,
                                        ),
                                    );
                                } else {
                                    out.extend(
                                        push_wrapped_diff_line_inner_with_theme_and_color_level(
                                            old_ln,
                                            DiffLineType::Delete,
                                            s,
                                            width,
                                            line_number_width,
                                            None,
                                            style_context.theme,
                                            style_context.color_level,
                                            style_context.diff_backgrounds,
                                        ),
                                    );
                                }
                                old_ln += 1;
                            }
                            diffy::Line::Context(text) => {
                                let s = text.trim_end_matches('\n');
                                if let Some(syn) = syntax_spans {
                                    out.extend(
                                        push_wrapped_diff_line_inner_with_theme_and_color_level(
                                            new_ln,
                                            DiffLineType::Context,
                                            s,
                                            width,
                                            line_number_width,
                                            Some(syn),
                                            style_context.theme,
                                            style_context.color_level,
                                            style_context.diff_backgrounds,
                                        ),
                                    );
                                } else {
                                    out.extend(
                                        push_wrapped_diff_line_inner_with_theme_and_color_level(
                                            new_ln,
                                            DiffLineType::Context,
                                            s,
                                            width,
                                            line_number_width,
                                            None,
                                            style_context.theme,
                                            style_context.color_level,
                                            style_context.diff_backgrounds,
                                        ),
                                    );
                                }
                                old_ln += 1;
                                new_ln += 1;
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn display_path_for(path: &Path, cwd: &Path) -> String {
    if path.is_relative() {
        return path.display().to_string();
    }

    if let Ok(stripped) = path.strip_prefix(cwd) {
        return stripped.display().to_string();
    }

    let path_in_same_repo = match (get_git_repo_root(cwd), get_git_repo_root(path)) {
        (Some(cwd_repo), Some(path_repo)) => cwd_repo == path_repo,
        _ => false,
    };
    let chosen = if path_in_same_repo {
        pathdiff::diff_paths(path, cwd).unwrap_or_else(|| path.to_path_buf())
    } else {
        relativize_to_home(path)
            .map(|p| PathBuf::from_iter([Path::new("~"), p.as_path()]))
            .unwrap_or_else(|| path.to_path_buf())
    };
    chosen.display().to_string()
}

pub fn calculate_add_remove_from_diff(diff: &str) -> (usize, usize) {
    if let Ok(patch) = diffy::Patch::from_str(diff) {
        patch
            .hunks()
            .iter()
            .flat_map(Hunk::lines)
            .fold((0, 0), |(a, d), l| match l {
                diffy::Line::Insert(_) => (a + 1, d),
                diffy::Line::Delete(_) => (a, d + 1),
                diffy::Line::Context(_) => (a, d),
            })
    } else {
        (0, 0)
    }
}

pub fn push_wrapped_diff_line_with_style_context(
    line_number: usize,
    kind: DiffLineType,
    text: &str,
    width: usize,
    line_number_width: usize,
    style_context: DiffRenderStyleContext,
) -> Vec<RtLine<'static>> {
    push_wrapped_diff_line_inner_with_theme_and_color_level(
        line_number,
        kind,
        text,
        width,
        line_number_width,
        None,
        style_context.theme,
        style_context.color_level,
        style_context.diff_backgrounds,
    )
}

pub fn push_wrapped_diff_line_with_syntax_and_style_context(
    line_number: usize,
    kind: DiffLineType,
    text: &str,
    width: usize,
    line_number_width: usize,
    syntax_spans: &[RtSpan<'static>],
    style_context: DiffRenderStyleContext,
) -> Vec<RtLine<'static>> {
    push_wrapped_diff_line_inner_with_theme_and_color_level(
        line_number,
        kind,
        text,
        width,
        line_number_width,
        Some(syntax_spans),
        style_context.theme,
        style_context.color_level,
        style_context.diff_backgrounds,
    )
}

#[allow(clippy::too_many_arguments)]
fn push_wrapped_diff_line_inner_with_theme_and_color_level(
    line_number: usize,
    kind: DiffLineType,
    text: &str,
    width: usize,
    line_number_width: usize,
    syntax_spans: Option<&[RtSpan<'static>]>,
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Vec<RtLine<'static>> {
    let ln_str = line_number.to_string();

    let gutter_width = line_number_width.max(1);
    let prefix_cols = gutter_width + 1;

    let (sign_char, sign_style, content_style) = match kind {
        DiffLineType::Insert => (
            '+',
            style_sign_add(theme, color_level, diff_backgrounds),
            style_add(theme, color_level, diff_backgrounds),
        ),
        DiffLineType::Delete => (
            '-',
            style_sign_del(theme, color_level, diff_backgrounds),
            style_del(theme, color_level, diff_backgrounds),
        ),
        DiffLineType::Context => (' ', style_context(), style_context()),
    };

    let line_bg = style_line_bg_for(kind, diff_backgrounds);
    let gutter_style = style_gutter_for(kind, theme, color_level);

    if let Some(syn_spans) = syntax_spans {
        let gutter = format!("{ln_str:>gutter_width$} ");
        let sign = format!("{sign_char}");
        let styled: Vec<RtSpan<'static>> = syn_spans
            .iter()
            .map(|sp| {
                let style = if matches!(kind, DiffLineType::Delete) {
                    sp.style.add_modifier(Modifier::DIM)
                } else {
                    sp.style
                };
                RtSpan::styled(sp.content.clone().into_owned(), style)
            })
            .collect();

        let available_content_cols = width.saturating_sub(prefix_cols + 1).max(1);

        let wrapped_chunks = wrap_styled_spans(&styled, available_content_cols);

        let mut lines: Vec<RtLine<'static>> = Vec::new();
        for (i, chunk) in wrapped_chunks.into_iter().enumerate() {
            let mut row_spans: Vec<RtSpan<'static>> = Vec::new();
            if i == 0 {
                row_spans.push(RtSpan::styled(gutter.clone(), gutter_style));
                row_spans.push(RtSpan::styled(sign.clone(), sign_style));
            } else {
                let cont_gutter = format!("{:gutter_width$}  ", "");
                row_spans.push(RtSpan::styled(cont_gutter, gutter_style));
            }
            row_spans.extend(chunk);
            lines.push(RtLine::from(row_spans).style(line_bg));
        }
        return lines;
    }

    let available_content_cols = width.saturating_sub(prefix_cols + 1).max(1);
    let styled = vec![RtSpan::styled(text.to_string(), content_style)];
    let wrapped_chunks = wrap_styled_spans(&styled, available_content_cols);

    let mut lines: Vec<RtLine<'static>> = Vec::new();
    for (i, chunk) in wrapped_chunks.into_iter().enumerate() {
        let mut row_spans: Vec<RtSpan<'static>> = Vec::new();
        if i == 0 {
            let gutter = format!("{ln_str:>gutter_width$} ");
            let sign = format!("{sign_char}");
            row_spans.push(RtSpan::styled(gutter, gutter_style));
            row_spans.push(RtSpan::styled(sign, sign_style));
        } else {
            let cont_gutter = format!("{:gutter_width$}  ", "");
            row_spans.push(RtSpan::styled(cont_gutter, gutter_style));
        }
        row_spans.extend(chunk);
        lines.push(RtLine::from(row_spans).style(line_bg));
    }

    lines
}

fn wrap_styled_spans(spans: &[RtSpan<'static>], max_cols: usize) -> Vec<Vec<RtSpan<'static>>> {
    let mut result: Vec<Vec<RtSpan<'static>>> = Vec::new();
    let mut current_line: Vec<RtSpan<'static>> = Vec::new();
    let mut col: usize = 0;

    for span in spans {
        let style = span.style;
        let text = span.content.as_ref();
        let mut remaining = text;

        while !remaining.is_empty() {
            let mut byte_end = 0;
            let mut chars_col = 0;

            for ch in remaining.chars() {
                let w = ch.width().unwrap_or(if ch == '\t' { TAB_WIDTH } else { 0 });
                if col + chars_col + w > max_cols {
                    break;
                }
                byte_end += ch.len_utf8();
                chars_col += w;
            }

            if byte_end == 0 {
                if !current_line.is_empty() {
                    result.push(std::mem::take(&mut current_line));
                }
                let Some(ch) = remaining.chars().next() else {
                    break;
                };
                let ch_len = ch.len_utf8();
                current_line.push(RtSpan::styled(remaining[..ch_len].to_string(), style));
                col = ch.width().unwrap_or(if ch == '\t' { TAB_WIDTH } else { 1 });
                remaining = &remaining[ch_len..];
                continue;
            }

            let (chunk, rest) = remaining.split_at(byte_end);
            current_line.push(RtSpan::styled(chunk.to_string(), style));
            col += chars_col;
            remaining = rest;

            if col >= max_cols {
                result.push(std::mem::take(&mut current_line));
                col = 0;
            }
        }
    }

    if !current_line.is_empty() || result.is_empty() {
        result.push(current_line);
    }

    result
}

pub fn line_number_width(max_line_number: usize) -> usize {
    if max_line_number == 0 {
        1
    } else {
        max_line_number.to_string().len()
    }
}

fn diff_theme_for_bg(bg: Option<(u8, u8, u8)>) -> DiffTheme {
    if let Some(rgb) = bg
        && is_light(rgb)
    {
        return DiffTheme::Light;
    }
    DiffTheme::Dark
}

fn diff_theme() -> DiffTheme {
    diff_theme_for_bg(default_bg())
}

fn diff_color_level() -> DiffColorLevel {
    diff_color_level_for_terminal(
        stdout_color_level(),
        terminal_info().name,
        std::env::var_os("WT_SESSION").is_some(),
        has_force_color_override(),
    )
}

fn has_force_color_override() -> bool {
    std::env::var_os("FORCE_COLOR").is_some()
}

fn diff_color_level_for_terminal(
    stdout_level: StdoutColorLevel,
    terminal_name: TerminalName,
    has_wt_session: bool,
    has_force_color_override: bool,
) -> DiffColorLevel {
    if has_wt_session && !has_force_color_override {
        return DiffColorLevel::TrueColor;
    }

    let base = match stdout_level {
        StdoutColorLevel::TrueColor => DiffColorLevel::TrueColor,
        StdoutColorLevel::Ansi256 => DiffColorLevel::Ansi256,
        StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown => DiffColorLevel::Ansi16,
    };

    if stdout_level == StdoutColorLevel::Ansi16
        && terminal_name == TerminalName::WindowsTerminal
        && !has_force_color_override
    {
        DiffColorLevel::TrueColor
    } else {
        base
    }
}

fn style_line_bg_for(kind: DiffLineType, diff_backgrounds: ResolvedDiffBackgrounds) -> Style {
    match kind {
        DiffLineType::Insert => diff_backgrounds
            .add
            .map_or_else(Style::default, |bg| Style::default().bg(bg)),
        DiffLineType::Delete => diff_backgrounds
            .del
            .map_or_else(Style::default, |bg| Style::default().bg(bg)),
        DiffLineType::Context => Style::default(),
    }
}

fn style_context() -> Style {
    Style::default()
}

fn add_line_bg(theme: DiffTheme, color_level: RichDiffColorLevel) -> Color {
    match (theme, color_level) {
        (DiffTheme::Dark, RichDiffColorLevel::TrueColor) => rgb_color(DARK_TC_ADD_LINE_BG_RGB),
        (DiffTheme::Dark, RichDiffColorLevel::Ansi256) => indexed_color(DARK_256_ADD_LINE_BG_IDX),
        (DiffTheme::Light, RichDiffColorLevel::TrueColor) => rgb_color(LIGHT_TC_ADD_LINE_BG_RGB),
        (DiffTheme::Light, RichDiffColorLevel::Ansi256) => indexed_color(LIGHT_256_ADD_LINE_BG_IDX),
    }
}

fn del_line_bg(theme: DiffTheme, color_level: RichDiffColorLevel) -> Color {
    match (theme, color_level) {
        (DiffTheme::Dark, RichDiffColorLevel::TrueColor) => rgb_color(DARK_TC_DEL_LINE_BG_RGB),
        (DiffTheme::Dark, RichDiffColorLevel::Ansi256) => indexed_color(DARK_256_DEL_LINE_BG_IDX),
        (DiffTheme::Light, RichDiffColorLevel::TrueColor) => rgb_color(LIGHT_TC_DEL_LINE_BG_RGB),
        (DiffTheme::Light, RichDiffColorLevel::Ansi256) => indexed_color(LIGHT_256_DEL_LINE_BG_IDX),
    }
}

fn light_gutter_fg(color_level: DiffColorLevel) -> Color {
    match color_level {
        DiffColorLevel::TrueColor => rgb_color(LIGHT_TC_GUTTER_FG_RGB),
        DiffColorLevel::Ansi256 => indexed_color(LIGHT_256_GUTTER_FG_IDX),
        DiffColorLevel::Ansi16 => Color::Black,
    }
}

fn light_add_num_bg(color_level: RichDiffColorLevel) -> Color {
    match color_level {
        RichDiffColorLevel::TrueColor => rgb_color(LIGHT_TC_ADD_NUM_BG_RGB),
        RichDiffColorLevel::Ansi256 => indexed_color(LIGHT_256_ADD_NUM_BG_IDX),
    }
}

fn light_del_num_bg(color_level: RichDiffColorLevel) -> Color {
    match color_level {
        RichDiffColorLevel::TrueColor => rgb_color(LIGHT_TC_DEL_NUM_BG_RGB),
        RichDiffColorLevel::Ansi256 => indexed_color(LIGHT_256_DEL_NUM_BG_IDX),
    }
}

fn style_gutter_for(kind: DiffLineType, theme: DiffTheme, color_level: DiffColorLevel) -> Style {
    match (
        theme,
        kind,
        RichDiffColorLevel::from_diff_color_level(color_level),
    ) {
        (DiffTheme::Light, DiffLineType::Insert, None) => {
            Style::default().fg(light_gutter_fg(color_level))
        }
        (DiffTheme::Light, DiffLineType::Delete, None) => {
            Style::default().fg(light_gutter_fg(color_level))
        }
        (DiffTheme::Light, DiffLineType::Insert, Some(level)) => Style::default()
            .fg(light_gutter_fg(color_level))
            .bg(light_add_num_bg(level)),
        (DiffTheme::Light, DiffLineType::Delete, Some(level)) => Style::default()
            .fg(light_gutter_fg(color_level))
            .bg(light_del_num_bg(level)),
        _ => style_gutter_dim(),
    }
}

fn style_sign_add(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match theme {
        DiffTheme::Light => Style::default().fg(Color::Green),
        DiffTheme::Dark => style_add(theme, color_level, diff_backgrounds),
    }
}

fn style_sign_del(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match theme {
        DiffTheme::Light => Style::default().fg(Color::Red),
        DiffTheme::Dark => style_del(theme, color_level, diff_backgrounds),
    }
}

fn style_add(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match (theme, color_level, diff_backgrounds.add) {
        (_, DiffColorLevel::Ansi16, _) => Style::default().fg(Color::Green),
        (DiffTheme::Light, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Light, DiffColorLevel::Ansi256, Some(bg)) => Style::default().bg(bg),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, Some(bg)) => {
            Style::default().fg(Color::Green).bg(bg)
        }
        (DiffTheme::Light, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Light, DiffColorLevel::Ansi256, None) => Style::default(),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, None) => Style::default().fg(Color::Green),
    }
}

fn style_del(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match (theme, color_level, diff_backgrounds.del) {
        (_, DiffColorLevel::Ansi16, _) => Style::default().fg(Color::Red),
        (DiffTheme::Light, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Light, DiffColorLevel::Ansi256, Some(bg)) => Style::default().bg(bg),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, Some(bg)) => {
            Style::default().fg(Color::Red).bg(bg)
        }
        (DiffTheme::Light, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Light, DiffColorLevel::Ansi256, None) => Style::default(),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, None) => Style::default().fg(Color::Red),
    }
}

fn style_gutter_dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_debug_snapshot;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::text::Text;
    use ratatui::widgets::Paragraph;
    use ratatui::widgets::WidgetRef;
    use ratatui::widgets::Wrap;

    #[test]
    fn ansi16_add_style_uses_foreground_only() {
        let style = style_add(
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        assert_eq!(style.fg, Some(Color::Green));
        assert_eq!(style.bg, None);
    }

    #[test]
    fn ansi16_del_style_uses_foreground_only() {
        let style = style_del(
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        assert_eq!(style.fg, Some(Color::Red));
        assert_eq!(style.bg, None);
    }

    #[test]
    fn ansi16_sign_styles_use_foreground_only() {
        let add_sign = style_sign_add(
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        assert_eq!(add_sign.fg, Some(Color::Green));
        assert_eq!(add_sign.bg, None);

        let del_sign = style_sign_del(
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        assert_eq!(del_sign.fg, Some(Color::Red));
        assert_eq!(del_sign.bg, None);
    }
    fn diff_summary_for_tests(changes: &HashMap<PathBuf, FileChange>) -> Vec<RtLine<'static>> {
        create_diff_summary(changes, &PathBuf::from("/"), 80)
    }

    fn snapshot_lines(name: &str, lines: Vec<RtLine<'static>>, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|f| {
                Paragraph::new(Text::from(lines))
                    .wrap(Wrap { trim: false })
                    .render_ref(f.area(), f.buffer_mut())
            })
            .expect("draw");
        assert_snapshot!(name, terminal.backend());
    }

    fn display_width(text: &str) -> usize {
        text.chars()
            .map(|ch| ch.width().unwrap_or(if ch == '\t' { TAB_WIDTH } else { 0 }))
            .sum()
    }

    fn line_display_width(line: &RtLine<'static>) -> usize {
        line.spans
            .iter()
            .map(|span| display_width(span.content.as_ref()))
            .sum()
    }

    fn snapshot_lines_text(name: &str, lines: &[RtLine<'static>]) {
        let text = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .map(|s| s.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_snapshot!(name, text);
    }

    fn diff_gallery_changes() -> HashMap<PathBuf, FileChange> {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

        let rust_original =
            "fn greet(name: &str) {\n    println!(\"hello\");\n    println!(\"bye\");\n}\n";
        let rust_modified = "fn greet(name: &str) {\n    println!(\"hello {name}\");\n    println!(\"emoji: 🚀✨ and CJK: 你好世界\");\n}\n";
        let rust_patch = diffy::create_patch(rust_original, rust_modified).to_string();
        changes.insert(
            PathBuf::from("src/lib.rs"),
            FileChange::Update {
                unified_diff: rust_patch,
                move_path: None,
            },
        );

        let py_original = "def add(a, b):\n\treturn a + b\n\nprint(add(1, 2))\n";
        let py_modified = "def add(a, b):\n\treturn a + b + 42\n\nprint(add(1, 2))\n";
        let py_patch = diffy::create_patch(py_original, py_modified).to_string();
        changes.insert(
            PathBuf::from("scripts/calc.txt"),
            FileChange::Update {
                unified_diff: py_patch,
                move_path: Some(PathBuf::from("scripts/calc.py")),
            },
        );

        changes.insert(
            PathBuf::from("assets/banner.txt"),
            FileChange::Add {
                content: "HEADER\tVALUE\nrocket\t🚀\ncity\t東京\n".to_string(),
            },
        );
        changes.insert(
            PathBuf::from("examples/new_sample.rs"),
            FileChange::Add {
                content: "pub fn greet(name: &str) {\n    println!(\"Hello, {name}!\");\n}\n"
                    .to_string(),
            },
        );

        changes.insert(
            PathBuf::from("tmp/obsolete.log"),
            FileChange::Delete {
                content: "old line 1\nold line 2\nold line 3\n".to_string(),
            },
        );
        changes.insert(
            PathBuf::from("legacy/old_script.py"),
            FileChange::Delete {
                content: "def legacy(x):\n    return x + 1\nprint(legacy(3))\n".to_string(),
            },
        );

        changes
    }

    fn snapshot_diff_gallery(name: &str, width: u16, height: u16) {
        let lines = create_diff_summary(
            &diff_gallery_changes(),
            &PathBuf::from("/"),
            usize::from(width),
        );
        snapshot_lines(name, lines, width, height);
    }

    #[test]
    fn display_path_prefers_cwd_without_git_repo() {
        let cwd = if cfg!(windows) {
            PathBuf::from(r"C:\workspace\codex")
        } else {
            PathBuf::from("/workspace/codex")
        };
        let path = cwd.join("tui").join("example.png");

        let rendered = display_path_for(&path, &cwd);

        assert_eq!(
            rendered,
            PathBuf::from("tui")
                .join("example.png")
                .display()
                .to_string()
        );
    }

    #[test]
    fn ui_snapshot_wrap_behavior_insert() {
        let long_line = "this is a very long line that should wrap across multiple terminal columns and continue";

        let lines = push_wrapped_diff_line_with_style_context(
            1,
            DiffLineType::Insert,
            long_line,
            80,
            line_number_width(1),
            current_diff_render_style_context(),
        );

        snapshot_lines("wrap_behavior_insert", lines, 90, 8);
    }

    #[test]
    fn ui_snapshot_apply_update_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        let original = "line one\nline two\nline three\n";
        let modified = "line one\nline two changed\nline three\n";
        let patch = diffy::create_patch(original, modified).to_string();

        changes.insert(
            PathBuf::from("example.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_update_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_with_rename_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        let original = "A\nB\nC\n";
        let modified = "A\nB changed\nC\n";
        let patch = diffy::create_patch(original, modified).to_string();

        changes.insert(
            PathBuf::from("old_name.rs"),
            FileChange::Update {
                unified_diff: patch,
                move_path: Some(PathBuf::from("new_name.rs")),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_update_with_rename_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_multiple_files_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

        let patch_a = diffy::create_patch("one\n", "one changed\n").to_string();
        changes.insert(
            PathBuf::from("a.txt"),
            FileChange::Update {
                unified_diff: patch_a,
                move_path: None,
            },
        );

        changes.insert(
            PathBuf::from("b.txt"),
            FileChange::Add {
                content: "new\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_multiple_files_block", lines, 80, 14);
    }

    #[test]
    fn ui_snapshot_apply_add_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("new_file.txt"),
            FileChange::Add {
                content: "alpha\nbeta\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_add_block", lines, 80, 10);
    }

    #[test]
    fn ui_snapshot_apply_delete_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("tmp_delete_example.txt"),
            FileChange::Delete {
                content: "first\nsecond\nthird\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);
        snapshot_lines("apply_delete_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_block_wraps_long_lines() {
        let original = "line 1\nshort\nline 3\n";
        let modified = "line 1\nshort this_is_a_very_long_modified_line_that_should_wrap_across_multiple_terminal_columns_and_continue_even_further_beyond_eighty_columns_to_force_multiple_wraps\nline 3\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("long_example.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 72);

        snapshot_lines("apply_update_block_wraps_long_lines", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_block_wraps_long_lines_text() {
        let original = "1\n2\n3\n4\n";
        let modified = "1\nadded long line which wraps and_if_there_is_a_long_token_it_will_be_broken\n3\n4 context line which also wraps across\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("wrap_demo.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 28);
        snapshot_lines_text("apply_update_block_wraps_long_lines_text", &lines);
    }

    #[test]
    fn ui_snapshot_apply_update_block_line_numbers_three_digits_text() {
        let original = (1..=110).map(|i| format!("line {i}\n")).collect::<String>();
        let modified = (1..=110)
            .map(|i| {
                if i == 100 {
                    format!("line {i} changed\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect::<String>();
        let patch = diffy::create_patch(&original, &modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("hundreds.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        snapshot_lines_text("apply_update_block_line_numbers_three_digits_text", &lines);
    }

    #[test]
    fn ui_snapshot_apply_update_block_relativizes_path() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let abs_old = cwd.join("abs_old.rs");
        let abs_new = cwd.join("abs_new.rs");

        let original = "X\nY\n";
        let modified = "X changed\nY\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            abs_old,
            FileChange::Update {
                unified_diff: patch,
                move_path: Some(abs_new),
            },
        );

        let lines = create_diff_summary(&changes, &cwd, 80);

        snapshot_lines("apply_update_block_relativizes_path", lines, 80, 10);
    }

    #[test]
    fn ui_snapshot_syntax_highlighted_insert_wraps() {
        let long_rust = "fn very_long_function_name(arg_one: String, arg_two: String, arg_three: String, arg_four: String) -> Result<String, Box<dyn std::error::Error>> { Ok(arg_one) }";

        let syntax_spans =
            highlight_code_to_styled_spans(long_rust, "rust").expect("rust highlighting");
        let spans = &syntax_spans[0];

        let lines = push_wrapped_diff_line_with_syntax_and_style_context(
            1,
            DiffLineType::Insert,
            long_rust,
            80,
            line_number_width(1),
            spans,
            current_diff_render_style_context(),
        );

        assert!(
            lines.len() > 1,
            "syntax-highlighted long line should wrap to multiple lines, got {}",
            lines.len()
        );

        snapshot_lines("syntax_highlighted_insert_wraps", lines, 90, 10);
    }

    #[test]
    fn ui_snapshot_syntax_highlighted_insert_wraps_text() {
        let long_rust = "fn very_long_function_name(arg_one: String, arg_two: String, arg_three: String, arg_four: String) -> Result<String, Box<dyn std::error::Error>> { Ok(arg_one) }";

        let syntax_spans =
            highlight_code_to_styled_spans(long_rust, "rust").expect("rust highlighting");
        let spans = &syntax_spans[0];

        let lines = push_wrapped_diff_line_with_syntax_and_style_context(
            1,
            DiffLineType::Insert,
            long_rust,
            80,
            line_number_width(1),
            spans,
            current_diff_render_style_context(),
        );

        snapshot_lines_text("syntax_highlighted_insert_wraps_text", &lines);
    }

    #[test]
    fn ui_snapshot_diff_gallery_80x24() {
        snapshot_diff_gallery("diff_gallery_80x24", 80, 24);
    }

    #[test]
    fn ui_snapshot_diff_gallery_94x35() {
        snapshot_diff_gallery("diff_gallery_94x35", 94, 35);
    }

    #[test]
    fn ui_snapshot_diff_gallery_120x40() {
        snapshot_diff_gallery("diff_gallery_120x40", 120, 40);
    }

    #[test]
    fn ui_snapshot_ansi16_insert_delete_no_background() {
        let mut lines = push_wrapped_diff_line_inner_with_theme_and_color_level(
            1,
            DiffLineType::Insert,
            "added in ansi16 mode",
            80,
            line_number_width(2),
            None,
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        );
        lines.extend(push_wrapped_diff_line_inner_with_theme_and_color_level(
            2,
            DiffLineType::Delete,
            "deleted in ansi16 mode",
            80,
            line_number_width(2),
            None,
            DiffTheme::Dark,
            DiffColorLevel::Ansi16,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        ));

        snapshot_lines("ansi16_insert_delete_no_background", lines, 40, 4);
    }

    #[test]
    fn truecolor_dark_theme_uses_configured_backgrounds() {
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor)
            ),
            Style::default().bg(rgb_color(DARK_TC_ADD_LINE_BG_RGB))
        );
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor)
            ),
            Style::default().bg(rgb_color(DARK_TC_DEL_LINE_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Dark,
                DiffColorLevel::TrueColor
            ),
            style_gutter_dim()
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Delete,
                DiffTheme::Dark,
                DiffColorLevel::TrueColor
            ),
            style_gutter_dim()
        );
    }

    #[test]
    fn ansi256_dark_theme_uses_distinct_add_and_delete_backgrounds() {
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
            ),
            Style::default().bg(indexed_color(DARK_256_ADD_LINE_BG_IDX))
        );
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
            ),
            Style::default().bg(indexed_color(DARK_256_DEL_LINE_BG_IDX))
        );
        assert_ne!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
            ),
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
            ),
            "256-color mode should keep add/delete backgrounds distinct"
        );
    }

    #[test]
    fn theme_scope_backgrounds_override_truecolor_fallback_when_available() {
        let backgrounds = resolve_diff_backgrounds_for(
            DiffTheme::Dark,
            DiffColorLevel::TrueColor,
            DiffScopeBackgroundRgbs {
                inserted: Some((1, 2, 3)),
                deleted: Some((4, 5, 6)),
            },
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            Style::default().bg(rgb_color((1, 2, 3)))
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, backgrounds),
            Style::default().bg(rgb_color((4, 5, 6)))
        );
    }

    #[test]
    fn theme_scope_backgrounds_quantize_to_ansi256() {
        let backgrounds = resolve_diff_backgrounds_for(
            DiffTheme::Dark,
            DiffColorLevel::Ansi256,
            DiffScopeBackgroundRgbs {
                inserted: Some((0, 95, 0)),
                deleted: None,
            },
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            Style::default().bg(indexed_color(22))
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, backgrounds),
            Style::default().bg(indexed_color(DARK_256_DEL_LINE_BG_IDX))
        );
    }

    #[test]
    fn ui_snapshot_theme_scope_background_resolution() {
        let backgrounds = resolve_diff_backgrounds_for(
            DiffTheme::Dark,
            DiffColorLevel::TrueColor,
            DiffScopeBackgroundRgbs {
                inserted: Some((12, 34, 56)),
                deleted: None,
            },
        );
        let snapshot = format!(
            "insert={:?}\ndelete={:?}",
            style_line_bg_for(DiffLineType::Insert, backgrounds).bg,
            style_line_bg_for(DiffLineType::Delete, backgrounds).bg,
        );
        assert_snapshot!("theme_scope_background_resolution", snapshot);
    }

    #[test]
    fn ansi16_disables_line_and_gutter_backgrounds() {
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16)
            ),
            Style::default()
        );
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::Ansi16)
            ),
            Style::default()
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Light,
                DiffColorLevel::Ansi16
            ),
            Style::default().fg(Color::Black)
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Delete,
                DiffTheme::Light,
                DiffColorLevel::Ansi16
            ),
            Style::default().fg(Color::Black)
        );
        let themed_backgrounds = resolve_diff_backgrounds_for(
            DiffTheme::Light,
            DiffColorLevel::Ansi16,
            DiffScopeBackgroundRgbs {
                inserted: Some((8, 9, 10)),
                deleted: Some((11, 12, 13)),
            },
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, themed_backgrounds),
            Style::default()
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, themed_backgrounds),
            Style::default()
        );
    }

    #[test]
    fn light_truecolor_theme_uses_readable_gutter_and_line_backgrounds() {
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Insert,
                fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor)
            ),
            Style::default().bg(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB))
        );
        assert_eq!(
            style_line_bg_for(
                DiffLineType::Delete,
                fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor)
            ),
            Style::default().bg(rgb_color(LIGHT_TC_DEL_LINE_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Light,
                DiffColorLevel::TrueColor
            ),
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Delete,
                DiffTheme::Light,
                DiffColorLevel::TrueColor
            ),
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_DEL_NUM_BG_RGB))
        );
    }

    #[test]
    fn light_theme_wrapped_lines_keep_number_gutter_contrast() {
        let lines = push_wrapped_diff_line_inner_with_theme_and_color_level(
            12,
            DiffLineType::Insert,
            "abcdefghij",
            8,
            line_number_width(12),
            None,
            DiffTheme::Light,
            DiffColorLevel::TrueColor,
            fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor),
        );

        assert!(
            lines.len() > 1,
            "expected wrapped output for gutter style verification"
        );
        assert_eq!(
            lines[0].spans[0].style,
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
        );
        assert_eq!(
            lines[1].spans[0].style,
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
        );
        assert_eq!(lines[0].style.bg, Some(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB)));
        assert_eq!(lines[1].style.bg, Some(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB)));
    }

    #[test]
    fn windows_terminal_promotes_ansi16_to_truecolor_for_diffs() {
        assert_eq!(
            diff_color_level_for_terminal(
                StdoutColorLevel::Ansi16,
                TerminalName::WindowsTerminal,
                false,
                false,
            ),
            DiffColorLevel::TrueColor
        );
    }

    #[test]
    fn wt_session_promotes_ansi16_to_truecolor_for_diffs() {
        assert_eq!(
            diff_color_level_for_terminal(
                StdoutColorLevel::Ansi16,
                TerminalName::Unknown,
                true,
                false,
            ),
            DiffColorLevel::TrueColor
        );
    }

    #[test]
    fn non_windows_terminal_keeps_ansi16_diff_palette() {
        assert_eq!(
            diff_color_level_for_terminal(
                StdoutColorLevel::Ansi16,
                TerminalName::WezTerm,
                false,
                false,
            ),
            DiffColorLevel::Ansi16
        );
    }

    #[test]
    fn wt_session_promotes_unknown_color_level_to_truecolor() {
        assert_eq!(
            diff_color_level_for_terminal(
                StdoutColorLevel::Unknown,
                TerminalName::WindowsTerminal,
                true,
                false,
            ),
            DiffColorLevel::TrueColor
        );
    }

    #[test]
    fn non_wt_windows_terminal_keeps_unknown_color_level_conservative() {
        assert_eq!(
            diff_color_level_for_terminal(
                StdoutColorLevel::Unknown,
                TerminalName::WindowsTerminal,
                false,
                false,
            ),
            DiffColorLevel::Ansi16
        );
    }

    #[test]
    fn explicit_force_override_keeps_ansi16_on_windows_terminal() {
        assert_eq!(
            diff_color_level_for_terminal(
                StdoutColorLevel::Ansi16,
                TerminalName::WindowsTerminal,
                false,
                true,
            ),
            DiffColorLevel::Ansi16
        );
    }

    #[test]
    fn explicit_force_override_keeps_ansi256_on_windows_terminal() {
        assert_eq!(
            diff_color_level_for_terminal(
                StdoutColorLevel::Ansi256,
                TerminalName::WindowsTerminal,
                true,
                true,
            ),
            DiffColorLevel::Ansi256
        );
    }

    #[test]
    fn add_diff_uses_path_extension_for_highlighting() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("highlight_add.rs"),
            FileChange::Add {
                content: "pub fn sum(a: i32, b: i32) -> i32 { a + b }\n".to_string(),
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        let has_rgb = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            has_rgb,
            "add diff for .rs file should produce syntax-highlighted (RGB) spans"
        );
    }

    #[test]
    fn cpp_module_extensions_use_cpp_highlighting() {
        let highlighted_tokens = ["cpp", "cppm", "CPPM", "cxxm", "CxXm", "ixx", "IXX"]
            .into_iter()
            .map(|extension| {
                let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
                changes.insert(
                    PathBuf::from(format!("math.{extension}")),
                    FileChange::Add {
                        content:
                            "export module math;\nexport int sum(int a, int b) { return a + b; }\n"
                                .to_string(),
                    },
                );

                let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
                let rgb_tokens = lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .filter(|span| matches!(span.style.fg, Some(ratatui::style::Color::Rgb(..))))
                    .map(|span| span.content.to_string())
                    .collect::<Vec<_>>();
                assert!(
                    !rgb_tokens.is_empty(),
                    "add diff for .{extension} file should produce syntax-highlighted (RGB) spans"
                );
                (extension, rgb_tokens.join("|"))
            })
            .collect::<Vec<_>>();

        assert_debug_snapshot!("cpp_module_extension_highlighting", highlighted_tokens);
    }

    #[test]
    fn unknown_extension_falls_back_without_syntax_highlighting() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("math.unknown-extension"),
            FileChange::Add {
                content: "export module math;\nexport int value = 42;\n".to_string(),
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        assert!(lines.iter().all(|line| {
            line.spans
                .iter()
                .all(|span| !matches!(span.style.fg, Some(ratatui::style::Color::Rgb(..))))
        }));
    }

    #[test]
    fn delete_diff_uses_path_extension_for_highlighting() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("highlight_delete.py"),
            FileChange::Delete {
                content: "def scale(x):\n    return x * 2\n".to_string(),
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        let has_rgb = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            has_rgb,
            "delete diff for .py file should produce syntax-highlighted (RGB) spans"
        );
    }

    #[test]
    fn detect_lang_for_common_paths() {
        assert!(detect_lang_for_path(Path::new("foo.rs")).is_some());
        assert!(detect_lang_for_path(Path::new("bar.py")).is_some());
        assert!(detect_lang_for_path(Path::new("app.tsx")).is_some());

        assert!(detect_lang_for_path(Path::new("Makefile")).is_none());
        assert!(detect_lang_for_path(Path::new("randomfile")).is_none());
    }

    #[test]
    fn wrap_styled_spans_single_line() {
        let spans = vec![RtSpan::raw("short")];
        let result = wrap_styled_spans(&spans, 80);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn wrap_styled_spans_splits_long_content() {
        let long_text = "a".repeat(100);
        let spans = vec![RtSpan::raw(long_text)];
        let result = wrap_styled_spans(&spans, 40);
        assert!(
            result.len() >= 3,
            "100 chars at 40 cols should produce at least 3 lines, got {}",
            result.len()
        );
    }

    #[test]
    fn wrap_styled_spans_flushes_at_span_boundary() {
        let style_a = Style::default().fg(Color::Red);
        let style_b = Style::default().fg(Color::Blue);
        let spans = vec![
            RtSpan::styled("aaaa", style_a),
            RtSpan::styled("bb", style_b),
        ];
        let result = wrap_styled_spans(&spans, 4);
        assert_eq!(
            result.len(),
            2,
            "span ending exactly at max_cols should flush before next span: {result:?}"
        );
        let first_width: usize = result[0].iter().map(|s| s.content.chars().count()).sum();
        assert!(
            first_width <= 4,
            "first line should be at most 4 cols wide, got {first_width}"
        );
    }

    #[test]
    fn wrap_styled_spans_preserves_styles() {
        let style = Style::default().fg(Color::Green);
        let text = "x".repeat(50);
        let spans = vec![RtSpan::styled(text, style)];
        let result = wrap_styled_spans(&spans, 20);
        for chunk in &result {
            for span in chunk {
                assert_eq!(span.style, style, "style should be preserved across wraps");
            }
        }
    }

    #[test]
    fn wrap_styled_spans_tabs_have_visible_width() {
        let spans = vec![RtSpan::raw("\tabcde")];
        let result = wrap_styled_spans(&spans, 8);
        assert!(
            result.len() >= 2,
            "tab + 5 chars should exceed 8 cols and wrap, got {} line(s): {result:?}",
            result.len()
        );
    }

    #[test]
    fn wrap_styled_spans_wraps_before_first_overflowing_char() {
        let spans = vec![RtSpan::raw("abcd\t界")];
        let result = wrap_styled_spans(&spans, 5);

        let line_text: Vec<String> = result
            .iter()
            .map(|line| {
                line.iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(line_text, vec!["abcd", "\t", "界"]);

        let line_width = |line: &[RtSpan<'static>]| -> usize {
            line.iter()
                .flat_map(|span| span.content.chars())
                .map(|ch| ch.width().unwrap_or(if ch == '\t' { TAB_WIDTH } else { 0 }))
                .sum()
        };
        for line in &result {
            assert!(
                line_width(line) <= 5,
                "wrapped line exceeded width 5: {line:?}"
            );
        }
    }

    #[test]
    fn fallback_wrapping_uses_display_width_for_tabs_and_wide_chars() {
        let width = 8;
        let lines = push_wrapped_diff_line_with_style_context(
            1,
            DiffLineType::Insert,
            "abcd\t界🙂",
            width,
            line_number_width(1),
            current_diff_render_style_context(),
        );

        assert!(lines.len() >= 2, "expected wrapped output, got {lines:?}");
        for line in &lines {
            assert!(
                line_display_width(line) <= width,
                "fallback wrapped line exceeded width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn large_update_diff_skips_highlighting() {
        let line_count = 10_500;
        let original: String = (0..line_count).map(|i| format!("line {i}\n")).collect();
        let modified: String = (0..line_count)
            .map(|i| {
                if i % 2 == 0 {
                    format!("line {i} changed\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        let patch = diffy::create_patch(&original, &modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("huge.rs"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);

        assert!(
            lines.len() > 100,
            "expected many output lines from large diff, got {}",
            lines.len(),
        );

        for line in &lines {
            for span in &line.spans {
                if let Some(ratatui::style::Color::Rgb(..)) = span.style.fg {
                    panic!(
                        "large diff should not have syntax-highlighted spans, \
                         got RGB color in style {:?} for {:?}",
                        span.style, span.content,
                    );
                }
            }
        }
    }

    #[test]
    fn rename_diff_uses_destination_extension_for_highlighting() {
        let original = "fn main() {}\n";
        let modified = "fn main() { println!(\"hi\"); }\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("foo.xyzzy"),
            FileChange::Update {
                unified_diff: patch,
                move_path: Some(PathBuf::from("foo.rs")),
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        let has_rgb = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            has_rgb,
            "rename from .xyzzy to .rs should produce syntax-highlighted (RGB) spans"
        );
    }

    #[test]
    fn update_diff_preserves_multiline_highlight_state_within_hunk() {
        let original = "fn demo() {\n    let s = \"hello\";\n}\n";
        let modified = "fn demo() {\n    let s = \"hello\nworld\";\n}\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("demo.rs"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let expected_multiline =
            highlight_code_to_styled_spans("    let s = \"hello\nworld\";\n", "rust")
                .expect("rust highlighting");
        let expected_style = expected_multiline
            .get(1)
            .and_then(|line| {
                line.iter()
                    .find(|span| span.content.as_ref().contains("world"))
            })
            .map(|span| span.style)
            .expect("expected highlighted span for second multiline string line");

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 120);
        let actual_style = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref().contains("world"))
            .map(|span| span.style)
            .expect("expected rendered diff span containing 'world'");

        assert_eq!(actual_style, expected_style);
    }
}
