#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]
use crate::provider_config_runtime::ProviderConfigRuntime;
use crate::provider_config_runtime::SessionHarnessOverrides;
use crate::provider_config_runtime::SessionLoadRequest;
use crate::session_vocab::provider_card::default_model_for_oss_provider;
use additional_dirs::add_dir_warning_message;
use app::App;
pub use app::AppExitInfo;
use app::AppRunExit;
use app::DetachReceipt;
pub use app::ExitReason;
use app_server_session::AppServerSession;
use app_server_session::ThreadParamsMode;
use color_eyre::eyre::WrapErr;
use octopus_exec::EnvironmentManager;
use octopus_exec::ExecServerRuntimePaths;
use octopus_substrate_contract::approvals_substrate::AskForApproval;
use octopus_substrate_contract::sandbox_policy::SandboxMode;
#[cfg(target_os = "windows")]
use octopus_substrate_contract::shell_env_config::WindowsSandboxLevel;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_abspath::canonicalize_existing_preserving_symlinks;
use octopus_utils_home_dir::find_octopus_home as find_codex_home;
pub use octopus_world_terminal_client::DaemonEndpoint;
use octopus_world_terminal_config::AltScreenMode;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
pub use token_usage::TokenUsage;
use tracing::error;
use tracing::warn;
use tracing_appender::non_blocking;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod additional_dirs;
mod app;
mod app_backtrack;
mod app_command;
mod app_event;
mod app_event_sender;
mod app_server_session;
mod approval_events;
mod approval_presets;
mod ascii_animation;
mod bottom_pane;
mod branch_summary;
mod chatwidget;
mod cli;
mod clipboard_copy;
mod clipboard_paste;
mod collaboration_modes;
pub use octopus_world_terminal_render::color;
mod config_update;
pub use octopus_world_terminal_render::custom_terminal;
mod pets;
pub use custom_terminal::Terminal;
mod auto_review_denials;
mod cwd_prompt;
mod dashboard;
mod debug_config;
pub use octopus_world_terminal_render::diff_model;
pub use octopus_world_terminal_render::diff_render;
mod embedded_registry_read;
pub mod embedded_spine;
mod exec_cell;
pub use octopus_world_terminal_render::exec_command;
mod external_editor;
mod file_search;
mod get_git_diff;
mod git_action_directives;
mod goal_display;
mod goal_files;
mod guardian_events;
mod history_cell;
mod hooks_surface;
mod ide_context;
pub use insert_history::insert_history_lines;
pub use octopus_world_terminal_render::insert_history;
#[allow(dead_code)]
pub use octopus_world_terminal_render::key_hint;
mod keymap;
mod keymap_setup;
pub use live_wrap::RowBuilder;
pub use octopus_world_terminal_render::line_truncation;
pub use octopus_world_terminal_render::live_wrap;
pub use octopus_world_terminal_render::markdown;
pub use octopus_world_terminal_render::markdown_render;
pub use octopus_world_terminal_render::markdown_stream;
pub use octopus_world_terminal_render::markdown_text_merge;
mod mention_codec;
mod metrics;
mod model_catalog;
pub use octopus_world_terminal_render::motion;
mod multi_agents;
mod notifications;
pub(crate) mod onboarding;
mod oss_selection;
mod pager_overlay;
mod permission_compat;
mod plan_type;
pub mod provider_config_runtime;
pub(crate) mod public_widgets;
mod registry_explorer;
pub mod session_posture;
pub use octopus_world_terminal_render::render;
mod resize_reflow_cap;
mod resume_picker;
mod sandbox_summary;
mod unavailable_interface_action;
pub use octopus_world_terminal_render::selection_list;
mod service_tier_resolution;
mod session_log;
mod session_state;
pub mod session_telemetry;
pub mod session_vocab;
pub use octopus_world_terminal_render::shimmer;
mod slash_command;
mod startup_hooks_review;
mod status;
mod status_indicator_widget;
mod streaming;
pub use octopus_world_terminal_render::style;
pub use octopus_world_terminal_render::terminal_hyperlinks;
pub use octopus_world_terminal_render::terminal_palette;
pub use octopus_world_terminal_render::terminal_probe;
mod terminal_title;
pub use octopus_world_terminal_render::text_formatting;
mod theme_picker;
mod token_usage;
mod tooltips;
pub use octopus_world_terminal_render::transcript_reflow;
mod tui;
pub(crate) mod tui_config;
mod turn_items;
pub use octopus_world_terminal_render::ui_consts;
mod version;
pub use octopus_world_terminal_render::width;
#[cfg(any(target_os = "windows", test))]
mod windows_sandbox;
mod workspace_command;

pub use octopus_world_terminal_render::wrapping;

pub use octopus_world_terminal_render::table_detect;
#[cfg(test)]
pub use octopus_world_terminal_render::test_backend;
#[cfg(test)]
pub(crate) mod test_support;

use crate::tui_config::TuiConfig;

use crate::onboarding::onboarding_screen::OnboardingScreenArgs;
use crate::onboarding::onboarding_screen::run_onboarding_app;
use crate::startup_hooks_review::StartupHooksReviewOutcome;
use crate::startup_hooks_review::load_startup_hooks_review_entry;
use crate::startup_hooks_review::maybe_run_startup_hooks_review;
use crate::tui::Tui;
pub use cli::Cli;
pub use markdown_render::render_markdown_text;
pub use octopus_utils_cli::ApprovalModeCliArg as TerminalApprovalModeCliArg;
pub use octopus_utils_cli::CliConfigOverrides as TerminalCliConfigOverrides;
pub use octopus_utils_cli::ProfileV2Name as TerminalProfileV2Name;
pub use octopus_utils_cli::SandboxModeCliArg as TerminalSandboxModeCliArg;
pub use octopus_utils_cli::SharedCliOptions as TerminalSharedCliOptions;
pub use public_widgets::composer_input::ComposerAction;
pub use public_widgets::composer_input::ComposerInput;

const TUI_LOG_FILE_NAME: &str = "codex-tui.log";

#[cfg(unix)]
const AUTO_CONNECT_DAEMON_CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(50);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppServerTarget {
    Embedded,
    LocalDaemon { endpoint: DaemonEndpoint },
    Remote { endpoint: DaemonEndpoint },
}

