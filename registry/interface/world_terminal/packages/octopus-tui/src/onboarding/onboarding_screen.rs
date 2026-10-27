use crate::tui_config::TuiConfig;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use octopus_exec::LOCAL_FS;
use octopus_git_utils::resolve_root_git_project_for_trust;
#[cfg(target_os = "windows")]
use octopus_substrate_contract::shell_env_config::WindowsSandboxLevel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::widgets::Clear;
use ratatui::widgets::WidgetRef;

use crate::session_vocab::config_types::ForcedLoginMethod;

use crate::LoginStatus;
use crate::config_update::format_config_error;
use crate::config_update::write_trusted_project_local;
use crate::key_hint::KeyBindingListExt;
use crate::onboarding::auth::AuthModeWidget;
use crate::onboarding::auth::SignInOption;
use crate::onboarding::auth::SignInState;
use crate::onboarding::keys;
use crate::onboarding::trust_directory::TrustDirectorySelection;
use crate::onboarding::trust_directory::TrustDirectoryWidget;
use crate::onboarding::welcome::WelcomeWidget;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use color_eyre::eyre::Result;
use octopus_util_abspath::AbsolutePathBuf;
use std::sync::Arc;
use std::sync::RwLock;

#[allow(clippy::large_enum_variant)]
enum Step {
    Welcome(WelcomeWidget),
    Auth(AuthModeWidget),
    TrustDirectory(TrustDirectoryWidget),
}

pub(crate) trait KeyboardHandler {
    fn handle_key_event(&mut self, key_event: KeyEvent);
    fn handle_paste(&mut self, _pasted: String) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    Hidden,
    InProgress,
    Complete,
}

pub(crate) trait StepStateProvider {
    fn get_step_state(&self) -> StepState;
}

pub(crate) struct OnboardingScreen {
    request_frame: FrameRequester,
    steps: Vec<Step>,
    is_done: bool,
    should_exit: bool,
}

pub(crate) struct OnboardingScreenArgs {
    pub show_trust_screen: bool,
    pub show_login_screen: bool,
    pub login_status: LoginStatus,
    pub config: TuiConfig,
    pub api_key_prefill: Option<String>,
    pub config_runtime: std::sync::Arc<dyn crate::provider_config_runtime::ProviderConfigRuntime>,
}

pub(crate) struct OnboardingResult {
    pub directory_trust_persisted: bool,
    pub should_exit: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ApiKeyEntryContext {
    active: bool,
    has_text: bool,
}

impl OnboardingScreen {
    pub(crate) async fn new(tui: &mut Tui, args: OnboardingScreenArgs) -> Self {
        let OnboardingScreenArgs {
            show_trust_screen,
            show_login_screen,
            login_status,
            config,
            api_key_prefill,
            config_runtime: _,
        } = args;
        let cwd = config.cwd.to_path_buf();
        let forced_login_method = config.forced_login_method;
        let mut steps: Vec<Step> = Vec::new();
        steps.push(Step::Welcome(WelcomeWidget::new(
            !matches!(login_status, LoginStatus::NotAuthenticated),
            tui.frame_requester(),
            config.terminal.animations,
        )));
        if show_login_screen {
            let highlighted_mode = match forced_login_method {
                Some(ForcedLoginMethod::Api) => SignInOption::ApiKey,
                _ => SignInOption::ChatGpt,
            };
            steps.push(Step::Auth(AuthModeWidget {
                request_frame: tui.frame_requester(),
                highlighted_mode,
                error: Arc::new(RwLock::new(None)),
                sign_in_state: Arc::new(RwLock::new(SignInState::PickMode)),
                login_status,
                forced_login_method,
                animations_enabled: config.terminal.animations,
                animations_suppressed: std::cell::Cell::new(false),
                api_key_prefill,
            }));
        }
        #[cfg(target_os = "windows")]
        let show_windows_create_sandbox_hint =
            crate::windows_sandbox::level_from_config(&config) == WindowsSandboxLevel::Disabled;
        #[cfg(not(target_os = "windows"))]
        let show_windows_create_sandbox_hint = false;
        let highlighted = TrustDirectorySelection::Trust;
        if show_trust_screen {
            let trust_target = resolve_root_git_project_for_trust(LOCAL_FS.as_ref(), &config.cwd)
                .await
                .map(Into::into)
                .unwrap_or_else(|| cwd.clone());
            steps.push(Step::TrustDirectory(TrustDirectoryWidget {
                cwd,
                trust_target,
                show_windows_create_sandbox_hint,
                should_quit: false,
                selection: None,
                highlighted,
                error: None,
            }))
        }
        Self {
            request_frame: tui.frame_requester(),
            steps,
            is_done: false,
            should_exit: false,
        }
    }

