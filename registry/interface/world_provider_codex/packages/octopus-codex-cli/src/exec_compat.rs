use crate::cli_args::CliConfigOverrides;
use crate::cli_args::SharedCliOptions;
use clap::Args;
use clap::FromArgMatches;
use clap::Parser;
use clap::ValueEnum;
use octopus_arg0::Arg0DispatchPaths;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    version,
    override_usage = "octopus exec [OPTIONS] [PROMPT]\n       octopus exec [OPTIONS] <COMMAND> [ARGS]"
)]
pub struct Cli {
    #[doc = " Action to perform. If omitted, runs a new non-interactive session."]
    #[command(subcommand)]
    pub command: Option<Command>,

    #[doc = " Error out when config.toml contains fields that are not recognized by this version."]
    #[arg(long = "strict-config", global = true, default_value_t = false)]
    pub strict_config: bool,

    #[clap(flatten)]
    pub shared: ExecSharedCliOptions,

    #[doc = " Allow running outside a Git repository."]
    #[arg(long = "skip-git-repo-check", global = true, default_value_t = false)]
    pub skip_git_repo_check: bool,

    #[doc = " Run without persisting session files to disk."]
    #[arg(long = "ephemeral", global = true, default_value_t = false)]
    pub ephemeral: bool,

    #[doc = " Do not load `$CODEX_HOME/config.toml`; auth still uses `CODEX_HOME`."]
    #[arg(long = "ignore-user-config", global = true, default_value_t = false)]
    pub ignore_user_config: bool,

    #[doc = " Do not load user or project execpolicy `.rules` files."]
    #[arg(long = "ignore-rules", global = true, default_value_t = false)]
    pub ignore_rules: bool,

    #[doc = " Legacy compatibility trap for the removed `--full-auto` flag."]
    #[arg(
        long = "full-auto",
        hide = true,
        global = true,
        default_value_t = false,
        conflicts_with = "dangerously_bypass_approvals_and_sandbox"
    )]
    pub removed_full_auto: bool,

    #[doc = " Path to a JSON Schema file describing the model's final response shape."]
    #[arg(long = "output-schema", value_name = "FILE", global = true)]
    pub output_schema: Option<PathBuf>,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[doc = " Specifies color settings for use in the output."]
    #[arg(long = "color", value_enum, default_value_t = Color::Auto)]
    pub color: Color,

    #[doc = " Print events to stdout as JSONL."]
    #[arg(
        long = "json",
        alias = "experimental-json",
        default_value_t = false,
        global = true
    )]
    pub json: bool,

    #[doc = " Specifies file where the last message from the agent should be written."]
    #[arg(
        long = "output-last-message",
        short = 'o',
        value_name = "FILE",
        global = true
    )]
    pub last_message_file: Option<PathBuf>,

    #[doc = " Initial instructions for the agent. If not provided as an argument (or"]
    #[doc = " if `-` is used), instructions are read from stdin. If stdin is piped and"]
    #[doc = " a prompt is also provided, stdin is appended as a `<stdin>` block."]
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,
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

impl Cli {
    pub fn removed_full_auto_warning(&self) -> Option<&'static str> {
        if self.removed_full_auto {
            return Some(
                "warning: `--full-auto` is deprecated; use `--sandbox workspace-write` instead.",
            );
        }

        None
    }
}

#[derive(Debug, Default)]
pub struct ExecSharedCliOptions(SharedCliOptions);

impl ExecSharedCliOptions {
    pub fn into_inner(self) -> SharedCliOptions {
        self.0
    }
}

impl std::ops::Deref for ExecSharedCliOptions {
    type Target = SharedCliOptions;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ExecSharedCliOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Args for ExecSharedCliOptions {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        mark_exec_global_args(SharedCliOptions::augment_args(cmd))
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        mark_exec_global_args(SharedCliOptions::augment_args_for_update(cmd))
    }
}

impl FromArgMatches for ExecSharedCliOptions {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        SharedCliOptions::from_arg_matches(matches).map(Self)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.0.update_from_arg_matches(matches)
    }
}

