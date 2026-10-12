use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_tool_door_runtime::ToolProcessEnvironment;
use octopus_worker_env::{WorkerEnvKindSpec, WorkerEnvironmentFormation};
use serde::Deserialize;

const CONFIG_FILE: &str = "mcp_stdio.json";
const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 65_536;
const MAX_TIMEOUT_MS: u64 = 600_000;
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_STDERR_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STDERR_RETAINED_SEGMENTS: usize = 64;
const MAX_EVENT_JOURNAL_SEGMENT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EVENT_JOURNAL_RETAINED_SEGMENTS: usize = 1024;
const MAX_PROTOCOL_VERSIONS: usize = 16;
const RESERVED_ENV: &[&str] = &[
    "OCTOPUS_INTERFACE_ID",
    "OCTOPUS_INTERFACE_DIR",
    "OCTOPUS_VAR_DIR",
    "OCTOPUS_ROOT_SLOTS_JSON",
    "OCTOPUS_MOVEMENTS_JSONL",
    "OCTOPUS_BLOBS_DIR",
    "OCTOPUS_LEDGER_INDEX_SQLITE",
    "TMPDIR",
    "HOME",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpStdioConfig {
    schema_version: u32,
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    #[serde(default)]
    pub(crate) forward_env: BTreeSet<String>,
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) cwd_root_slot: Option<String>,
    pub(crate) protocol_versions: Vec<String>,
    pub(crate) client_name: String,
    pub(crate) client_version: String,
    #[serde(default = "default_request_timeout_ms")]
    pub(crate) request_timeout_ms: u64,
    #[serde(default = "default_max_message_bytes")]
    pub(crate) max_message_bytes: usize,
    pub(crate) max_stderr_segment_bytes: u64,
    pub(crate) stderr_retained_segments: usize,
    pub(crate) event_journal_segment_bytes: u64,
    pub(crate) event_journal_retained_segments: usize,
}

impl McpStdioConfig {
    pub(crate) fn load_formed(
        environment: &ToolProcessEnvironment,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<Self, String> {
        let path = environment.interface_dir.join(CONFIG_FILE);
        let mut config: Self =
            environment.load_formed_json_sidecar(formation, kind, CONFIG_FILE)?;
        config.validate(&path, environment)?;
        let search_path = std::env::var_os("PATH");
        let command = environment.resolve_program(&config.command, kind, search_path.as_deref())?;
        config.command = command
            .to_str()
            .ok_or("MCP stdio command path is not UTF-8")?
            .to_string();
        for argument in &mut config.args {
            if argument.starts_with("owner://") {
                *argument = environment
                    .resolve_package_file(argument, kind)?
                    .to_str()
                    .ok_or("MCP stdio argument path is not UTF-8")?
                    .to_string();
            }
        }
        Ok(config)
    }

    pub(crate) fn cwd(
        &self,
        environment: &ToolProcessEnvironment,
        room_root: &Path,
    ) -> Result<PathBuf, String> {
        match self.cwd_root_slot.as_deref() {
            Some(slot) => environment.root_slot(slot).map(Path::to_path_buf),
            None => Ok(room_root.to_path_buf()),
        }
    }

    fn validate(&self, path: &Path, environment: &ToolProcessEnvironment) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "MCP stdio room config '{}' has schema_version {}, expected 1",
                path.display(),
                self.schema_version
            ));
        }
        validate_text("command", &self.command)?;
        if self.args.len() > MAX_ARGUMENTS {
            return Err(format!(
                "MCP stdio room command has more than {MAX_ARGUMENTS} arguments"
            ));
        }
        let mut argument_bytes = 0usize;
        for (index, argument) in self.args.iter().enumerate() {
            if argument.contains('\0') {
                return Err(format!("MCP stdio room args[{index}] contains a NUL byte"));
            }
            argument_bytes = argument_bytes
                .checked_add(argument.len())
                .ok_or_else(|| "MCP stdio room argument bytes overflowed".to_string())?;
        }
        if argument_bytes > MAX_ARGUMENT_BYTES {
            return Err(format!(
                "MCP stdio room arguments exceed {MAX_ARGUMENT_BYTES} bytes"
            ));
        }
        if self.request_timeout_ms == 0 || self.request_timeout_ms > MAX_TIMEOUT_MS {
            return Err(format!(
                "MCP stdio room request_timeout_ms must be in 1..={MAX_TIMEOUT_MS}"
            ));
        }
        if self.protocol_versions.is_empty() || self.protocol_versions.len() > MAX_PROTOCOL_VERSIONS
        {
            return Err(format!(
                "MCP stdio room protocol_versions must contain 1..={MAX_PROTOCOL_VERSIONS} entries"
            ));
        }
        let mut protocols = BTreeSet::new();
        for protocol in &self.protocol_versions {
            validate_protocol_version(protocol)?;
            if !protocols.insert(protocol) {
                return Err(format!(
                    "MCP stdio room protocol_versions repeats '{protocol}'"
                ));
            }
        }
        validate_text("client_name", &self.client_name)?;
        validate_text("client_version", &self.client_version)?;
        if !(1024..=MAX_MESSAGE_BYTES).contains(&self.max_message_bytes) {
            return Err(format!(
                "MCP stdio room max_message_bytes must be in 1024..={MAX_MESSAGE_BYTES}"
            ));
        }
        if !(1024..=MAX_STDERR_SEGMENT_BYTES).contains(&self.max_stderr_segment_bytes) {
            return Err(format!(
                "MCP stdio room max_stderr_segment_bytes must be in 1024..={MAX_STDERR_SEGMENT_BYTES}"
            ));
        }
        if !(1..=MAX_STDERR_RETAINED_SEGMENTS).contains(&self.stderr_retained_segments) {
            return Err(format!(
                "MCP stdio room stderr_retained_segments must be in 1..={MAX_STDERR_RETAINED_SEGMENTS}"
            ));
        }
        if !(1024..=MAX_EVENT_JOURNAL_SEGMENT_BYTES).contains(&self.event_journal_segment_bytes) {
            return Err(format!(
                "MCP stdio room event_journal_segment_bytes must be in 1024..={MAX_EVENT_JOURNAL_SEGMENT_BYTES}"
            ));
        }
        if !(2..=MAX_EVENT_JOURNAL_RETAINED_SEGMENTS)
            .contains(&self.event_journal_retained_segments)
        {
            return Err(format!(
                "MCP stdio room event_journal_retained_segments must be in 2..={MAX_EVENT_JOURNAL_RETAINED_SEGMENTS}"
            ));
        }
        if let Some(slot) = self.cwd_root_slot.as_deref() {
            validate_name("cwd_root_slot", slot)?;
            environment.root_slot(slot)?;
        }
        for name in &self.forward_env {
            validate_env_name("forward_env", name)?;
            if std::env::var_os(name).is_none() {
                return Err(format!(
                    "MCP stdio room requires forwarded environment variable '{name}'"
                ));
            }
        }
        for (name, value) in &self.env {
            validate_env_name("env", name)?;
            if self.forward_env.contains(name) {
                return Err(format!(
                    "MCP stdio room environment variable '{name}' is both forwarded and literal"
                ));
            }
            if value.contains('\0') {
                return Err(format!(
                    "MCP stdio room literal environment variable '{name}' contains a NUL byte"
                ));
            }
        }
        let declared = declared_process_env_keys(&environment.interface_dir)?;
        if declared != self.forward_env {
            return Err(format!(
                "MCP stdio room forward_env {:?} does not match interface process_env_keys {:?}",
                self.forward_env, declared
            ));
        }
        Ok(())
    }
}

