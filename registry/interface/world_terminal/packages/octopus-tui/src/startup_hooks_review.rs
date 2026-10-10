use crate::tui_config::TuiConfig;
use color_eyre::eyre::Result;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::StreamExt;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line_for_keymap;
use crate::config_update::format_config_error;
use crate::hooks_surface::HookTrustUpdate;
use crate::hooks_surface::fetch_hooks_catalog;
use crate::hooks_surface::write_hook_trusts;
use crate::keymap::RuntimeKeymap;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use octopus_world_terminal_client::HookCatalog;
use std::path::PathBuf;

pub(crate) enum StartupHooksReviewOutcome {
    Continue,
    OpenHooksBrowser(HookCatalog),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupHooksReviewSelection {
    ReviewHooks,
    TrustAllAndContinue,
    ContinueWithoutTrusting,
}

pub(crate) async fn load_startup_hooks_review_entry(cwd: PathBuf) -> HookCatalog {
    match fetch_hooks_catalog(cwd).await {
        Ok(catalog) => catalog,
        Err(err) => {
            tracing::warn!("failed to load startup hook review state: {err:#}");
            HookCatalog::default()
        }
    }
}

pub(crate) async fn maybe_run_startup_hooks_review(
    tui: &mut Tui,
    config: &TuiConfig,
    bypass_hook_trust: bool,
    entry: HookCatalog,
) -> Result<StartupHooksReviewOutcome> {
    if !review_is_needed(bypass_hook_trust, &entry) {
        return Ok(StartupHooksReviewOutcome::Continue);
    }

    run_startup_hooks_review_app(tui, config, entry).await
}

async fn run_startup_hooks_review_app(
    tui: &mut Tui,
    config: &TuiConfig,
    entry: HookCatalog,
) -> Result<StartupHooksReviewOutcome> {
    let keymap = RuntimeKeymap::from_config(&config.terminal.keymap)
        .map_err(|err| color_eyre::eyre::eyre!(err))?;
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(tx_raw);
    let mut trust_all_error = None;
    let mut view = selection_view(
        &entry,
        trust_all_error.as_deref(),
        false,
        app_event_tx.clone(),
        &keymap,
    );
    draw_view(tui, &view)?;

    let tui_events = tui.event_stream();
    tokio::pin!(tui_events);

    loop {
        let Some(event) = tui_events.next().await else {
            return Ok(StartupHooksReviewOutcome::Continue);
        };
        match event {
            TuiEvent::Key(key_event) => {
                if matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    view.handle_key_event(key_event);
                }
                let Some(selection) = selected_choice(&mut view) else {
                    draw_view(tui, &view)?;
                    continue;
                };
                match selection {
                    StartupHooksReviewSelection::ReviewHooks => {
                        return Ok(StartupHooksReviewOutcome::OpenHooksBrowser(entry));
                    }
                    StartupHooksReviewSelection::ContinueWithoutTrusting => {
                        return Ok(StartupHooksReviewOutcome::Continue);
                    }
                    StartupHooksReviewSelection::TrustAllAndContinue => {
                        view = selection_view(
                            &entry,
                            trust_all_error.as_deref(),
                            true,
                            app_event_tx.clone(),
                            &keymap,
                        );
                        draw_view(tui, &view)?;
                        let result = write_hook_trusts(
                            entry
                                .hooks
                                .iter()
                                .filter(|hook| hook.needs_review())
                                .map(|hook| HookTrustUpdate {
                                    key: hook.key.clone(),
                                    current_hash: hook.current_hash.clone(),
                                })
                                .collect(),
                        )
                        .await
                        .map(|_| ())
                        .map_err(|err| {
                            format!("Failed to trust hooks: {}", format_config_error(&err))
                        });
                        match result {
                            Ok(()) => return Ok(StartupHooksReviewOutcome::Continue),
                            Err(err) => {
                                trust_all_error = Some(err);
                                view = selection_view(
                                    &entry,
                                    trust_all_error.as_deref(),
                                    false,
                                    app_event_tx.clone(),
                                    &keymap,
                                );
                                draw_view(tui, &view)?;
                            }
                        }
                    }
                }
            }
            TuiEvent::Paste(_) => {}
            TuiEvent::Draw | TuiEvent::Resize => draw_view(tui, &view)?,
        }
    }
}

fn selected_choice(view: &mut ListSelectionView) -> Option<StartupHooksReviewSelection> {
    if !view.is_complete() {
        return None;
    }
    match view.take_last_selected_index() {
        Some(0) => Some(StartupHooksReviewSelection::ReviewHooks),
        Some(1) => Some(StartupHooksReviewSelection::TrustAllAndContinue),
        Some(2) | None => Some(StartupHooksReviewSelection::ContinueWithoutTrusting),
        Some(_) => None,
    }
}

fn selection_view(
    entry: &HookCatalog,
    trust_all_error: Option<&str>,
    trusting_all: bool,
    app_event_tx: AppEventSender,
    keymap: &RuntimeKeymap,
) -> ListSelectionView {
    ListSelectionView::new(
        selection_view_params(entry, trust_all_error, trusting_all, keymap),
        app_event_tx,
        keymap.list.clone(),
    )
}

