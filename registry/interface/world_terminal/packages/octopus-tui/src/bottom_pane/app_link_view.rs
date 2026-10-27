use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use textwrap::wrap;
use url::Url;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height;
use super::selection_popup_common::render_rows;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::key_hint;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ListKeymap;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::style::user_message_style;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppLinkScreen {
    Link,
    InstallConfirmation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppLinkElicitationTarget {
    pub(crate) thread_id: String,
    pub(crate) server_name: String,
    pub(crate) request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppLinkViewParams {
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    pub(crate) instructions: String,
    pub(crate) url: String,
    pub(crate) suggest_reason: Option<String>,
    pub(crate) elicitation_target: Option<AppLinkElicitationTarget>,
}

impl AppLinkViewParams {
    pub(crate) fn from_url_elicitation(
        thread_id: String,
        server_name: &str,
        request_id: String,
        message: &str,
        url: &str,
    ) -> Option<Self> {
        let url = validate_external_url(url)?;
        Some(Self::from_generic_url_parts(
            thread_id,
            server_name,
            request_id,
            message,
            url.as_str(),
        ))
    }

    fn from_generic_url_parts(
        thread_id: String,
        server_name: &str,
        request_id: String,
        message: &str,
        url: &str,
    ) -> Self {
        Self {
            title: "Action required".to_string(),
            description: Some(format!("Server: {server_name}")),
            instructions: "Complete the requested action in your browser, then return here."
                .to_string(),
            url: url.to_string(),
            suggest_reason: Some(message.to_string()),
            elicitation_target: Some(AppLinkElicitationTarget {
                thread_id,
                server_name: server_name.to_string(),
                request_id,
            }),
        }
    }
}

fn validate_external_url(url: &str) -> Option<Url> {
    let parsed = Url::parse(url).ok()?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    Some(parsed)
}

pub(crate) struct AppLinkView {
    title: String,
    description: Option<String>,
    instructions: String,
    url: String,
    suggest_reason: Option<String>,
    elicitation_target: Option<AppLinkElicitationTarget>,
    app_event_tx: AppEventSender,
    screen: AppLinkScreen,
    selected_action: usize,
    complete: bool,
    list_keymap: ListKeymap,
}

impl AppLinkView {
    #[cfg(test)]
    pub(crate) fn new(params: AppLinkViewParams, app_event_tx: AppEventSender) -> Self {
        Self::new_with_keymap(
            params,
            app_event_tx,
            crate::keymap::RuntimeKeymap::defaults().list,
        )
    }

    pub(crate) fn new_with_keymap(
        params: AppLinkViewParams,
        app_event_tx: AppEventSender,
        list_keymap: ListKeymap,
    ) -> Self {
        let AppLinkViewParams {
            title,
            description,
            instructions,
            url,
            suggest_reason,
            elicitation_target,
        } = params;
        Self {
            title,
            description,
            instructions,
            url,
            suggest_reason,
            elicitation_target,
            app_event_tx,
            screen: AppLinkScreen::Link,
            selected_action: 0,
            complete: false,
            list_keymap,
        }
    }

    fn action_labels(&self) -> Vec<&'static str> {
        match self.screen {
            AppLinkScreen::Link => vec!["Open link", "Back"],
            AppLinkScreen::InstallConfirmation => vec!["I finished", "Back"],
        }
    }

    fn move_selection_prev(&mut self) {
        self.selected_action = self.selected_action.saturating_sub(1);
    }

    fn move_selection_next(&mut self) {
        self.selected_action = (self.selected_action + 1).min(self.action_labels().len() - 1);
    }

    fn has_elicitation_target(&self) -> bool {
        self.elicitation_target.is_some()
    }

    fn resolve_elicitation(&self, action: &str) {
        let Some(target) = self.elicitation_target.as_ref() else {
            return;
        };
        self.app_event_tx.elicitation_answer(
            target.request_id.clone(),
            serde_json::json!({ "action": action }),
        );
    }

    fn decline_elicitation_request(&mut self) {
        self.resolve_elicitation("decline");
        self.complete = true;
    }

    fn open_external_url(&mut self) {
        self.app_event_tx.send(AppEvent::OpenUrlInBrowser {
            url: self.url.clone(),
        });
        self.screen = AppLinkScreen::InstallConfirmation;
        self.selected_action = 0;
    }

    fn complete_external_flow_and_close(&mut self) {
        if self.has_elicitation_target() {
            self.resolve_elicitation("accept");
        }
        self.complete = true;
    }

    fn back_to_link_screen(&mut self) {
        self.screen = AppLinkScreen::Link;
        self.selected_action = 0;
    }

    fn activate_selected_action(&mut self) {
        if self.has_elicitation_target() {
            match self.screen {
                AppLinkScreen::Link => match self.selected_action {
                    0 => self.open_external_url(),
                    _ => self.decline_elicitation_request(),
                },
                AppLinkScreen::InstallConfirmation => match self.selected_action {
                    0 => self.complete_external_flow_and_close(),
                    _ => self.decline_elicitation_request(),
                },
            }
            return;
        }

        match self.screen {
            AppLinkScreen::Link => match self.selected_action {
                0 => self.open_external_url(),
                _ => self.complete = true,
            },
            AppLinkScreen::InstallConfirmation => match self.selected_action {
                0 => self.complete_external_flow_and_close(),
                _ => self.back_to_link_screen(),
            },
        }
    }

    fn content_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self.screen {
            AppLinkScreen::Link => self.link_content_lines(width),
            AppLinkScreen::InstallConfirmation => self.install_confirmation_lines(width),
        }
    }

    fn link_content_lines(&self, width: u16) -> Vec<Line<'static>> {
        let usable_width = width.max(1) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(self.title.clone().bold()));
        if let Some(description) = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
        {
            for line in wrap(description, usable_width) {
                lines.push(Line::from(line.into_owned().dim()));
            }
        }

        lines.push(Line::from(""));
        if let Some(suggest_reason) = self
            .suggest_reason
            .as_deref()
            .map(str::trim)
            .filter(|suggest_reason| !suggest_reason.is_empty())
        {
            for line in wrap(suggest_reason, usable_width) {
                lines.push(Line::from(line.into_owned().italic()));
            }
            lines.push(Line::from(""));
        }
        lines.push(Line::from("URL".dim()));
        for line in wrap(&self.url, usable_width) {
            lines.push(Line::from(line.into_owned()));
        }
        lines.push(Line::from(""));

        let instructions = self.instructions.trim();
        if !instructions.is_empty() {
            for line in wrap(instructions, usable_width) {
                lines.push(Line::from(line.into_owned()));
            }
            lines.push(Line::from(""));
        }

        lines
    }

    fn install_confirmation_lines(&self, width: u16) -> Vec<Line<'static>> {
        let usable_width = width.max(1) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from("Finish in Browser".bold()));
        lines.push(Line::from(""));

        for line in wrap(
            "Complete the requested action in the browser window that just opened.",
            usable_width,
        ) {
            lines.push(Line::from(line.into_owned()));
        }
        for line in wrap("Then return here and select \"I finished\".", usable_width) {
            lines.push(Line::from(line.into_owned()));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(vec!["Link:".dim()]));
        let url_line = Line::from(vec![self.url.clone().cyan().underlined()]);
        lines.extend(adaptive_wrap_lines(
            vec![url_line],
            RtOptions::new(usable_width),
        ));

        lines
    }

    fn action_rows(&self) -> Vec<GenericDisplayRow> {
        self.action_labels()
            .into_iter()
            .enumerate()
            .map(|(index, label)| {
                let prefix = if self.selected_action == index {
                    '›'
                } else {
                    ' '
                };
                GenericDisplayRow {
                    name: format!("{prefix} {}. {label}", index + 1),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn action_state(&self) -> ScrollState {
        let mut state = ScrollState::new();
        state.selected_idx = Some(self.selected_action);
        state
    }

    fn action_rows_height(&self, width: u16) -> u16 {
        let rows = self.action_rows();
        let state = self.action_state();
        measure_rows_height(&rows, &state, rows.len().max(1), width.max(1))
    }

    fn hint_line(&self) -> Line<'static> {
        Line::from(vec![
            "Use ".into(),
            key_hint::plain(KeyCode::Tab).into(),
            " / ".into(),
            key_hint::plain(KeyCode::Up).into(),
            " ".into(),
            key_hint::plain(KeyCode::Down).into(),
            " to move, ".into(),
            key_hint::plain(KeyCode::Enter).into(),
            " to select, ".into(),
            key_hint::plain(KeyCode::Esc).into(),
            " to close".into(),
        ])
    }
}

impl BottomPaneView for AppLinkView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::BackTab,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.move_selection_prev(),
            _ if self.list_keymap.move_left.is_pressed(key_event) => self.move_selection_prev(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.move_selection_next(),
            _ if self.list_keymap.move_right.is_pressed(key_event) => self.move_selection_next(),
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(index) = c
                    .to_digit(10)
                    .and_then(|digit| digit.checked_sub(1))
                    .map(|index| index as usize)
                    && index < self.action_labels().len()
                {
                    self.selected_action = index;
                    self.activate_selected_action();
                }
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.activate_selected_action(),
            _ => {}
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if self.has_elicitation_target() {
            self.resolve_elicitation("decline");
        }
        self.complete = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn terminal_title_requires_action(&self) -> bool {
        self.has_elicitation_target()
    }
}

impl crate::render::renderable::Renderable for AppLinkView {
    fn desired_height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(4).max(1);
        let content_lines = self.content_lines(content_width);
        let content_rows = Paragraph::new(content_lines)
            .wrap(Wrap { trim: false })
            .line_count(content_width)
            .max(1) as u16;
        let action_rows_height = self.action_rows_height(content_width);
        content_rows + action_rows_height + 3
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        Block::default()
            .style(user_message_style())
            .render(area, buf);

        let actions_height = self.action_rows_height(area.width.saturating_sub(4));
        let [content_area, actions_area, hint_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(actions_height),
            Constraint::Length(1),
        ])
        .areas(area);

        let inner = content_area.inset(Insets::vh(1, 2));
        let content_width = inner.width.max(1);
        let lines = self.content_lines(content_width);
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
        crate::terminal_hyperlinks::mark_url_hyperlink(buf, inner, &self.url);

        if actions_area.height > 0 {
            let actions_area = Rect {
                x: actions_area.x.saturating_add(2),
                y: actions_area.y,
                width: actions_area.width.saturating_sub(2),
                height: actions_area.height,
            };
            let action_rows = self.action_rows();
            let action_state = self.action_state();
            render_rows(
                actions_area,
                buf,
                &action_rows,
                &action_state,
                action_rows.len().max(1),
                "No actions",
            );
        }

        if hint_area.height > 0 {
            let hint_area = Rect {
                x: hint_area.x.saturating_add(2),
                y: hint_area.y,
                width: hint_area.width.saturating_sub(2),
                height: hint_area.height,
            };
            self.hint_line().dim().render(hint_area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_command::AppCommand as Op;
    use crate::app_event::AppEvent;
    use crate::render::renderable::Renderable;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc::unbounded_channel;

    fn target() -> AppLinkElicitationTarget {
        AppLinkElicitationTarget {
            thread_id: String::try_from("00000000-0000-0000-0000-000000000001")
                .expect("valid thread id"),
            server_name: "system_ui".to_string(),
            request_id: "request-1".to_string(),
        }
    }

    fn generic_params(
        url: &str,
        elicitation_target: Option<AppLinkElicitationTarget>,
    ) -> AppLinkViewParams {
        AppLinkViewParams {
            title: "Action required".to_string(),
            description: Some("Server: system_ui".to_string()),
            instructions: "Complete the requested action in your browser, then return here."
                .to_string(),
            url: url.to_string(),
            suggest_reason: Some("Complete this external interface step.".to_string()),
            elicitation_target,
        }
    }

    #[test]
    fn url_elicitation_builds_generic_link_params() {
        let target = target();
        let params = AppLinkViewParams::from_url_elicitation(
            target.thread_id.clone(),
            &target.server_name,
            target.request_id.clone(),
            "Complete this external interface step.",
            "https://payments.example/checkout/123",
        )
        .expect("expected generic URL app link params");

        assert_eq!(
            params,
            generic_params(
                "https://payments.example/checkout/123",
                Some(target.clone())
            )
        );
    }

    #[test]
    fn url_elicitation_rejects_untrusted_urls() {
        let target = target();
        for url in [
            "http://payments.example/checkout/123",
            "https://user:pass@payments.example/checkout/123",
            "not-a-url",
        ] {
            let params = AppLinkViewParams::from_url_elicitation(
                target.thread_id.clone(),
                &target.server_name,
                target.request_id.clone(),
                "Complete this external interface step.",
                url,
            );
            assert!(params.is_none(), "expected {url} to be rejected");
        }
    }

    #[test]
    fn regular_link_does_not_require_terminal_title_action() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let view = AppLinkView::new(
            generic_params("https://example.test/notion", None),
            AppEventSender::new(tx_raw),
        );

        assert_eq!(view.action_labels(), vec!["Open link", "Back"]);
        assert!(!view.terminal_title_requires_action());
    }

    #[test]
    fn elicitation_link_requires_terminal_title_action() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let view = AppLinkView::new(
            generic_params("https://example.test/notion", Some(target())),
            AppEventSender::new(tx_raw),
        );

        assert_eq!(view.action_labels(), vec!["Open link", "Back"]);
        assert!(view.terminal_title_requires_action());
    }

    #[test]
    fn horizontal_list_keys_move_action_selection() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let mut view = AppLinkView::new(
            generic_params("https://example.test/notion", None),
            AppEventSender::new(tx_raw),
        );

        assert_eq!(view.selected_action, 0);
        view.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(view.selected_action, 1);
        view.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(view.selected_action, 0);
    }

    #[test]
    fn generic_url_elicitation_resolves_after_confirmation() {
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let target = target();
        let mut view = AppLinkView::new(
            generic_params(
                "https://payments.example/checkout/123",
                Some(target.clone()),
            ),
            AppEventSender::new(tx_raw),
        );

        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match rx.try_recv() {
            Ok(AppEvent::OpenUrlInBrowser { url }) => {
                assert_eq!(url, "https://payments.example/checkout/123");
            }
            Ok(other) => panic!("unexpected app event: {other:?}"),
            Err(err) => panic!("missing app event: {err}"),
        }
        assert_eq!(view.screen, AppLinkScreen::InstallConfirmation);

        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match rx.try_recv() {
            Ok(AppEvent::CodexOp(Op::ElicitationAnswer { request_id, answer })) => {
                assert_eq!(request_id, "request-1");
                assert_eq!(answer, serde_json::json!({ "action": "accept" }));
            }
            Ok(other) => panic!("unexpected app event: {other:?}"),
            Err(err) => panic!("missing app event: {err}"),
        }
        assert!(view.is_complete());
    }

    #[test]
    fn declined_elicitation_resolves_decline() {
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let target = target();
        let mut view = AppLinkView::new(
            generic_params(
                "https://payments.example/checkout/123",
                Some(target.clone()),
            ),
            AppEventSender::new(tx_raw),
        );

        view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));

        match rx.try_recv() {
            Ok(AppEvent::CodexOp(Op::ElicitationAnswer { request_id, answer })) => {
                assert_eq!(request_id, "request-1");
                assert_eq!(answer, serde_json::json!({ "action": "decline" }));
            }
            Ok(other) => panic!("unexpected app event: {other:?}"),
            Err(err) => panic!("missing app event: {err}"),
        }
        assert!(view.is_complete());
    }

    #[test]
    fn confirmation_keeps_url_tail_visible_when_narrow() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let url = "https://example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path/tail42";
        let mut view = AppLinkView::new(generic_params(url, None), AppEventSender::new(tx_raw));
        view.screen = AppLinkScreen::InstallConfirmation;

        let width: u16 = 36;
        let height = view.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let rendered_blob = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| {
                        let symbol = buf[(x, y)].symbol();
                        if symbol.is_empty() {
                            ' '
                        } else {
                            crate::terminal_hyperlinks::strip_osc8(symbol)
                                .chars()
                                .next()
                                .unwrap_or(' ')
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered_blob.contains("tail42"),
            "expected wrapped setup URL tail to remain visible in narrow pane, got:\n{rendered_blob}"
        );
    }
}