fn mark_exec_global_args(cmd: clap::Command) -> clap::Command {
    cmd.mut_arg("model", |arg| arg.global(true))
        .mut_arg("dangerously_bypass_approvals_and_sandbox", |arg| {
            arg.global(true)
        })
        .mut_arg("bypass_hook_trust", |arg| arg.global(true))
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    #[doc = " Resume a previous session by id or pick the most recent with --last."]
    Resume(ResumeArgs),

    #[doc = " Run a code review against the current repository."]
    Review(ReviewArgs),
}

#[derive(Args, Debug)]
struct ResumeArgsRaw {
    #[doc = " Conversation/session id (UUID) or thread name. UUIDs take precedence if it parses."]
    #[doc = " If omitted, use --last to pick the most recent recorded session."]
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    #[doc = " Resume the most recent recorded session (newest) without specifying an id."]
    #[arg(long = "last", default_value_t = false)]
    last: bool,

    #[doc = " Show all sessions (disables cwd filtering)."]
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    #[doc = " Optional image(s) to attach to the prompt sent after resuming."]
    #[arg(
        long = "image",
        short = 'i',
        value_name = "FILE",
        value_delimiter = ',',
        num_args = 1
    )]
    images: Vec<PathBuf>,

    #[doc = " Prompt to send after resuming the session. If `-` is used, read from stdin."]
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    prompt: Option<String>,
}

#[derive(Debug)]
pub struct ResumeArgs {
    #[doc = " Conversation/session id (UUID) or thread name. UUIDs take precedence if it parses."]
    #[doc = " If omitted, use --last to pick the most recent recorded session."]
    pub session_id: Option<String>,

    #[doc = " Resume the most recent recorded session (newest) without specifying an id."]
    pub last: bool,

    #[doc = " Show all sessions (disables cwd filtering)."]
    pub all: bool,

    #[doc = " Optional image(s) to attach to the prompt sent after resuming."]
    pub images: Vec<PathBuf>,

    #[doc = " Prompt to send after resuming the session. If `-` is used, read from stdin."]
    pub prompt: Option<String>,
}

impl From<ResumeArgsRaw> for ResumeArgs {
    fn from(raw: ResumeArgsRaw) -> Self {
        let (session_id, prompt) = if raw.last && raw.prompt.is_none() {
            (None, raw.session_id)
        } else {
            (raw.session_id, raw.prompt)
        };
        Self {
            session_id,
            last: raw.last,
            all: raw.all,
            images: raw.images,
            prompt,
        }
    }
}

impl Args for ResumeArgs {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        ResumeArgsRaw::augment_args(cmd)
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        ResumeArgsRaw::augment_args_for_update(cmd)
    }
}

impl FromArgMatches for ResumeArgs {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        ResumeArgsRaw::from_arg_matches(matches).map(Self::from)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = ResumeArgsRaw::from_arg_matches(matches).map(Self::from)?;
        Ok(())
    }
}

#[derive(Args, Debug)]
pub struct ReviewArgs {
    #[doc = " Review staged, unstaged, and untracked changes."]
    #[arg(
        long = "uncommitted",
        default_value_t = false,
        conflicts_with_all = ["base", "commit", "prompt"]
    )]
    pub uncommitted: bool,

    #[doc = " Review changes against the given base branch."]
    #[arg(
        long = "base",
        value_name = "BRANCH",
        conflicts_with_all = ["uncommitted", "commit", "prompt"]
    )]
    pub base: Option<String>,

    #[doc = " Review the changes introduced by a commit."]
    #[arg(
        long = "commit",
        value_name = "SHA",
        conflicts_with_all = ["uncommitted", "base", "prompt"]
    )]
    pub commit: Option<String>,

    #[doc = " Optional commit title to display in the review summary."]
    #[arg(long = "title", value_name = "TITLE", requires = "commit")]
    pub commit_title: Option<String>,

    #[doc = " Custom review instructions. If `-` is used, read from stdin."]
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Color {
    Always,
    Never,
    #[default]
    Auto,
}

pub async fn run_main(_cli: Cli, _arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    anyhow::bail!(
        "non-interactive `exec`/`review` is unavailable: this operation requires the \
         artifact-native terminal interface"
    )
}
