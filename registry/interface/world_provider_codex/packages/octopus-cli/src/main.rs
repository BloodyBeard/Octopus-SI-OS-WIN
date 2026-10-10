use clap::Args;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::Shell;
use clap_complete::generate;
use octopus_arg0::Arg0DispatchPaths;
use octopus_arg0::arg0_dispatch_or_else;
use octopus_chatgpt::apply_command::ApplyCommand;
use octopus_chatgpt::apply_command::run_apply_command;
use octopus_codex_cli::clear_memory_roots_contents;
use octopus_codex_cli::cli_args::ApprovalModeCliArg as CodexApprovalModeCliArg;
use octopus_codex_cli::cli_args::CliConfigOverrides;
use octopus_codex_cli::cli_args::ProfileV2Name;
use octopus_codex_cli::cli_args::SandboxModeCliArg as CodexSandboxModeCliArg;
use octopus_codex_cli::cli_args::SharedCliOptions as CodexSharedCliOptions;
use octopus_codex_cli::doctor;
use octopus_codex_cli::doctor::DoctorCommand;
use octopus_codex_cli::exec_compat::Cli as ExecCli;
use octopus_codex_cli::exec_compat::Command as ExecCommand;
use octopus_codex_cli::exec_compat::ReviewArgs;
use octopus_codex_cli::exec_server_telemetry;
use octopus_codex_cli::features_cmd;
use octopus_codex_cli::read_access_token_from_stdin;
use octopus_codex_cli::read_api_key_from_stdin;
use octopus_codex_cli::run_login_status;
use octopus_codex_cli::run_login_with_access_token;
use octopus_codex_cli::run_login_with_api_key;
use octopus_codex_cli::run_login_with_chatgpt;
use octopus_codex_cli::run_login_with_device_code;
use octopus_codex_cli::run_logout;
use octopus_execpolicy::ExecPolicyCheckCommand;
use octopus_responses_api_proxy::Args as ResponsesApiProxyArgs;
use octopus_terminal_detection::TerminalName;
use octopus_tui::AppExitInfo;
use octopus_tui::Cli as TuiCli;
use octopus_tui::ExitReason;
use octopus_util_abspath::AbsolutePathBuf;
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use supports_color::Stream;

mod stdio_to_uds;

use octopus_config::LoaderOverrides;
use octopus_core::config::ConfigBuilder;
use octopus_core::config::ConfigOverrides;
use octopus_core::config::find_codex_home;
use octopus_core::config::resolve_profile_v2_config_path;
use octopus_cxhome::CodexHomeUserInstructionsProvider;
use octopus_login::AuthManager;
use octopus_login::CodexAuth;
use octopus_login::read_codex_access_token_from_env;
use octopus_protocol::protocol::AskForApproval;
use octopus_protocol::user_input::UserInput;

#[derive(Debug, Parser)]
#[clap(
    author,
    version,
    subcommand_negates_reqs = true,
    bin_name = "octopus",
    override_usage = "octopus [OPTIONS] [PROMPT]\n       octopus [OPTIONS] <COMMAND> [ARGS]"
)]
struct MultitoolCli {
    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    pub feature_toggles: FeatureToggles,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    interactive: TuiCli,

    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    #[clap(visible_alias = "e")]
    Exec(ExecCli),

    Review(ReviewCommand),

    Login(LoginCommand),

    Logout(LogoutCommand),

    AppServer(AppServerCommand),

    Completion(CompletionCommand),

    Doctor(DoctorCommand),

    Sandbox(HostSandboxArgs),

    Debug(DebugCommand),

    #[clap(hide = true)]
    Execpolicy(ExecpolicyCommand),

    #[clap(visible_alias = "a")]
    Apply(ApplyCommand),

    Resume(ResumeCommand),

    Fork(ForkCommand),

    #[clap(hide = true)]
    ResponsesApiProxy(ResponsesApiProxyArgs),

    #[clap(hide = true, name = "stdio-to-uds")]
    StdioToUds(StdioToUdsCommand),

    ExecServer(ExecServerCommand),

    Features(FeaturesCli),
}

#[derive(Debug, Parser)]
struct CompletionCommand {
    #[clap(value_enum, default_value_t = Shell::Bash)]
    shell: Shell,
}

#[derive(Debug, Parser)]
struct DebugCommand {
    #[command(subcommand)]
    subcommand: DebugSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum DebugSubcommand {
    PromptInput(DebugPromptInputCommand),

    #[clap(hide = true)]
    ClearMemories,
}

#[derive(Debug, Parser)]
struct DebugPromptInputCommand {
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    #[arg(long = "image", short = 'i', value_name = "FILE", value_delimiter = ',', num_args = 1..)]
    images: Vec<PathBuf>,
}

#[derive(Debug, Parser)]
struct ReviewCommand {
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    #[clap(flatten)]
    args: ReviewArgs,
}

#[derive(Debug, Parser)]
struct ResumeCommand {
    #[arg(value_name = "RUN_ID")]
    run_id: Option<String>,

    #[arg(long = "last", default_value_t = false)]
    last: bool,

    #[arg(long = "all", default_value_t = false)]
    all: bool,

    #[arg(long = "include-non-interactive", default_value_t = false)]
    include_non_interactive: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: RunTuiCli,
}

#[derive(Debug, Parser)]
struct ForkCommand {
    #[arg(value_name = "RUN_ID")]
    run_id: Option<String>,

    #[arg(long = "last", default_value_t = false)]
    last: bool,

    #[arg(long = "all", default_value_t = false)]
    all: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: RunTuiCli,
}

#[derive(Debug)]
struct RunTuiCli(TuiCli);

impl Args for RunTuiCli {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        TuiCli::augment_args(cmd).mut_arg("prompt", |arg| arg.conflicts_with("last"))
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        TuiCli::augment_args_for_update(cmd).mut_arg("prompt", |arg| arg.conflicts_with("last"))
    }
}

impl clap::FromArgMatches for RunTuiCli {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        TuiCli::from_arg_matches(matches).map(Self)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.0.update_from_arg_matches(matches)
    }
}

#[cfg(target_os = "macos")]
type HostSandboxArgs = octopus_sandbox_launcher::SeatbeltCommand;
#[cfg(target_os = "linux")]
type HostSandboxArgs = octopus_sandbox_launcher::LandlockCommand;
#[cfg(target_os = "windows")]
type HostSandboxArgs = octopus_sandbox_launcher::WindowsCommand;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
type HostSandboxArgs = UnsupportedSandboxArgs;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
#[derive(Debug, Parser)]
struct UnsupportedSandboxArgs {
    #[arg(long = "profile", short = 'p')]
    pub config_profile: Option<ProfileV2Name>,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Parser)]
struct ExecpolicyCommand {
    #[command(subcommand)]
    sub: ExecpolicySubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ExecpolicySubcommand {
    #[clap(name = "check")]
    Check(ExecPolicyCheckCommand),
}

#[derive(Debug, Parser)]
struct LoginCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    #[arg(
        long = "with-api-key",
        help = "Read the API key from stdin (e.g. `printenv OPENAI_API_KEY | octopus login --with-api-key`)"
    )]
    with_api_key: bool,

    #[arg(
        long = "with-access-token",
        help = "Read the access token from stdin (e.g. `printenv CODEX_ACCESS_TOKEN | octopus login --with-access-token`)"
    )]
    with_access_token: bool,

    #[arg(
        long = "api-key",
        num_args = 0..=1,
        default_missing_value = "",
        value_name = "API_KEY",
        help = "(deprecated) Previously accepted the API key directly; now exits with guidance to use --with-api-key",
        hide = true
    )]
    api_key: Option<String>,

    #[arg(long = "device-auth")]
    use_device_code: bool,

    #[arg(long = "experimental_issuer", value_name = "URL", hide = true)]
    issuer_base_url: Option<String>,

    #[arg(long = "experimental_client-id", value_name = "CLIENT_ID", hide = true)]
    client_id: Option<String>,

    #[command(subcommand)]
    action: Option<LoginSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum LoginSubcommand {
    Status,
}

#[derive(Debug, Parser)]
struct LogoutCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,
}

#[derive(Debug, Parser)]
struct AppServerCommand {
    #[command(subcommand)]
    subcommand: Option<AppServerSubcommand>,

    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    #[arg(
        long = "listen",
        value_name = "URL",
        default_value = octopus_app_server_transport::AppServerTransport::DEFAULT_LISTEN_URL
    )]
    listen: octopus_app_server_transport::AppServerTransport,

    #[arg(long = "stdio", conflicts_with = "listen")]
    stdio: bool,

    #[arg(long = "remote-control", hide = true)]
    remote_control: bool,

    #[arg(long = "analytics-default-enabled")]
    analytics_default_enabled: bool,

    #[command(flatten)]
    auth: octopus_app_server_transport::auth::AppServerWebsocketAuthArgs,
}

#[derive(Debug, Parser)]
struct ExecServerCommand {
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    #[arg(long = "listen", value_name = "URL", conflicts_with = "remote")]
    listen: Option<String>,

    #[arg(
        long = "auth-secret-handle",
        value_name = "PATH",
        conflicts_with = "remote"
    )]
    auth_secret_handle: Option<PathBuf>,

    #[arg(long = "remote", value_name = "URL", requires = "environment_id")]
    remote: Option<String>,

    #[arg(long = "environment-id", value_name = "ID")]
    environment_id: Option<String>,

    #[arg(long = "name", value_name = "NAME")]
    name: Option<String>,

    #[arg(long = "use-agent-identity-auth", requires = "remote")]
    use_agent_identity_auth: bool,
}

#[derive(Debug, clap::Subcommand)]
#[allow(clippy::enum_variant_names)]
enum AppServerSubcommand {
    Proxy(AppServerProxyCommand),

    GenerateTs(GenerateTsCommand),

    GenerateJsonSchema(GenerateJsonSchemaCommand),

    #[clap(hide = true)]
    GenerateInternalJsonSchema(GenerateInternalJsonSchemaCommand),
}

#[derive(Debug, Args)]
struct AppServerProxyCommand {
    #[arg(long = "sock", value_name = "SOCKET_PATH", value_parser = parse_socket_path)]
    socket_path: Option<AbsolutePathBuf>,
}