    fn current_steps_mut(&mut self) -> Vec<&mut Step> {
        let mut out: Vec<&mut Step> = Vec::new();
        for step in self.steps.iter_mut() {
            match step.get_step_state() {
                StepState::Hidden => continue,
                StepState::Complete => out.push(step),
                StepState::InProgress => {
                    out.push(step);
                    break;
                }
            }
        }
        out
    }

    fn current_steps(&self) -> Vec<&Step> {
        let mut out: Vec<&Step> = Vec::new();
        for step in self.steps.iter() {
            match step.get_step_state() {
                StepState::Hidden => continue,
                StepState::Complete => out.push(step),
                StepState::InProgress => {
                    out.push(step);
                    break;
                }
            }
        }
        out
    }

    fn should_suppress_animations(&self) -> bool {
        self.current_steps().into_iter().any(|step| match step {
            Step::Auth(widget) => widget.should_suppress_animations(),
            Step::Welcome(_) | Step::TrustDirectory(_) => false,
        })
    }

    fn is_auth_in_progress(&self) -> bool {
        self.steps.iter().any(|step| {
            matches!(step, Step::Auth(_)) && matches!(step.get_step_state(), StepState::InProgress)
        })
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
            || !self
                .steps
                .iter()
                .any(|step| matches!(step.get_step_state(), StepState::InProgress))
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }

    fn cancel_auth_if_active(&self) {
        for step in &self.steps {
            if let Step::Auth(widget) = step {
                widget.cancel_active_attempt();
            }
        }
    }

    fn api_key_entry_context(&self) -> ApiKeyEntryContext {
        self.steps
            .iter()
            .find_map(|step| {
                if let Step::Auth(widget) = step {
                    Some(ApiKeyEntryContext {
                        active: widget.is_api_key_entry_active(),
                        has_text: widget.api_key_entry_has_text(),
                    })
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }
}

impl KeyboardHandler for OnboardingScreen {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        let api_key_entry_context = self.api_key_entry_context();
        let should_quit = key_event.kind == KeyEventKind::Press
            && keys::QUIT.is_pressed(key_event)
            && !suppress_quit_while_typing_api_key(key_event, api_key_entry_context);
        if should_quit {
            if self.is_auth_in_progress() {
                self.cancel_auth_if_active();
                self.should_exit = true;
            }
            self.is_done = true;
        } else {
            if let Some(Step::Welcome(widget)) = self
                .steps
                .iter_mut()
                .find(|step| matches!(step, Step::Welcome(_)))
            {
                widget.handle_key_event(key_event);
            }
            if let Some(active_step) = self.current_steps_mut().into_iter().last() {
                active_step.handle_key_event(key_event);
            }
            if self.steps.iter().any(|step| {
                if let Step::TrustDirectory(widget) = step {
                    widget.should_quit()
                } else {
                    false
                }
            }) {
                self.should_exit = true;
                self.is_done = true;
            }
        }
        self.request_frame.schedule_frame();
    }

    fn handle_paste(&mut self, pasted: String) {
        if pasted.is_empty() {
            return;
        }

        if let Some(active_step) = self.current_steps_mut().into_iter().last() {
            active_step.handle_paste(pasted);
        }
        self.request_frame.schedule_frame();
    }
}

fn suppress_quit_while_typing_api_key(
    key_event: KeyEvent,
    api_key_entry_context: ApiKeyEntryContext,
) -> bool {
    api_key_entry_context.active
        && api_key_entry_context.has_text
        && matches!(key_event.code, KeyCode::Char(_))
        && !key_event
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

impl WidgetRef for &OnboardingScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let suppress_animations = self.should_suppress_animations();
        for step in self.current_steps() {
            match step {
                Step::Welcome(widget) => widget.set_animations_suppressed(suppress_animations),
                Step::Auth(widget) => widget.set_animations_suppressed(suppress_animations),
                Step::TrustDirectory(_) => {}
            }
        }

        Clear.render(area, buf);
        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);
        let width = area.width;

        fn used_rows(tmp: &Buffer, width: u16, height: u16) -> u16 {
            if width == 0 || height == 0 {
                return 0;
            }
            let mut last_non_empty: Option<u16> = None;
            for yy in 0..height {
                let mut any = false;
                for xx in 0..width {
                    let cell = &tmp[(xx, yy)];
                    let has_symbol = !cell.symbol().trim().is_empty();
                    let has_style = cell.fg != Color::Reset
                        || cell.bg != Color::Reset
                        || !cell.modifier.is_empty();
                    if has_symbol || has_style {
                        any = true;
                        break;
                    }
                }
                if any {
                    last_non_empty = Some(yy);
                }
            }
            last_non_empty.map(|v| v + 2).unwrap_or(0)
        }

        let mut i = 0usize;
        let current_steps = self.current_steps();

        while i < current_steps.len() && y < bottom {
            let step = &current_steps[i];
            let max_h = bottom.saturating_sub(y);
            if max_h == 0 || width == 0 {
                break;
            }
            let scratch_area = Rect::new(0, 0, width, max_h);
            let mut scratch = Buffer::empty(scratch_area);
            if let Step::Welcome(widget) = step {
                widget.update_layout_area(scratch_area);
            }
            step.render_ref(scratch_area, &mut scratch);
            let h = used_rows(&scratch, width, max_h).min(max_h);
            if h > 0 {
                let target = Rect {
                    x: area.x,
                    y,
                    width,
                    height: h,
                };
                Clear.render(target, buf);
                step.render_ref(target, buf);
                y = y.saturating_add(h);
            }
            i += 1;
        }
    }
}

impl KeyboardHandler for Step {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match self {
            Step::Welcome(widget) => widget.handle_key_event(key_event),
            Step::Auth(widget) => widget.handle_key_event(key_event),
            Step::TrustDirectory(widget) => widget.handle_key_event(key_event),
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        match self {
            Step::Welcome(_) => {}
            Step::Auth(widget) => widget.handle_paste(pasted),
            Step::TrustDirectory(widget) => widget.handle_paste(pasted),
        }
    }
}

impl StepStateProvider for Step {
    fn get_step_state(&self) -> StepState {
        match self {
            Step::Welcome(w) => w.get_step_state(),
            Step::Auth(w) => w.get_step_state(),
            Step::TrustDirectory(w) => w.get_step_state(),
        }
    }
}

impl WidgetRef for Step {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Step::Welcome(widget) => {
                widget.render_ref(area, buf);
            }
            Step::Auth(widget) => {
                widget.render_ref(area, buf);
            }
            Step::TrustDirectory(widget) => {
                widget.render_ref(area, buf);
            }
        }
    }
}