impl AppServerTarget {
    pub(crate) fn uses_remote_workspace(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    fn daemon_endpoint(&self) -> Option<&DaemonEndpoint> {
        match self {
            Self::Embedded => None,
            Self::LocalDaemon { endpoint } | Self::Remote { endpoint } => Some(endpoint),
        }
    }

    fn thread_params_mode(&self) -> ThreadParamsMode {
        if self.uses_remote_workspace() {
            ThreadParamsMode::Remote
        } else {
            ThreadParamsMode::Embedded
        }
    }
}

fn remove_legacy_tui_log_file(codex_home: &Path) {
    let _ = std::fs::remove_file(codex_home.join("log").join(TUI_LOG_FILE_NAME));
}

pub fn resolve_remote_addr(addr: &str) -> color_eyre::Result<DaemonEndpoint> {
    let codex_home = find_codex_home().wrap_err("failed to resolve CODEX_HOME")?;
    if let Some(socket_path) = addr.strip_prefix("unix://") {
        if socket_path.is_empty() {
            return Ok(DaemonEndpoint::local_default(&codex_home));
        }
        let socket_path = AbsolutePathBuf::relative_to_current_dir(socket_path)
            .map_err(color_eyre::Report::new)?;
        return Ok(DaemonEndpoint::with_control_socket(
            &codex_home,
            socket_path.into_path_buf(),
        ));
    }

    color_eyre::eyre::bail!(
        "invalid remote address `{addr}`; expected `unix://` or `unix://PATH`. The daemon endpoint \
         is data (control socket + durable ledger root) — the retired app-server websocket \
         transport (`ws://`/`wss://`) is no longer a lane: the runtime intake accepts only \
         `interface/artifact` notifications and the terminal reads outcomes from the movement ledger."
    );
}

#[cfg(unix)]
async fn maybe_probe_default_daemon_socket(codex_home: &Path) -> Option<DaemonEndpoint> {
    let endpoint = DaemonEndpoint::local_default(codex_home);
    let socket_path = endpoint.control_socket.clone();
    if !socket_path.try_exists().unwrap_or(false) {
        return None;
    }

    match tokio::time::timeout(
        AUTO_CONNECT_DAEMON_CONNECT_TIMEOUT,
        tokio::net::UnixStream::connect(&socket_path),
    )
    .await
    {
        Ok(Ok(_stream)) => Some(endpoint),
        Ok(Err(err)) => {
            tracing::debug!(%err, socket_path = %socket_path.display(), "skipping default app-server daemon socket");
            None
        }
        Err(_) => {
            tracing::debug!(
                socket_path = %socket_path.display(),
                timeout_ms = AUTO_CONNECT_DAEMON_CONNECT_TIMEOUT.as_millis(),
                "timed out probing default app-server daemon socket"
            );
            None
        }
    }
}

#[cfg(not(unix))]
async fn maybe_probe_default_daemon_socket(_codex_home: &Path) -> Option<DaemonEndpoint> {
    None
}

fn attach_private_spine(
    session: AppServerSession,
    octopus_home: PathBuf,
    roots: octopus_world_terminal_client::TerminalHostRoots,
    durable_root: PathBuf,
    topology: octopus_world_terminal_client::PrivateRunTopology,
) -> color_eyre::Result<AppServerSession> {
    session.with_private_topology(octopus_home, roots, durable_root, topology)
}

async fn resolve_private_run_topology(
    selection: &resume_picker::SessionSelection,
    octopus_home: &Path,
    new_run_model: Option<&str>,
    new_run_reasoning_effort: Option<&str>,
) -> color_eyre::Result<octopus_world_terminal_client::PrivateRunTopology> {
    let roots = octopus_world_terminal_client::TerminalHostRoots::load_from_home(octopus_home)
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    match selection {
        resume_picker::SessionSelection::Resume(target) => {
            let topology = octopus_world_terminal_client::load_private_run_topology(
                octopus_home,
                &roots.registry_root,
                &roots.run_records_root,
                &target.run,
            )
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            octopus_world_terminal_client::await_private_run_interfaces(
                &roots.registry_root,
                &topology,
            )
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            Ok(topology)
        }
        resume_picker::SessionSelection::Fork(_) | resume_picker::SessionSelection::StartFresh => {
            let request = match selection {
                resume_picker::SessionSelection::Fork(target) => {
                    octopus_world_terminal_client::fork_private_run_request(
                        octopus_home,
                        &roots.registry_root,
                        &roots.run_records_root,
                        &target.run,
                    )
                }
                resume_picker::SessionSelection::StartFresh => {
                    let mut request = octopus_world_terminal_client::default_private_run_request(
                        &roots.registry_root,
                    )?;
                    if let Some(model) = new_run_model {
                        request.model = model.to_string();
                    }
                    if let Some(reasoning_effort) = new_run_reasoning_effort {
                        request.reasoning_effort = reasoning_effort.to_string();
                    }
                    Ok(request)
                }
                _ => unreachable!(),
            }
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            let authored = octopus_world_terminal_client::author_private_run(
                octopus_home,
                &roots.registry_root,
                request,
            )
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            Ok(authored.topology)
        }
        resume_picker::SessionSelection::Exit => {
            color_eyre::eyre::bail!("cannot resolve a private run after the session picker exited")
        }
    }
}

#[cfg(test)]
pub(crate) async fn start_app_server_for_picker(
    _config: &TuiConfig,
    target: &AppServerTarget,
    _environment_manager: Arc<EnvironmentManager>,
) -> color_eyre::Result<AppServerSession> {
    Ok(AppServerSession::new(target.thread_params_mode()))
}

#[cfg(test)]
pub(crate) async fn start_embedded_app_server_for_picker(
    config: &TuiConfig,
) -> color_eyre::Result<AppServerSession> {
    start_app_server_for_picker(
        config,
        &AppServerTarget::Embedded,
        Arc::new(EnvironmentManager::default_for_tests()),
    )
    .await
}

async fn shutdown_app_server_if_present(app_server: Option<AppServerSession>) {
    if let Some(app_server) = app_server
        && let Err(err) = app_server.shutdown().await
    {
        warn!(%err, "Failed to shut down temporary embedded app server");
    }
}

pub(crate) fn lookup_session_target_by_run_id(
    config: &TuiConfig,
    run_id: &str,
) -> color_eyre::Result<Option<resume_picker::SessionTarget>> {
    let runs = resume_picker::list_terminal_runs(config)?;
    Ok(runs
        .into_iter()
        .find(|entry| entry.run.as_str() == run_id)
        .map(|entry| resume_picker::session_target_from_run(&entry)))
}

fn lookup_latest_session_target(
    config: &TuiConfig,
) -> color_eyre::Result<Option<resume_picker::SessionTarget>> {
    let runs = resume_picker::list_terminal_runs(config)?;
    Ok(runs.first().map(resume_picker::session_target_from_run))
}

fn config_cwd_for_app_server_target(
    cwd: Option<&Path>,
    app_server_target: &AppServerTarget,
    environment_manager: &EnvironmentManager,
) -> std::io::Result<Option<AbsolutePathBuf>> {
    if app_server_target.uses_remote_workspace()
        || environment_manager
            .default_environment()
            .is_some_and(|environment| environment.is_remote())
    {
        return Ok(None);
    }

    let cwd = match cwd {
        Some(path) => {
            AbsolutePathBuf::from_absolute_path(canonicalize_existing_preserving_symlinks(path)?)
        }
        None => AbsolutePathBuf::current_dir(),
    }?;
    Ok(Some(cwd))
}

fn should_load_configured_environments(
    non_default_config_loading: bool,
    app_server_target: &AppServerTarget,
) -> bool {
    !non_default_config_loading && !app_server_target.uses_remote_workspace()
}

#[cfg(test)]
fn latest_session_cwd_filter<'a>(
    uses_remote_workspace: bool,
    remote_cwd_override: Option<&'a Path>,
    config: &'a TuiConfig,
    show_all: bool,
) -> Option<&'a Path> {
    if show_all {
        return None;
    }

    if uses_remote_workspace {
        remote_cwd_override
    } else {
        Some(config.cwd.as_path())
    }
}