#[derive(Debug, Args)]
struct GenerateTsCommand {
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    #[arg(short = 'p', long = "prettier", value_name = "PRETTIER_BIN")]
    prettier: Option<PathBuf>,

    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateJsonSchemaCommand {
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateInternalJsonSchemaCommand {
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,
}

#[derive(Debug, Parser)]
struct StdioToUdsCommand {
    #[arg(value_name = "SOCKET_PATH", value_parser = parse_socket_path)]
    socket_path: AbsolutePathBuf,
}

fn parse_socket_path(raw: &str) -> Result<AbsolutePathBuf, String> {
    AbsolutePathBuf::relative_to_current_dir(raw)
        .map_err(|err| format!("failed to resolve socket path `{raw}`: {err}"))
}

fn format_exit_messages(exit_info: AppExitInfo, color_enabled: bool) -> Vec<String> {
    let is_fatal = matches!(&exit_info.exit_reason, ExitReason::Fatal(_));
    let AppExitInfo {
        token_usage,
        thread_id: run_id,
        resume_hint,
        ..
    } = exit_info;

    let mut lines = Vec::new();
    if !token_usage.is_zero() {
        lines.push(token_usage.to_string());
    }

    if let Some(resume_cmd) = resume_hint {
        let command = if color_enabled {
            resume_cmd.cyan().to_string()
        } else {
            resume_cmd
        };
        lines.push(format!("To continue this chat, run {command}"));
    } else if is_fatal && let Some(run_id) = run_id {
        lines.push(format!("Run ID: {run_id}"));
    }

    lines
}

fn handle_app_exit(exit_info: AppExitInfo) -> anyhow::Result<()> {
    let is_fatal = match &exit_info.exit_reason {
        ExitReason::Fatal(message) => {
            eprintln!("ERROR: {message}");
            true
        }
        ExitReason::UserRequested => false,
    };

    let color_enabled = supports_color::on(Stream::Stdout).is_some();
    for line in format_exit_messages(exit_info, color_enabled) {
        println!("{line}");
    }
    if is_fatal {
        std::io::stdout().flush()?;
        std::process::exit(1);
    }
    Ok(())
}

fn run_execpolicycheck(cmd: ExecPolicyCheckCommand) -> anyhow::Result<()> {
    cmd.run()
}

#[derive(Debug, Default, Parser, Clone)]
struct FeatureToggles {
    #[arg(long = "enable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    enable: Vec<String>,

    #[arg(long = "disable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    disable: Vec<String>,
}

#[derive(Debug, Default, Parser, Clone)]
struct InteractiveRemoteOptions {
    #[arg(long = "remote", value_name = "ADDR")]
    remote: Option<String>,

    #[arg(long = "remote-auth-token-env", value_name = "ENV_VAR")]
    remote_auth_token_env: Option<String>,
}

impl FeatureToggles {
    fn to_overrides(&self) -> anyhow::Result<Vec<String>> {
        features_cmd::feature_toggle_overrides(&self.enable, &self.disable)
    }
}

#[derive(Debug, Parser)]
struct FeaturesCli {
    #[command(subcommand)]
    sub: FeaturesSubcommand,
}

#[derive(Debug, Parser)]
enum FeaturesSubcommand {
    List,
    Enable(FeatureSetArgs),
    Disable(FeatureSetArgs),
}

#[derive(Debug, Parser)]
struct FeatureSetArgs {
    feature: String,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(move |arg0_paths: Arg0DispatchPaths| async move {
        cli_main(arg0_paths).await?;
        Ok(())
    })
}

async fn cli_main(arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    let MultitoolCli {
        config_overrides: mut root_config_overrides,
        feature_toggles,
        remote,
        mut interactive,
        subcommand,
    } = MultitoolCli::parse();

    let toggle_overrides = feature_toggles.to_overrides()?;
    root_config_overrides.raw_overrides.extend(toggle_overrides);
    let root_remote = remote.remote;
    let root_remote_auth_token_env = remote.remote_auth_token_env;
    let root_strict_config = interactive.strict_config;
    reject_root_strict_config_for_subcommand(root_strict_config, &subcommand)?;
    if let Some(subcommand) = subcommand.as_ref() {
        profile_v2_for_subcommand(&interactive, subcommand)?;
    }

    match subcommand {
        None => {
            prepend_terminal_config_flags(
                &mut interactive.config_overrides,
                root_config_overrides.clone(),
            );
            let exit_info = run_interactive_tui(
                interactive,
                root_remote.clone(),
                root_remote_auth_token_env.clone(),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Exec(mut exec_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "exec",
            )?;
            let interactive_shared = codex_shared_cli_options(&interactive)?;
            exec_cli
                .shared
                .inherit_exec_root_options(&interactive_shared);
            exec_cli.strict_config |= root_strict_config;
            prepend_config_flags(
                &mut exec_cli.config_overrides,
                root_config_overrides.clone(),
            );
            octopus_codex_cli::exec_compat::run_main(exec_cli, arg0_paths.clone()).await?;
        }
        Some(Subcommand::Review(ReviewCommand {
            strict_config,
            args: review_args,
        })) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "review",
            )?;
            let mut exec_cli = ExecCli::try_parse_from(["codex", "exec"])?;
            let interactive_shared = codex_shared_cli_options(&interactive)?;
            exec_cli
                .shared
                .inherit_exec_root_options(&interactive_shared);
            exec_cli.command = Some(ExecCommand::Review(review_args));
            exec_cli.strict_config = strict_config || root_strict_config;
            prepend_config_flags(
                &mut exec_cli.config_overrides,
                root_config_overrides.clone(),
            );
            octopus_codex_cli::exec_compat::run_main(exec_cli, arg0_paths.clone()).await?;
        }
        Some(Subcommand::AppServer(app_server_cli)) => {
            let AppServerCommand {
                subcommand,
                strict_config: app_server_strict_config,
                listen,
                stdio,
                remote_control,
                analytics_default_enabled,
                auth,
            } = app_server_cli;
            let strict_config = app_server_strict_config || root_strict_config;
            reject_strict_config_for_app_server_subcommand(strict_config, subcommand.as_ref())?;
            reject_remote_mode_for_app_server_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                subcommand.as_ref(),
            )?;
            match subcommand {
                None => {
                    let transport = if stdio {
                        octopus_app_server_transport::AppServerTransport::Stdio
                    } else {
                        listen
                    };
                    let auth = auth.try_into_settings()?;
                    let _ = (
                        &transport,
                        &auth,
                        remote_control,
                        strict_config,
                        analytics_default_enabled,
                        &arg0_paths,
                        &root_config_overrides,
                    );
                    anyhow::bail!(
                        "the embedded `octopus app-server` foreground server is not available: it \
                         drove the dissolved octopus-app-server in-process frontdoor (now deleted; \
                         a request is an artifact at the bound client interface, pending the \
                         runtime arrival boundary); run the octopus-app daemon binary instead"
                    );
                }
                Some(AppServerSubcommand::Proxy(proxy_cli)) => {
                    let socket_path = match proxy_cli.socket_path {
                        Some(socket_path) => socket_path,
                        None => {
                            let octopus_cxhome = find_codex_home()?;
                            octopus_app_server_transport::app_server_control_socket_path(
                                &octopus_cxhome,
                            )?
                        }
                    };
                    crate::stdio_to_uds::run(socket_path.as_path()).await?;
                }
                Some(AppServerSubcommand::GenerateTs(gen_cli)) => {
                    let options = octopus_appserver_proto::GenerateTsOptions {
                        experimental_api: gen_cli.experimental,
                        ..Default::default()
                    };
                    octopus_appserver_proto::generate_ts_with_options(
                        &gen_cli.out_dir,
                        gen_cli.prettier.as_deref(),
                        options,
                    )?;
                }
                Some(AppServerSubcommand::GenerateJsonSchema(gen_cli)) => {
                    octopus_appserver_proto::generate_json_with_experimental(
                        &gen_cli.out_dir,
                        gen_cli.experimental,
                    )?;
                }
                Some(AppServerSubcommand::GenerateInternalJsonSchema(gen_cli)) => {
                    octopus_appserver_proto::generate_internal_json_schema(&gen_cli.out_dir)?;
                }
            }
        }
        Some(Subcommand::Resume(ResumeCommand {
            run_id,
            last,
            all,
            include_non_interactive,
            remote,
            config_overrides,
        })) => {
            let RunTuiCli(config_overrides) = config_overrides;
            interactive = finalize_resume_interactive(
                interactive,
                root_config_overrides.clone(),
                run_id,
                last,
                all,
                include_non_interactive,
                config_overrides,
            );
            let exit_info = run_interactive_tui(
                interactive,
                remote.remote.or(root_remote.clone()),
                remote
                    .remote_auth_token_env
                    .or(root_remote_auth_token_env.clone()),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Fork(ForkCommand {
            run_id,
            last,
            all,
            remote,
            config_overrides,
        })) => {
            let RunTuiCli(config_overrides) = config_overrides;
            interactive = finalize_fork_interactive(
                interactive,
                root_config_overrides.clone(),
                run_id,
                last,
                all,
                config_overrides,
            );
            let exit_info = run_interactive_tui(
                interactive,
                remote.remote.or(root_remote.clone()),
                remote
                    .remote_auth_token_env
                    .or(root_remote_auth_token_env.clone()),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Login(mut login_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "login",
            )?;
            prepend_config_flags(
                &mut login_cli.config_overrides,
                root_config_overrides.clone(),
            );
            match login_cli.action {
                Some(LoginSubcommand::Status) => {
                    run_login_status(login_cli.config_overrides).await;
                }
                None => {
                    if login_cli.with_api_key && login_cli.with_access_token {
                        eprintln!(
                            "Choose one login credential source: --with-api-key or --with-access-token."
                        );
                        std::process::exit(1);
                    } else if login_cli.use_device_code {
                        run_login_with_device_code(
                            login_cli.config_overrides,
                            login_cli.issuer_base_url,
                            login_cli.client_id,
                        )
                        .await;
                    } else if login_cli.api_key.is_some() {
                        eprintln!(
                            "The --api-key flag is no longer supported. Pipe the key instead, e.g. `printenv OPENAI_API_KEY | octopus login --with-api-key`."
                        );
                        std::process::exit(1);
                    } else if login_cli.with_api_key {
                        let api_key = read_api_key_from_stdin();
                        run_login_with_api_key(login_cli.config_overrides, api_key).await;
                    } else if login_cli.with_access_token {
                        let access_token = read_access_token_from_stdin();
                        run_login_with_access_token(login_cli.config_overrides, access_token).await;
                    } else {
                        run_login_with_chatgpt(login_cli.config_overrides).await;
                    }
                }
            }
        }
        Some(Subcommand::Logout(mut logout_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "logout",
            )?;
            prepend_config_flags(
                &mut logout_cli.config_overrides,
                root_config_overrides.clone(),
            );
            run_logout(logout_cli.config_overrides).await;
        }
        Some(Subcommand::Completion(completion_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "completion",
            )?;
            print_completion(completion_cli);
        }
        Some(Subcommand::Doctor(doctor_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "doctor",
            )?;
            let doctor_runtime = doctor::DoctorRuntimeOptions {
                shared: codex_shared_cli_options(&interactive)?,
                approval_policy: interactive.approval_policy.map(codex_approval_mode),
            };
            doctor::run_doctor(
                doctor_cli,
                root_config_overrides.clone(),
                &doctor_runtime,
                &arg0_paths,
            )
            .await?;
        }
        Some(Subcommand::Sandbox(mut sandbox_cli)) => {
            #[cfg(target_os = "windows")]
            if let Some(setup_cli) =
                octopus_sandbox_launcher::sandbox_setup::parse_setup_command(&sandbox_cli.command)?
            {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "sandbox setup",
                )?;
                octopus_sandbox_launcher::sandbox_setup::run(setup_cli).await?;
                return Ok(());
            }
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "sandbox",
            )?;
            let terminal_profile = interactive
                .config_profile_v2
                .as_ref()
                .map(codex_profile_name)
                .transpose()?;
            let config_profile = sandbox_cli
                .config_profile
                .as_ref()
                .or(terminal_profile.as_ref());
            let loader_overrides = loader_overrides_for_profile(config_profile)?;
            prepend_config_flags(
                &mut sandbox_cli.config_overrides,
                root_config_overrides.clone(),
            );
            #[cfg(target_os = "macos")]
            octopus_sandbox_launcher::run_command_under_seatbelt(
                sandbox_cli,
                arg0_paths.codex_linux_sandbox_exe.clone(),
                loader_overrides,
            )
            .await?;
            #[cfg(target_os = "linux")]
            octopus_sandbox_launcher::run_command_under_landlock(
                sandbox_cli,
                arg0_paths.codex_linux_sandbox_exe.clone(),
                loader_overrides,
            )
            .await?;
            #[cfg(target_os = "windows")]
            octopus_sandbox_launcher::run_command_under_windows_sandbox(
                sandbox_cli,
                arg0_paths.codex_linux_sandbox_exe.clone(),
                loader_overrides,
            )
            .await?;
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            {
                let _ = loader_overrides;
                anyhow::bail!("`octopus sandbox` is not supported on this operating system");
            }
        }
        Some(Subcommand::Debug(DebugCommand { subcommand })) => match subcommand {
            DebugSubcommand::PromptInput(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug prompt-input",
                )?;
                run_debug_prompt_input_command(
                    cmd,
                    root_config_overrides,
                    interactive,
                    arg0_paths.clone(),
                )
                .await?;
            }
            DebugSubcommand::ClearMemories => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug clear-memories",
                )?;
                run_debug_clear_memories_command(&root_config_overrides).await?;
            }
        },
        Some(Subcommand::Execpolicy(ExecpolicyCommand { sub })) => match sub {
            ExecpolicySubcommand::Check(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "execpolicy check",
                )?;
                run_execpolicycheck(cmd)?
            }
        },
        Some(Subcommand::Apply(mut apply_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "apply",
            )?;
            apply_cli
                .config_overrides
                .raw_overrides
                .splice(0..0, root_config_overrides.raw_overrides.clone());
            run_apply_command(apply_cli, None).await?;
        }
        Some(Subcommand::ResponsesApiProxy(args)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "responses-api-proxy",
            )?;
            tokio::task::spawn_blocking(move || octopus_responses_api_proxy::run_main(args))
                .await??;
        }
        Some(Subcommand::StdioToUds(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "stdio-to-uds",
            )?;
            let socket_path = cmd.socket_path;
            crate::stdio_to_uds::run(socket_path.as_path()).await?;
        }
        Some(Subcommand::ExecServer(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "exec-server",
            )?;
            let strict_config = cmd.strict_config || root_strict_config;
            run_exec_server_command(cmd, &arg0_paths, &root_config_overrides, strict_config)
                .await?;
        }
        Some(Subcommand::Features(FeaturesCli { sub })) => match sub {
            FeaturesSubcommand::List => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features list",
                )?;
                features_cmd::run_features_list(root_config_overrides).await?;
            }
            FeaturesSubcommand::Enable(FeatureSetArgs { feature }) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features enable",
                )?;
                features_cmd::enable_feature_in_config(&feature).await?;
            }
            FeaturesSubcommand::Disable(FeatureSetArgs { feature }) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features disable",
                )?;
                features_cmd::disable_feature_in_config(&feature).await?;
            }
        },
    }

    Ok(())
}