#[allow(clippy::disallowed_methods)]
fn selection_view_params(
    entry: &HookCatalog,
    trust_all_error: Option<&str>,
    trusting_all: bool,
    keymap: &RuntimeKeymap,
) -> SelectionViewParams {
    let count = review_needed_count(entry);
    let count_line = match count {
        1 => "1 hook is new or changed.".to_string(),
        count => format!("{count} hooks are new or changed."),
    };
    let mut header = ColumnRenderable::new();
    header.push(Line::from("Hooks need review".bold()));
    header.push(Line::from(count_line).yellow());
    header.push(Line::from(
        "Hooks can run outside the sandbox after you trust them.".dim(),
    ));
    if let Some(error) = trust_all_error {
        header.push(Paragraph::new(Line::from(error.to_string()).red()).wrap(Wrap { trim: false }));
    } else if trusting_all {
        header.push(Line::from("Trusting hooks...".dim()));
    }

    SelectionViewParams {
        footer_hint: Some(standard_popup_hint_line_for_keymap(&keymap.list)),
        items: vec![
            selection_item("Review hooks", trusting_all),
            selection_item("Trust all and continue", trusting_all),
            selection_item("Continue without trusting (hooks won't run)", trusting_all),
        ],
        header: Box::new(header),
        ..Default::default()
    }
}

fn review_needed_count(entry: &HookCatalog) -> usize {
    entry
        .hooks
        .iter()
        .filter(|hook| hook.needs_review())
        .count()
}

fn review_is_needed(bypass_hook_trust: bool, entry: &HookCatalog) -> bool {
    !bypass_hook_trust && review_needed_count(entry) > 0
}

fn selection_item(name: &str, is_disabled: bool) -> SelectionItem {
    SelectionItem {
        name: name.to_string(),
        dismiss_on_select: true,
        is_disabled,
        ..Default::default()
    }
}

fn draw_view(tui: &mut Tui, view: &ListSelectionView) -> Result<()> {
    tui.draw(u16::MAX, |frame| {
        let area = frame.area();
        frame.render_widget_ref(Clear, area);
        let view_area = Rect::new(
            area.x,
            area.y,
            area.width,
            view.desired_height(area.width).min(area.height),
        );
        frame.render_widget_ref(&StandaloneSelectionView { view }, view_area);
    })?;
    Ok(())
}

struct StandaloneSelectionView<'a> {
    view: &'a ListSelectionView,
}

impl WidgetRef for &StandaloneSelectionView<'_> {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        self.view.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::review_is_needed;
    use super::selection_view;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use crate::keymap::RuntimeKeymap;
    use crate::render::renderable::Renderable;
    use crate::test_support::PathBufExt;
    use crate::test_support::test_path_buf;
    use insta::assert_snapshot;
    use octopus_world_terminal_client::HookCatalog;
    use octopus_world_terminal_client::HookCatalogItem;
    use octopus_world_terminal_client::HookTrustState;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use tokio::sync::mpsc::unbounded_channel;

    fn hook(key: &str, trust: HookTrustState) -> HookCatalogItem {
        HookCatalogItem {
            key: key.to_string(),
            event: "PreToolUse".to_string(),
            matcher: Some("Bash".to_string()),
            command: Some("/tmp/hook.sh".to_string()),
            timeout_sec: 30,
            source: "user".to_string(),
            source_path: "/tmp/hooks.json".to_string(),
            plugin_id: None,
            display_order: 0,
            enabled: false,
            is_managed: false,
            current_hash: format!("sha256:{key}"),
            trust,
        }
    }

    fn entry() -> HookCatalog {
        HookCatalog {
            hooks: vec![
                hook("path:new", HookTrustState::Untrusted),
                hook("path:changed", HookTrustState::Modified),
            ],
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn render_lines(view: &crate::bottom_pane::ListSelectionView, width: u16) -> String {
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
                rendered.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn bypass_hook_trust_suppresses_startup_review() {
        assert!(!review_is_needed(true, &entry()));
    }

    #[test]
    fn untrusted_hooks_need_review_without_bypass() {
        assert!(review_is_needed(false, &entry()));
    }

    #[test]
    fn renders_prompt() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let keymap = RuntimeKeymap::defaults();
        let view = selection_view(&entry(), None, false, AppEventSender::new(tx_raw), &keymap);

        assert_snapshot!("startup_hooks_review_prompt", render_lines(&view, 80));
    }

    #[test]
    fn renders_prompt_with_trust_error() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let keymap = RuntimeKeymap::defaults();
        let view = selection_view(
            &entry(),
            Some(
                "Failed to trust hooks: config/batchWrite failed in TUI: Invalid configuration: features.fast_mode=true is not supported; allowed set [fast_mode=false]",
            ),
            false,
            AppEventSender::new(tx_raw),
            &keymap,
        );

        assert_snapshot!(
            "startup_hooks_review_prompt_with_trust_error",
            render_lines(&view, 62)
        );
    }
}