fn app_server_target_for_launch(
    explicit_remote_endpoint: Option<DaemonEndpoint>,
    default_daemon_endpoint: Option<DaemonEndpoint>,
    can_reuse_implicit_local_daemon: bool,
) -> AppServerTarget {
    match explicit_remote_endpoint {
        Some(endpoint) => AppServerTarget::Remote { endpoint },
        None if can_reuse_implicit_local_daemon => default_daemon_endpoint
            .map_or(AppServerTarget::Embedded, |endpoint| {
                AppServerTarget::LocalDaemon { endpoint }
            }),
        None => AppServerTarget::Embedded,
    }
}

fn can_reuse_implicit_local_daemon(
    cli_kv_overrides: &[(String, toml::Value)],
    non_default_config_loading: bool,
    profile_v2_selected: bool,
    strict_config: bool,
    has_non_replayable_launch_overrides: bool,
) -> bool {
    cli_kv_overrides.is_empty()
        && !non_default_config_loading
        && !profile_v2_selected
        && !strict_config
        && !has_non_replayable_launch_overrides
}

pub type OssProviderReadiness = Arc<
    dyn Fn(
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>>
        + Send
        + Sync,
>;

pub type LoginStartupGate = Arc<
    dyn Fn(bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>>
        + Send
        + Sync,
>;

#[derive(Default)]
pub struct LauncherProviderHooks {
    pub config_runtime: Option<Arc<dyn ProviderConfigRuntime>>,
    pub non_default_config_loading: bool,
    pub oss_provider_readiness: Option<OssProviderReadiness>,
    pub login_startup_gate: Option<LoginStartupGate>,
    pub telemetry_originator: Option<String>,
    pub onboarding_api_key_prefill: Option<String>,
    pub telemetry_session_factory: Option<session_telemetry::TelemetrySessionFactory>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TerminalRuntimePaths {
    pub self_exe: Option<PathBuf>,
    pub linux_sandbox_exe: Option<PathBuf>,
    pub execve_wrapper_exe: Option<PathBuf>,
}

impl LauncherProviderHooks {
    fn resolved_config_runtime(&self) -> Arc<dyn ProviderConfigRuntime> {
        self.config_runtime
            .clone()
            .unwrap_or_else(crate::provider_config_runtime::degraded_runtime)
    }

    fn resolved_telemetry_originator(&self) -> String {
        self.telemetry_originator
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_NAME").to_string())
    }

    fn telemetry_launcher(&self) -> session_telemetry::TelemetryLauncher {
        session_telemetry::TelemetryLauncher {
            originator: self.resolved_telemetry_originator(),
            session_factory: self.telemetry_session_factory.clone(),
        }
    }
}

pub async fn run_main(
    cli: Cli,
    runtime_paths: TerminalRuntimePaths,
    explicit_remote_endpoint: Option<DaemonEndpoint>,
    launcher_hooks: LauncherProviderHooks,
) -> std::io::Result<AppExitInfo> {
    let strict_config = cli.strict_config;
    let (sandbox_mode, approval_policy) = if cli.dangerously_bypass_approvals_and_sandbox {
        (
            Some(SandboxMode::DangerFullAccess),
            Some(AskForApproval::Never),
        )
    } else {
        (
            cli.sandbox_mode.map(Into::<SandboxMode>::into),
            cli.approval_policy.map(Into::into),
        )
    };

    let raw_overrides = cli.config_overrides.raw_overrides.clone();
    let overrides_cli = octopus_utils_cli::CliConfigOverrides { raw_overrides };
    let cli_kv_overrides = match overrides_cli.parse_overrides() {
        Ok(v) => v,
        #[allow(clippy::print_stderr)]
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };

    #[allow(clippy::print_stderr)]
    let codex_home = match find_codex_home() {
        Ok(codex_home) => codex_home.to_path_buf(),
        Err(err) => {
            eprintln!("Error finding codex home: {err}");
            std::process::exit(1);
        }
    };
    let octopus_home = codex_home.clone();

    let config_runtime = launcher_hooks.resolved_config_runtime();
    let reuse_implicit_local_daemon = can_reuse_implicit_local_daemon(
        &cli_kv_overrides,
        launcher_hooks.non_default_config_loading,
        cli.config_profile_v2.is_some(),
        strict_config,
        cli.bypass_hook_trust,
    );
    let default_daemon = if explicit_remote_endpoint.is_none() && reuse_implicit_local_daemon {
        maybe_probe_default_daemon_socket(&codex_home).await
    } else {
        None
    };
    let app_server_target = app_server_target_for_launch(
        explicit_remote_endpoint,
        default_daemon,
        reuse_implicit_local_daemon,
    );
    let remote_cwd_override = cli
        .cwd
        .clone()
        .filter(|_| app_server_target.uses_remote_workspace());

    let local_runtime_paths = ExecServerRuntimePaths::from_optional_paths(
        runtime_paths.self_exe.clone(),
        runtime_paths.linux_sandbox_exe.clone(),
    )?;
    let environment_manager = if should_load_configured_environments(
        launcher_hooks.non_default_config_loading,
        &app_server_target,
    ) {
        EnvironmentManager::from_codex_home(codex_home.clone(), Some(local_runtime_paths)).await
    } else {
        EnvironmentManager::from_env(Some(local_runtime_paths)).await
    }
    .map(Arc::new)
    .map_err(std::io::Error::other)?;
    let cwd = cli.cwd.clone();

    let cwd_override = if app_server_target.uses_remote_workspace() {
        None
    } else {
        cwd.clone()
    };

    let mut manually_selected_oss_provider = None;
    let model_provider_override = if cli.oss {
        let resolved = config_runtime
            .resolve_oss_provider(cli.oss_provider.clone())
            .await;

        if let Some(provider) = resolved {
            Some(provider)
        } else {
            let selection = oss_selection::select_oss_provider().await?;
            let provider = selection.provider;
            if provider == "__CANCELLED__" {
                return Err(std::io::Error::other(
                    "OSS provider selection was cancelled by user",
                ));
            }
            if selection.manually_selected {
                manually_selected_oss_provider = Some(provider.clone());
            }
            Some(provider)
        }
    } else {
        None
    };

    let model = if let Some(model) = &cli.model {
        Some(model.clone())
    } else if cli.oss {
        model_provider_override
            .as_ref()
            .and_then(|provider_id| default_model_for_oss_provider(provider_id))
            .map(std::borrow::ToOwned::to_owned)
    } else {
        None
    };

    let additional_dirs = cli.add_dir.clone();

    let load_request = SessionLoadRequest {
        codex_home: Some(codex_home.clone()),
        fallback_cwd: None,
        cli_kv_overrides: cli_kv_overrides.clone(),
        profile_v2: cli.config_profile_v2.as_ref().map(ToString::to_string),
        strict_config,
        harness: SessionHarnessOverrides {
            model,
            approval_policy,
            sandbox_mode,
            cwd: cwd_override,
            model_provider: model_provider_override.clone(),
            show_raw_agent_reasoning: cli.oss.then_some(true),
            bypass_hook_trust: cli.bypass_hook_trust.then_some(true),
            additional_writable_roots: additional_dirs,
            default_permissions: None,
            codex_self_exe: runtime_paths.self_exe.clone(),
            codex_linux_sandbox_exe: runtime_paths.linux_sandbox_exe.clone(),
            main_execve_wrapper_exe: runtime_paths.execve_wrapper_exe.clone(),
        },
    };

    let config = load_config_or_exit(config_runtime.as_ref(), load_request.clone()).await;

    remove_legacy_tui_log_file(config.codex_home.as_path());

    let otel_originator = launcher_hooks.resolved_telemetry_originator();
    let telemetry_layers = config_runtime.telemetry_layers(otel_originator.as_str());

    #[allow(clippy::print_stderr)]
    if let Err(message) = config_runtime.execpolicy_warnings().await {
        eprintln!("Error loading rules:\n{message}");
        std::process::exit(1);
    }

    if let Some(warning) = add_dir_warning_message(
        &cli.add_dir,
        &config.permissions.effective_permission_profile(),
        config.cwd.as_path(),
    ) {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("Error adding directories: {warning}");
            std::process::exit(1);
        }
    }

    if let Some(login_gate) = launcher_hooks.login_startup_gate.as_ref() {
        #[allow(clippy::print_stderr)]
        if let Err(err) = login_gate(!app_server_target.uses_remote_workspace()).await {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }

    let (tui_file_layer, _tui_file_log_guard) = if config.log_dir_configured {
        let log_dir = config.log_dir.clone();
        std::fs::create_dir_all(&log_dir)?;
        let mut log_file_opts = OpenOptions::new();
        log_file_opts.create(true).append(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            log_file_opts.mode(0o600);
        }

        let log_file = log_file_opts.open(log_dir.join(TUI_LOG_FILE_NAME))?;
        let (non_blocking, guard) = non_blocking(log_file);
        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("octopus_tui=info"));
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_target(true)
            .with_ansi(false)
            .with_span_events(
                tracing_subscriber::fmt::format::FmtSpan::NEW
                    | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
            )
            .with_filter(env_filter);
        (Some(file_layer), Some(guard))
    } else {
        (None, None)
    };

    if cli.oss && model_provider_override.is_some() {
        let provider_id = match model_provider_override.as_ref() {
            Some(id) => id,
            None => {
                error!("OSS provider unexpectedly not set when oss flag is used");
                return Err(std::io::Error::other(
                    "OSS provider not set but oss flag was used",
                ));
            }
        };
        if let Some(readiness) = launcher_hooks.oss_provider_readiness.as_ref() {
            readiness(provider_id.clone()).await?;
        }
    }

    let mut layers: Vec<
        Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>,
    > = Vec::new();
    if let Some(layer) = tui_file_layer {
        layers.push(Box::new(layer));
    }
    if let Some(layer) = telemetry_layers.logger_layer {
        layers.push(layer);
    }
    if let Some(layer) = telemetry_layers.tracing_layer {
        layers.push(layer);
    }
    let support_capture_layer = tracing_subscriber::fmt::layer()
        .with_writer(octopus_world_terminal_support::SupportLogCapture::global())
        .with_target(true)
        .with_ansi(false)
        .with_filter(EnvFilter::new(
            "warn,octopus_tui=info,octopus_world_terminal_client=info",
        ));
    layers.push(Box::new(support_capture_layer));
    let _ = tracing_subscriber::registry().with(layers).try_init();

    run_ratatui_app(
        cli,
        app_server_target,
        octopus_home,
        remote_cwd_override,
        config,
        manually_selected_oss_provider,
        config_runtime,
        load_request,
        environment_manager,
        launcher_hooks,
    )
    .await
    .map_err(|err| std::io::Error::other(format!("{err:?}")))
}

#[allow(clippy::too_many_arguments)]
async fn run_ratatui_app(
    cli: Cli,
    app_server_target: AppServerTarget,
    octopus_home: PathBuf,
    remote_cwd_override: Option<PathBuf>,
    initial_config: TuiConfig,
    manually_selected_oss_provider: Option<String>,
    config_runtime: Arc<dyn ProviderConfigRuntime>,
    load_request: SessionLoadRequest,
    environment_manager: Arc<EnvironmentManager>,
    launcher_hooks: LauncherProviderHooks,
) -> color_eyre::Result<AppExitInfo> {
    let uses_remote_workspace = app_server_target.uses_remote_workspace();
    color_eyre::install()?;

    tooltips::announcement::prewarm();

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        prev_hook(info);
    }));
    let mut initialized_terminal = tui::init()?;
    initialized_terminal.terminal.clear()?;

    let mut tui = Tui::new(
        initialized_terminal.terminal,
        initialized_terminal.enhanced_keys_supported,
        initialized_terminal.stderr_guard,
    );
    let mut terminal_restore_guard = TerminalRestoreGuard::new();

    session_log::maybe_init(&initial_config);

    let app_server_session = AppServerSession::new(app_server_target.thread_params_mode())
        .with_remote_cwd_override(remote_cwd_override.clone());
    let spine_attach_notice = None;
    if let Some(provider) = manually_selected_oss_provider.as_deref()
        && let Err(err) = config_update::write_config_batch_local(
            config_runtime.as_ref(),
            vec![config_update::build_oss_provider_edit(provider)],
        )
        .await
    {
        warn!(
            %err,
            provider,
            "Failed to persist selected OSS provider preference"
        );
    }
    let mut app_server = Some(app_server_session);

    let should_show_trust_screen_flag =
        !uses_remote_workspace && should_show_trust_screen(&initial_config);
    #[cfg(target_os = "windows")]
    let mut trust_decision_was_made = false;
    let login_status = if initial_config.model_provider.requires_openai_auth {
        let Some(app_server) = app_server.as_mut() else {
            unreachable!("app server should exist when auth is required");
        };
        get_login_status(app_server, &initial_config).await?
    } else {
        LoginStatus::NotAuthenticated
    };
    let should_show_onboarding = should_show_onboarding(
        login_status,
        &initial_config,
        should_show_trust_screen_flag,
        uses_remote_workspace,
    );

    let config = if should_show_onboarding {
        let show_login_screen =
            should_show_login_screen(login_status, &initial_config, uses_remote_workspace);
        let onboarding_result = run_onboarding_app(
            OnboardingScreenArgs {
                show_login_screen,
                show_trust_screen: should_show_trust_screen_flag,
                login_status,
                config: initial_config.clone(),
                api_key_prefill: launcher_hooks.onboarding_api_key_prefill.clone(),
                config_runtime: config_runtime.clone(),
            },
            &mut tui,
        )
        .await?;
        if onboarding_result.should_exit {
            shutdown_app_server_if_present(app_server.take()).await;
            terminal_restore_guard.restore_silently();
            session_log::log_session_end();
            let _ = tui.terminal.clear();
            return Ok(AppExitInfo {
                token_usage: crate::token_usage::TokenUsage::default(),
                thread_id: None,
                resume_hint: None,
                exit_reason: ExitReason::UserRequested,
            });
        }
        #[cfg(target_os = "windows")]
        {
            trust_decision_was_made = onboarding_result.directory_trust_persisted;
        }

        if onboarding_result.directory_trust_persisted
            || (show_login_screen && !uses_remote_workspace)
        {
            load_config_or_exit(config_runtime.as_ref(), load_request.clone()).await
        } else {
            initial_config
        }
    } else {
        initial_config
    };

    let mut missing_session_exit = |id_str: &str, action: &str| {
        error!("Error finding conversation path: {id_str}");
        terminal_restore_guard.restore_silently();
        session_log::log_session_end();
        let _ = tui.terminal.clear();
        Ok(AppExitInfo {
            token_usage: crate::token_usage::TokenUsage::default(),
            thread_id: None,
            resume_hint: None,
            exit_reason: ExitReason::Fatal(format!(
                "No saved session found with ID {id_str}. Run `codex {action}` without an ID to choose from existing sessions."
            )),
        })
    };

    let use_fork = cli.fork_picker || cli.fork_last || cli.fork_run_id.is_some();
    let session_selection = if use_fork {
        if let Some(id_str) = cli.fork_run_id.as_deref() {
            match lookup_session_target_by_run_id(&config, id_str)? {
                Some(target_session) => resume_picker::SessionSelection::Fork(target_session),
                None => {
                    shutdown_app_server_if_present(app_server.take()).await;
                    return missing_session_exit(id_str, "fork");
                }
            }
        } else if cli.fork_last {
            match lookup_latest_session_target(&config)? {
                Some(target_session) => resume_picker::SessionSelection::Fork(target_session),
                None => resume_picker::SessionSelection::StartFresh,
            }
        } else if cli.fork_picker {
            match resume_picker::run_fork_picker(&mut tui, &config).await? {
                resume_picker::SessionSelection::Exit => {
                    terminal_restore_guard.restore_silently();
                    session_log::log_session_end();
                    return Ok(AppExitInfo {
                        token_usage: crate::token_usage::TokenUsage::default(),
                        thread_id: None,
                        resume_hint: None,
                        exit_reason: ExitReason::UserRequested,
                    });
                }
                other => other,
            }
        } else {
            resume_picker::SessionSelection::StartFresh
        }
    } else if let Some(id_str) = cli.resume_run_id.as_deref() {
        match lookup_session_target_by_run_id(&config, id_str)? {
            Some(target_session) => resume_picker::SessionSelection::Resume(target_session),
            None => {
                shutdown_app_server_if_present(app_server.take()).await;
                return missing_session_exit(id_str, "resume");
            }
        }
    } else if cli.resume_last {
        match lookup_latest_session_target(&config)? {
            Some(target_session) => resume_picker::SessionSelection::Resume(target_session),
            None => resume_picker::SessionSelection::StartFresh,
        }
    } else if cli.resume_picker {
        match resume_picker::run_resume_picker(&mut tui, &config).await? {
            resume_picker::SessionSelection::Exit => {
                terminal_restore_guard.restore_silently();
                session_log::log_session_end();
                return Ok(AppExitInfo {
                    token_usage: crate::token_usage::TokenUsage::default(),
                    thread_id: None,
                    resume_hint: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
            other => other,
        }
    } else {
        resume_picker::SessionSelection::StartFresh
    };

    let current_cwd = config.cwd.clone();
    let fallback_cwd = match &session_selection {
        resume_picker::SessionSelection::Resume(target_session)
        | resume_picker::SessionSelection::Fork(target_session) => {
            if uses_remote_workspace {
                Some(current_cwd.to_path_buf())
            } else {
                target_session
                    .cwd
                    .clone()
                    .filter(|cwd| cwd.exists())
                    .or_else(|| Some(current_cwd.to_path_buf()))
            }
        }
        _ => None,
    };

    let picker_cancelled_without_selection = matches!(
        session_selection,
        resume_picker::SessionSelection::StartFresh
    ) && (cli.resume_picker || cli.fork_picker);

    let mut config = match &session_selection {
        resume_picker::SessionSelection::Resume(_) | resume_picker::SessionSelection::Fork(_) => {
            let mut request = load_request.clone();
            request.fallback_cwd = fallback_cwd;
            load_config_or_exit(config_runtime.as_ref(), request).await
        }
        resume_picker::SessionSelection::StartFresh if picker_cancelled_without_selection => {
            load_config_or_exit(config_runtime.as_ref(), load_request.clone()).await
        }
        _ => config,
    };

    if let Some(w) = crate::render::highlight::set_theme_override(
        config.terminal.theme.clone(),
        find_codex_home().ok().map(AbsolutePathBuf::into_path_buf),
    ) {
        config.startup_warnings.push(w);
    }

    if let Some(login_gate) = launcher_hooks.login_startup_gate.as_ref() {
        login_gate(false)
            .await
            .map_err(|err| color_eyre::eyre::eyre!("provider login gate failed: {err}"))?;
    }
    let should_show_trust_screen = should_show_trust_screen(&config);
    #[cfg(target_os = "windows")]
    let windows_sandbox_level = crate::windows_sandbox::level_from_config(&config);
    #[cfg(target_os = "windows")]
    let required_elevated_sandbox_needs_setup = windows_sandbox_level
        == WindowsSandboxLevel::Elevated
        && config.windows_sandbox_requirements.required
        && !crate::windows_sandbox::sandbox_setup_is_complete(config.codex_home.as_path());
    #[cfg(target_os = "windows")]
    let should_prompt_windows_sandbox_nux_at_startup = (trust_decision_was_made
        && windows_sandbox_level == WindowsSandboxLevel::Disabled)
        || required_elevated_sandbox_needs_setup;
    #[cfg(not(target_os = "windows"))]
    let should_prompt_windows_sandbox_nux_at_startup = false;

    let Cli {
        prompt,
        shared,
        no_alt_screen,
        ..
    } = cli;
    let images = shared.into_inner().images;

    let use_alt_screen = determine_alt_screen_mode(no_alt_screen, config.terminal.alternate_screen);
    tui.set_alt_screen_enabled(use_alt_screen);
    let topology = match resolve_private_run_topology(
        &session_selection,
        &octopus_home,
        config.model.as_deref(),
        config
            .model_reasoning_effort
            .as_ref()
            .map(|effort| effort.as_str()),
    )
    .await
    {
        Ok(topology) => topology,
        Err(error) => {
            terminal_restore_guard.restore_silently();
            session_log::log_session_end();
            return Err(error.wrap_err("terminal private run topology could not be resolved"));
        }
    };
    let roots = octopus_world_terminal_client::TerminalHostRoots::load_from_home(&octopus_home)
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    let durable_root = app_server_target
        .daemon_endpoint()
        .map(|endpoint| endpoint.durable_ledger_root.clone())
        .unwrap_or_else(|| roots.durable_ledger_root.clone());
    let session = app_server.take().ok_or_else(|| {
        color_eyre::eyre::eyre!("terminal session disappeared before private topology attachment")
    })?;
    let mut app_server =
        match attach_private_spine(session, octopus_home.clone(), roots, durable_root, topology) {
            Ok(session) => session,
            Err(error) => {
                terminal_restore_guard.restore_silently();
                session_log::log_session_end();
                return Err(error.wrap_err("terminal private spine could not be attached"));
            }
        };
    let is_persistent_resume = !matches!(&app_server_target, AppServerTarget::Embedded)
        && matches!(
            &session_selection,
            resume_picker::SessionSelection::Resume(_)
        );
    let bypass_hook_trust_for_startup_review = config.bypass_hook_trust && !is_persistent_resume;
    let hooks_cwd = config.cwd.to_path_buf();
    let startup_prefetch_started_at = Instant::now();
    let (startup_bootstrap, startup_hooks_entry) = tokio::join!(
        app_server.bootstrap(&config),
        load_startup_hooks_review_entry(hooks_cwd),
    );
    let startup_bootstrap = Some(startup_bootstrap?);
    let startup_elapsed_before_app = startup_prefetch_started_at.elapsed();
    let startup_hooks_browser = match maybe_run_startup_hooks_review(
        &mut tui,
        &config,
        bypass_hook_trust_for_startup_review,
        startup_hooks_entry,
    )
    .await?
    {
        StartupHooksReviewOutcome::Continue => None,
        StartupHooksReviewOutcome::OpenHooksBrowser(data) => Some(data),
    };

    let app_result = App::run(
        &mut tui,
        app_server,
        config,
        config_runtime,
        load_request,
        prompt,
        images,
        session_selection,
        should_show_trust_screen,
        should_prompt_windows_sandbox_nux_at_startup,
        app_server_target,
        environment_manager,
        startup_elapsed_before_app,
        startup_bootstrap,
        startup_hooks_browser,
        spine_attach_notice,
        launcher_hooks.telemetry_launcher(),
    )
    .await;

    terminal_restore_guard.restore_silently();
    session_log::log_session_end();
    let AppRunExit {
        exit_info,
        detach_receipt,
    } = app_result?;
    if let Some(receipt) = detach_receipt {
        print_detach_receipt(&receipt)?;
    }
    Ok(exit_info)
}

fn detach_receipt_lines(receipt: &DetachReceipt) -> [String; 2] {
    [
        format!(
            "Detached from run {}. Existing daemon work was not interrupted.",
            receipt.run_id
        ),
        format!("Reconnect with {}", receipt.reconnect_command),
    ]
}

fn print_detach_receipt(receipt: &DetachReceipt) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    for line in detach_receipt_lines(receipt) {
        writeln!(stdout, "{line}")?;
    }
    stdout.flush()
}

#[expect(
    clippy::print_stderr,
    reason = "TUI should no longer be displayed, so we can write to stderr."
)]
fn restore() {
    if let Err(err) = tui::restore_after_exit() {
        eprintln!(
            "failed to restore terminal. Run `reset` or restart your terminal to recover: {err}"
        );
    }
}