fn profile_v2_for_subcommand(
    interactive: &TuiCli,
    subcommand: &Subcommand,
) -> anyhow::Result<Option<ProfileV2Name>> {
    let Some(profile_v2) = interactive.config_profile_v2.as_ref() else {
        return Ok(None);
    };

    match subcommand {
        Subcommand::Exec(_)
        | Subcommand::Review(_)
        | Subcommand::Resume(_)
        | Subcommand::Fork(_)
        | Subcommand::Sandbox(_)
        | Subcommand::Debug(DebugCommand {
            subcommand: DebugSubcommand::PromptInput(_),
        }) => Ok(Some(codex_profile_name(profile_v2)?)),
        _ => anyhow::bail!(
            "--profile only applies to runtime commands: `octopus`, `octopus exec`, `octopus review`, `octopus resume`, `octopus fork`, `octopus sandbox`, and `octopus debug prompt-input`."
        ),
    }
}

async fn run_exec_server_command(
    cmd: ExecServerCommand,
    arg0_paths: &Arg0DispatchPaths,
    root_config_overrides: &CliConfigOverrides,
    strict_config: bool,
) -> anyhow::Result<()> {
    let codex_self_exe = arg0_paths
        .codex_self_exe
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Codex executable path is not configured"))?;
    let runtime_paths = octopus_exec::ExecServerRuntimePaths::new(
        codex_self_exe,
        arg0_paths.codex_linux_sandbox_exe.clone(),
    )?;
    if let Some(base_url) = cmd.remote {
        let environment_id = cmd
            .environment_id
            .ok_or_else(|| anyhow::anyhow!("--environment-id is required when --remote is set"))?;
        let config = load_exec_server_config(root_config_overrides, strict_config).await?;
        let _otel = exec_server_telemetry::init(Some(&config))
            .inspect_err(|err| eprintln!("Could not create otel exporter: {err}"))
            .ok();
        let auth_provider =
            load_exec_server_remote_auth_provider(&config, &base_url, cmd.use_agent_identity_auth)
                .await?;
        let mut remote_config =
            octopus_exec::RemoteEnvironmentConfig::new(base_url, environment_id, auth_provider)?;
        if let Some(name) = cmd.name {
            remote_config.name = name;
        }
        octopus_exec::run_remote_environment(remote_config, runtime_paths).await?;
        Ok(())
    } else {
        let config_result = load_exec_server_config(root_config_overrides, strict_config).await;
        let config = if strict_config {
            Some(config_result?)
        } else {
            config_result.ok()
        };
        let _otel = exec_server_telemetry::init(config.as_ref())
            .inspect_err(|err| eprintln!("Could not create otel exporter: {err}"))
            .ok();
        let listen_url = cmd
            .listen
            .as_deref()
            .unwrap_or(octopus_exec::DEFAULT_LISTEN_URL);
        if listen_url.starts_with("ws://") {
            let handle = cmd.auth_secret_handle.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "direct exec-server websocket listener requires --auth-secret-handle"
                )
            })?;
            let auth = octopus_exec::DirectExecServerAuth::load_private_file(handle)?;
            octopus_exec::run_main_authenticated(listen_url, runtime_paths, auth)
                .await
                .map_err(anyhow::Error::from_boxed)
        } else {
            if cmd.auth_secret_handle.is_some() {
                anyhow::bail!("--auth-secret-handle is only valid with a websocket listener");
            }
            octopus_exec::run_main(listen_url, runtime_paths)
                .await
                .map_err(anyhow::Error::from_boxed)
        }
    }
}

async fn load_exec_server_remote_auth_provider(
    config: &octopus_core::config::Config,
    base_url: &str,
    use_agent_identity_auth: bool,
) -> anyhow::Result<octopus_exec::SharedExecServerAuth> {
    if use_agent_identity_auth {
        let agent_identity_jwt = read_codex_access_token_from_env().ok_or_else(|| {
            anyhow::anyhow!("CODEX_ACCESS_TOKEN is required when --use-agent-identity-auth is set")
        })?;
        let auth =
            CodexAuth::from_agent_identity_jwt(&agent_identity_jwt, Some(&config.chatgpt_base_url))
                .await?;
        return Ok(octopus_codex_cli::auth_provider_from_auth(&auth));
    }

    let auth = load_exec_server_remote_auth(
        config,
        "remote exec-server registration requires ChatGPT authentication or API key authentication; run `octopus login` or set CODEX_API_KEY",
    )
    .await?;

    if !is_supported_exec_server_remote_auth(&auth) {
        anyhow::bail!(
            "remote exec-server registration requires ChatGPT authentication or API key authentication; Agent Identity auth requires --use-agent-identity-auth"
        );
    }

    if auth.is_api_key_auth() {
        validate_api_key_remote_host(base_url)?;
    }

    Ok(octopus_codex_cli::auth_provider_from_auth(&auth))
}

fn is_supported_exec_server_remote_auth(auth: &CodexAuth) -> bool {
    auth.is_chatgpt_auth() || auth.is_api_key_auth()
}

