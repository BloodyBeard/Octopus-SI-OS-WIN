use crate::tui_config::TuiConfig;
use std::path::Path;
use std::path::PathBuf;

use crate::key_hint::is_plain_text_key_event;
use crate::keymap::RuntimeKeymap;
use crate::status::format_directory_display;
use crate::text_formatting::truncate_text;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use octopus_descriptors::RunId;
use octopus_world_terminal_client::RunEntry;
use octopus_world_terminal_client::TaskEntry;
use octopus_world_terminal_client::TaskStatus;
use octopus_world_terminal_client::TerminalHostRoots;
use octopus_world_terminal_client::TerminalTaskProjection;
use octopus_world_terminal_client::TranscriptEntry;
use octopus_world_terminal_client::TranscriptSide;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use tracing::warn;

const PICKER_CHROME_HEIGHT: u16 = 6;

#[derive(Debug, Clone)]
pub struct SessionTarget {
    pub run: RunId,
    pub record_path: PathBuf,
    pub cwd: Option<PathBuf>,
}

impl SessionTarget {
    pub fn display_label(&self) -> String {
        format!("run {}", self.run.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum SessionSelection {
    StartFresh,
    Resume(SessionTarget),
    Fork(SessionTarget),
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPickerAction {
    Resume,
    Fork,
    Manage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPickerLaunchContext {
    Startup,
    ExistingSession,
}

impl SessionPickerAction {
    fn title(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "Resume a previous chat",
            SessionPickerAction::Fork => "Fork a previous chat",
            SessionPickerAction::Manage => "Tasks",
        }
    }

    fn selection(self, target: SessionTarget) -> SessionSelection {
        match self {
            SessionPickerAction::Resume => SessionSelection::Resume(target),
            SessionPickerAction::Fork => SessionSelection::Fork(target),
            SessionPickerAction::Manage => SessionSelection::Resume(target),
        }
    }
}

fn run_ledger_source(_config: &TuiConfig) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf)> {
    let octopus_home = octopus_utils_home_dir::find_octopus_home()
        .map_err(|error| color_eyre::eyre::eyre!("failed to resolve OCTOPUS_HOME: {error}"))?
        .to_path_buf();
    let roots = TerminalHostRoots::load_from_home(&octopus_home)
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    Ok((
        roots.run_records_root,
        roots.durable_ledger_root,
        octopus_home,
        roots.registry_root,
    ))
}

fn record_cwd(record_path: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(record_path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
}

pub fn session_target_from_run(entry: &RunEntry) -> SessionTarget {
    SessionTarget {
        run: entry.run.clone(),
        record_path: entry.record_path.clone(),
        cwd: record_cwd(&entry.record_path),
    }
}

pub fn list_terminal_runs(config: &TuiConfig) -> Result<Vec<RunEntry>> {
    list_runs(config)
}

pub async fn run_resume_picker(tui: &mut Tui, config: &TuiConfig) -> Result<SessionSelection> {
    run_session_picker(
        tui,
        config,
        SessionPickerAction::Resume,
        SessionPickerLaunchContext::Startup,
    )
    .await
}

pub async fn run_resume_picker_from_existing_session(
    tui: &mut Tui,
    config: &TuiConfig,
) -> Result<SessionSelection> {
    run_session_picker(
        tui,
        config,
        SessionPickerAction::Resume,
        SessionPickerLaunchContext::ExistingSession,
    )
    .await
}

pub async fn run_task_manager(tui: &mut Tui, config: &TuiConfig) -> Result<SessionSelection> {
    run_session_picker(
        tui,
        config,
        SessionPickerAction::Manage,
        SessionPickerLaunchContext::ExistingSession,
    )
    .await
}

pub async fn run_fork_picker(tui: &mut Tui, config: &TuiConfig) -> Result<SessionSelection> {
    run_session_picker(
        tui,
        config,
        SessionPickerAction::Fork,
        SessionPickerLaunchContext::Startup,
    )
    .await
}

fn list_runs(config: &TuiConfig) -> Result<Vec<RunEntry>> {
    let tasks = list_tasks(config)?;
    Ok(tasks
        .into_iter()
        .map(|task| RunEntry {
            run: task.run,
            ee: octopus_descriptors::EeId::new(task.ee),
            worker: task.worker,
            record_path: task.record_path,
            last_activity_unix_ms: task.last_activity_unix_ms,
            preview: task.preview,
            movement_count: task.movement_count,
        })
        .collect())
}

fn list_tasks(config: &TuiConfig) -> Result<Vec<TaskEntry>> {
    TerminalTaskProjection::load(config.codex_home.as_path())
        .map_err(|error| color_eyre::eyre::eyre!("failed to project terminal tasks: {error}"))
}

pub fn run_transcript(config: &TuiConfig, run: &RunId) -> Result<Vec<TranscriptEntry>> {
    let (runs_dir, durable_root, octopus_home, registry_root) = run_ledger_source(config)?;
    let topology = octopus_world_terminal_client::load_private_run_topology(
        &octopus_home,
        &registry_root,
        &runs_dir,
        run,
    )
    .map_err(|err| {
        color_eyre::eyre::eyre!(
            "private terminal run '{}' failed topology validation: {err}",
            run.as_str()
        )
    })?;
    let mut pager = octopus_world_terminal_client::ReverseHistoryPager::open(
        &durable_root,
        run.clone(),
        topology.terminal_interface,
    )
    .map_err(|err| color_eyre::eyre::eyre!("failed to open run history: {err}"))?;
    let page = pager
        .read_page(100, octopus_world_terminal_client::MAX_HISTORY_SCAN_ROWS)
        .map_err(|err| color_eyre::eyre::eyre!("failed to read run history tail: {err}"))?;
    Ok(page.entries.into_iter().map(|entry| entry.entry).collect())
}

pub fn render_run_transcript_lines(entries: &[TranscriptEntry]) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in entries {
        for text_line in entry.text.lines() {
            match entry.side {
                TranscriptSide::Inbound => lines.push(format!("you: {text_line}")),
                TranscriptSide::Outbound => lines.push(text_line.to_string()),
            }
        }
    }
    lines
}

async fn run_session_picker(
    tui: &mut Tui,
    config: &TuiConfig,
    action: SessionPickerAction,
    launch_context: SessionPickerLaunchContext,
) -> Result<SessionSelection> {
    let _keymap = picker_runtime_keymap(config)?;
    let alt = AltScreenGuard::enter(tui);

    let (rows, load_error) = match list_tasks(config) {
        Ok(rows) => (rows.into_iter().filter(|row| !row.hidden).collect(), None),
        Err(err) => {
            warn!(%err, "failed to list terminal runs for the session picker");
            (Vec::new(), Some(err.to_string()))
        }
    };

    let mut state = PickerState::new(
        alt.tui.frame_requester(),
        rows,
        action,
        launch_context,
        config,
    );
    state.error = load_error;
    state.request_frame();

    use tokio_stream::StreamExt as _;
    let mut tui_events = alt.tui.event_stream().fuse();

    while let Some(ev) = tui_events.next().await {
        match ev {
            TuiEvent::Key(key) => {
                if matches!(key.kind, KeyEventKind::Release) {
                    continue;
                }
                if let Some(sel) = state.handle_key(key) {
                    return Ok(sel);
                }
            }
            TuiEvent::Paste(pasted) => {
                state.handle_paste(pasted);
            }
            TuiEvent::Draw | TuiEvent::Resize => {
                if let Ok(size) = alt.tui.terminal.size() {
                    let list_height = size.height.saturating_sub(PICKER_CHROME_HEIGHT) as usize;
                    state.update_viewport(list_height.max(1));
                }
                draw_picker(alt.tui, &state)?;
            }
        }
    }

    Ok(SessionSelection::StartFresh)
}

fn picker_runtime_keymap(config: &TuiConfig) -> Result<RuntimeKeymap> {
    RuntimeKeymap::from_config(&config.terminal.keymap)
        .map_err(|err| color_eyre::eyre::eyre!("invalid keymap configuration: {err}"))
}

#[derive(Clone)]
struct Row {
    run: RunId,
    record_path: PathBuf,
    ee: String,
    worker: String,
    title: String,
    status: TaskStatus,
    pending_request_count: usize,
    preview: Option<String>,
    cwd: Option<PathBuf>,
}

impl Row {
    fn from_entry(entry: &TaskEntry) -> Self {
        Self {
            run: entry.run.clone(),
            record_path: entry.record_path.clone(),
            ee: entry.ee.clone(),
            worker: entry.worker.clone(),
            title: entry.title.clone(),
            status: entry.status,
            pending_request_count: entry.pending_request_count,
            preview: entry.preview.clone(),
            cwd: Some(entry.root.clone()),
        }
    }

    fn target(&self) -> SessionTarget {
        SessionTarget {
            run: self.run.clone(),
            record_path: self.record_path.clone(),
            cwd: self.cwd.clone(),
        }
    }

    fn display_preview(&self) -> &str {
        self.preview.as_deref().unwrap_or("(no activity yet)")
    }

    fn matches_query(&self, query: &str) -> bool {
        if self.run.as_str().to_lowercase().contains(query) {
            return true;
        }
        if self.title.to_lowercase().contains(query) {
            return true;
        }
        if self.worker.to_lowercase().contains(query) {
            return true;
        }
        if self.ee.to_lowercase().contains(query) {
            return true;
        }
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.to_lowercase().contains(query))
        {
            return true;
        }
        if self
            .cwd
            .as_ref()
            .is_some_and(|cwd| cwd.to_string_lossy().to_lowercase().contains(query))
        {
            return true;
        }
        false
    }
}

struct AltScreenGuard<'a> {
    tui: &'a mut Tui,
}

impl<'a> AltScreenGuard<'a> {
    fn enter(tui: &'a mut Tui) -> Self {
        let _ = tui.enter_alt_screen();
        Self { tui }
    }
}

impl Drop for AltScreenGuard<'_> {
    fn drop(&mut self) {
        let _ = self.tui.leave_alt_screen();
    }
}

