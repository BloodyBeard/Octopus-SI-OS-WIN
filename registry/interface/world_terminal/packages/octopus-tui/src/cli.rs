use clap::Args;
use clap::FromArgMatches;
use clap::Parser;
use octopus_utils_cli::ApprovalModeCliArg;
use octopus_utils_cli::CliConfigOverrides;
use octopus_utils_cli::SharedCliOptions;

#[derive(Parser, Clone, Debug)]
#[command(version)]
pub struct Cli {
    #[doc = " Optional user prompt to start the session."]
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,

    #[doc = " Error out when config.toml contains fields that are not recognized by this version of Codex."]
    #[arg(long = "strict-config", default_value_t = false)]
    pub strict_config: bool,

    #[clap(skip)]
    pub resume_picker: bool,

    #[clap(skip)]
    pub resume_last: bool,

    #[doc = " Internal: resume a specific recorded run by id. Set by the"]
    #[doc = " top-level `octopus resume <RUN_ID>` wrapper; not exposed as a public flag."]
    #[clap(skip)]
    pub resume_run_id: Option<String>,

    #[doc = " Internal: show all sessions (disables cwd filtering and shows CWD column)."]
    #[clap(skip)]
    pub resume_show_all: bool,

    #[doc = " Internal: include non-interactive sessions in resume listings."]
    #[clap(skip)]
    pub resume_include_non_interactive: bool,

    #[clap(skip)]
    pub fork_picker: bool,

    #[clap(skip)]
    pub fork_last: bool,

    #[doc = " Internal: fork a specific recorded run by id. Set by the"]
    #[doc = " top-level `octopus fork <RUN_ID>` wrapper; not exposed as a public flag."]
    #[clap(skip)]
    pub fork_run_id: Option<String>,

    #[doc = " Internal: show all sessions (disables cwd filtering and shows CWD column)."]
    #[clap(skip)]
    pub fork_show_all: bool,

    #[clap(flatten)]
    pub shared: TuiSharedCliOptions,

    #[doc = " Configure when the model requires human approval before executing a command."]
    #[arg(long = "ask-for-approval", short = 'a')]
    pub approval_policy: Option<ApprovalModeCliArg>,

    #[doc = " Disable alternate screen mode"]
    #[doc = ""]
    #[doc = " Runs the TUI in inline mode, preserving terminal scrollback history."]
    #[arg(long = "no-alt-screen", default_value_t = false)]
    pub no_alt_screen: bool,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,
}

impl std::ops::Deref for Cli {
    type Target = SharedCliOptions;

    fn deref(&self) -> &Self::Target {
        &self.shared.0
    }
}

impl std::ops::DerefMut for Cli {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.shared.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct TuiSharedCliOptions(SharedCliOptions);

impl TuiSharedCliOptions {
    pub fn into_inner(self) -> SharedCliOptions {
        self.0
    }
}

impl std::ops::Deref for TuiSharedCliOptions {
    type Target = SharedCliOptions;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for TuiSharedCliOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Args for TuiSharedCliOptions {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        mark_tui_args(SharedCliOptions::augment_args(cmd))
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        mark_tui_args(SharedCliOptions::augment_args_for_update(cmd))
    }
}

impl FromArgMatches for TuiSharedCliOptions {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        SharedCliOptions::from_arg_matches(matches).map(Self)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.0.update_from_arg_matches(matches)
    }
}

fn mark_tui_args(cmd: clap::Command) -> clap::Command {
    cmd.mut_arg("dangerously_bypass_approvals_and_sandbox", |arg| {
        arg.conflicts_with("approval_policy")
    })
}