fn declared_process_env_keys(interface_dir: &Path) -> Result<BTreeSet<String>, String> {
    let path = interface_dir.join("interface.json");
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "MCP stdio interface descriptor '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    let descriptor: octopus_interfaces::InterfaceDescriptor =
        octopus_descriptors::parse_strict(&bytes).map_err(|error| {
            format!(
                "MCP stdio interface descriptor '{}' is malformed: {error}",
                path.display()
            )
        })?;
    let values = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("process_env_keys"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            format!(
                "MCP stdio interface descriptor '{}' has no process_env_keys array",
                path.display()
            )
        })?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let name = value.as_str().ok_or_else(|| {
                format!("MCP stdio interface process_env_keys[{index}] is not a string")
            })?;
            validate_env_name("process_env_keys", name)?;
            Ok(name.to_string())
        })
        .collect()
}

fn validate_text(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value || value.contains('\0') {
        return Err(format!("MCP stdio room {field} is empty or malformed"));
    }
    Ok(())
}

fn validate_name(field: &str, value: &str) -> Result<(), String> {
    validate_text(field, value)?;
    if value.contains('/') || value.contains('\\') || value.contains('=') {
        return Err(format!("MCP stdio room {field} '{value}' is malformed"));
    }
    Ok(())
}

fn validate_env_name(field: &str, value: &str) -> Result<(), String> {
    validate_name(field, value)?;
    if RESERVED_ENV.contains(&value) || value == "PATH" {
        return Err(format!(
            "MCP stdio room {field} contains reserved environment variable '{value}'"
        ));
    }
    Ok(())
}

fn validate_protocol_version(value: &str) -> Result<(), String> {
    validate_text("protocol_version", value)?;
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit())
    {
        return Err(format!(
            "MCP stdio room protocol version '{value}' must use YYYY-MM-DD"
        ));
    }
    Ok(())
}

fn default_request_timeout_ms() -> u64 {
    120_000
}

fn default_max_message_bytes() -> usize {
    8 * 1024 * 1024
}
