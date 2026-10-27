use crate::AppServerTarget;
use crate::app_backtrack::BacktrackState;
use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event::ExitMode;
use crate::app_event::PermissionProfileSelection;
use crate::app_event::RateLimitRefreshOrigin;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;
use crate::app_event_sender::AppEventSender;
use crate::app_server_session::AppServerBootstrap;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::ApprovalRequest;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::ExternalEditorState;
use crate::diff_render::DiffSummary;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::external_editor;
use crate::file_search::FileSearchManager;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::hooks_surface::HookTrustUpdate;
use crate::keymap::RuntimeKeymap;
use crate::model_catalog::ModelCatalog;
use crate::pager_overlay::Overlay;
use crate::provider_config_runtime::ProviderConfigRuntime;
use crate::provider_config_runtime::SessionLoadRequest;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::SessionSelection;
use crate::resume_picker::SessionTarget;
use crate::session_posture::SessionPermissionSnapshot;
use crate::session_state::ThreadSessionState;
use crate::session_telemetry::SessionTelemetry;
use crate::session_telemetry::TelemetryLauncher;
use crate::session_telemetry::TelemetrySessionInit;
use crate::session_vocab::config_types::ApprovalsReviewer;
use crate::session_vocab::config_types::Personality;
use crate::session_vocab::openai_models::ModelAvailabilityNux;
use crate::session_vocab::openai_models::ModelPreset;
use crate::session_vocab::openai_models::ReasoningEffort as ReasoningEffortConfig;
#[cfg(test)]
use crate::test_support::PathBufExt;
#[cfg(test)]
use crate::test_support::test_path_buf;
use crate::token_usage::TokenUsage;
use crate::transcript_reflow::TranscriptReflowState;
use crate::tui;
use crate::tui::TuiEvent;
use crate::tui_config::TuiConfig;
use crate::version::CODEX_CLI_VERSION;
use crate::workspace_command::AppServerWorkspaceCommandRunner;
use crate::workspace_command::WorkspaceCommandRunner;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use octopus_ansi_escape::ansi_escape_line;
use octopus_descriptors::RunId;
use octopus_substrate_contract::approvals_substrate::AskForApproval;
use octopus_substrate_contract::permission_profile::ActivePermissionProfile;
use octopus_substrate_contract::permission_profile::PermissionProfile;
#[cfg(target_os = "windows")]
use octopus_substrate_contract::permissions::FileSystemSandboxKind;
#[cfg(target_os = "windows")]
use octopus_substrate_contract::shell_env_config::WindowsSandboxLevel;
use octopus_terminal_detection::user_agent;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_world_terminal_config::ModelAvailabilityNuxConfig;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::select;
mod agent_message_consolidation;
mod app_server_events;
mod background_requests;
mod config_persistence;
mod event_dispatch;
mod history_ui;
mod input;
mod ops;
mod pets;
mod platform_actions;
mod resize_reflow;
mod session_lifecycle;
mod startup_prompts;
mod support;
mod thread_goal_actions;
mod transcript_export;

use self::platform_actions::*;
use self::startup_prompts::*;

const EXTERNAL_EDITOR_HINT: &str = "Save and close external editor to continue.";

#[derive(Clone, Debug, PartialEq, Eq)]
struct AutoReviewMode {
    approval_policy: AskForApproval,
    approvals_reviewer: ApprovalsReviewer,
    active_permission_profile: ActivePermissionProfile,
}

fn auto_review_mode() -> AutoReviewMode {
    AutoReviewMode {
        approval_policy: AskForApproval::OnRequest,
        approvals_reviewer: ApprovalsReviewer::AutoReview,
        active_permission_profile: ActivePermissionProfile::new(
            octopus_substrate_contract::permission_profile::BUILT_IN_PERMISSION_PROFILE_WORKSPACE,
        ),
    }
}

#[cfg(test)]
impl AutoReviewMode {
    fn permission_profile(&self) -> PermissionProfile {
        crate::approval_presets::builtin_permission_profile_for_active_permission_profile(
            &self.active_permission_profile,
        )
        .expect("auto-review mode should use a built-in permission profile")
    }
}