pub(crate) async fn run_onboarding_app(
    args: OnboardingScreenArgs,
    tui: &mut Tui,
) -> Result<OnboardingResult> {
    use tokio_stream::StreamExt;

    let codex_home = args.config.codex_home.clone();
    let config_runtime = args.config_runtime.clone();
    let mut onboarding_screen = OnboardingScreen::new(tui, args).await;
    let mut directory_trust_persisted = false;

    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&onboarding_screen, frame.area());
    })?;

    let tui_events = tui.event_stream();
    tokio::pin!(tui_events);

    while !onboarding_screen.is_done() {
        if let Some(event) = tui_events.next().await {
            match event {
                TuiEvent::Key(key_event) => {
                    onboarding_screen.handle_key_event(key_event);
                    if !directory_trust_persisted {
                        directory_trust_persisted = persist_selected_trust(
                            &mut onboarding_screen,
                            config_runtime.as_ref(),
                            &codex_home,
                        )
                        .await;
                    }
                }
                TuiEvent::Paste(text) => {
                    onboarding_screen.handle_paste(text);
                }
                TuiEvent::Draw | TuiEvent::Resize => {
                    let _ = tui.draw(u16::MAX, |frame| {
                        frame.render_widget_ref(&onboarding_screen, frame.area());
                    });
                }
            }
        }
    }
    Ok(OnboardingResult {
        directory_trust_persisted,
        should_exit: onboarding_screen.should_exit(),
    })
}

