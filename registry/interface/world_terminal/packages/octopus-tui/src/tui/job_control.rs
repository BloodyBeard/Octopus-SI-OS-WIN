use std::io::Result;
use std::io::stdout;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU16;
use std::sync::atomic::Ordering;

use crossterm::cursor::MoveTo;
use crossterm::cursor::Show;
use crossterm::event::KeyCode;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use ratatui::crossterm::execute;
use ratatui::layout::Rect;

use crate::key_hint;

use super::DisableAlternateScroll;
use super::EnableAlternateScroll;
use super::Terminal;

pub const SUSPEND_KEY: key_hint::KeyBinding = key_hint::ctrl(KeyCode::Char('z'));

#[derive(Clone)]
pub struct SuspendContext {
    resume_pending: Arc<Mutex<Option<ResumeAction>>>,
    suspend_cursor_y: Arc<AtomicU16>,
}

impl SuspendContext {
    pub(crate) fn new() -> Self {
        Self {
            resume_pending: Arc::new(Mutex::new(None)),
            suspend_cursor_y: Arc::new(AtomicU16::new(0)),
        }
    }

    pub(crate) fn suspend(&self, alt_screen_active: &Arc<AtomicBool>) -> Result<()> {
        if alt_screen_active.load(Ordering::Relaxed) {
            let _ = execute!(stdout(), DisableAlternateScroll);
            let _ = execute!(stdout(), LeaveAlternateScreen);
            self.set_resume_action(ResumeAction::RestoreAlt);
        } else {
            self.set_resume_action(ResumeAction::RealignInline);
        }
        let y = self.suspend_cursor_y.load(Ordering::Relaxed);
        let _ = execute!(stdout(), MoveTo(0, y), Show);
        suspend_process()?;
        super::reapply_raw_mode_after_resume()?;

        match crate::terminal_probe::cursor_position(crate::terminal_probe::DEFAULT_TIMEOUT) {
            Ok(Some(position)) => self.set_cursor_y(position.y),
            Ok(None) => tracing::debug!("terminal cursor position unavailable after resume"),
            Err(err) => tracing::debug!(
                error = %err,
                "failed to read terminal cursor position after resume"
            ),
        }
        super::flush_terminal_input_buffer();
        tracing::trace!(
            event = "tui_suspend_resumed",
            cursor_y = self.cursor_y(),
            "restored terminal state after resume"
        );

        Ok(())
    }

    pub(crate) fn prepare_resume_action(
        &self,
        alt_saved_viewport: &mut Option<Rect>,
    ) -> Option<PreparedResumeAction> {
        let action = self.take_resume_action()?;
        match action {
            ResumeAction::RealignInline => {
                let viewport = Rect::new(0, self.cursor_y(), 0, 0);
                Some(PreparedResumeAction::RealignViewport(viewport))
            }
            ResumeAction::RestoreAlt => {
                if let Some(saved) = alt_saved_viewport.as_mut() {
                    saved.y = self.cursor_y();
                }
                Some(PreparedResumeAction::RestoreAltScreen)
            }
        }
    }

    pub(crate) fn set_cursor_y(&self, value: u16) {
        self.suspend_cursor_y.store(value, Ordering::Relaxed);
    }

    fn cursor_y(&self) -> u16 {
        self.suspend_cursor_y.load(Ordering::Relaxed)
    }

    fn set_resume_action(&self, value: ResumeAction) {
        *self
            .resume_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(value);
    }

    fn take_resume_action(&self) -> Option<ResumeAction> {
        self.resume_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResumeAction {
    RealignInline,
    RestoreAlt,
}

#[derive(Clone, Debug)]
pub(crate) enum PreparedResumeAction {
    RestoreAltScreen,
    RealignViewport(Rect),
}

impl PreparedResumeAction {
    pub(crate) fn apply(self, terminal: &mut Terminal) -> Result<()> {
        match self {
            PreparedResumeAction::RealignViewport(area) => {
                terminal.set_viewport_area(area);
            }
            PreparedResumeAction::RestoreAltScreen => {
                execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                execute!(terminal.backend_mut(), EnableAlternateScroll)?;
                if let Ok(size) = terminal.size() {
                    terminal.set_viewport_area(Rect::new(0, 0, size.width, size.height));
                    terminal.clear()?;
                }
            }
        }
        Ok(())
    }
}

fn suspend_process() -> Result<()> {
    super::restore()?;
    super::terminal_stderr::pause()?;
    unsafe { libc::kill(0, libc::SIGTSTP) };
    super::terminal_stderr::resume()?;
    super::set_modes()?;
    Ok(())
}