#[cfg(target_os = "windows")]
fn managed_filesystem_sandbox_is_restricted(permission_profile: &PermissionProfile) -> bool {
    matches!(
        permission_profile.file_system_sandbox_policy().kind,
        FileSystemSandboxKind::Restricted
    )
}

const COMMIT_ANIMATION_TICK: Duration = tui::TARGET_FRAME_INTERVAL;

pub(crate) const PROMPT_HISTORY_SNAPSHOT_ID: u64 = 1;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub thread_id: Option<String>,
    pub resume_hint: Option<String>,
    pub exit_reason: ExitReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetachReceipt {
    pub(crate) run_id: String,
    pub(crate) reconnect_command: String,
}

#[derive(Debug)]
pub(crate) struct AppRunExit {
    pub(crate) exit_info: AppExitInfo,
    pub(crate) detach_receipt: Option<DetachReceipt>,
}

impl AppExitInfo {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            token_usage: TokenUsage::default(),
            thread_id: None,
            resume_hint: None,
            exit_reason: ExitReason::Fatal(message.into()),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AppRunControl {
    Continue,
    Exit(ExitReason),
    Detached { run_id: String },
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    UserRequested,
    Fatal(String),
}

fn session_summary(
    token_usage: TokenUsage,
    thread_id: Option<String>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<SessionSummary> {
    let usage_line = (!token_usage.is_zero()).then(|| token_usage.to_string());
    let resume_hint = resume_hint_for_resumable_thread(thread_id, thread_name, rollout_path);

    if usage_line.is_none() && resume_hint.is_none() {
        return None;
    }

    Some(SessionSummary {
        usage_line,
        resume_hint,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumableThread {
    thread_id: String,
    thread_name: Option<String>,
}

fn resumable_thread(
    thread_id: Option<String>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<ResumableThread> {
    let thread_id = thread_id?;
    let rollout_path = rollout_path?;
    rollout_path_is_resumable(rollout_path).then_some(ResumableThread {
        thread_id,
        thread_name,
    })
}

fn resume_hint_for_resumable_thread(
    thread_id: Option<String>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<String> {
    let thread = resumable_thread(thread_id, thread_name, rollout_path)?;
    octopus_utils_cli::resume_hint(
        thread.thread_name.as_deref(),
        Some(thread.thread_id.as_str()),
    )
}

pub(crate) fn detach_reconnect_command(target: &AppServerTarget, run_id: &str) -> Result<String> {
    let command = octopus_utils_cli::resume_command(None, Some(run_id))
        .ok_or_else(|| color_eyre::eyre::eyre!("detached run has no reconnectable run identity"))?;
    match target {
        AppServerTarget::Embedded => Err(color_eyre::eyre::eyre!(
            "embedded execution cannot be detached from a durable daemon"
        )),
        AppServerTarget::LocalDaemon { .. } => Ok(command),
        AppServerTarget::Remote { endpoint } => {
            let address = format!("unix://{}", endpoint.control_socket.display());
            let address = octopus_shell_command::parse_command::shlex_join(&[address]);
            Ok(format!("{command} --remote {address}"))
        }
    }
}

fn rollout_path_is_resumable(rollout_path: &Path) -> bool {
    std::fs::metadata(rollout_path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSummary {
    usage_line: Option<String>,
    resume_hint: Option<String>,
}

#[derive(Debug, Default)]
struct InitialHistoryReplayBuffer {
    retained_lines: VecDeque<crate::terminal_hyperlinks::HyperlinkLine>,
    render_from_transcript_tail: bool,
}

pub(crate) struct App {
    model_catalog: Arc<ModelCatalog>,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_widget: ChatWidget,
    workspace_command_runner: Option<WorkspaceCommandRunner>,
    pub(crate) config: TuiConfig,
    config_runtime: Arc<dyn ProviderConfigRuntime>,
    load_request: SessionLoadRequest,
    runtime_approval_policy_override: Option<AskForApproval>,
    runtime_permission_profile_override: Option<RuntimePermissionProfileOverride>,

    pub(crate) file_search: FileSearchManager,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,

    pub(crate) overlay: Option<Overlay>,
    pub(crate) deferred_history_lines: Vec<crate::terminal_hyperlinks::HyperlinkLine>,
    has_emitted_history_lines: bool,
    transcript_reflow: TranscriptReflowState,
    initial_history_replay_buffer: Option<InitialHistoryReplayBuffer>,

    pub(crate) enhanced_keys_supported: bool,
    pub(crate) keymap: RuntimeKeymap,

    pub(crate) commit_anim_running: Arc<AtomicBool>,
    status_line_invalid_items_warned: Arc<AtomicBool>,
    terminal_title_invalid_items_warned: Arc<AtomicBool>,
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    pub(crate) backtrack_render_pending: bool,
    app_server_target: AppServerTarget,

    windows_sandbox: WindowsSandboxState,

    prompt_history: Option<std::sync::Arc<octopus_world_terminal_client::PromptHistory>>,

    pending_hook_enabled_writes: HashMap<String, Option<bool>>,
    side_parent_run: Option<octopus_descriptors::RunId>,
    side_parent_was_running: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct RuntimePermissionProfileOverride {
    permission_profile: PermissionProfile,
    active_permission_profile: Option<ActivePermissionProfile>,
}

impl RuntimePermissionProfileOverride {
    fn from_config(config: &TuiConfig) -> Self {
        Self {
            permission_profile: config.permissions.permission_profile().clone(),
            active_permission_profile: config.permissions.active_permission_profile(),
        }
    }
}

impl App {
    pub fn chatwidget_init_for_forked_or_resumed_thread(
        &self,
        tui: &mut tui::Tui,
        cfg: TuiConfig,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
    ) -> crate::chatwidget::ChatWidgetInit {
        crate::chatwidget::ChatWidgetInit {
            config: cfg,
            config_runtime: self.config_runtime.clone(),
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            workspace_command_runner: self.workspace_command_runner.clone(),
            initial_user_message,
            enhanced_keys_supported: self.enhanced_keys_supported,
            has_chatgpt_account: self.chat_widget.has_chatgpt_account(),
            has_codex_backend_auth: self.chat_widget.has_codex_backend_auth(),
            model_catalog: self.model_catalog.clone(),
            is_first_run: false,
            status_account_display: self.chat_widget.status_account_display().cloned(),
            runtime_model_provider_base_url: self
                .chat_widget
                .runtime_model_provider_base_url()
                .map(str::to_string),
            initial_plan_type: self.chat_widget.current_plan_type(),
            model: Some(self.chat_widget.current_model().to_string()),
            startup_tooltip_override: None,
            status_line_invalid_items_warned: self.status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: self.terminal_title_invalid_items_warned.clone(),
            session_telemetry: self.session_telemetry.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        mut app_server: AppServerSession,
        mut config: TuiConfig,
        config_runtime: Arc<dyn ProviderConfigRuntime>,
        load_request: SessionLoadRequest,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        is_first_run: bool,
        should_prompt_windows_sandbox_nux_at_startup: bool,
        app_server_target: AppServerTarget,
        startup_elapsed_before_app: Duration,
        startup_bootstrap: Option<AppServerBootstrap>,
        startup_hooks_browser: Option<octopus_world_terminal_client::HookCatalog>,
        embedded_spine_attach_notice: Option<String>,
        telemetry: TelemetryLauncher,
    ) -> Result<AppRunExit> {
        use tokio_stream::StreamExt;
        let startup_started_at = Instant::now();
        let (app_event_tx, mut app_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        emit_project_config_warnings(&app_event_tx, &config);
        emit_system_bwrap_warning(&app_event_tx, &config);
        emit_embedded_spine_attach_warning(&app_event_tx, embedded_spine_attach_notice);
        tui.set_notification_settings(
            config.terminal.notifications.method,
            config.terminal.notifications.condition,
        );

        let load_request = SessionLoadRequest {
            harness: normalize_harness_overrides_for_cwd(load_request.harness, &config.cwd)?,
            ..load_request
        };
        let bootstrap = match startup_bootstrap {
            Some(bootstrap) => bootstrap,
            None => app_server.bootstrap(&config).await?,
        };
        let bootstrap_ms = bootstrap.duration.as_millis();
        let mut model = bootstrap.default_model;
        let available_models = bootstrap.available_models;
        let embedded_spine_boot = app_server.drives_embedded_spine();
        if embedded_spine_boot {
            config.model = Some(model.clone());
        }
        let remote_connection = crate::status::remote_connection::remote_connection_status_value(
            &app_server_target,
            None,
        );
        let exit_info = handle_model_migration_prompt_if_needed(
            tui,
            &mut config,
            model.as_str(),
            &app_event_tx,
            &available_models,
        )
        .await;
        if let Some(exit_info) = exit_info {
            app_server
                .shutdown()
                .await
                .inspect_err(|err| {
                    tracing::warn!("app-server shutdown failed: {err}");
                })
                .ok();
            return Ok(AppRunExit {
                exit_info,
                detach_receipt: None,
            });
        }
        if !embedded_spine_boot {
            if let Some(updated_model) = config.model.clone() {
                model = updated_model;
            }
        }
        let collaboration_modes = match app_server.registry_root() {
            Ok(root) => crate::collaboration_modes::load_presets(root),
            Err(error) => {
                tracing::error!(%error, "collaboration mode presets are unavailable without an attached registry");
                Vec::new()
            }
        };
        let model_catalog = Arc::new(ModelCatalog::new(
            available_models.clone(),
            collaboration_modes,
        ));
        let has_chatgpt_account = bootstrap.has_chatgpt_account;
        let has_codex_backend_auth = false;
        let requires_openai_auth = bootstrap.requires_openai_auth;
        let status_account_display = bootstrap.status_account_display.clone();
        let initial_plan_type = bootstrap.plan_type;
        let session_telemetry = telemetry.new_session(TelemetrySessionInit {
            run_id: app_server
                .active_run_id()
                .ok_or_else(|| color_eyre::eyre::eyre!("terminal telemetry has no active run"))?
                .as_str()
                .to_string(),
            model: model.clone(),
            account_email: bootstrap.account_email.clone(),
            originator: telemetry.originator.clone(),
            terminal_user_agent: user_agent(),
        });
        if config
            .terminal
            .status_line
            .as_ref()
            .is_some_and(|cmd| !cmd.is_empty())
        {
            session_telemetry.counter("codex.status_line", 1, &[]);
        }

        let status_line_invalid_items_warned = Arc::new(AtomicBool::new(false));
        let terminal_title_invalid_items_warned = Arc::new(AtomicBool::new(false));
        let workspace_command_runner: WorkspaceCommandRunner =
            Arc::new(AppServerWorkspaceCommandRunner::new());
        let runtime_model_provider_base_url = None;

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let should_prompt_for_paused_goal_after_startup_resume =
            Self::should_prompt_for_paused_goal_after_startup_resume(
                &session_selection,
                &initial_prompt,
                &initial_images,
            );
        let thread_and_widget_started_at = Instant::now();
        let mut resume_transcript_run: Option<RunId> = None;
        if let SessionSelection::Resume(target) | SessionSelection::Fork(target) =
            &session_selection
        {
            resume_transcript_run = Some(target.run.clone());
        }
        let startup_tooltip_override =
            prepare_startup_tooltip_override(&mut config, &available_models, is_first_run).await;
        let init = crate::chatwidget::ChatWidgetInit {
            config: config.clone(),
            config_runtime: config_runtime.clone(),
            frame_requester: tui.frame_requester(),
            app_event_tx: app_event_tx.clone(),
            workspace_command_runner: Some(workspace_command_runner.clone()),
            initial_user_message: crate::chatwidget::create_initial_user_message(
                initial_prompt.clone(),
                initial_images.clone(),
                Vec::new(),
            ),
            enhanced_keys_supported,
            has_chatgpt_account,
            has_codex_backend_auth,
            model_catalog: model_catalog.clone(),
            is_first_run,
            status_account_display: status_account_display.clone(),
            runtime_model_provider_base_url: runtime_model_provider_base_url.clone(),
            initial_plan_type,
            model: Some(model.clone()),
            startup_tooltip_override,
            status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: terminal_title_invalid_items_warned.clone(),
            session_telemetry: session_telemetry.clone(),
        };
        let mut chat_widget = ChatWidget::new_with_app_event(init);
        chat_widget.remote_connection = remote_connection;
        let active_run = app_server.active_run_id().ok_or_else(|| {
            color_eyre::eyre::eyre!("terminal bootstrap completed without an active private run")
        })?;
        let reasoning_effort = available_models
            .iter()
            .find(|preset| preset.model == model)
            .map(|preset| preset.default_reasoning_effort.clone())
            .or_else(|| config.model_reasoning_effort.clone());
        let (forked_from_id, fork_parent_title) = match &session_selection {
            SessionSelection::Fork(target) => (Some(target.run.as_str().to_string()), None),
            _ => (None, None),
        };
        let rollout_path = octopus_world_terminal_client::TerminalHostRoots::load_from_home(
            config.codex_home.as_path(),
        )
        .ok()
        .map(|roots| {
            roots
                .run_records_root
                .join(format!("{}.json", active_run.as_str()))
        });
        chat_widget.handle_thread_session_quiet(ThreadSessionState {
            thread_id: active_run.as_str().to_string(),
            forked_from_id,
            fork_parent_title,
            thread_name: None,
            model: model.clone(),
            model_provider_id: config.model_provider_id.clone(),
            service_tier: config.service_tier.clone(),
            approval_policy: config.permissions.approval_policy.value(),
            approvals_reviewer: config.approvals_reviewer,
            permission_profile: config.permissions.permission_profile().clone(),
            active_permission_profile: config.permissions.active_permission_profile(),
            cwd: config.cwd.clone(),
            runtime_workspace_roots: config.effective_workspace_roots(),
            instruction_source_paths: Vec::new(),
            reasoning_effort,
            collaboration_mode: None,
            personality: config.personality.clone(),
            network_proxy: None,
            rollout_path,
        });
        let prompt_history = app_server.prompt_history_durable_root().and_then(|root| {
            match octopus_world_terminal_client::PromptHistory::load(&root) {
                Ok(history) => Some(std::sync::Arc::new(history)),
                Err(err) => {
                    tracing::warn!(%err, "prompt-history ledger fold failed; cross-session recall unavailable");
                    None
                }
            }
        });
        if let Some(history) = prompt_history.as_deref() {
            chat_widget
                .set_prompt_history_metadata(PROMPT_HISTORY_SNAPSHOT_ID, history.entry_count());
        }
        let thread_and_widget_ms = thread_and_widget_started_at.elapsed().as_millis();
        chat_widget
            .maybe_prompt_windows_sandbox_enable(should_prompt_windows_sandbox_nux_at_startup);

        let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
        let runtime_keymap =
            RuntimeKeymap::from_config(&config.terminal.keymap).map_err(|err| {
                color_eyre::eyre::eyre!(
                    "Invalid `tui.keymap` configuration: {err}\n\
Fix the config and retry.\n\
See the Codex keymap documentation for supported actions and examples."
                )
            })?;
        let mut app = Self {
            model_catalog,
            session_telemetry: session_telemetry.clone(),
            app_event_tx,
            chat_widget,
            workspace_command_runner: Some(workspace_command_runner),
            config,
            config_runtime,
            load_request,
            runtime_approval_policy_override: None,
            runtime_permission_profile_override: None,
            file_search,
            enhanced_keys_supported,
            keymap: runtime_keymap,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            transcript_reflow: TranscriptReflowState::default(),
            initial_history_replay_buffer: None,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: terminal_title_invalid_items_warned.clone(),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            app_server_target,
            windows_sandbox: WindowsSandboxState::default(),
            prompt_history,
            pending_hook_enabled_writes: HashMap::new(),
            side_parent_run: None,
            side_parent_was_running: false,
        };
        app.refresh_task_mentions(&app_server);
        if let Some(entry) = startup_hooks_browser {
            app.chat_widget.open_hooks_browser(entry);
        }
        let initial_session_started_at = Instant::now();
        if should_prompt_for_paused_goal_after_startup_resume {
            app.maybe_prompt_resume_paused_goal_after_resume(&mut app_server)
                .await;
        }
        if let Some(run) = resume_transcript_run {
            app.replay_run_transcript_as_history(&run);
        }
        let initial_session_ms = initial_session_started_at.elapsed().as_millis();

        #[cfg(target_os = "windows")]
        {
            let startup_permission_profile = app.config.permissions.effective_permission_profile();
            let should_check = crate::windows_sandbox::level_from_config(&app.config)
                != WindowsSandboxLevel::Disabled
                && managed_filesystem_sandbox_is_restricted(&startup_permission_profile)
                && !app
                    .config
                    .notices
                    .hide_world_writable_warning
                    .unwrap_or(false);
            if should_check {
                let cwd = app.config.cwd.clone();
                let workspace_roots = app.config.effective_workspace_roots();
                let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
                let tx = app.app_event_tx.clone();
                let logs_base_dir = app.config.codex_home.clone();
                Self::spawn_world_writable_scan(
                    cwd,
                    workspace_roots,
                    env_map,
                    logs_base_dir,
                    startup_permission_profile,
                    tx,
                );
            }
        }

        let event_stream_started_at = Instant::now();
        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        tui.frame_requester().schedule_frame();
        tracing::info!(
            duration_ms = %(startup_elapsed_before_app + startup_started_at.elapsed()).as_millis(),
            bootstrap_ms = %bootstrap_ms,
            thread_and_widget_ms = %thread_and_widget_ms,
            initial_session_ms = %initial_session_ms,
            event_stream_ms = %event_stream_started_at.elapsed().as_millis(),
            "tui startup initial frame scheduled"
        );
        if requires_openai_auth && has_chatgpt_account {
            let reset_hint_request_id = app.chat_widget.start_rate_limit_reset_startup_check();
            app.refresh_rate_limits(
                &app_server,
                RateLimitRefreshOrigin::StartupPrefetch {
                    reset_hint_request_id,
                },
            );
        }

        let mut listen_for_app_server_events = true;

        let pre_loop_exit_reason: Option<ExitReason> = None;

        let local_set = tokio::task::LocalSet::new();
        let exit_reason_result = if let Some(exit_reason) = pre_loop_exit_reason {
            Ok((exit_reason, None))
        } else {
            local_set.run_until(async {
            loop {
                let control = select! {
                    Some(event) = app_event_rx.recv() => {
                        match Box::pin(app.handle_event(tui, &mut app_server, event)).await {
                            Ok(control) => control,
                            Err(err) => break Err(err),
                        }
                    }
                    event = tui_events.next() => {
                        if let Some(event) = event {
                            match app.handle_tui_event(tui, &mut app_server, event).await {
                                Ok(control) => control,
                                Err(err) => break Err(err),
                            }
                        } else {
                            tracing::warn!("terminal input stream closed; shutting down");
                            app.handle_exit_mode(ExitMode::ShutdownFirst).await
                        }
                    }
                    app_server_reply = app_server.next_embedded_reply_lane(),
                        if listen_for_app_server_events => {
                        match app_server_reply {
                            Some(Some(lane_result)) => {
                                app.handle_embedded_reply_lane(&app_server, lane_result);
                            }
                            Some(None) | None => {
                                listen_for_app_server_events = false;
                                tracing::debug!("terminal outbound reader consumer ended");
                            }
                        }
                        AppRunControl::Continue
                    }
                };
                match control {
                    AppRunControl::Continue => {}
                    AppRunControl::Exit(reason) => break Ok((reason, None)),
                    AppRunControl::Detached { run_id } => {
                        break Ok((ExitReason::UserRequested, Some(run_id)));
                    }
                }
            }
            }).await
        };
        if let Err(err) = app_server.shutdown().await {
            tracing::warn!(error = %err, "failed to shut down embedded app server");
        }
        let clear_pet_result = tui.clear_ambient_pet_image();
        let clear_result = tui.terminal.clear();
        let (exit_reason, detached_run_id) = match exit_reason_result {
            Ok(exit) => {
                clear_pet_result?;
                clear_result?;
                exit
            }
            Err(err) => {
                if let Err(clear_pet_err) = clear_pet_result {
                    tracing::warn!(error = %clear_pet_err, "failed to clear ambient pet image");
                }
                if let Err(clear_err) = clear_result {
                    tracing::warn!(error = %clear_err, "failed to clear terminal UI");
                }
                return Err(err);
            }
        };
        let detach_receipt = detached_run_id
            .as_deref()
            .map(|run_id| -> Result<DetachReceipt> {
                Ok(DetachReceipt {
                    run_id: run_id.to_string(),
                    reconnect_command: detach_reconnect_command(&app.app_server_target, run_id)?,
                })
            })
            .transpose()?;
        let thread_id = detached_run_id.or_else(|| app.chat_widget.thread_id());
        let resume_hint = if detach_receipt.is_some() {
            None
        } else {
            resume_hint_for_resumable_thread(
                thread_id.clone(),
                app.chat_widget.thread_name(),
                app.chat_widget.rollout_path().as_deref(),
            )
        };
        Ok(AppRunExit {
            exit_info: AppExitInfo {
                token_usage: app.token_usage(),
                thread_id,
                resume_hint,
                exit_reason,
            },
            detach_receipt,
        })
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: TuiEvent,
    ) -> Result<AppRunControl> {
        if matches!(event, TuiEvent::Draw | TuiEvent::Resize) {
            self.handle_draw_pre_render(tui)?;
        }

        if matches!(self.overlay, Some(Overlay::Registry(_))) {
            self.handle_registry_overlay_event(tui, event)?;
        } else if matches!(self.overlay, Some(Overlay::Dashboard(_))) {
            self.handle_dashboard_overlay_event(tui, app_server, event)?;
        } else if matches!(self.overlay, Some(Overlay::Metrics(_))) {
            self.handle_metrics_overlay_event(tui, app_server, event)?;
        } else if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, app_server, key_event).await;
                }
                TuiEvent::Paste(pasted) => {
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_widget.handle_paste(pasted);
                }
                TuiEvent::Draw | TuiEvent::Resize => {
                    if self.backtrack_render_pending {
                        self.rebuild_transcript_after_backtrack(tui)?;
                        self.backtrack_render_pending = false;
                    }
                    self.chat_widget.maybe_post_pending_notification(tui);
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(AppRunControl::Continue);
                    }
                    self.chat_widget.pre_draw_tick();
                    let rendered_area = self.render_chat_widget_frame(tui)?;
                    if self.chat_widget.ambient_pet_image_enabled() {
                        let terminal_size = tui.terminal.size()?;
                        let ambient_pet_area =
                            Rect::new(0, 0, terminal_size.width, terminal_size.height);
                        if let Err(err) = tui.draw_ambient_pet_image(
                            self.chat_widget
                                .ambient_pet_draw(ambient_pet_area, rendered_area.bottom()),
                        ) {
                            self.handle_ambient_pet_image_render_error(tui, err)?;
                        }
                    }
                    if let Some(request) = self.chat_widget.pet_picker_preview_draw() {
                        if let Err(err) = tui.draw_pet_picker_preview_image(Some(request)) {
                            self.handle_pet_picker_preview_image_render_error(tui, err)?;
                        }
                    } else if self.chat_widget.should_clear_pet_picker_preview_image()
                        && let Err(err) = tui.draw_pet_picker_preview_image(None)
                    {
                        self.handle_pet_picker_preview_image_render_error(tui, err)?;
                    }
                    if self.chat_widget.external_editor_state() == ExternalEditorState::Requested {
                        self.chat_widget
                            .set_external_editor_state(ExternalEditorState::Active);
                        self.app_event_tx.send(AppEvent::LaunchExternalEditor);
                    }
                }
            }
        }
        Ok(AppRunControl::Continue)
    }

    fn handle_registry_overlay_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        if let Some(overlay) = &mut self.overlay {
            overlay.handle_event(tui, event)?;
        }
        if self.overlay.as_ref().is_some_and(Overlay::is_done) {
            self.close_transcript_overlay(tui);
        }
        Ok(())
    }

    fn handle_dashboard_overlay_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &AppServerSession,
        event: TuiEvent,
    ) -> Result<()> {
        let mut is_draw_tick = false;
        if let Some(Overlay::Dashboard(dash)) = &self.overlay {
            if matches!(event, TuiEvent::Draw | TuiEvent::Resize) && dash.wants_refresh() {
                is_draw_tick = true;
                let movements = app_server.dashboard_movements_snapshot();
                let spine_wired = app_server.drives_embedded_spine();
                if let Some(Overlay::Dashboard(dash)) = &mut self.overlay {
                    dash.refresh(movements, spine_wired);
                }
            }
        }
        if let Some(overlay) = &mut self.overlay {
            overlay.handle_event(tui, event)?;
        }
        if is_draw_tick && matches!(self.overlay, Some(Overlay::Dashboard(_))) {
            tui.frame_requester()
                .schedule_frame_in(std::time::Duration::from_millis(500));
        }
        if self.overlay.as_ref().is_some_and(Overlay::is_done) {
            self.close_transcript_overlay(tui);
        }
        Ok(())
    }

    fn handle_metrics_overlay_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &AppServerSession,
        event: TuiEvent,
    ) -> Result<()> {
        let mut is_draw_tick = false;
        if let Some(Overlay::Metrics(metrics)) = &self.overlay {
            if matches!(event, TuiEvent::Draw | TuiEvent::Resize) && metrics.wants_refresh() {
                is_draw_tick = true;
                let movements = app_server.dashboard_movements_snapshot();
                let spine_wired = app_server.drives_embedded_spine();
                let tokens = self.chat_widget.token_usage();
                if let Some(Overlay::Metrics(metrics)) = &mut self.overlay {
                    metrics.refresh(movements, tokens, spine_wired);
                }
            }
        }
        if let Some(overlay) = &mut self.overlay {
            overlay.handle_event(tui, event)?;
        }
        if is_draw_tick && matches!(self.overlay, Some(Overlay::Metrics(_))) {
            tui.frame_requester()
                .schedule_frame_in(std::time::Duration::from_millis(500));
        }
        if self.overlay.as_ref().is_some_and(Overlay::is_done) {
            self.close_transcript_overlay(tui);
        }
        Ok(())
    }

    pub(super) fn show_shutdown_feedback(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.disable_ambient_pet_before_shutdown(tui)?;
        self.chat_widget.show_shutdown_in_progress();
        self.handle_draw_pre_render(tui)?;
        self.chat_widget.pre_draw_tick();
        self.render_chat_widget_frame(tui)?;
        Ok(())
    }

    fn render_chat_widget_frame(&mut self, tui: &mut tui::Tui) -> Result<Rect> {
        let desired_height = self.chat_widget.desired_height(tui.terminal.size()?.width);
        let mut rendered_area = Rect::default();
        tui.draw_with_resize_reflow(desired_height, |frame| {
            let area = frame.area();
            rendered_area = area;
            self.chat_widget.render(area, frame.buffer);
            if let Some((x, y)) = self.chat_widget.cursor_pos(area) {
                frame.set_cursor_style(self.chat_widget.cursor_style(area));
                frame.set_cursor_position((x, y));
            }
        })?;
        Ok(rendered_area)
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Err(err) = self.chat_widget.clear_managed_terminal_title() {
            tracing::debug!(error = %err, "failed to clear terminal title on app drop");
        }
    }
}

#[cfg(test)]
pub(super) mod test_support;
#[cfg(test)]
mod tests;
