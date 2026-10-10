use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub(crate) type WorkspaceCommandRunner = Arc<dyn WorkspaceCommandExecutor>;

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceCommand {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) env: HashMap<String, Option<String>>,
    pub(crate) timeout: Duration,
    pub(crate) output_bytes_cap: usize,
    pub(crate) disable_output_cap: bool,
}

impl WorkspaceCommand {
    pub(crate) fn new(argv: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            cwd: None,
            env: HashMap::new(),
            timeout: Duration::from_secs(5),
            output_bytes_cap: 64 * 1024,
            disable_output_cap: false,
        }
    }

    pub(crate) fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub(crate) fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), Some(value.into()));
        self
    }

    pub(crate) fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub(crate) fn disable_output_cap(mut self) -> Self {
        self.disable_output_cap = true;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceCommandOutput {
    pub(crate) exit_code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl WorkspaceCommandOutput {
    pub(crate) fn success(&self) -> bool {
        self.exit_code == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceCommandError {
    message: String,
}

impl WorkspaceCommandError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WorkspaceCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkspaceCommandError {}

pub(crate) trait WorkspaceCommandExecutor: Send + Sync {
    fn run(
        &self,
        command: WorkspaceCommand,
    ) -> Pin<
        Box<dyn Future<Output = Result<WorkspaceCommandOutput, WorkspaceCommandError>> + Send + '_>,
    >;
}

#[derive(Clone)]
pub(crate) struct AppServerWorkspaceCommandRunner {}

impl AppServerWorkspaceCommandRunner {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl WorkspaceCommandExecutor for AppServerWorkspaceCommandRunner {
    fn run(
        &self,
        command: WorkspaceCommand,
    ) -> Pin<
        Box<dyn Future<Output = Result<WorkspaceCommandOutput, WorkspaceCommandError>> + Send + '_>,
    > {
        Box::pin(async move {
            let WorkspaceCommand {
                argv,
                cwd,
                env,
                timeout,
                output_bytes_cap,
                disable_output_cap,
            } = command;
            let _ = (
                argv,
                cwd,
                env,
                timeout,
                output_bytes_cap,
                disable_output_cap,
            );
            Err(WorkspaceCommandError::new(
                crate::unavailable_interface_action::message("workspace command execution"),
            ))
        })
    }
}
