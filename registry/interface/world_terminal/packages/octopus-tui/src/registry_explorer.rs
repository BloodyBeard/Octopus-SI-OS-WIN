use std::io::Result;
use std::path::Path;
use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DescriptorEntry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RegistryListing {
    pub(crate) kinds: Vec<(String, Vec<DescriptorEntry>)>,
}

impl RegistryListing {
    pub(crate) fn scan(root: &Path) -> std::io::Result<Self> {
        let mut kinds: Vec<(String, Vec<DescriptorEntry>)> = Vec::new();
        let kind_dirs = std::fs::read_dir(root)?;
        let mut kind_paths: Vec<PathBuf> = kind_dirs
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        kind_paths.sort();
        for kind_path in kind_paths {
            let Some(kind) = kind_path
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let name_dirs = std::fs::read_dir(&kind_path)?;
            let mut name_paths: Vec<PathBuf> = name_dirs
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            name_paths.sort();
            let mut entries: Vec<DescriptorEntry> = Vec::new();
            for name_path in name_paths {
                let Some(name) = name_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                let body = name_path.join(format!("{kind}.json"));
                if body.is_file() {
                    entries.push(DescriptorEntry { name, path: body });
                }
            }
            if !entries.is_empty() {
                kinds.push((kind, entries));
            }
        }
        Ok(Self { kinds })
    }

    pub(crate) fn config_summary(&self) -> Vec<(String, usize)> {
        self.kinds
            .iter()
            .map(|(kind, entries)| (kind.clone(), entries.len()))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    List,
    Detail,
}

pub(crate) struct RegistryExplorerOverlay {
    listing: RegistryListing,
    rows: Vec<RowRef>,
    selected: usize,
    focus: Focus,
    detail: String,
    detail_scroll: u16,
    registry_root: Option<PathBuf>,
    load_error: Option<String>,
    reads: crate::app_server_session::RegistryTabReads,
    is_done: bool,
}

#[derive(Clone, Debug)]
struct RowRef {
    kind_index: usize,
    entry_index: usize,
}

impl RegistryExplorerOverlay {
    pub(crate) fn open(reads: crate::app_server_session::RegistryTabReads) -> Self {
        Self::open_focused(reads, None)
    }

    pub(crate) fn open_focused(
        reads: crate::app_server_session::RegistryTabReads,
        focus_kind: Option<&str>,
    ) -> Self {
        let (registry_root, listing, load_error) = match &reads.registry_root {
            Ok(root) => match RegistryListing::scan(root) {
                Ok(listing) => (Some(root.clone()), listing, None),
                Err(error) => (
                    Some(root.clone()),
                    RegistryListing::default(),
                    Some(format!(
                        "registry root '{}' could not be scanned: {error}",
                        root.display()
                    )),
                ),
            },
            Err(error) => (None, RegistryListing::default(), Some(error.clone())),
        };
        let mut rows = Vec::new();
        for (ki, (_kind, entries)) in listing.kinds.iter().enumerate() {
            for ei in 0..entries.len() {
                rows.push(RowRef {
                    kind_index: ki,
                    entry_index: ei,
                });
            }
        }
        let selected = focus_kind
            .and_then(|kind| {
                listing
                    .kinds
                    .iter()
                    .position(|(k, _)| k == kind)
                    .and_then(|ki| rows.iter().position(|r| r.kind_index == ki))
            })
            .unwrap_or(0);
        let mut me = Self {
            listing,
            rows,
            selected,
            focus: Focus::List,
            detail: String::new(),
            detail_scroll: 0,
            registry_root,
            load_error,
            reads,
            is_done: false,
        };
        me.load_selected_detail();
        me
    }

    fn selected_entry(&self) -> Option<(&str, &DescriptorEntry)> {
        let row = self.rows.get(self.selected)?;
        let (kind, entries) = self.listing.kinds.get(row.kind_index)?;
        let entry = entries.get(row.entry_index)?;
        Some((kind.as_str(), entry))
    }

    fn load_selected_detail(&mut self) {
        self.detail_scroll = 0;
        let Some((_kind, entry)) = self.selected_entry() else {
            self.detail = self.load_error.clone().unwrap_or_else(|| {
                "(no descriptor selected — the registry tree is empty)".to_string()
            });
            return;
        };
        match std::fs::read(&entry.path) {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(_validated) => {
                    self.detail = String::from_utf8_lossy(&bytes).into_owned();
                }
                Err(e) => {
                    self.detail = format!(
                        "(descriptor body is not valid JSON — fail-closed)\n{}\n\nerror: {e}",
                        String::from_utf8_lossy(&bytes)
                    );
                }
            },
            Err(e) => {
                self.detail = format!(
                    "(could not read {} — fail-closed: {e})",
                    entry.path.display()
                );
            }
        }
    }

    fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.rows.len() - 1);
        self.load_selected_detail();
    }

    fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.load_selected_detail();
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
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
                Focus::List => self.select_next(),
                Focus::Detail => self.detail_scroll = self.detail_scroll.saturating_add(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::List => self.select_prev(),
                Focus::Detail => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            },
            KeyCode::Tab | KeyCode::Right | KeyCode::Left => {
                self.focus = match self.focus {
                    Focus::List => Focus::Detail,
                    Focus::Detail => Focus::List,
                };
            }
            KeyCode::Char('r') => {
                let keep_selected = self.selected;
                match self
                    .registry_root
                    .as_ref()
                    .map(|root| RegistryListing::scan(root))
                {
                    Some(Ok(listing)) => {
                        self.listing = listing;
                        self.rows.clear();
                        for (kind_index, (_, entries)) in self.listing.kinds.iter().enumerate() {
                            for entry_index in 0..entries.len() {
                                self.rows.push(RowRef {
                                    kind_index,
                                    entry_index,
                                });
                            }
                        }
                        self.selected = keep_selected.min(self.rows.len().saturating_sub(1));
                        self.load_error = None;
                    }
                    Some(Err(error)) => {
                        self.load_error = Some(error.to_string());
                    }
                    None => {}
                }
                self.load_selected_detail();
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
                Constraint::Length(1),
            ])
            .split(area);
        let header = chunks[0];
        let body = chunks[1];
        let footer = chunks[2];

        self.render_header(header, buf);
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
            .split(body);
        self.render_list(panes[0], buf);
        self.render_detail(panes[1], buf);
        self.render_footer(footer, buf);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let config = match &self.reads.config_kind_count {
            Ok(kinds) => format!("{kinds} registry kinds"),
            Err(_) => "registry kinds: not projected".to_string(),
        };
        let runs = match &self.reads.run_count {
            Ok(runs) => format!("{runs} ledger runs"),
            Err(_) => "ledger runs: not projected".to_string(),
        };
        let line = Line::from(vec![
            Span::from(" TUI reads — ").dim(),
            Span::from(config).cyan(),
            Span::from("  ·  ").dim(),
            Span::from(runs).cyan(),
        ]);
        Paragraph::new(line).render(area, buf);
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focus == Focus::List;
        let block = Block::default()
            .title(" registries — descriptor file explorer ")
            .borders(Borders::ALL)
            .border_style(if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().dim()
            });
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for (ki, (kind, entries)) in self.listing.kinds.iter().enumerate() {
            lines.push(Line::from(Span::from(format!("{kind}/")).bold().cyan()));
            for (ei, entry) in entries.iter().enumerate() {
                let is_selected = self
                    .rows
                    .get(self.selected)
                    .is_some_and(|r| r.kind_index == ki && r.entry_index == ei);
                let prefix = if is_selected { "› " } else { "  " };
                let span = Span::from(format!("{prefix}{}", entry.name));
                let span = if is_selected { span.reversed() } else { span };
                lines.push(Line::from(span));
            }
        }
        if lines.is_empty() {
            lines.push(Line::from(
                "(no descriptors found under the registry root)".dim(),
            ));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }

    fn render_detail(&self, area: Rect, buf: &mut Buffer) {
        let focused = self.focus == Focus::Detail;
        let title = match self.selected_entry() {
            Some((kind, entry)) => format!(" {kind}/{} ", entry.name),
            None => " (no descriptor) ".to_string(),
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(if focused {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().dim()
            });
        let inner = block.inner(area);
        block.render(area, buf);

        Paragraph::new(self.detail.clone())
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0))
            .render(inner, buf);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(Line::from(
            "read-only · j/k move · Tab switch pane · r rescan · q/Esc close".dim(),
        ))
        .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_descriptor(root: &Path, kind: &str, name: &str, body: &str) {
        let dir = root.join(kind).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{kind}.json")), body).unwrap();
    }

    #[test]
    fn scan_lists_descriptors_kind_agnostically() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_descriptor(
            root,
            "interface",
            "world_terminal",
            r#"{"id":"world_terminal"}"#,
        );
        write_descriptor(root, "interface", "world_x", r#"{"id":"world_x"}"#);
        write_descriptor(
            root,
            "verifier",
            "ocr_text_contains",
            r#"{"id":"ocr_text_contains","package":"ocr_text_contains","runtime_handler_ref":"ocr_text_contains","default_params":{}}"#,
        );
        write_descriptor(root, "some_future_kind", "thing", r#"{"k":1}"#);

        let listing = RegistryListing::scan(root).unwrap();
        let kinds: Vec<&str> = listing.kinds.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, vec!["interface", "some_future_kind", "verifier"]);
        let iface = &listing.kinds[0].1;
        assert_eq!(
            iface.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["world_terminal", "world_x"]
        );
        let summary = listing.config_summary();
        assert!(summary.contains(&("interface".to_string(), 2)));
        assert!(summary.contains(&("verifier".to_string(), 1)));
    }

    #[test]
    fn scan_missing_root_fails_closed() {
        let error = RegistryListing::scan(Path::new("/nonexistent/registry/root")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