struct PickerState {
    requester: FrameRequester,
    all_rows: Vec<Row>,
    filtered_rows: Vec<Row>,
    selected: usize,
    scroll_top: usize,
    view_rows: usize,
    query: String,
    action: SessionPickerAction,
    launch_context: SessionPickerLaunchContext,
    octopus_home: PathBuf,
    rename: Option<String>,
    details: Option<(RunId, Vec<String>)>,
    error: Option<String>,
}

impl PickerState {
    fn new(
        requester: FrameRequester,
        entries: Vec<TaskEntry>,
        action: SessionPickerAction,
        launch_context: SessionPickerLaunchContext,
        config: &TuiConfig,
    ) -> Self {
        let all_rows: Vec<Row> = entries.iter().map(Row::from_entry).collect();
        let filtered_rows = all_rows.clone();
        let octopus_home = config.codex_home.to_path_buf();
        Self {
            requester,
            all_rows,
            filtered_rows,
            selected: 0,
            scroll_top: 0,
            view_rows: 1,
            query: String::new(),
            action,
            launch_context,
            octopus_home,
            rename: None,
            details: None,
            error: None,
        }
    }

    fn request_frame(&self) {
        self.requester.schedule_frame();
    }

    fn update_viewport(&mut self, rows: usize) {
        self.view_rows = if self.action == SessionPickerAction::Manage {
            rows.saturating_sub(4).max(1)
        } else {
            rows.max(1)
        };
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        if self.selected >= self.filtered_rows.len() {
            self.selected = self.filtered_rows.len().saturating_sub(1);
        }
        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
        } else if self.selected >= self.scroll_top + self.view_rows {
            self.scroll_top = self.selected + 1 - self.view_rows;
        }
    }

    fn recompute_filter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.filtered_rows = if query.is_empty() {
            self.all_rows.clone()
        } else {
            self.all_rows
                .iter()
                .filter(|row| row.matches_query(&query))
                .cloned()
                .collect()
        };
        self.selected = 0;
        self.scroll_top = 0;
        self.clamp_scroll();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered_rows.is_empty() {
            return;
        }
        let len = self.filtered_rows.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, len - 1);
        self.selected = next as usize;
        self.clamp_scroll();
        if self.details.as_ref().is_some_and(|(run, _)| {
            Some(run) != self.filtered_rows.get(self.selected).map(|row| &row.run)
        }) {
            self.details = None;
        }
        self.request_frame();
    }

    fn handle_paste(&mut self, pasted: String) {
        let cleaned: String = pasted.chars().filter(|c| !c.is_control()).collect();
        if cleaned.is_empty() {
            return;
        }
        if let Some(rename) = self.rename.as_mut() {
            rename.push_str(&cleaned);
        } else {
            self.query.push_str(&cleaned);
            self.recompute_filter();
        }
        self.request_frame();
    }

    fn selected_target(&self) -> Option<SessionTarget> {
        self.filtered_rows.get(self.selected).map(Row::target)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<SessionSelection> {
        if self.rename.is_some() {
            return self.handle_rename_key(key);
        }
        match key.code {
            KeyCode::Esc => {
                return Some(match self.launch_context {
                    SessionPickerLaunchContext::Startup => SessionSelection::StartFresh,
                    SessionPickerLaunchContext::ExistingSession => SessionSelection::Exit,
                });
            }
            KeyCode::Enter => {
                if let Some(target) = self.selected_target() {
                    return Some(self.action.selection(target));
                }
            }
            KeyCode::Char('n')
                if self.action == SessionPickerAction::Manage
                    && key.modifiers == KeyModifiers::CONTROL =>
            {
                return Some(SessionSelection::StartFresh);
            }
            KeyCode::Char('r')
                if self.action == SessionPickerAction::Manage
                    && key.modifiers == KeyModifiers::CONTROL =>
            {
                if let Some(row) = self.filtered_rows.get(self.selected) {
                    self.rename = Some(row.title.clone());
                    self.error = None;
                    self.request_frame();
                }
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_details();
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-(self.view_rows as isize)),
            KeyCode::PageDown => self.move_selection(self.view_rows as isize),
            KeyCode::Home => self.move_selection(isize::MIN / 2),
            KeyCode::End => self.move_selection(isize::MAX / 2),
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    self.recompute_filter();
                    self.request_frame();
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(SessionSelection::Exit);
            }
            _ => {
                if is_plain_text_key_event(key)
                    && let KeyCode::Char(c) = key.code
                {
                    self.query.push(c);
                    self.recompute_filter();
                    self.request_frame();
                }
            }
        }
        None
    }

    fn handle_rename_key(&mut self, key: KeyEvent) -> Option<SessionSelection> {
        match key.code {
            KeyCode::Esc => {
                self.rename = None;
                self.error = None;
            }
            KeyCode::Enter => {
                let title = self.rename.take().unwrap_or_default();
                let Some(row) = self.filtered_rows.get(self.selected) else {
                    return None;
                };
                match octopus_world_terminal_client::TaskStateStore::open(&self.octopus_home)
                    .and_then(|mut state| state.rename(&row.run, &title))
                {
                    Ok(()) => {
                        let run = row.run.clone();
                        for candidate in &mut self.all_rows {
                            if candidate.run == run {
                                candidate.title = title.trim().to_string();
                            }
                        }
                        self.recompute_filter();
                        self.error = None;
                    }
                    Err(error) => {
                        self.rename = Some(title);
                        self.error = Some(error.to_string());
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(rename) = self.rename.as_mut() {
                    rename.pop();
                }
            }
            KeyCode::Char(value) if is_plain_text_key_event(key) => {
                if let Some(rename) = self.rename.as_mut() {
                    rename.push(value);
                }
            }
            _ => {}
        }
        self.request_frame();
        None
    }

    fn toggle_details(&mut self) {
        let Some(row) = self.filtered_rows.get(self.selected) else {
            return;
        };
        if self
            .details
            .as_ref()
            .is_some_and(|(run, _)| run == &row.run)
        {
            self.details = None;
            self.request_frame();
            return;
        }
        let details = (|| -> Result<Vec<String>> {
            let roots = TerminalHostRoots::load_from_home(&self.octopus_home)
                .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            let topology = octopus_world_terminal_client::load_private_run_topology(
                &self.octopus_home,
                &roots.registry_root,
                &roots.run_records_root,
                &row.run,
            )
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            let mut pager = octopus_world_terminal_client::ReverseHistoryPager::open(
                &roots.durable_ledger_root,
                row.run.clone(),
                topology.terminal_interface,
            )
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            let page = pager
                .read_page(12, octopus_world_terminal_client::MAX_HISTORY_SCAN_ROWS)
                .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            Ok(render_run_transcript_lines(
                &page
                    .entries
                    .into_iter()
                    .map(|entry| entry.entry)
                    .collect::<Vec<_>>(),
            ))
        })();
        match details {
            Ok(lines) => {
                self.details = Some((row.run.clone(), lines));
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        self.request_frame();
    }

    fn visible_rows(&self) -> &[Row] {
        let end = (self.scroll_top + self.view_rows).min(self.filtered_rows.len());
        &self.filtered_rows[self.scroll_top..end]
    }
}

fn draw_picker(tui: &mut Tui, state: &PickerState) -> std::io::Result<()> {
    let height = match tui.terminal.size() {
        Ok(size) => size.height,
        Err(_) => return Ok(()),
    };
    tui.draw(height, |frame| {
        let area = frame.area();
        let [title, search, count, list, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(area.height.saturating_sub(PICKER_CHROME_HEIGHT)),
            Constraint::Length(1),
        ])
        .areas(area);

        let width = area.width;

        let title_line = Line::from(vec![Span::from(state.action.title()).bold().cyan()]);
        frame.render_widget_ref(title_line, title);

        let (input_label, input_value) = state
            .rename
            .as_ref()
            .map(|rename| ("rename: ", rename.clone()))
            .unwrap_or_else(|| ("search: ", state.query.clone()));
        let search_line = Line::from(vec![
            Span::from(input_label).dim(),
            Span::from(input_value),
            Span::from("▎").dim(),
        ]);
        frame.render_widget_ref(search_line, search);

        let count_line = if let Some(error) = state.error.as_deref() {
            Line::from(Span::from(error.to_string()).red())
        } else {
            let count = |status| {
                state
                    .filtered_rows
                    .iter()
                    .filter(|row| row.status == status)
                    .count()
            };
            Line::from(vec![
                Span::from(format!("{} tasks", state.filtered_rows.len())).dim(),
                Span::from(format!("  Needs input {}", count(TaskStatus::NeedsInput))).yellow(),
                Span::from(format!("  Working {}", count(TaskStatus::Working))).cyan(),
                Span::from(format!("  Ready {}", count(TaskStatus::Ready))).green(),
                Span::from(format!("  Finished {}", count(TaskStatus::Finished))).dim(),
            ])
        };
        frame.render_widget_ref(count_line, count);

        let (list, details) = if width >= 90 && state.details.is_some() {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(list);
            (left, Some(right))
        } else {
            (list, None)
        };
        if state.filtered_rows.is_empty() {
            let empty = Line::from(vec![
                Span::from("No resumable sessions in this terminal's runs.").dim(),
            ]);
            frame.render_widget_ref(empty, list);
        } else {
            let mut y = list.y;
            let list_bottom = list.y + list.height;
            let mut prior_status = None;
            for (offset, row) in state.visible_rows().iter().enumerate() {
                if y >= list_bottom {
                    break;
                }
                if prior_status != Some(row.status) {
                    frame.render_widget_ref(
                        Line::from(row.status.label()).bold().dim(),
                        Rect::new(list.x, y, list.width, 1),
                    );
                    y += 1;
                    if y >= list_bottom {
                        break;
                    }
                    prior_status = Some(row.status);
                }
                let absolute = state.scroll_top + offset;
                let selected = absolute == state.selected;
                let row_area = Rect {
                    x: list.x,
                    y,
                    width: list.width,
                    height: 1,
                };
                frame.render_widget_ref(session_row_line(row, selected, list.width), row_area);
                y += 1;
            }
        }

        if let Some(details) = details {
            if let Some((run, lines)) = state.details.as_ref() {
                let mut rendered =
                    vec![Line::from(format!("Transcript · {}", run.as_str())).bold()];
                rendered.extend(
                    lines
                        .iter()
                        .rev()
                        .take(details.height.saturating_sub(2) as usize)
                        .rev()
                        .cloned()
                        .map(Line::from),
                );
                frame.render_widget_ref(ratatui::widgets::Paragraph::new(rendered), details);
            }
        }

        let footer_text = if state.action == SessionPickerAction::Manage {
            "↑↓ move  Enter switch  Ctrl+N new  Ctrl+R rename  Ctrl+T transcript  type to search  Esc close"
        } else {
            "↑↓ move  Enter select  Ctrl+T transcript  type to search  Esc cancel"
        };
        let footer_line = Line::from(Span::from(footer_text).dim());
        frame.render_widget_ref(footer_line, footer);
    })
    .map_err(std::io::Error::other)
}

fn session_row_line(row: &Row, selected: bool, width: u16) -> Line<'static> {
    let marker = if selected { "❯ " } else { "  " };
    let preview = truncate_text(row.display_preview(), width.saturating_sub(34) as usize);
    let cwd = row
        .cwd
        .as_ref()
        .map(|cwd| format_directory_display(cwd, None))
        .unwrap_or_default();
    let mut spans = vec![
        Span::from(marker),
        Span::from(format!("{} ", row.title)).cyan(),
        Span::from(preview).dim(),
    ];
    if row.pending_request_count > 0 {
        spans.push(Span::from(format!("  {} request", row.pending_request_count)).yellow());
    }
    if !cwd.is_empty() {
        spans.push(Span::from(format!("  {cwd}")).dim());
    }
    let style = if selected {
        Style::default().bold()
    } else {
        Style::default()
    };
    Line::from(spans).style(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_transcript_lines_prefixes_inbound_and_bares_outbound() {
        let entries = vec![
            TranscriptEntry {
                side: TranscriptSide::Inbound,
                recorded_at_unix_ms: 1,
                label: "operator.terminal.input".to_string(),
                text: "hello".to_string(),
            },
            TranscriptEntry {
                side: TranscriptSide::Outbound,
                recorded_at_unix_ms: 2,
                label: "agent.message.delta".to_string(),
                text: "world\nsecond line".to_string(),
            },
        ];
        let lines = render_run_transcript_lines(&entries);
        assert_eq!(lines, vec!["you: hello", "world", "second line"]);
    }

    #[test]
    fn row_matches_query_over_run_worker_and_preview() {
        let row = Row {
            run: RunId::new("run_alpha"),
            record_path: PathBuf::from("/x/run_alpha.json"),
            ee: "ee_operator".to_string(),
            worker: "system_worker_flash".to_string(),
            title: "Alpha build".to_string(),
            status: TaskStatus::Working,
            pending_request_count: 0,
            preview: Some("build the thing".to_string()),
            cwd: Some(PathBuf::from("/work/project")),
        };
        assert!(row.matches_query("alpha"));
        assert!(row.matches_query("flash"));
        assert!(row.matches_query("build"));
        assert!(row.matches_query("project"));
        assert!(!row.matches_query("nonexistent"));
    }
}
