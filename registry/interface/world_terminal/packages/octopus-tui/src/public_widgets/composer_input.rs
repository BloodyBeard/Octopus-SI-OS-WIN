use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::time::Duration;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ChatComposer;
use crate::bottom_pane::InputResult;
use crate::render::renderable::Renderable;

pub enum ComposerAction {
    Submitted(String),
    None,
}

pub struct ComposerInput {
    inner: ChatComposer,
    _tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    rx: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
}

impl ComposerInput {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = AppEventSender::new(tx.clone());
        let inner = ChatComposer::new(true, sender, true, "Compose new task".to_string(), false);
        Self { inner, _tx: tx, rx }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn clear(&mut self) {
        self.inner
            .set_text_content(String::new(), Vec::new(), Vec::new());
    }

    pub fn input(&mut self, key: KeyEvent) -> ComposerAction {
        let action = match self.inner.handle_key_event(key).0 {
            InputResult::Submitted { text, .. } => ComposerAction::Submitted(text),
            _ => ComposerAction::None,
        };
        self.drain_app_events();
        action
    }

    pub fn handle_paste(&mut self, pasted: String) -> bool {
        let handled = self.inner.handle_paste(pasted);
        self.drain_app_events();
        handled
    }

    pub fn set_hint_items(&mut self, items: Vec<(impl Into<String>, impl Into<String>)>) {
        let mapped: Vec<(String, String)> = items
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self.inner.set_footer_hint_override(Some(mapped));
    }

    pub fn clear_hint_items(&mut self) {
        self.inner.set_footer_hint_override(None);
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        self.inner.desired_height(width)
    }

    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.inner.cursor_pos(area)
    }

    pub fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        self.inner.render(area, buf);
    }

    pub fn is_in_paste_burst(&self) -> bool {
        self.inner.is_in_paste_burst()
    }

    pub fn flush_paste_burst_if_due(&mut self) -> bool {
        let flushed = self.inner.flush_paste_burst_if_due();
        self.drain_app_events();
        flushed
    }

    pub fn recommended_flush_delay() -> Duration {
        crate::bottom_pane::ChatComposer::recommended_paste_flush_delay()
    }

    fn drain_app_events(&mut self) {
        while self.rx.try_recv().is_ok() {}
    }
}

impl Default for ComposerInput {
    fn default() -> Self {
        Self::new()
    }
}