fn validate_api_key_remote_host(base_url: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(base_url)
        .map_err(|err| anyhow::anyhow!("invalid remote exec-server registration URL: {err}"))?;
    let host = url.host().ok_or_else(|| {
        anyhow::anyhow!("remote exec-server registration URL must include a host")
    })?;

    let is_loopback = match &host {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    let is_openai_host = match &host {
        url::Host::Domain(host) => ["openai.com", "openai.org"].into_iter().any(|domain| {
            host.eq_ignore_ascii_case(domain)
                || host.to_ascii_lowercase().ends_with(&format!(".{domain}"))
        }),
        _ => false,
    };
    let is_allowed = match url.scheme() {
        "https" => is_loopback || is_openai_host,
        "http" => is_loopback,
        _ => false,
    };

    if !is_allowed {
        anyhow::bail!(
            "remote exec-server API-key authentication is restricted to HTTPS openai.com and openai.org hosts and subdomains or loopback hosts"
        );
    }

    Ok(())
}

async fn load_exec_server_config(
    root_config_overrides: &CliConfigOverrides,
    strict_config: bool,
) -> anyhow::Result<octopus_core::config::Config> {
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    Ok(ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .strict_config(strict_config)
        .build()
        .await?)
}

async fn load_exec_server_remote_auth(
    config: &octopus_core::config::Config,
    missing_auth_error: &'static str,
) -> anyhow::Result<octopus_login::CodexAuth> {
    let auth_manager = AuthManager::shared_from_config(config, true).await;

    let auth = match auth_manager.auth().await {
        Some(auth) => auth,
        None => {
            auth_manager.reload().await;
            auth_manager
                .auth()
                .await
                .ok_or_else(|| anyhow::anyhow!(missing_auth_error))?
        }
    };

    Ok(auth)
}

fn loader_overrides_for_profile(
    profile_v2: Option<&ProfileV2Name>,
) -> anyhow::Result<LoaderOverrides> {
    match profile_v2 {
        Some(profile_v2) => {
            let octopus_cxhome = find_codex_home()?;
            Ok(LoaderOverrides {
                user_config_path: Some(resolve_profile_v2_config_path(&octopus_cxhome, profile_v2)),
                user_config_profile: Some(profile_v2.clone()),
                ..Default::default()
            })
        }
        None => Ok(LoaderOverrides::default()),
    }
}

async fn run_debug_prompt_input_command(
    cmd: DebugPromptInputCommand,
    root_config_overrides: CliConfigOverrides,
    interactive: TuiCli,
    arg0_paths: Arg0DispatchPaths,
) -> anyhow::Result<()> {
    let terminal_profile = interactive
        .config_profile_v2
        .as_ref()
        .map(codex_profile_name)
        .transpose()?;
    let loader_overrides = loader_overrides_for_profile(terminal_profile.as_ref())?;
    let shared = interactive.shared.into_inner();
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let approval_policy = if shared.dangerously_bypass_approvals_and_sandbox {
        Some(AskForApproval::Never)
    } else {
        interactive.approval_policy.map(terminal_approval_policy)
    };
    let sandbox_mode = if shared.dangerously_bypass_approvals_and_sandbox {
        Some(octopus_protocol::config_types::SandboxMode::DangerFullAccess)
    } else {
        shared.sandbox_mode.map(terminal_sandbox_mode)
    };
    let overrides = ConfigOverrides {
        model: shared.model,
        approval_policy,
        sandbox_mode,
        cwd: shared.cwd,
        codex_self_exe: arg0_paths.codex_self_exe,
        codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe,
        main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe,
        show_raw_agent_reasoning: shared.oss.then_some(true),
        ephemeral: Some(true),
        bypass_hook_trust: shared.bypass_hook_trust.then_some(true),
        additional_writable_roots: shared.add_dir,
        ..Default::default()
    };
    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .loader_overrides(loader_overrides)
        .build()
        .await?;

    let mut input = shared
        .images
        .into_iter()
        .chain(cmd.images)
        .map(|path| UserInput::LocalImage { path, detail: None })
        .collect::<Vec<_>>();
    if let Some(prompt) = cmd.prompt.or(interactive.prompt) {
        input.push(UserInput::Text {
            text: prompt.replace("\r\n", "\n").replace('\r', "\n"),
            text_elements: Vec::new(),
        });
    }

    let user_instructions_provider = Arc::new(CodexHomeUserInstructionsProvider::new(
        config.codex_home.clone(),
    ));
    let prompt_input =
        octopus_core::build_prompt_input(config, input, user_instructions_provider).await?;
    println!("{}", serde_json::to_string_pretty(&prompt_input)?);

    Ok(())
}

async fn run_debug_clear_memories_command(
    root_config_overrides: &CliConfigOverrides,
) -> anyhow::Result<()> {
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .build()
        .await?;

    clear_memory_roots_contents(&config.codex_home).await?;

    let message = format!(
        "Cleared memory artifact directories under {}.",
        config.codex_home.display()
    );

    println!("{message}");

    Ok(())
}

fn prepend_config_flags(
    subcommand_config_overrides: &mut CliConfigOverrides,
    cli_config_overrides: CliConfigOverrides,
) {
    subcommand_config_overrides.prepend_root_overrides(cli_config_overrides);
}

fn prepend_terminal_config_flags(
    subcommand_config_overrides: &mut octopus_tui::TerminalCliConfigOverrides,
    cli_config_overrides: CliConfigOverrides,
) {
    subcommand_config_overrides.prepend_root_overrides(octopus_tui::TerminalCliConfigOverrides {
        raw_overrides: cli_config_overrides.raw_overrides,
    });
}

fn codex_profile_name(value: &octopus_tui::TerminalProfileV2Name) -> anyhow::Result<ProfileV2Name> {
    value
        .as_str()
        .parse()
        .map_err(|error| anyhow::anyhow!("terminal profile is invalid for Codex: {error}"))
}

fn codex_approval_mode(value: octopus_tui::TerminalApprovalModeCliArg) -> CodexApprovalModeCliArg {
    match value {
        octopus_tui::TerminalApprovalModeCliArg::Untrusted => CodexApprovalModeCliArg::Untrusted,
        octopus_tui::TerminalApprovalModeCliArg::OnFailure => CodexApprovalModeCliArg::OnFailure,
        octopus_tui::TerminalApprovalModeCliArg::OnRequest => CodexApprovalModeCliArg::OnRequest,
        octopus_tui::TerminalApprovalModeCliArg::Never => CodexApprovalModeCliArg::Never,
    }
}

fn codex_sandbox_mode(value: octopus_tui::TerminalSandboxModeCliArg) -> CodexSandboxModeCliArg {
    match value {
        octopus_tui::TerminalSandboxModeCliArg::ReadOnly => CodexSandboxModeCliArg::ReadOnly,
        octopus_tui::TerminalSandboxModeCliArg::WorkspaceWrite => {
            CodexSandboxModeCliArg::WorkspaceWrite
        }
        octopus_tui::TerminalSandboxModeCliArg::DangerFullAccess => {
            CodexSandboxModeCliArg::DangerFullAccess
        }
    }
}

fn codex_shared_cli_options(interactive: &TuiCli) -> anyhow::Result<CodexSharedCliOptions> {
    Ok(CodexSharedCliOptions {
        images: interactive.images.clone(),
        model: interactive.model.clone(),
        oss: interactive.oss,
        oss_provider: interactive.oss_provider.clone(),
        config_profile_v2: interactive
            .config_profile_v2
            .as_ref()
            .map(codex_profile_name)
            .transpose()?,
        sandbox_mode: interactive.sandbox_mode.map(codex_sandbox_mode),
        dangerously_bypass_approvals_and_sandbox: interactive
            .dangerously_bypass_approvals_and_sandbox,
        bypass_hook_trust: interactive.bypass_hook_trust,
        cwd: interactive.cwd.clone(),
        add_dir: interactive.add_dir.clone(),
    })
}

fn terminal_approval_policy(value: octopus_tui::TerminalApprovalModeCliArg) -> AskForApproval {
    match value {
        octopus_tui::TerminalApprovalModeCliArg::Untrusted => AskForApproval::UnlessTrusted,
        octopus_tui::TerminalApprovalModeCliArg::OnFailure => AskForApproval::OnFailure,
        octopus_tui::TerminalApprovalModeCliArg::OnRequest => AskForApproval::OnRequest,
        octopus_tui::TerminalApprovalModeCliArg::Never => AskForApproval::Never,
    }
}

fn terminal_sandbox_mode(
    value: octopus_tui::TerminalSandboxModeCliArg,
) -> octopus_protocol::config_types::SandboxMode {
    match value {
        octopus_tui::TerminalSandboxModeCliArg::ReadOnly => {
            octopus_protocol::config_types::SandboxMode::ReadOnly
        }
        octopus_tui::TerminalSandboxModeCliArg::WorkspaceWrite => {
            octopus_protocol::config_types::SandboxMode::WorkspaceWrite
        }
        octopus_tui::TerminalSandboxModeCliArg::DangerFullAccess => {
            octopus_protocol::config_types::SandboxMode::DangerFullAccess
        }
    }
}

fn reject_remote_mode_for_subcommand(
    remote: Option<&str>,
    remote_auth_token_env: Option<&str>,
    subcommand: &str,
) -> anyhow::Result<()> {
    if let Some(remote) = remote {
        anyhow::bail!(
            "`--remote {remote}` is only supported for interactive TUI commands, not `octopus {subcommand}`"
        );
    }
    if remote_auth_token_env.is_some() {
        anyhow::bail!(
            "`--remote-auth-token-env` is only supported for interactive TUI commands, not `octopus {subcommand}`"
        );
    }
    Ok(())
}

fn reject_root_strict_config_for_subcommand(
    strict_config: bool,
    subcommand: &Option<Subcommand>,
) -> anyhow::Result<()> {
    if !strict_config {
        return Ok(());
    }

    match unsupported_subcommand_name_for_strict_config(subcommand) {
        Some(subcommand_name) => {
            reject_strict_config_for_unsupported_subcommand(strict_config, subcommand_name)
        }
        None => Ok(()),
    }
}

fn unsupported_subcommand_name_for_strict_config(
    subcommand: &Option<Subcommand>,
) -> Option<&'static str> {
    match subcommand {
        None
        | Some(Subcommand::Exec(_))
        | Some(Subcommand::Review(_))
        | Some(Subcommand::ExecServer(_))
        | Some(Subcommand::Resume(_))
        | Some(Subcommand::Fork(_))
        | Some(Subcommand::Doctor(_)) => None,
        Some(Subcommand::AppServer(app_server)) if app_server.subcommand.is_none() => None,
        Some(Subcommand::AppServer(app_server)) => {
            Some(app_server_subcommand_name(app_server.subcommand.as_ref()))
        }
        Some(Subcommand::Login(_)) => Some("login"),
        Some(Subcommand::Logout(_)) => Some("logout"),
        Some(Subcommand::Completion(_)) => Some("completion"),
        Some(Subcommand::Sandbox(_)) => Some("sandbox"),
        Some(Subcommand::Debug(_)) => Some("debug"),
        Some(Subcommand::Execpolicy(_)) => Some("execpolicy"),
        Some(Subcommand::Apply(_)) => Some("apply"),
        Some(Subcommand::ResponsesApiProxy(_)) => Some("responses-api-proxy"),
        Some(Subcommand::StdioToUds(_)) => Some("stdio-to-uds"),
        Some(Subcommand::Features(_)) => Some("features"),
    }
}

