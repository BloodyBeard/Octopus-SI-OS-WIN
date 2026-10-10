#[cfg(test)]
use octopus_protocol::config_types::EnvironmentVariablePattern;
use octopus_protocol::config_types::ShellEnvironmentPolicy;
use octopus_protocol::shell_environment;
use std::collections::HashMap;

pub use octopus_protocol::shell_environment::CODEX_THREAD_ID_ENV_VAR;

pub fn create_env(
    policy: &ShellEnvironmentPolicy,
    thread_id: Option<&str>,
) -> HashMap<String, String> {
    shell_environment::create_env(policy, thread_id)
}

#[cfg(all(test, target_os = "windows"))]
fn create_env_from_vars<I>(
    vars: I,
    policy: &ShellEnvironmentPolicy,
    thread_id: Option<&str>,
) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    shell_environment::create_env_from_vars(vars, policy, thread_id)
}

#[cfg(test)]
fn populate_env<I>(
    vars: I,
    policy: &ShellEnvironmentPolicy,
    thread_id: Option<&str>,
) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    shell_environment::populate_env(vars, policy, thread_id)
}

#[cfg(test)]
#[path = "exec_env_tests.rs"]
mod tests;