struct TerminalRestoreGuard {
    active: bool,
}

impl TerminalRestoreGuard {
    fn new() -> Self {
        Self { active: true }
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    fn restore(&mut self) -> color_eyre::Result<()> {
        if self.active {
            crate::tui::restore_after_exit()?;
            self.active = false;
        }
        Ok(())
    }

    fn restore_silently(&mut self) {
        if self.active {
            restore();
            self.active = false;
        }
    }
}

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        self.restore_silently();
    }
}

fn determine_alt_screen_mode(no_alt_screen: bool, tui_alternate_screen: AltScreenMode) -> bool {
    if no_alt_screen {
        return false;
    }

    tui_alternate_screen != AltScreenMode::Never
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStatus {
    Authenticated,
    NotAuthenticated,
}

async fn get_login_status(
    app_server: &mut AppServerSession,
    config: &TuiConfig,
) -> color_eyre::Result<LoginStatus> {
    if !config.model_provider.requires_openai_auth {
        return Ok(LoginStatus::NotAuthenticated);
    }

    let account = app_server.read_account().await?;
    Ok(match account.identity {
        Some(_) => LoginStatus::Authenticated,
        None => LoginStatus::NotAuthenticated,
    })
}

async fn load_config_or_exit(
    config_runtime: &dyn ProviderConfigRuntime,
    request: SessionLoadRequest,
) -> TuiConfig {
    #[allow(clippy::print_stderr)]
    let snapshot = match config_runtime.load(request).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            eprintln!("Error loading configuration: {err}");
            std::process::exit(1);
        }
    };
    #[allow(clippy::print_stderr)]
    match TuiConfig::from_snapshot(snapshot) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("Error loading configuration: {err}");
            std::process::exit(1);
        }
    }
}