fn reject_strict_config_for_app_server_subcommand(
    strict_config: bool,
    subcommand: Option<&AppServerSubcommand>,
) -> anyhow::Result<()> {
    if subcommand.is_none() {
        return Ok(());
    }
    reject_strict_config_for_unsupported_subcommand(
        strict_config,
        app_server_subcommand_name(subcommand),
    )
}

fn reject_strict_config_for_unsupported_subcommand(
    strict_config: bool,
    subcommand: &str,
) -> anyhow::Result<()> {
    if strict_config {
        anyhow::bail!("`--strict-config` is not supported for `octopus {subcommand}`");
    }
    Ok(())
}

fn reject_remote_mode_for_app_server_subcommand(
    remote: Option<&str>,
    remote_auth_token_env: Option<&str>,
    subcommand: Option<&AppServerSubcommand>,
) -> anyhow::Result<()> {
    let subcommand_name = app_server_subcommand_name(subcommand);
    reject_remote_mode_for_subcommand(remote, remote_auth_token_env, subcommand_name)
}

fn app_server_subcommand_name(subcommand: Option<&AppServerSubcommand>) -> &'static str {
    match subcommand {
        None => "app-server",
        Some(AppServerSubcommand::Proxy(_)) => "app-server proxy",
        Some(AppServerSubcommand::GenerateTs(_)) => "app-server generate-ts",
        Some(AppServerSubcommand::GenerateJsonSchema(_)) => "app-server generate-json-schema",
        Some(AppServerSubcommand::GenerateInternalJsonSchema(_)) => {
            "app-server generate-internal-json-schema"
        }
    }
}

#[cfg(test)]
fn read_remote_auth_token_from_env_var_with<F>(
    env_var_name: &str,
    get_var: F,
) -> anyhow::Result<String>
where
    F: FnOnce(&str) -> Result<String, std::env::VarError>,
{
    let auth_token = get_var(env_var_name)
        .map_err(|_| anyhow::anyhow!("environment variable `{env_var_name}` is not set"))?;
    let auth_token = auth_token.trim().to_string();
    if auth_token.is_empty() {
        anyhow::bail!("environment variable `{env_var_name}` is empty");
    }
    Ok(auth_token)
}

async fn run_interactive_tui(
    mut interactive: TuiCli,
    remote: Option<String>,
    remote_auth_token_env: Option<String>,
    arg0_paths: Arg0DispatchPaths,
) -> std::io::Result<AppExitInfo> {
    if let Some(prompt) = interactive.prompt.take() {
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

    let terminal_info = octopus_terminal_detection::terminal_info();
    if terminal_info.name == TerminalName::Dumb {
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            return Ok(AppExitInfo::fatal(
                "TERM is set to \"dumb\". Refusing to start the interactive TUI because no terminal is available for a confirmation prompt (stdin/stderr is not a TTY). Run in a supported terminal or unset TERM.",
            ));
        }

        eprintln!(
            "WARNING: TERM is set to \"dumb\". Octopus's interactive TUI may not work in this terminal."
        );
        if !confirm("Continue anyway? [y/N]: ")? {
            return Ok(AppExitInfo::fatal(
                "Refusing to start the interactive TUI because TERM is set to \"dumb\". Run in a supported terminal or unset TERM.",
            ));
        }
    }

    let remote_endpoint = match resolve_remote_endpoint(remote, remote_auth_token_env) {
        Ok(remote_endpoint) => remote_endpoint,
        Err(err) if is_remote_auth_usage_error(&err) => {
            return Ok(AppExitInfo::fatal(err.to_string()));
        }
        Err(err) => return Err(err),
    };
    let terminal_runtime_paths = octopus_tui::TerminalRuntimePaths {
        self_exe: arg0_paths.codex_self_exe.clone(),
        linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe.clone(),
        execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
    };
    octopus_tui::run_main(
        interactive.clone(),
        terminal_runtime_paths,
        remote_endpoint.clone(),
        terminal_launcher_hooks(),
    )
    .await
}

fn terminal_launcher_hooks() -> octopus_tui::LauncherProviderHooks {
    octopus_tui::LauncherProviderHooks {
        telemetry_session_factory: Some(std::sync::Arc::new(|init| {
            provider_session_telemetry(init, false)
        })),
        ..Default::default()
    }
}

struct ProviderSessionMetricsSink {
    telemetry: octopus_otel::SessionTelemetry,
}

impl octopus_tui::session_telemetry::SessionMetricsSink for ProviderSessionMetricsSink {
    fn counter(&self, name: &str, inc: i64, tags: &[(&str, &str)]) {
        self.telemetry.counter(name, inc, tags);
    }

    fn record_duration(&self, name: &str, duration: std::time::Duration, tags: &[(&str, &str)]) {
        self.telemetry.record_duration(name, duration, tags);
    }

    fn runtime_metrics_summary(
        &self,
    ) -> Option<octopus_tui::session_telemetry::RuntimeMetricsSummary> {
        self.telemetry
            .runtime_metrics_summary()
            .map(terminal_runtime_metrics_summary)
    }

    fn reset_runtime_metrics(&self) {
        self.telemetry.reset_runtime_metrics();
    }
}

fn terminal_runtime_metrics_summary(
    summary: octopus_otel::RuntimeMetricsSummary,
) -> octopus_tui::session_telemetry::RuntimeMetricsSummary {
    let totals = |t: octopus_otel::RuntimeMetricTotals| {
        octopus_tui::session_telemetry::RuntimeMetricTotals {
            count: t.count,
            duration_ms: t.duration_ms,
        }
    };
    octopus_tui::session_telemetry::RuntimeMetricsSummary {
        tool_calls: totals(summary.tool_calls),
        api_calls: totals(summary.api_calls),
        streaming_events: totals(summary.streaming_events),
        websocket_calls: totals(summary.websocket_calls),
        websocket_events: totals(summary.websocket_events),
        responses_api_overhead_ms: summary.responses_api_overhead_ms,
        responses_api_inference_time_ms: summary.responses_api_inference_time_ms,
        responses_api_engine_iapi_ttft_ms: summary.responses_api_engine_iapi_ttft_ms,
        responses_api_engine_service_ttft_ms: summary.responses_api_engine_service_ttft_ms,
        responses_api_engine_iapi_tbt_ms: summary.responses_api_engine_iapi_tbt_ms,
        responses_api_engine_service_tbt_ms: summary.responses_api_engine_service_tbt_ms,
        turn_ttft_ms: summary.turn_ttft_ms,
        turn_ttfm_ms: summary.turn_ttfm_ms,
    }
}

fn provider_session_telemetry(
    init: octopus_tui::session_telemetry::TelemetrySessionInit,
    log_user_prompt: bool,
) -> octopus_tui::session_telemetry::SessionTelemetry {
    let session = octopus_otel::SessionTelemetry::new(
        init.run_id,
        init.model.as_str(),
        init.model.as_str(),
        None,
        init.account_email,
        None,
        init.originator,
        log_user_prompt,
        init.terminal_user_agent,
        serde_json::from_value(serde_json::json!("cli"))
            .unwrap_or_else(|err| panic!("cli session source should deserialize: {err}")),
    );
    octopus_tui::session_telemetry::SessionTelemetry::from_sink(std::sync::Arc::new(
        ProviderSessionMetricsSink { telemetry: session },
    ))
}

fn resolve_remote_endpoint(
    remote: Option<String>,
    remote_auth_token_env: Option<String>,
) -> std::io::Result<Option<octopus_tui::DaemonEndpoint>> {
    if remote_auth_token_env.is_some() {
        return Err(std::io::Error::other(
            "`--remote-auth-token-env` requires the retired app-server websocket transport; a \
             remote daemon is addressed as endpoint data (`--remote unix://PATH`, its control \
             socket), and outcomes are read from its durable movement ledger.",
        ));
    }
    remote
        .as_deref()
        .map(octopus_tui::resolve_remote_addr)
        .transpose()
        .map_err(std::io::Error::other)
}

fn is_remote_auth_usage_error(err: &std::io::Error) -> bool {
    err.to_string()
        .starts_with("`--remote-auth-token-env` requires")
}

