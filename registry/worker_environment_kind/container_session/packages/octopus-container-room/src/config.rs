use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use octopus_tool_door_runtime::ToolProcessEnvironment;
use octopus_worker_env::{WorkerEnvKindSpec, WorkerEnvironmentFormation};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PullPolicy {
    Never,
    IfMissing,
    Always,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerNetwork {
    None,
    Bridge,
    Host,
    Custom,
}

impl ContainerNetwork {
    pub fn as_engine_value(self, custom_name: &Option<String>) -> Result<&str, String> {
        match self {
            Self::None => Ok("none"),
            Self::Bridge => Ok("bridge"),
            Self::Host => Ok("host"),
            Self::Custom => custom_name
                .as_deref()
                .ok_or_else(|| "container custom network requires network_name".to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPolicy {
    Refuse,
    Restart,
    RestartOrRecreate,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadyCondition {
    Running,
    Healthy,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MountSourceType {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MountAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerMount {
    pub root_slot: String,
    pub target: String,
    pub access: MountAccess,
    pub source_type: MountSourceType,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerPortBinding {
    pub container_port: u16,
    pub host_port: u16,
    pub host_ip: String,
    pub protocol: ContainerPortProtocol,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContainerPortProtocol {
    Tcp,
    Udp,
}

impl ContainerPortProtocol {
    pub fn as_engine_value(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerConfig {
    pub schema_version: u32,
    pub engine: String,
    pub image: String,
    pub pull_policy: PullPolicy,
    pub name_prefix: String,
    pub workdir: String,
    pub start_argv: Vec<String>,
    pub network: ContainerNetwork,
    pub network_name: Option<String>,
    pub publish_ports: Vec<ContainerPortBinding>,
    pub dns: Vec<String>,
    pub hostname: Option<String>,
    pub init: bool,
    pub read_only_root: bool,
    pub user: Option<String>,
    pub cpu_limit: f64,
    pub memory_bytes: u64,
    pub storage_bytes: Option<u64>,
    pub shm_bytes: u64,
    pub pids_limit: u64,
    pub startup_timeout_ms: u64,
    pub ready_condition: ReadyCondition,
    pub ready_timeout_ms: u64,
    pub ready_poll_ms: u64,
    pub exec_timeout_ms: u64,
    pub max_output_bytes: usize,
    pub max_file_bytes: usize,
    pub max_directory_entries: usize,
    pub inline_text_preview_bytes: usize,
    pub reuse_stopped: bool,
    pub recovery_policy: RecoveryPolicy,
    pub env: BTreeMap<String, String>,
    pub forward_env: Vec<String>,
    pub labels: BTreeMap<String, String>,
    pub mounts: Vec<ContainerMount>,
    pub readable_paths: Vec<String>,
    pub writable_paths: Vec<String>,
}

impl ContainerConfig {
    pub fn load_formed(
        environment: &ToolProcessEnvironment,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<Self, String> {
        let config: Self =
            environment.load_formed_json_sidecar(formation, kind, "container.json")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 2 {
            return Err(format!(
                "container config schema_version must be 2, got {}",
                self.schema_version
            ));
        }
        validate_program("engine", &self.engine)?;
        validate_atom("image", &self.image, 512)?;
        validate_name_prefix(&self.name_prefix)?;
        normalize_container_path(&self.workdir)?;
        if self.start_argv.is_empty() || self.start_argv.len() > 256 {
            return Err("container start_argv must contain between 1 and 256 entries".to_string());
        }
        for (index, value) in self.start_argv.iter().enumerate() {
            validate_atom(&format!("start_argv[{index}]"), value, 65_536)?;
        }
        if let Some(user) = self.user.as_deref() {
            validate_atom("user", user, 256)?;
        }
        match self.network {
            ContainerNetwork::Custom => {
                validate_network_name(self.network_name.as_deref().ok_or_else(|| {
                    "container custom network requires network_name".to_string()
                })?)?;
            }
            _ if self.network_name.is_some() => {
                return Err("container network_name is only valid for custom network".to_string());
            }
            _ => {}
        }
        if matches!(
            self.network,
            ContainerNetwork::None | ContainerNetwork::Host
        ) && !self.publish_ports.is_empty()
        {
            return Err(
                "container published ports require bridge or custom networking".to_string(),
            );
        }
        let mut published = BTreeSet::new();
        for (index, binding) in self.publish_ports.iter().enumerate() {
            if binding.container_port == 0 || binding.host_port == 0 {
                return Err(format!(
                    "publish_ports[{index}] ports must be between 1 and 65535"
                ));
            }
            binding
                .host_ip
                .parse::<std::net::IpAddr>()
                .map_err(|_| format!("publish_ports[{index}].host_ip must be an IP address"))?;
            if !published.insert((
                binding.host_ip.as_str(),
                binding.host_port,
                binding.protocol,
            )) {
                return Err(format!("publish_ports[{index}] duplicates a host endpoint"));
            }
        }
        if self.dns.len() > 16 {
            return Err("container dns exceeds 16 entries".to_string());
        }
        for (index, server) in self.dns.iter().enumerate() {
            server
                .parse::<std::net::IpAddr>()
                .map_err(|_| format!("dns[{index}] must be an IP address"))?;
        }
        if let Some(hostname) = self.hostname.as_deref() {
            validate_hostname(hostname)?;
        }
        if !self.cpu_limit.is_finite() || !(0.1..=256.0).contains(&self.cpu_limit) {
            return Err("container cpu_limit must be finite and between 0.1 and 256".to_string());
        }
        if !(16 * 1024 * 1024..=1024 * 1024 * 1024 * 1024).contains(&self.memory_bytes) {
            return Err(
                "container memory_bytes must be between 16777216 and 1099511627776".to_string(),
            );
        }
        if let Some(storage_bytes) = self.storage_bytes {
            if !(16 * 1024 * 1024..=16 * 1024 * 1024 * 1024 * 1024_u64).contains(&storage_bytes) {
                return Err(
                    "container storage_bytes must be between 16777216 and 17592186044416"
                        .to_string(),
                );
            }
        }
        if !(64 * 1024..=64 * 1024 * 1024 * 1024_u64).contains(&self.shm_bytes) {
            return Err("container shm_bytes must be between 65536 and 68719476736".to_string());
        }
        if !(1..=1_048_576).contains(&self.pids_limit) {
            return Err("container pids_limit must be between 1 and 1048576".to_string());
        }
        if !(1_000..=1_800_000).contains(&self.startup_timeout_ms) {
            return Err(
                "container startup_timeout_ms must be between 1000 and 1800000".to_string(),
            );
        }
        if !(1_000..=1_800_000).contains(&self.ready_timeout_ms) {
            return Err("container ready_timeout_ms must be between 1000 and 1800000".to_string());
        }
        if !(10..=10_000).contains(&self.ready_poll_ms) {
            return Err("container ready_poll_ms must be between 10 and 10000".to_string());
        }
        if !(1..=600_000).contains(&self.exec_timeout_ms) {
            return Err("container exec_timeout_ms must be between 1 and 600000".to_string());
        }
        if !(1..=64 * 1024 * 1024).contains(&self.max_output_bytes) {
            return Err("container max_output_bytes must be between 1 and 67108864".to_string());
        }
        if !(1..=64 * 1024 * 1024).contains(&self.max_file_bytes) {
            return Err("container max_file_bytes must be between 1 and 67108864".to_string());
        }
        if !(1..=16_384).contains(&self.max_directory_entries) {
            return Err("container max_directory_entries must be between 1 and 16384".to_string());
        }
        if self.inline_text_preview_bytes > self.max_output_bytes
            || self.inline_text_preview_bytes > self.max_file_bytes
        {
            return Err(
                "container inline_text_preview_bytes must not exceed output or file bounds"
                    .to_string(),
            );
        }
        validate_environment(&self.env, "env")?;
        let mut forwarded = BTreeSet::new();
        for (index, name) in self.forward_env.iter().enumerate() {
            validate_env_name(name).map_err(|error| format!("forward_env[{index}] {error}"))?;
            if !forwarded.insert(name) {
                return Err(format!("container forward_env contains duplicate '{name}'"));
            }
            if self.env.contains_key(name) {
                return Err(format!(
                    "container environment '{name}' is both literal and forwarded"
                ));
            }
        }
        for (key, value) in &self.labels {
            validate_label(key, value)?;
        }
        let readable = normalize_roots("readable_paths", &self.readable_paths)?;
        let writable = normalize_roots("writable_paths", &self.writable_paths)?;
        let normalized_workdir = normalize_container_path(&self.workdir)?;
        if !readable
            .iter()
            .any(|root| container_path_within(&normalized_workdir, root))
        {
            return Err("container workdir is outside readable_paths".to_string());
        }
        for root in &writable {
            if !readable
                .iter()
                .any(|readable_root| container_path_within(root, readable_root))
            {
                return Err(format!(
                    "container writable path '{root}' is outside readable_paths"
                ));
            }
        }
        let mut mount_targets = BTreeSet::new();
        for (index, mount) in self.mounts.iter().enumerate() {
            validate_slot(&mount.root_slot)
                .map_err(|error| format!("mounts[{index}].root_slot {error}"))?;
            let target = normalize_container_path(&mount.target)
                .map_err(|error| format!("mounts[{index}].target {error}"))?;
            if target == "/" || target == "/var/run/docker.sock" || target == "/run/docker.sock" {
                return Err(format!("container mount target '{target}' is forbidden"));
            }
            if !mount_targets.insert(target.clone()) {
                return Err(format!("container mounts duplicate target '{target}'"));
            }
            let allowed = match mount.access {
                MountAccess::ReadOnly => readable
                    .iter()
                    .any(|root| container_path_within(&target, root)),
                MountAccess::ReadWrite => writable
                    .iter()
                    .any(|root| container_path_within(&target, root)),
            };
            if !allowed {
                return Err(format!(
                    "container mount target '{target}' is outside its configured access roots"
                ));
            }
            if mount.access == MountAccess::ReadOnly
                && writable
                    .iter()
                    .any(|root| container_path_within(&target, root))
            {
                return Err(format!(
                    "container read-only mount target '{target}' sits inside a writable path"
                ));
            }
            if mount.source_type == MountSourceType::File
                && container_path_within(&normalized_workdir, &target)
            {
                return Err(format!(
                    "container file mount target '{target}' cannot contain the workdir"
                ));
            }
        }
        Ok(())
    }

    pub fn normalized_workdir(&self) -> Result<String, String> {
        normalize_container_path(&self.workdir)
    }

    pub fn network_engine_value(&self) -> Result<&str, String> {
        self.network.as_engine_value(&self.network_name)
    }

    pub fn path_for_read(&self, value: &str) -> Result<String, String> {
        self.path_under(value, &self.readable_paths, "readable")
    }

    pub fn path_for_write(&self, value: &str) -> Result<String, String> {
        self.path_under(value, &self.writable_paths, "writable")
    }

    fn path_under(&self, value: &str, roots: &[String], access: &str) -> Result<String, String> {
        let path = normalize_container_path(value)?;
        let normalized = roots
            .iter()
            .map(|root| normalize_container_path(root))
            .collect::<Result<Vec<_>, _>>()?;
        if normalized
            .iter()
            .any(|root| container_path_within(&path, root))
        {
            Ok(path)
        } else {
            Err(format!(
                "container path '{path}' is outside configured {access} paths"
            ))
        }
    }
}

fn validate_program(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value, 4096)?;
    if value.chars().any(char::is_whitespace) {
        return Err(format!(
            "container {field} must name one executable without arguments"
        ));
    }
    if value.contains('/') && !Path::new(value).is_absolute() {
        return Err(format!("container {field} path must be absolute"));
    }
    Ok(())
}

fn validate_atom(field: &str, value: &str, maximum: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(format!(
            "container {field} must be non-empty, at most {maximum} bytes, and contain no null"
        ));
    }
    Ok(())
}

fn validate_name_prefix(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 24
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || value.starts_with('-')
        || value.ends_with('-')
    {
        return Err(
            "container name_prefix must be 1..24 lowercase alphanumeric/hyphen characters"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_network_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err("container network_name is not a safe engine network name".to_string());
    }
    Ok(())
}

fn validate_hostname(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 253
        || value.starts_with('.')
        || value.ends_with('.')
        || value.split('.').any(|part| {
            part.is_empty()
                || part.len() > 63
                || part.starts_with('-')
                || part.ends_with('-')
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err("container hostname is invalid".to_string());
    }
    Ok(())
}

fn validate_environment(values: &BTreeMap<String, String>, field: &str) -> Result<(), String> {
    if values.len() > 256 {
        return Err(format!("container {field} exceeds 256 entries"));
    }
    for (name, value) in values {
        validate_env_name(name).map_err(|error| format!("{field}.{name} {error}"))?;
        if value.len() > 1024 * 1024 || value.contains('\0') {
            return Err(format!(
                "container {field}.{name} exceeds 1048576 bytes or contains null"
            ));
        }
    }
    Ok(())
}

pub fn validate_env_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let first = bytes
        .next()
        .ok_or_else(|| "must be a non-empty environment name".to_string())?;
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err("must match [A-Za-z_][A-Za-z0-9_]*".to_string());
    }
    Ok(())
}

fn validate_label(key: &str, value: &str) -> Result<(), String> {
    validate_atom("label key", key, 128)?;
    if !key
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
    {
        return Err(format!("container label '{key}' has an invalid key"));
    }
    if key.starts_with("octopus.") {
        return Err(format!(
            "container label '{key}' uses reserved octopus namespace"
        ));
    }
    if value.len() > 4096 || value.contains('\0') || value.contains('\n') {
        return Err(format!("container label '{key}' has an invalid value"));
    }
    Ok(())
}

fn validate_slot(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("must be a non-empty root-slot identifier".to_string());
    }
    Ok(())
}

fn normalize_roots(field: &str, values: &[String]) -> Result<Vec<String>, String> {
    if values.is_empty() || values.len() > 64 {
        return Err(format!(
            "container {field} must contain between 1 and 64 paths"
        ));
    }
    let mut unique = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let normalized =
            normalize_container_path(value).map_err(|error| format!("{field}[{index}] {error}"))?;
        if !unique.insert(normalized.clone()) {
            return Err(format!("container {field} duplicates '{normalized}'"));
        }
    }
    Ok(unique.into_iter().collect())
}

pub fn normalize_container_path(value: &str) -> Result<String, String> {
    validate_atom("path", value, 4096)?;
    // A path inside the Linux container: POSIX on every host, so it is parsed
    // as text rather than with the host's path rules (Windows would call
    // `/srv/room` relative).
    let Some(rest) = value.strip_prefix('/') else {
        return Err("container path must be absolute".to_string());
    };
    let mut parts = Vec::new();
    for part in rest.split('/') {
        // Like `Path::components`: repeated or trailing slashes and `.` are
        // skipped, `..` is refused.
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err("container path contains a non-normal component".to_string());
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Ok("/".to_string());
    }
    Ok(format!("/{}", parts.join("/")))
}

pub fn container_path_within(path: &str, root: &str) -> bool {
    root == "/"
        || path == root
        || path
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ContainerConfig {
        serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "engine": "docker",
            "image": "ubuntu:24.04",
            "pull_policy": "if_missing",
            "name_prefix": "octopus",
            "workdir": "/workspace",
            "start_argv": ["sleep", "infinity"],
            "network": "bridge",
            "network_name": null,
            "publish_ports": [],
            "dns": [],
            "hostname": null,
            "init": true,
            "read_only_root": false,
            "user": null,
            "cpu_limit": 2.0,
            "memory_bytes": 4294967296_u64,
            "storage_bytes": null,
            "shm_bytes": 67108864,
            "pids_limit": 512,
            "startup_timeout_ms": 300000,
            "ready_condition": "running",
            "ready_timeout_ms": 300000,
            "ready_poll_ms": 250,
            "exec_timeout_ms": 120000,
            "max_output_bytes": 8388608,
            "max_file_bytes": 16777216,
            "max_directory_entries": 4096,
            "inline_text_preview_bytes": 65536,
            "reuse_stopped": true,
            "recovery_policy": "restart_or_recreate",
            "env": {},
            "forward_env": [],
            "labels": {},
            "mounts": [],
            "readable_paths": ["/workspace", "/tmp"],
            "writable_paths": ["/workspace", "/tmp"]
        }))
        .unwrap()
    }

    #[test]
    fn validates_strict_container_contract() {
        config().validate().unwrap();
        assert_eq!(
            config().path_for_write("/workspace/a/b").unwrap(),
            "/workspace/a/b"
        );
        assert!(config().path_for_read("/etc/passwd").is_err());
        assert!(normalize_container_path("/workspace/../etc").is_err());
    }

    #[test]
    fn network_and_mount_configuration_fail_closed() {
        let mut custom = config();
        custom.network = ContainerNetwork::Custom;
        assert!(custom.validate().is_err());
        custom.network_name = Some("private-build-net".to_string());
        custom.validate().unwrap();

        let mut host = config();
        host.network = ContainerNetwork::Host;
        host.publish_ports.push(ContainerPortBinding {
            container_port: 8080,
            host_port: 8080,
            host_ip: "127.0.0.1".to_string(),
            protocol: ContainerPortProtocol::Tcp,
        });
        assert!(host.validate().is_err());
    }
}