fn should_show_trust_screen(config: &TuiConfig) -> bool {
    config.project_trust_level.is_none()
}

fn should_show_onboarding(
    login_status: LoginStatus,
    config: &TuiConfig,
    show_trust_screen: bool,
    uses_remote_workspace: bool,
) -> bool {
    if show_trust_screen {
        return true;
    }

    should_show_login_screen(login_status, config, uses_remote_workspace)
}

fn should_show_login_screen(
    login_status: LoginStatus,
    config: &TuiConfig,
    uses_remote_workspace: bool,
) -> bool {
    if !uses_remote_workspace {
        return false;
    }

    if !config.model_provider.requires_openai_auth {
        return false;
    }

    login_status == LoginStatus::NotAuthenticated
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    async fn build_config(temp_dir: &TempDir) -> std::io::Result<TuiConfig> {
        let home = AbsolutePathBuf::from_absolute_path(temp_dir.path())?;
        let snapshot = crate::provider_config_runtime::ProviderSessionSnapshot::degraded(
            home,
            AbsolutePathBuf::current_dir()?,
        );
        TuiConfig::from_snapshot(snapshot)
    }

    #[test]
    fn detached_exit_prints_honest_reconnect_receipt() {
        let local_target = AppServerTarget::LocalDaemon {
            endpoint: DaemonEndpoint::local_default(Path::new("/octopus/home")),
        };
        let local_command =
            app::detach_reconnect_command(&local_target, "run_123").expect("local reconnect");
        let receipt = DetachReceipt {
            run_id: "run_123".to_string(),
            reconnect_command: local_command,
        };
        assert_eq!(
            detach_receipt_lines(&receipt),
            [
                "Detached from run run_123. Existing daemon work was not interrupted.".to_string(),
                "Reconnect with octopus resume run_123".to_string(),
            ]
        );

        let remote_target = AppServerTarget::Remote {
            endpoint: DaemonEndpoint::with_control_socket(
                Path::new("/octopus/home"),
                PathBuf::from("/tmp/octopus daemon.sock"),
            ),
        };
        assert_eq!(
            app::detach_reconnect_command(&remote_target, "run_123").expect("remote reconnect"),
            "octopus resume run_123 --remote 'unix:///tmp/octopus daemon.sock'"
        );
    }

    #[test]
    fn startup_removes_legacy_tui_log_file() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let legacy_log_dir = temp_dir.path().join("log");
        std::fs::create_dir_all(&legacy_log_dir)?;
        let legacy_log = legacy_log_dir.join(TUI_LOG_FILE_NAME);
        std::fs::write(&legacy_log, "legacy log")?;

        remove_legacy_tui_log_file(temp_dir.path());

        assert!(!legacy_log.exists());
        Ok(())
    }

    #[test]
    fn alternate_screen_auto_uses_alt_screen() {
        assert!(determine_alt_screen_mode(false, AltScreenMode::Auto,));
        assert!(determine_alt_screen_mode(false, AltScreenMode::Always,));
        assert!(!determine_alt_screen_mode(false, AltScreenMode::Never,));
        assert!(!determine_alt_screen_mode(true, AltScreenMode::Auto,));
    }

    #[tokio::test]
    async fn embedded_octopus_shell_does_not_boot_into_provider_login() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.model_provider.requires_openai_auth = true;

        assert!(!should_show_login_screen(
            LoginStatus::NotAuthenticated,
            &config,
            false,
        ));
        assert!(!should_show_onboarding(
            LoginStatus::NotAuthenticated,
            &config,
            false,
            false,
        ));

        Ok(())
    }

    #[tokio::test]
    async fn remote_workspace_keeps_remote_provider_login_gate() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.model_provider.requires_openai_auth = true;

        assert!(should_show_login_screen(
            LoginStatus::NotAuthenticated,
            &config,
            true,
        ));
        assert!(should_show_onboarding(
            LoginStatus::NotAuthenticated,
            &config,
            false,
            true,
        ));

        Ok(())
    }

    #[test]
    fn session_target_display_label_is_the_run_id() {
        let target = crate::resume_picker::SessionTarget {
            run: octopus_descriptors::RunId::new("run_alpha"),
            record_path: std::path::PathBuf::from("/x/run_alpha.json"),
            cwd: None,
        };

        assert_eq!(target.display_label(), "run run_alpha");
    }

    #[test]
    fn resolve_remote_addr_rejects_retired_websocket_transport() {
        for addr in [
            "ws://127.0.0.1:4500",
            "wss://example.com:443",
            "ws://127.0.0.1",
            "wss://example.com",
            "127.0.0.1:4500",
            "https://127.0.0.1:4500",
        ] {
            let err = resolve_remote_addr(addr).expect_err("retired transport addresses must fail");
            assert!(
                err.to_string()
                    .contains("expected `unix://` or `unix://PATH`"),
                "{err}"
            );
        }
    }

    #[test]
    fn resolve_remote_addr_accepts_default_socket() -> color_eyre::Result<()> {
        let codex_home = find_codex_home().wrap_err("failed to resolve CODEX_HOME")?;
        assert_eq!(
            resolve_remote_addr("unix://")?,
            DaemonEndpoint::local_default(&codex_home),
        );
        Ok(())
    }

    #[test]
    fn resolve_remote_addr_accepts_relative_socket_path() -> color_eyre::Result<()> {
        let codex_home = find_codex_home().wrap_err("failed to resolve CODEX_HOME")?;
        let endpoint = resolve_remote_addr("unix://codex.sock")?;
        assert_eq!(
            endpoint.control_socket,
            AbsolutePathBuf::relative_to_current_dir("codex.sock")?.into_path_buf(),
        );
        assert_eq!(
            endpoint.durable_ledger_root,
            codex_home.join("ledger").to_path_buf(),
        );
        Ok(())
    }

    #[test]
    fn resolve_remote_addr_accepts_absolute_socket_path() -> color_eyre::Result<()> {
        let temp_dir = TempDir::new()?;
        let socket_path = temp_dir.path().join("codex.sock");
        assert_eq!(
            resolve_remote_addr(&format!("unix://{}", socket_path.display()))?.control_socket,
            AbsolutePathBuf::from_absolute_path(&socket_path)?.into_path_buf(),
        );
        Ok(())
    }

    #[tokio::test]
    async fn default_daemon_auto_connect_skips_missing_socket() -> color_eyre::Result<()> {
        let codex_home = TempDir::new()?;
        assert!(
            maybe_probe_default_daemon_socket(codex_home.path())
                .await
                .is_none()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn default_daemon_auto_connect_probes_socket_only() -> color_eyre::Result<()> {
        let codex_home = TempDir::new()?;
        let endpoint = DaemonEndpoint::local_default(codex_home.path());
        std::fs::create_dir_all(endpoint.control_socket.parent().expect("socket parent"))?;
        let _listener = tokio::net::UnixListener::bind(&endpoint.control_socket)?;

        assert_eq!(
            maybe_probe_default_daemon_socket(codex_home.path()).await,
            Some(endpoint)
        );
        Ok(())
    }

    #[test]
    fn app_server_target_for_launch_uses_local_daemon_for_default_socket() -> color_eyre::Result<()>
    {
        let endpoint = DaemonEndpoint::local_default(Path::new("/octo/home"));
        let target = app_server_target_for_launch(None, Some(endpoint.clone()), true);

        assert_eq!(target, AppServerTarget::LocalDaemon { endpoint });
        assert!(!target.uses_remote_workspace());
        assert_eq!(target.thread_params_mode(), ThreadParamsMode::Embedded);
        Ok(())
    }

    #[test]
    fn app_server_target_for_launch_prefers_explicit_remote_endpoint() -> color_eyre::Result<()> {
        let explicit_endpoint = DaemonEndpoint::with_control_socket(
            Path::new("/octo/home"),
            PathBuf::from("/octo/explicit.sock"),
        );
        let target = app_server_target_for_launch(
            Some(explicit_endpoint.clone()),
            Some(DaemonEndpoint::local_default(Path::new("/octo/home"))),
            false,
        );

        assert_eq!(
            target,
            AppServerTarget::Remote {
                endpoint: explicit_endpoint,
            }
        );
        assert!(target.uses_remote_workspace());
        assert_eq!(target.thread_params_mode(), ThreadParamsMode::Remote);
        Ok(())
    }

    #[test]
    fn app_server_target_for_launch_skips_local_daemon_when_launch_config_is_not_replayable()
    -> color_eyre::Result<()> {
        let endpoint = DaemonEndpoint::local_default(Path::new("/octo/home"));
        let target = app_server_target_for_launch(None, Some(endpoint), false);

        assert_eq!(target, AppServerTarget::Embedded);
        Ok(())
    }

    #[test]
    fn can_reuse_implicit_local_daemon_requires_default_launch_config() -> color_eyre::Result<()> {
        let cli_kv_overrides = vec![("model".to_string(), toml::Value::String("gpt-5.2".into()))];

        assert!(can_reuse_implicit_local_daemon(
            &[],
            false,
            false,
            false,
            false,
        ));
        assert!(!can_reuse_implicit_local_daemon(
            &cli_kv_overrides,
            false,
            false,
            false,
            false,
        ));
        assert!(!can_reuse_implicit_local_daemon(
            &[],
            true,
            false,
            false,
            false,
        ));
        assert!(!can_reuse_implicit_local_daemon(
            &[],
            false,
            true,
            false,
            false,
        ));
        assert!(!can_reuse_implicit_local_daemon(
            &[],
            false,
            false,
            true,
            false,
        ));
        assert!(!can_reuse_implicit_local_daemon(
            &[],
            false,
            false,
            false,
            true,
        ));
        Ok(())
    }

    #[test]
    fn should_load_configured_environments_for_local_daemon() -> color_eyre::Result<()> {
        let target = AppServerTarget::LocalDaemon {
            endpoint: DaemonEndpoint::local_default(Path::new("/octo/home")),
        };

        assert!(should_load_configured_environments(false, &target));
        Ok(())
    }

    #[tokio::test]
    async fn latest_session_cwd_filter_respects_scope_options() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let config = build_config(&temp_dir).await?;
        let remote_cwd = Path::new("repo/on/server");

        let local_filter = latest_session_cwd_filter(false, None, &config, false);
        let show_all_filter = latest_session_cwd_filter(false, None, &config, true);
        let remote_filter = latest_session_cwd_filter(true, Some(remote_cwd), &config, false);

        assert_eq!(local_filter, Some(config.cwd.as_path()));
        assert_eq!(show_all_filter, None);
        assert_eq!(remote_filter, Some(remote_cwd));
        Ok(())
    }

    #[tokio::test]
    async fn config_cwd_for_app_server_target_omits_cwd_for_remote_sessions() -> std::io::Result<()>
    {
        let remote_only_cwd = if cfg!(windows) {
            Path::new(r"C:\definitely\not\local\to\this\test")
        } else {
            Path::new("/definitely/not/local/to/this/test")
        };
        let target = AppServerTarget::Remote {
            endpoint: DaemonEndpoint::local_default(Path::new("/octo/home")),
        };
        let environment_manager = EnvironmentManager::default_for_tests();

        let config_cwd =
            config_cwd_for_app_server_target(Some(remote_only_cwd), &target, &environment_manager)?;

        assert_eq!(config_cwd, None);
        Ok(())
    }

    #[tokio::test]
    async fn config_cwd_for_app_server_target_canonicalizes_embedded_cli_cwd() -> std::io::Result<()>
    {
        let temp_dir = TempDir::new()?;
        let target = AppServerTarget::Embedded;
        let environment_manager = EnvironmentManager::default_for_tests();

        let config_cwd =
            config_cwd_for_app_server_target(Some(temp_dir.path()), &target, &environment_manager)?;

        assert_eq!(
            config_cwd,
            Some(AbsolutePathBuf::from_absolute_path(dunce::canonicalize(
                temp_dir.path()
            )?)?)
        );
        Ok(())
    }

    #[tokio::test]
    async fn config_cwd_for_app_server_target_canonicalizes_local_daemon_cli_cwd()
    -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let target = AppServerTarget::LocalDaemon {
            endpoint: DaemonEndpoint::local_default(Path::new("/octo/home")),
        };
        let environment_manager = EnvironmentManager::default_for_tests();

        let config_cwd =
            config_cwd_for_app_server_target(Some(temp_dir.path()), &target, &environment_manager)?;

        assert_eq!(
            config_cwd,
            Some(AbsolutePathBuf::from_absolute_path(dunce::canonicalize(
                temp_dir.path()
            )?)?)
        );
        Ok(())
    }

    #[tokio::test]
    async fn config_cwd_for_app_server_target_errors_for_missing_embedded_cli_cwd()
    -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let missing = temp_dir.path().join("missing");
        let target = AppServerTarget::Embedded;
        let environment_manager = EnvironmentManager::default_for_tests();

        let err = config_cwd_for_app_server_target(Some(&missing), &target, &environment_manager)
            .expect_err("missing embedded cwd should fail");

        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        Ok(())
    }

    #[tokio::test]
    async fn config_cwd_for_app_server_target_omits_cwd_for_remote_exec_server()
    -> std::io::Result<()> {
        let remote_only_cwd = if cfg!(windows) {
            Path::new(r"C:\definitely\not\local\to\this\test")
        } else {
            Path::new("/definitely/not/local/to/this/test")
        };
        let target = AppServerTarget::Embedded;
        let environment_manager = EnvironmentManager::create_for_tests(
            Some("ws://127.0.0.1:8765".to_string()),
            Some(ExecServerRuntimePaths::new(
                std::env::current_exe().expect("current exe"),
                None,
            )?),
        )
        .await;

        let config_cwd =
            config_cwd_for_app_server_target(Some(remote_only_cwd), &target, &environment_manager)?;

        assert_eq!(config_cwd, None);
        Ok(())
    }

    #[tokio::test]
    async fn windows_shows_trust_prompt_without_sandbox() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.project_trust_level = None;
        config.set_windows_sandbox_enabled(false);

        let should_show = should_show_trust_screen(&config);
        assert!(
            should_show,
            "Trust prompt should be shown when project trust is undecided"
        );
        Ok(())
    }

    #[tokio::test]
    async fn windows_shows_trust_prompt_with_sandbox() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.project_trust_level = None;
        config.set_windows_sandbox_enabled(true);

        let should_show = should_show_trust_screen(&config);
        if cfg!(target_os = "windows") {
            assert!(
                should_show,
                "Windows trust prompt should be shown on native Windows with sandbox enabled"
            );
        } else {
            assert!(
                should_show,
                "Non-Windows should still show trust prompt when project is untrusted"
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn untrusted_project_skips_trust_prompt() -> std::io::Result<()> {
        use crate::session_vocab::config_types::TrustLevel;
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.project_trust_level = Some(TrustLevel::Untrusted);

        let should_show = should_show_trust_screen(&config);
        assert!(
            !should_show,
            "Trust prompt should not be shown for projects explicitly marked as untrusted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn theme_warning_uses_final_config() -> std::io::Result<()> {
        use crate::render::highlight::validate_theme_name;

        let temp_dir = TempDir::new()?;

        let initial_config = build_config(&temp_dir).await?;
        assert!(initial_config.terminal.theme.is_none());

        let mut config = build_config(&temp_dir).await?;
        config.terminal.theme = Some("bogus-theme".into());

        if let Some(w) =
            validate_theme_name(config.terminal.theme.as_deref(), Some(temp_dir.path()))
        {
            config.startup_warnings.push(w);
        }

        assert_eq!(
            config.startup_warnings.len(),
            1,
            "warning from final config's invalid theme should be present"
        );
        assert!(
            config.startup_warnings[0].contains("bogus-theme"),
            "warning should reference the final config's theme name"
        );
        Ok(())
    }
}