fn confirm(prompt: &str) -> std::io::Result<bool> {
    eprintln!("{prompt}");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

fn finalize_resume_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    run_id: Option<String>,
    last: bool,
    show_all: bool,
    include_non_interactive: bool,
    mut resume_cli: TuiCli,
) -> TuiCli {
    let resume_run_id = if last && resume_cli.prompt.is_none() {
        resume_cli.prompt = run_id;
        None
    } else {
        run_id
    };
    interactive.resume_picker = resume_run_id.is_none() && !last;
    interactive.resume_last = last;
    interactive.resume_run_id = resume_run_id;
    interactive.resume_show_all = show_all;
    interactive.resume_include_non_interactive = include_non_interactive;

    merge_interactive_cli_flags(&mut interactive, resume_cli);

    prepend_terminal_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

fn finalize_fork_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    run_id: Option<String>,
    last: bool,
    show_all: bool,
    mut fork_cli: TuiCli,
) -> TuiCli {
    let fork_run_id = if last && fork_cli.prompt.is_none() {
        fork_cli.prompt = run_id;
        None
    } else {
        run_id
    };
    interactive.fork_picker = fork_run_id.is_none() && !last;
    interactive.fork_last = last;
    interactive.fork_run_id = fork_run_id;
    interactive.fork_show_all = show_all;

    merge_interactive_cli_flags(&mut interactive, fork_cli);

    prepend_terminal_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

fn merge_interactive_cli_flags(interactive: &mut TuiCli, subcommand_cli: TuiCli) {
    let TuiCli {
        shared,
        strict_config,
        approval_policy,
        prompt,
        config_overrides,
        ..
    } = subcommand_cli;
    interactive
        .shared
        .apply_subcommand_overrides(shared.into_inner());
    if let Some(approval) = approval_policy {
        interactive.approval_policy = Some(approval);
    }
    if strict_config {
        interactive.strict_config = true;
    }
    if let Some(prompt) = prompt {
        interactive.prompt = Some(prompt.replace("\r\n", "\n").replace('\r', "\n"));
    }

    interactive
        .config_overrides
        .raw_overrides
        .extend(config_overrides.raw_overrides);
}

fn print_completion(cmd: CompletionCommand) {
    let mut app = MultitoolCli::command();
    let name = "octopus";
    generate(cmd.shell, &mut app, name, &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use octopus_tui::TokenUsage;
    use pretty_assertions::assert_eq;

    #[test]
    fn exec_server_remote_auth_accepts_api_key_auth() {
        let auth = CodexAuth::from_api_key("sk-test");

        assert!(is_supported_exec_server_remote_auth(&auth));
    }

    #[test]
    fn exec_server_remote_api_key_auth_accepts_https_openai_domains() {
        for base_url in [
            "https://openai.com/api",
            "https://service.openai.com/api",
            "https://openai.org/api",
            "https://service.openai.org/api",
        ] {
            assert!(validate_api_key_remote_host(base_url).is_ok());
        }
    }

    #[test]
    fn exec_server_remote_api_key_auth_accepts_http_loopback() {
        for base_url in [
            "http://localhost:8098/api",
            "http://127.0.0.1:8098/api",
            "http://[::1]:8098/api",
        ] {
            assert!(validate_api_key_remote_host(base_url).is_ok());
        }
    }

    #[test]
    fn exec_server_remote_api_key_auth_rejects_http_openai_domain() {
        for base_url in [
            "http://service.openai.com/api",
            "http://service.openai.org/api",
        ] {
            let error = validate_api_key_remote_host(base_url)
                .expect_err("reject plaintext OpenAI destination");

            assert_eq!(
                error.to_string(),
                "remote exec-server API-key authentication is restricted to HTTPS openai.com and openai.org hosts and subdomains or loopback hosts"
            );
        }
    }

    #[test]
    fn exec_server_remote_api_key_auth_rejects_suffix_spoof() {
        let error = validate_api_key_remote_host("https://service.openai.org.evil.example/api")
            .expect_err("reject suffix spoof");

        assert_eq!(
            error.to_string(),
            "remote exec-server API-key authentication is restricted to HTTPS openai.com and openai.org hosts and subdomains or loopback hosts"
        );
    }

    fn finalize_resume_from_args(args: &[&str]) -> TuiCli {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            interactive,
            config_overrides: root_overrides,
            subcommand,
            feature_toggles: _,
            remote: _,
        } = cli;

        let Subcommand::Resume(ResumeCommand {
            run_id,
            last,
            all,
            include_non_interactive,
            remote: _,
            config_overrides: resume_cli,
        }) = subcommand.expect("resume present")
        else {
            unreachable!()
        };
        let RunTuiCli(resume_cli) = resume_cli;

        finalize_resume_interactive(
            interactive,
            root_overrides,
            run_id,
            last,
            all,
            include_non_interactive,
            resume_cli,
        )
    }

    fn finalize_fork_from_args(args: &[&str]) -> TuiCli {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            interactive,
            config_overrides: root_overrides,
            subcommand,
            feature_toggles: _,
            remote: _,
        } = cli;

        let Subcommand::Fork(ForkCommand {
            run_id,
            last,
            all,
            remote: _,
            config_overrides: fork_cli,
        }) = subcommand.expect("fork present")
        else {
            unreachable!()
        };
        let RunTuiCli(fork_cli) = fork_cli;

        finalize_fork_interactive(interactive, root_overrides, run_id, last, all, fork_cli)
    }

    fn profile_v2_for_args(args: &[&str]) -> anyhow::Result<Option<String>> {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let Some(subcommand) = cli.subcommand.as_ref() else {
            return Ok(cli
                .interactive
                .config_profile_v2
                .as_ref()
                .map(std::string::ToString::to_string));
        };
        Ok(profile_v2_for_subcommand(&cli.interactive, subcommand)?
            .map(|profile| profile.to_string()))
    }

    #[test]
    fn profile_v2_is_allowed_for_runtime_subcommands() {
        assert_eq!(
            profile_v2_for_args(&["codex", "--profile", "work", "resume"])
                .expect("resume supports profile-v2")
                .as_deref(),
            Some("work")
        );
        assert_eq!(
            profile_v2_for_args(&["codex", "--profile", "work", "debug", "prompt-input"])
                .expect("debug prompt-input supports profile-v2")
                .as_deref(),
            Some("work")
        );
        assert_eq!(
            profile_v2_for_args(&["codex", "--profile", "work", "sandbox"])
                .expect("sandbox supports config profile")
                .as_deref(),
            Some("work")
        );
    }

    #[test]
    fn import_remains_an_interactive_prompt() {
        let cli = MultitoolCli::try_parse_from(["codex", "import"]).expect("parse");

        assert!(cli.subcommand.is_none());
        assert_eq!(cli.interactive.prompt.as_deref(), Some("import"));
    }

    #[test]
    fn profile_v2_rejects_non_plain_names_at_parse_time() {
        assert!(
            MultitoolCli::try_parse_from(["codex", "--profile", "nested/work", "resume"]).is_err()
        );
    }

    #[test]
    fn exec_resume_last_accepts_prompt_positional() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "exec", "--json", "resume", "--last", "2+2"])
                .expect("parse should succeed");

        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(octopus_codex_cli::exec_compat::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };

        assert!(args.last);
        assert_eq!(args.session_id, None);
        assert_eq!(args.prompt.as_deref(), Some("2+2"));
    }

    #[test]
    fn exec_resume_accepts_output_flags_after_subcommand() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "exec",
            "resume",
            "session-123",
            "-o",
            "/tmp/resume-output.md",
            "--output-schema",
            "/tmp/schema.json",
            "re-review",
        ])
        .expect("parse should succeed");

        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(octopus_codex_cli::exec_compat::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };

        assert_eq!(
            exec.last_message_file,
            Some(std::path::PathBuf::from("/tmp/resume-output.md"))
        );
        assert_eq!(
            exec.output_schema,
            Some(std::path::PathBuf::from("/tmp/schema.json"))
        );
        assert_eq!(args.session_id.as_deref(), Some("session-123"));
        assert_eq!(args.prompt.as_deref(), Some("re-review"));
    }

    #[test]
    fn dangerous_bypass_conflicts_with_approval_policy() {
        let err = MultitoolCli::try_parse_from([
            "codex",
            "--dangerously-bypass-approvals-and-sandbox",
            "--ask-for-approval",
            "on-request",
        ])
        .expect_err("conflicting permission flags should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    fn app_server_from_args(args: &[&str]) -> AppServerCommand {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let Subcommand::AppServer(app_server) = cli.subcommand.expect("app-server present") else {
            unreachable!()
        };
        app_server
    }

    fn default_app_server_socket_path() -> AbsolutePathBuf {
        let octopus_cxhome = find_codex_home().expect("codex home");
        octopus_app_server_transport::app_server_control_socket_path(&octopus_cxhome)
            .expect("default app-server socket path")
    }

    #[test]
    fn debug_prompt_input_parses_prompt_and_images() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "debug",
            "prompt-input",
            "hello",
            "--image",
            "/tmp/a.png,/tmp/b.png",
        ])
        .expect("parse");

        let Some(Subcommand::Debug(DebugCommand {
            subcommand: DebugSubcommand::PromptInput(cmd),
        })) = cli.subcommand
        else {
            panic!("expected debug prompt-input subcommand");
        };

        assert_eq!(cmd.prompt.as_deref(), Some("hello"));
        assert_eq!(
            cmd.images,
            vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")]
        );
    }

    #[test]
    fn responses_subcommand_is_not_registered() {
        let command = MultitoolCli::command();
        assert!(
            command
                .get_subcommands()
                .all(|subcommand| subcommand.get_name() != "responses")
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_parses_permissions_profile() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "sandbox",
            "--permissions-profile",
            ":workspace",
            "--",
            "echo",
        ])
        .expect("parse");

        let Some(Subcommand::Sandbox(command)) = cli.subcommand else {
            panic!("expected sandbox command");
        };

        assert_eq!(command.permissions_profile.as_deref(), Some(":workspace"));
        assert_eq!(command.command, vec!["echo"]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_parses_permissions_profile_short_alias() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "sandbox", "-P", ":workspace", "--", "echo"])
                .expect("parse");

        let Some(Subcommand::Sandbox(command)) = cli.subcommand else {
            panic!("expected sandbox command");
        };

        assert_eq!(command.permissions_profile.as_deref(), Some(":workspace"));
        assert_eq!(command.command, vec!["echo"]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_parses_config_profile() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "sandbox", "--profile", "work", "--", "echo"])
                .expect("parse");

        let Some(Subcommand::Sandbox(command)) = cli.subcommand else {
            panic!("expected sandbox command");
        };

        assert_eq!(command.config_profile.as_deref(), Some("work"));
        assert_eq!(command.command, vec!["echo"]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn sandbox_rejects_explicit_profile_controls_without_profile() {
        let err = MultitoolCli::try_parse_from(["codex", "sandbox", "-C", "/tmp"])
            .expect_err("parse should fail");

        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn full_auto_no_longer_parses_at_top_level() {
        let result = MultitoolCli::try_parse_from(["codex", "--full-auto"]);

        assert!(result.is_err());
    }

    #[test]
    fn exec_full_auto_reports_migration_path() {
        let cli = MultitoolCli::try_parse_from(["codex", "exec", "--full-auto", "summarize"])
            .expect("exec should accept removed flag long enough to report a migration path");
        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };

        assert_eq!(
            exec.removed_full_auto_warning(),
            Some("warning: `--full-auto` is deprecated; use `--sandbox workspace-write` instead.")
        );
    }

    #[test]
    fn sandbox_full_auto_no_longer_parses() {
        let result = MultitoolCli::try_parse_from(["codex", "sandbox", "--full-auto", "--"]);

        assert!(result.is_err());
    }

    fn sample_exit_info(run_id: Option<&str>, thread_name: Option<&str>) -> AppExitInfo {
        let token_usage = TokenUsage {
            output_tokens: 2,
            total_tokens: 2,
            ..Default::default()
        };
        let thread_id = run_id.map(str::to_string);
        AppExitInfo {
            token_usage,
            resume_hint: octopus_codex_cli::cli_args::resume_hint(
                thread_name,
                thread_id.as_deref(),
            ),
            thread_id,
            exit_reason: ExitReason::UserRequested,
        }
    }

    #[test]
    fn format_exit_messages_skips_zero_usage() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            thread_id: None,
            resume_hint: None,
            exit_reason: ExitReason::UserRequested,
        };
        let lines = format_exit_messages(exit_info, false);
        assert!(lines.is_empty());
    }

    #[test]
    fn format_exit_messages_includes_run_id_for_fatal_exit_without_resume_hint() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            thread_id: Some("123e4567-e89b-12d3-a456-426614174000".to_string()),
            resume_hint: None,
            exit_reason: ExitReason::Fatal("boom".to_string()),
        };
        let lines = format_exit_messages(exit_info, false);
        assert_eq!(
            lines,
            vec!["Run ID: 123e4567-e89b-12d3-a456-426614174000".to_string()]
        );
    }

    #[test]
    fn format_exit_messages_includes_resume_hint_for_fatal_exit() {
        let mut exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        exit_info.exit_reason = ExitReason::Fatal("boom".to_string());
        let lines = format_exit_messages(exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this chat, run octopus resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_includes_resume_hint_without_color() {
        let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        let lines = format_exit_messages(exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this chat, run octopus resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_applies_color_when_enabled() {
        let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        let lines = format_exit_messages(exit_info, true);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("\u{1b}[36m"));
    }

    #[test]
    fn format_exit_messages_names_picker_item_when_thread_has_name() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            Some("my-thread"),
        );
        let lines = format_exit_messages(exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this chat, run octopus resume, then select my-thread (123e4567-e89b-12d3-a456-426614174000)".to_string(),
            ]
        );
    }

    #[test]
    fn resume_model_flag_applies_when_no_root_flags() {
        let interactive =
            finalize_resume_from_args(["codex", "resume", "-m", "gpt-5.1-test"].as_ref());

        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_run_id, None);
    }

    #[test]
    fn resume_picker_logic_none_and_not_last() {
        let interactive = finalize_resume_from_args(["codex", "resume"].as_ref());
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_run_id, None);
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_picker_logic_last() {
        let interactive = finalize_resume_from_args(["codex", "resume", "--last"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(interactive.resume_last);
        assert_eq!(interactive.resume_run_id, None);
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_last_accepts_prompt_positional() {
        let interactive = finalize_resume_from_args(
            ["codex", "resume", "--last", "/compact focus on auth"].as_ref(),
        );

        assert!(!interactive.resume_picker);
        assert!(interactive.resume_last);
        assert_eq!(interactive.resume_run_id, None);
        assert_eq!(
            interactive.prompt.as_deref(),
            Some("/compact focus on auth")
        );
    }

    #[test]
    fn resume_last_rejects_explicit_run_and_prompt() {
        let err =
            MultitoolCli::try_parse_from(["codex", "resume", "--last", "1234", "continue here"])
                .expect_err("--last with an explicit run and prompt should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn resume_picker_logic_with_run_id() {
        let interactive = finalize_resume_from_args(["codex", "resume", "1234"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_run_id.as_deref(), Some("1234"));
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_with_run_id_accepts_prompt_positional() {
        let interactive =
            finalize_resume_from_args(["codex", "resume", "1234", "continue here"].as_ref());

        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_run_id.as_deref(), Some("1234"));
        assert_eq!(interactive.prompt.as_deref(), Some("continue here"));
    }

    #[test]
    fn resume_all_flag_sets_show_all() {
        let interactive = finalize_resume_from_args(["codex", "resume", "--all"].as_ref());
        assert!(interactive.resume_picker);
        assert!(interactive.resume_show_all);
    }

    #[test]
    fn resume_include_non_interactive_flag_sets_source_filter_override() {
        let interactive =
            finalize_resume_from_args(["codex", "resume", "--include-non-interactive"].as_ref());

        assert!(interactive.resume_picker);
        assert!(interactive.resume_include_non_interactive);
    }

    #[test]
    fn resume_merges_option_flags() {
        let interactive = finalize_resume_from_args(
            [
                "codex",
                "resume",
                "sid",
                "--oss",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "on-request",
                "-m",
                "gpt-5.1-test",
                "-p",
                "my-config",
                "-C",
                "/tmp",
                "--strict-config",
                "-i",
                "/tmp/a.png,/tmp/b.png",
            ]
            .as_ref(),
        );

        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert!(interactive.oss);
        assert_eq!(interactive.config_profile_v2.as_deref(), Some("my-config"));
        assert_matches!(
            interactive.sandbox_mode,
            Some(octopus_tui::TerminalSandboxModeCliArg::WorkspaceWrite)
        );
        assert_matches!(
            interactive.approval_policy,
            Some(octopus_tui::TerminalApprovalModeCliArg::OnRequest)
        );
        assert_eq!(
            interactive.cwd.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
        assert!(interactive.strict_config);
        let has_a = interactive
            .images
            .iter()
            .any(|p| p == std::path::Path::new("/tmp/a.png"));
        let has_b = interactive
            .images
            .iter()
            .any(|p| p == std::path::Path::new("/tmp/b.png"));
        assert!(has_a && has_b);
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_run_id.as_deref(), Some("sid"));
    }

    #[test]
    fn resume_merges_dangerously_bypass_flag() {
        let interactive = finalize_resume_from_args(
            [
                "codex",
                "resume",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
            .as_ref(),
        );
        assert!(interactive.dangerously_bypass_approvals_and_sandbox);
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_run_id, None);
    }

    #[test]
    fn resume_merges_bypass_hook_trust_flag() {
        let interactive = finalize_resume_from_args(
            ["codex", "resume", "--dangerously-bypass-hook-trust"].as_ref(),
        );

        assert!(interactive.bypass_hook_trust);
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_run_id, None);
    }

    #[test]
    fn fork_picker_logic_none_and_not_last() {
        let interactive = finalize_fork_from_args(["codex", "fork"].as_ref());
        assert!(interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_run_id, None);
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_picker_logic_last() {
        let interactive = finalize_fork_from_args(["codex", "fork", "--last"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(interactive.fork_last);
        assert_eq!(interactive.fork_run_id, None);
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_last_accepts_prompt_positional() {
        let interactive =
            finalize_fork_from_args(["codex", "fork", "--last", "/compact focus on auth"].as_ref());

        assert!(!interactive.fork_picker);
        assert!(interactive.fork_last);
        assert_eq!(interactive.fork_run_id, None);
        assert_eq!(
            interactive.prompt.as_deref(),
            Some("/compact focus on auth")
        );
    }

    #[test]
    fn fork_last_rejects_explicit_run_and_prompt() {
        let err =
            MultitoolCli::try_parse_from(["codex", "fork", "--last", "1234", "continue here"])
                .expect_err("--last with an explicit run and prompt should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn fork_picker_logic_with_run_id() {
        let interactive = finalize_fork_from_args(["codex", "fork", "1234"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_run_id.as_deref(), Some("1234"));
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_with_run_id_accepts_prompt_positional() {
        let interactive =
            finalize_fork_from_args(["codex", "fork", "1234", "continue here"].as_ref());

        assert!(!interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_run_id.as_deref(), Some("1234"));
        assert_eq!(interactive.prompt.as_deref(), Some("continue here"));
    }

    #[test]
    fn fork_all_flag_sets_show_all() {
        let interactive = finalize_fork_from_args(["codex", "fork", "--all"].as_ref());
        assert!(interactive.fork_picker);
        assert!(interactive.fork_show_all);
    }

    #[test]
    fn app_server_analytics_default_disabled_without_flag() {
        let app_server = app_server_from_args(["codex", "app-server"].as_ref());
        assert!(!app_server.analytics_default_enabled);
        assert!(!app_server.remote_control);
        assert_eq!(
            app_server.listen,
            octopus_app_server_transport::AppServerTransport::Stdio
        );
    }

    #[test]
    fn app_server_analytics_default_enabled_with_flag() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--analytics-default-enabled"].as_ref());
        assert!(app_server.analytics_default_enabled);
    }

    #[test]
    fn strict_config_parses_for_supported_commands() {
        let cli = MultitoolCli::try_parse_from(["codex", "--strict-config"]).expect("parse");
        assert!(cli.interactive.strict_config);

        let cli =
            MultitoolCli::try_parse_from(["codex", "review", "--strict-config", "--uncommitted"])
                .expect("parse");
        assert_matches!(
            cli.subcommand,
            Some(Subcommand::Review(ReviewCommand {
                strict_config: true,
                ..
            }))
        );

        let cli = MultitoolCli::try_parse_from(["codex", "exec-server", "--strict-config"])
            .expect("parse");
        assert_matches!(
            cli.subcommand,
            Some(Subcommand::ExecServer(ExecServerCommand {
                strict_config: true,
                ..
            }))
        );
    }

    #[test]
    fn root_strict_config_is_supported_for_exec_server() {
        let cli = MultitoolCli::try_parse_from(["codex", "--strict-config", "exec-server"])
            .expect("parse");

        reject_root_strict_config_for_subcommand(cli.interactive.strict_config, &cli.subcommand)
            .expect("exec-server should support root --strict-config");
    }

    #[test]
    fn root_strict_config_is_rejected_for_unsupported_subcommands() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "--strict-config", "login"]).expect("parse");
        let err = reject_root_strict_config_for_subcommand(
            cli.interactive.strict_config,
            &cli.subcommand,
        )
        .expect_err("login should not support root --strict-config");

        assert_eq!(
            err.to_string(),
            "`--strict-config` is not supported for `octopus login`"
        );

        let cli = MultitoolCli::try_parse_from(["codex", "--strict-config", "completion"])
            .expect("parse");
        let err = reject_root_strict_config_for_subcommand(
            cli.interactive.strict_config,
            &cli.subcommand,
        )
        .expect_err("completion should not support root --strict-config");

        assert_eq!(
            err.to_string(),
            "`--strict-config` is not supported for `octopus completion`"
        );
    }

    #[test]
    fn app_server_subcommands_reject_strict_config() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--strict-config", "proxy"].as_ref());
        let err = reject_strict_config_for_app_server_subcommand(
            app_server.strict_config,
            app_server.subcommand.as_ref(),
        )
        .expect_err("app-server proxy should not support --strict-config");

        assert_eq!(
            err.to_string(),
            "`--strict-config` is not supported for `octopus app-server proxy`"
        );
    }

    #[test]
    fn remote_flag_parses_for_interactive_root() {
        let cli = MultitoolCli::try_parse_from(["codex", "--remote", "unix://codex.sock"])
            .expect("parse");
        assert_eq!(cli.remote.remote.as_deref(), Some("unix://codex.sock"));
    }

    #[test]
    fn remote_auth_token_env_flag_parses_for_interactive_root() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "--remote-auth-token-env",
            "CODEX_REMOTE_AUTH_TOKEN",
            "--remote",
            "ws://127.0.0.1:4500",
        ])
        .expect("parse");
        assert_eq!(
            cli.remote.remote_auth_token_env.as_deref(),
            Some("CODEX_REMOTE_AUTH_TOKEN")
        );
    }

    #[test]
    fn remote_flag_parses_for_resume_subcommand() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "resume", "--remote", "unix://codex.sock"])
                .expect("parse");
        let Subcommand::Resume(ResumeCommand { remote, .. }) =
            cli.subcommand.expect("resume present")
        else {
            panic!("expected resume subcommand");
        };
        assert_eq!(remote.remote.as_deref(), Some("unix://codex.sock"));
    }

    #[test]
    fn reject_remote_mode_for_non_interactive_subcommands() {
        let err = reject_remote_mode_for_subcommand(Some("127.0.0.1:4500"), None, "exec")
            .expect_err("non-interactive subcommands should reject --remote");
        assert!(
            err.to_string()
                .contains("only supported for interactive TUI commands")
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_non_interactive_subcommands() {
        let err = reject_remote_mode_for_subcommand(None, Some("CODEX_REMOTE_AUTH_TOKEN"), "exec")
            .expect_err("non-interactive subcommands should reject --remote-auth-token-env");
        assert!(
            err.to_string()
                .contains("only supported for interactive TUI commands")
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_app_server_generate_internal_json_schema() {
        let subcommand =
            AppServerSubcommand::GenerateInternalJsonSchema(GenerateInternalJsonSchemaCommand {
                out_dir: PathBuf::from("/tmp/out"),
            });
        let err = reject_remote_mode_for_app_server_subcommand(
            None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            Some(&subcommand),
        )
        .expect_err("non-interactive app-server subcommands should reject --remote-auth-token-env");
        assert!(err.to_string().contains("generate-internal-json-schema"));
    }

    #[test]
    fn read_remote_auth_token_from_env_var_reports_missing_values() {
        let err = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
            Err(std::env::VarError::NotPresent)
        })
        .expect_err("missing env vars should be rejected");
        assert!(err.to_string().contains("is not set"));
    }

    #[test]
    fn read_remote_auth_token_from_env_var_trims_values() {
        let auth_token =
            read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
                Ok("  bearer-token  ".to_string())
            })
            .expect("env var should parse");
        assert_eq!(auth_token, "bearer-token");
    }

    #[test]
    fn read_remote_auth_token_from_env_var_rejects_empty_values() {
        let err = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
            Ok(" \n\t ".to_string())
        })
        .expect_err("empty env vars should be rejected");
        assert!(err.to_string().contains("is empty"));
    }

    #[test]
    fn app_server_listen_websocket_url_parses() {
        let app_server = app_server_from_args(
            ["codex", "app-server", "--listen", "ws://127.0.0.1:4500"].as_ref(),
        );
        assert_eq!(
            app_server.listen,
            octopus_app_server_transport::AppServerTransport::WebSocket {
                bind_address: "127.0.0.1:4500".parse().expect("valid socket address"),
            }
        );
    }

    #[test]
    fn app_server_listen_stdio_url_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--listen", "stdio://"].as_ref());
        assert_eq!(
            app_server.listen,
            octopus_app_server_transport::AppServerTransport::Stdio
        );
    }

    #[test]
    fn app_server_stdio_flag_parses() {
        let app_server = app_server_from_args(["codex", "app-server", "--stdio"].as_ref());
        assert!(app_server.stdio);
    }

    #[test]
    fn app_server_stdio_flag_conflicts_with_listen() {
        let err = MultitoolCli::try_parse_from([
            "codex",
            "app-server",
            "--stdio",
            "--listen",
            "stdio://",
        ])
        .expect_err("--stdio and --listen should be rejected together");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn app_server_listen_unix_socket_url_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--listen", "unix://"].as_ref());
        assert_eq!(
            app_server.listen,
            octopus_app_server_transport::AppServerTransport::UnixSocket {
                socket_path: default_app_server_socket_path()
            }
        );
    }

    #[test]
    fn app_server_listen_unix_socket_path_parses() {
        let app_server = app_server_from_args(
            ["codex", "app-server", "--listen", "unix:///tmp/codex.sock"].as_ref(),
        );
        assert_eq!(
            app_server.listen,
            octopus_app_server_transport::AppServerTransport::UnixSocket {
                socket_path: AbsolutePathBuf::from_absolute_path("/tmp/codex.sock")
                    .expect("absolute path should parse")
            }
        );
    }

    #[test]
    fn app_server_listen_off_parses() {
        let app_server = app_server_from_args(["codex", "app-server", "--listen", "off"].as_ref());
        assert_eq!(
            app_server.listen,
            octopus_app_server_transport::AppServerTransport::Off
        );
    }

    #[test]
    fn app_server_listen_invalid_url_fails_to_parse() {
        let parse_result =
            MultitoolCli::try_parse_from(["codex", "app-server", "--listen", "http://foo"]);
        assert!(parse_result.is_err());
    }

    #[test]
    fn app_server_proxy_subcommand_parses() {
        let app_server = app_server_from_args(["codex", "app-server", "proxy"].as_ref());
        assert!(matches!(
            app_server.subcommand,
            Some(AppServerSubcommand::Proxy(AppServerProxyCommand {
                socket_path: None
            }))
        ));
    }

    #[test]
    fn app_server_proxy_sock_path_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "proxy", "--sock", "codex.sock"].as_ref());
        let Some(AppServerSubcommand::Proxy(proxy)) = app_server.subcommand else {
            panic!("expected proxy subcommand");
        };
        assert_eq!(
            proxy.socket_path,
            Some(
                AbsolutePathBuf::relative_to_current_dir("codex.sock")
                    .expect("relative path should resolve")
            )
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_app_server_proxy() {
        let subcommand = AppServerSubcommand::Proxy(AppServerProxyCommand { socket_path: None });
        let err = reject_remote_mode_for_app_server_subcommand(
            None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            Some(&subcommand),
        )
        .expect_err("app-server proxy should reject --remote-auth-token-env");
        assert!(err.to_string().contains("app-server proxy"));
    }

    #[test]
    fn app_server_capability_token_flags_parse() {
        let app_server = app_server_from_args(
            [
                "codex",
                "app-server",
                "--ws-auth",
                "capability-token",
                "--ws-token-file",
                "/tmp/codex-token",
            ]
            .as_ref(),
        );
        assert_eq!(
            app_server.auth.ws_auth,
            Some(octopus_app_server_transport::auth::WebsocketAuthCliMode::CapabilityToken)
        );
        assert_eq!(
            app_server.auth.ws_token_file,
            Some(PathBuf::from("/tmp/codex-token"))
        );
    }

    #[test]
    fn app_server_signed_bearer_flags_parse() {
        let app_server = app_server_from_args(
            [
                "codex",
                "app-server",
                "--ws-auth",
                "signed-bearer-token",
                "--ws-shared-secret-file",
                "/tmp/codex-secret",
                "--ws-issuer",
                "issuer",
                "--ws-audience",
                "audience",
                "--ws-max-clock-skew-seconds",
                "9",
            ]
            .as_ref(),
        );
        assert_eq!(
            app_server.auth.ws_auth,
            Some(octopus_app_server_transport::auth::WebsocketAuthCliMode::SignedBearerToken)
        );
        assert_eq!(
            app_server.auth.ws_shared_secret_file,
            Some(PathBuf::from("/tmp/codex-secret"))
        );
        assert_eq!(app_server.auth.ws_issuer.as_deref(), Some("issuer"));
        assert_eq!(app_server.auth.ws_audience.as_deref(), Some("audience"));
        assert_eq!(app_server.auth.ws_max_clock_skew_seconds, Some(9));
    }

    #[test]
    fn app_server_rejects_removed_insecure_non_loopback_flag() {
        let parse_result = MultitoolCli::try_parse_from([
            "codex",
            "app-server",
            "--allow-unauthenticated-non-loopback-ws",
        ]);
        assert!(parse_result.is_err());
    }

    #[test]
    fn features_enable_parses_feature_name() {
        let cli = MultitoolCli::try_parse_from(["codex", "features", "enable", "unified_exec"])
            .expect("parse should succeed");
        let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
            panic!("expected features subcommand");
        };
        let FeaturesSubcommand::Enable(FeatureSetArgs { feature }) = sub else {
            panic!("expected features enable");
        };
        assert_eq!(feature, "unified_exec");
    }

    #[test]
    fn features_disable_parses_feature_name() {
        let cli = MultitoolCli::try_parse_from(["codex", "features", "disable", "shell_tool"])
            .expect("parse should succeed");
        let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
            panic!("expected features subcommand");
        };
        let FeaturesSubcommand::Disable(FeatureSetArgs { feature }) = sub else {
            panic!("expected features disable");
        };
        assert_eq!(feature, "shell_tool");
    }

    #[test]
    fn feature_toggles_known_features_generate_overrides() {
        let toggles = FeatureToggles {
            enable: vec!["web_search_request".to_string()],
            disable: vec!["unified_exec".to_string()],
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec![
                "features.web_search_request=true".to_string(),
                "features.unified_exec=false".to_string(),
            ]
        );
    }

    #[test]
    fn feature_toggles_accept_legacy_linux_sandbox_flag() {
        let toggles = FeatureToggles {
            enable: vec!["use_linux_sandbox_bwrap".to_string()],
            disable: Vec::new(),
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec!["features.use_linux_sandbox_bwrap=true".to_string(),]
        );
    }

    #[test]
    fn feature_toggles_accept_removed_image_detail_original_flag() {
        let toggles = FeatureToggles {
            enable: vec!["image_detail_original".to_string()],
            disable: Vec::new(),
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec!["features.image_detail_original=true".to_string(),]
        );
    }

    #[test]
    fn feature_toggles_unknown_feature_errors() {
        let toggles = FeatureToggles {
            enable: vec!["does_not_exist".to_string()],
            disable: Vec::new(),
        };
        let err = toggles
            .to_overrides()
            .expect_err("feature should be rejected");
        assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
    }

    #[test]
    fn strict_config_with_unknown_enable_errors() {
        let err = strict_config_feature_toggle_error(["--enable", "does_not_exist"].as_ref());
        assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
    }

    #[test]
    fn strict_config_with_unknown_disable_errors() {
        let err = strict_config_feature_toggle_error(["--disable", "does_not_exist"].as_ref());
        assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
    }

    #[test]
    fn strict_config_with_compound_enable_errors() {
        let err = strict_config_feature_toggle_error(
            ["--enable", "multi_agent_v2.subagent_usage_hint_text"].as_ref(),
        );
        assert_eq!(
            err.to_string(),
            "Unknown feature flag: multi_agent_v2.subagent_usage_hint_text"
        );
    }

    fn strict_config_feature_toggle_error(args: &[&str]) -> anyhow::Error {
        let cli_args = std::iter::once("codex")
            .chain(std::iter::once("--strict-config"))
            .chain(args.iter().copied());
        let cli = MultitoolCli::try_parse_from(cli_args).expect("parse should succeed");
        assert!(cli.interactive.strict_config);
        cli.feature_toggles
            .to_overrides()
            .expect_err("feature should be rejected")
    }
}