async fn persist_selected_trust(
    onboarding_screen: &mut OnboardingScreen,
    config_runtime: &dyn crate::provider_config_runtime::ProviderConfigRuntime,
    codex_home: &AbsolutePathBuf,
) -> bool {
    let Some((trust_step_index, trust_target)) = onboarding_screen
        .steps
        .iter()
        .enumerate()
        .find_map(|(index, step)| {
            if let Step::TrustDirectory(widget) = step
                && widget.selection == Some(TrustDirectorySelection::Trust)
            {
                return Some((index, widget.trust_target.clone()));
            }
            None
        })
    else {
        return false;
    };

    let result = write_trusted_project_local(config_runtime, codex_home.as_path(), &trust_target);

    match result {
        Ok(()) => true,
        Err(error) => {
            let error = format_config_error(&error);
            tracing::error!(
                "failed to persist trusted project state for {}: {error}",
                trust_target.display()
            );
            if let Step::TrustDirectory(widget) = &mut onboarding_screen.steps[trust_step_index] {
                widget.selection = None;
                widget.error = Some(format!(
                    "Failed to set trust for {}: {error}",
                    trust_target.display()
                ));
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ApiKeyEntryContext;
    use super::OnboardingScreen;
    use super::Step;
    use super::StepStateProvider;
    use super::persist_selected_trust;
    use super::suppress_quit_while_typing_api_key;
    use crate::onboarding::trust_directory::TrustDirectorySelection;
    use crate::onboarding::trust_directory::TrustDirectoryWidget;
    use crate::tui::FrameRequester;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use octopus_util_abspath::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn suppresses_printable_quit_key_during_api_key_entry() {
        let suppressed = suppress_quit_while_typing_api_key(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            ApiKeyEntryContext {
                active: true,
                has_text: true,
            },
        );
        assert!(suppressed);
    }

    #[test]
    fn does_not_suppress_printable_quit_key_when_api_key_input_is_empty() {
        let suppressed = suppress_quit_while_typing_api_key(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            ApiKeyEntryContext {
                active: true,
                has_text: false,
            },
        );
        assert!(!suppressed);
    }

    #[test]
    fn does_not_suppress_control_quit_key_during_api_key_entry() {
        let suppressed = suppress_quit_while_typing_api_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            ApiKeyEntryContext {
                active: true,
                has_text: true,
            },
        );
        assert!(!suppressed);
    }

    #[test]
    fn does_not_suppress_when_not_in_api_key_entry() {
        let suppressed = suppress_quit_while_typing_api_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            ApiKeyEntryContext {
                active: false,
                has_text: true,
            },
        );
        assert!(!suppressed);
    }

    #[tokio::test]
    async fn trust_persistence_failure_keeps_trust_step_in_progress() {
        let temp = tempdir().expect("temp dir");
        let blocked_codex_home = temp.path().join("not-a-dir");
        std::fs::write(&blocked_codex_home, "occupied").expect("seed blocked codex home");
        let blocked_codex_home =
            AbsolutePathBuf::from_absolute_path(&blocked_codex_home).expect("absolute codex home");
        let mut onboarding_screen = OnboardingScreen {
            request_frame: FrameRequester::test_dummy(),
            steps: vec![Step::TrustDirectory(TrustDirectoryWidget {
                cwd: PathBuf::from("/workspace/project"),
                trust_target: PathBuf::from("/workspace/project"),
                show_windows_create_sandbox_hint: false,
                should_quit: false,
                selection: Some(TrustDirectorySelection::Trust),
                highlighted: TrustDirectorySelection::Trust,
                error: None,
            })],
            is_done: false,
            should_exit: false,
        };

        let runtime = crate::test_support::TestProviderConfigRuntime::new();
        let persisted = persist_selected_trust(
            &mut onboarding_screen,
            runtime.as_ref(),
            &blocked_codex_home,
        )
        .await;

        assert!(!persisted);
        let Step::TrustDirectory(widget) = &onboarding_screen.steps[0] else {
            panic!("trust step should remain present");
        };
        assert_eq!(widget.selection, None);
        assert_eq!(widget.get_step_state(), super::StepState::InProgress);
        assert!(
            widget
                .error
                .as_deref()
                .is_some_and(|error| error.contains("failed to persist trusted project state"))
        );
    }

    #[tokio::test]
    async fn trust_persistence_writes_local_project_trust_without_app_server() {
        let codex_home_dir = tempdir().expect("temp codex home");
        let codex_home = AbsolutePathBuf::from_absolute_path(codex_home_dir.path())
            .expect("absolute codex home");
        let project_path = PathBuf::from("/workspace/project");
        let mut onboarding_screen = OnboardingScreen {
            request_frame: FrameRequester::test_dummy(),
            steps: vec![Step::TrustDirectory(TrustDirectoryWidget {
                cwd: project_path.clone(),
                trust_target: project_path.clone(),
                show_windows_create_sandbox_hint: false,
                should_quit: false,
                selection: Some(TrustDirectorySelection::Trust),
                highlighted: TrustDirectorySelection::Trust,
                error: None,
            })],
            is_done: false,
            should_exit: false,
        };

        let runtime = crate::test_support::TestProviderConfigRuntime::new();
        let persisted =
            persist_selected_trust(&mut onboarding_screen, runtime.as_ref(), &codex_home).await;

        assert!(persisted);
        let writes = runtime.trust_writes.lock().expect("test runtime lock");
        assert_eq!(
            writes.as_slice(),
            &[(codex_home_dir.path().to_path_buf(), project_path.clone())]
        );
    }
}
