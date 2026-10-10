use std::time::Duration;

use octopus_exec::DirectExecServerAuth;
use octopus_substrate_contract::shell_env_config::ShellEnvironmentPolicyInherit;
use octopus_tool_door_runtime::ToolProcessEnvironment;
use octopus_util_pathuri::PathUri;
use octopus_worker_env::{WorkerEnvKindSpec, WorkerEnvironmentFormation};
use serde::{Deserialize, Serialize};

pub(crate) const CONFIG_FILE: &str = "remote_exec.json";
const MAX_REQUEST_TIMEOUT_MS: u64 = 600_000;
const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROCESSES: usize = 256;
const MAX_LIST_ENTRIES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteEnvInherit {
    Core,
    All,
    None,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteExecConfig {
    schema_version: u32,
    pub(crate) websocket_url: String,
    pub(crate) auth_secret_handle: String,
    workspace_uri: String,
    #[serde(default = "default_request_timeout_ms")]
    pub(crate) request_timeout_ms: u64,
    #[serde(default = "default_max_output_bytes")]
    pub(crate) max_output_bytes: usize,
    #[serde(default = "default_max_file_bytes")]
    pub(crate) max_file_bytes: usize,
    pub(crate) max_stdin_bytes: usize,
    pub(crate) max_processes: usize,
    pub(crate) max_list_entries: usize,
    pub(crate) env_inherit: RemoteEnvInherit,
    pub(crate) env_override_allowlist: Vec<String>,
}

impl RemoteExecConfig {
    pub(crate) fn load_formed(
        environment: &ToolProcessEnvironment,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<Self, String> {
        let config: Self = environment.load_formed_json_sidecar(formation, kind, CONFIG_FILE)?;
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn workspace(&self) -> Result<PathUri, String> {
        PathUri::parse(&self.workspace_uri)
            .map_err(|error| format!("remote exec workspace_uri is invalid: {error}"))
    }

    pub(crate) fn load_auth(
        &self,
        environment: &ToolProcessEnvironment,
    ) -> Result<DirectExecServerAuth, String> {
        let path = environment.private_read_path(&self.auth_secret_handle)?;
        let parent = path.parent().ok_or_else(|| {
            "remote exec authentication secret materialization has no parent".to_string()
        })?;
        let leaf = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                "remote exec authentication secret materialization has no UTF-8 leaf".to_string()
            })?;
        DirectExecServerAuth::load_beneath(parent, leaf).map_err(|error| {
            format!("remote exec authentication secret handle is unavailable: {error}")
        })
    }

    pub(crate) fn identity(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("remote exec config identity failed: {error}"))?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }

    pub(crate) fn resolve_path(&self, path: &str) -> Result<PathUri, String> {
        if path.contains('\0') {
            return Err("remote exec path contains a NUL byte".to_string());
        }
        let workspace = self.workspace()?;
        let candidate = workspace
            .join(path)
            .map_err(|error| format!("remote exec path '{path}' is invalid: {error}"))?;
        if !candidate.ancestors().any(|ancestor| ancestor == workspace) {
            return Err(format!(
                "remote exec path '{}' leaves configured workspace '{}'",
                candidate, workspace
            ));
        }
        Ok(candidate)
    }

    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }

    pub(crate) fn bounded_timeout(&self, requested_ms: Option<u64>) -> Result<Duration, String> {
        let timeout_ms = requested_ms.unwrap_or(self.request_timeout_ms);
        if timeout_ms == 0 || timeout_ms > self.request_timeout_ms {
            return Err(format!(
                "remote exec timeout_ms must be in 1..={}",
                self.request_timeout_ms
            ));
        }
        Ok(Duration::from_millis(timeout_ms))
    }

    pub(crate) fn env_inherit(&self) -> ShellEnvironmentPolicyInherit {
        match self.env_inherit {
            RemoteEnvInherit::Core => ShellEnvironmentPolicyInherit::Core,
            RemoteEnvInherit::All => ShellEnvironmentPolicyInherit::All,
            RemoteEnvInherit::None => ShellEnvironmentPolicyInherit::None,
        }
    }

    pub(crate) fn admits_env_override(&self, key: &str) -> bool {
        self.env_override_allowlist
            .iter()
            .any(|allowed| allowed == key)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "remote exec config has schema_version {}, expected 1",
                self.schema_version
            ));
        }
        if self.websocket_url.trim() != self.websocket_url || self.websocket_url.is_empty() {
            return Err("remote exec websocket_url is empty or padded".to_string());
        }
        validate_secret_handle(&self.auth_secret_handle)?;
        let url = url::Url::parse(&self.websocket_url)
            .map_err(|error| format!("remote exec websocket_url is invalid: {error}"))?;
        if !matches!(url.scheme(), "ws" | "wss") || url.host_str().is_none() {
            return Err(
                "remote exec websocket_url must be an absolute ws:// or wss:// URL".to_string(),
            );
        }
        if url.scheme() == "ws" && !websocket_host_is_loopback(&url) {
            return Err(
                "remote exec websocket_url must use wss:// outside the loopback host".to_string(),
            );
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "remote exec websocket_url must not contain credentials, query, or fragment"
                    .to_string(),
            );
        }
        let workspace = self.workspace()?;
        if workspace.parent().is_none() {
            return Err(
                "remote exec workspace_uri must identify a directory below a filesystem root"
                    .to_string(),
            );
        }
        if self.request_timeout_ms == 0 || self.request_timeout_ms > MAX_REQUEST_TIMEOUT_MS {
            return Err(format!(
                "remote exec request_timeout_ms must be in 1..={MAX_REQUEST_TIMEOUT_MS}"
            ));
        }
        validate_capture_limit("max_output_bytes", self.max_output_bytes)?;
        validate_capture_limit("max_file_bytes", self.max_file_bytes)?;
        validate_capture_limit("max_stdin_bytes", self.max_stdin_bytes)?;
        if self.max_processes == 0 || self.max_processes > MAX_PROCESSES {
            return Err(format!(
                "remote exec max_processes must be in 1..={MAX_PROCESSES}"
            ));
        }
        if self.max_list_entries == 0 || self.max_list_entries > MAX_LIST_ENTRIES {
            return Err(format!(
                "remote exec max_list_entries must be in 1..={MAX_LIST_ENTRIES}"
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for key in &self.env_override_allowlist {
            if !valid_env_key(key) {
                return Err(format!(
                    "remote exec env_override_allowlist contains invalid key '{key}'"
                ));
            }
            if !seen.insert(key) {
                return Err(format!(
                    "remote exec env_override_allowlist repeats key '{key}'"
                ));
            }
        }
        Ok(())
    }
}

fn validate_secret_handle(handle: &str) -> Result<(), String> {
    if handle.trim() != handle || handle.is_empty() {
        return Err("remote exec auth_secret_handle is empty or padded".to_string());
    }
    let path = std::path::Path::new(handle);
    if path.is_absolute()
        || path.components().any(|component| {
            !matches!(component, std::path::Component::Normal(value) if !value.is_empty())
        })
    {
        return Err(
            "remote exec auth_secret_handle must be a relative path of normal components"
                .to_string(),
        );
    }
    Ok(())
}

fn websocket_host_is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

fn valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z' | 'a'..='z'))
        && chars.all(|character| matches!(character, '_' | 'A'..='Z' | 'a'..='z' | '0'..='9'))
}

fn validate_capture_limit(field: &str, value: usize) -> Result<(), String> {
    if !(1024..=MAX_CAPTURE_BYTES).contains(&value) {
        return Err(format!(
            "remote exec {field} must be in 1024..={MAX_CAPTURE_BYTES}"
        ));
    }
    Ok(())
}

fn default_request_timeout_ms() -> u64 {
    120_000
}

fn default_max_output_bytes() -> usize {
    8 * 1024 * 1024
}

fn default_max_file_bytes() -> usize {
    8 * 1024 * 1024
}
