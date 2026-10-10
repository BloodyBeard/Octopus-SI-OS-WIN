use std::collections::BTreeSet;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use octopus_descriptors::{Descriptor, ExecutorKind, RootAccess, RootRef, WorkerEnvironmentKindId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerConsent {
    ExplicitCeiling,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerEnvKindSpec {
    pub worker_environment_kind_id: WorkerEnvironmentKindId,
    pub substrate_family: String,
    pub door_driver_refs: Vec<String>,
    pub tool_process_bins: Vec<String>,
    pub worker_consent: WorkerConsent,
    #[serde(default)]
    pub instance_config_files: Vec<String>,
    pub supported_executor_kinds: Vec<ExecutorKind>,
    pub capabilities: Vec<EnvCapabilityTag>,
    pub boundary_defaults: BoundaryDefaults,
    pub lifecycle: WekLifecycle,
}

impl Descriptor for WorkerEnvKindSpec {
    const KIND: &'static str = "worker_environment_kind";
    const SCHEMA_VERSION: u32 = 4;
    const CONTRACT_VERSION: u32 = 4;

    fn declared_name(&self) -> Option<&str> {
        Some(self.worker_environment_kind_id.as_str())
    }

    fn validate(&self) -> Result<(), String> {
        validate_identifier(
            "worker_environment_kind_id",
            self.worker_environment_kind_id.as_str(),
        )?;
        validate_identifier("substrate_family", &self.substrate_family)?;
        validate_unique_strings(
            self.worker_environment_kind_id.as_str(),
            "door_driver_refs",
            self.door_driver_refs.iter().map(String::as_str),
            true,
        )?;
        validate_unique_process_bins(
            self.worker_environment_kind_id.as_str(),
            &self.tool_process_bins,
        )?;
        validate_unique_strings(
            self.worker_environment_kind_id.as_str(),
            "capabilities",
            self.capabilities
                .iter()
                .map(|capability| capability.0.as_str()),
            true,
        )?;
        if self.supported_executor_kinds.is_empty() {
            return Err(format!(
                "worker_environment_kind '{}' must declare at least one supported_executor_kind",
                self.worker_environment_kind_id
            ));
        }
        let mut executor_kinds = BTreeSet::new();
        for executor_kind in &self.supported_executor_kinds {
            validate_identifier("supported_executor_kinds", executor_kind.as_str())?;
            if !executor_kinds.insert(executor_kind.as_str()) {
                return Err(format!(
                    "worker_environment_kind '{}' repeats supported_executor_kind '{executor_kind}'",
                    self.worker_environment_kind_id
                ));
            }
        }
        let mut instance_config_files = BTreeSet::new();
        for name in &self.instance_config_files {
            if !valid_instance_config_file(name) || !instance_config_files.insert(name.as_str()) {
                return Err(format!(
                    "worker_environment_kind '{}' has invalid or duplicate instance_config_files entry '{}'",
                    self.worker_environment_kind_id, name
                ));
            }
        }
        validate_root_refs(
            self.worker_environment_kind_id.as_str(),
            "read_root_refs",
            &self.boundary_defaults.read_root_refs,
            Some(RootAccess::Read),
            false,
        )?;
        validate_root_refs(
            self.worker_environment_kind_id.as_str(),
            "write_root_refs",
            &self.boundary_defaults.write_root_refs,
            Some(RootAccess::Write),
            false,
        )?;
        validate_root_refs(
            self.worker_environment_kind_id.as_str(),
            "protected_read_only_subpaths",
            &self.boundary_defaults.protected_read_only_subpaths,
            None,
            true,
        )?;
        Ok(())
    }
}

fn validate_identifier(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().any(|character| {
            character.is_control() || character.is_whitespace() || matches!(character, '/' | '\\')
        })
    {
        return Err(format!(
            "{field} is not a non-empty registry identifier: {value:?}"
        ));
    }
    Ok(())
}

fn validate_unique_strings<'a>(
    kind: &str,
    field: &str,
    values: impl IntoIterator<Item = &'a str>,
    require_non_empty: bool,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for value in values {
        validate_identifier(field, value)?;
        if !seen.insert(value) {
            return Err(format!(
                "worker_environment_kind '{kind}' repeats {field} entry '{value}'"
            ));
        }
    }
    if require_non_empty && seen.is_empty() {
        return Err(format!(
            "worker_environment_kind '{kind}' must declare at least one {field} entry"
        ));
    }
    Ok(())
}

fn validate_unique_process_bins(kind: &str, values: &[String]) -> Result<(), String> {
    if values.is_empty() {
        return Err(format!(
            "worker_environment_kind '{kind}' must declare at least one tool_process_bins entry"
        ));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        let path = Path::new(value);
        if value.trim() != value
            || value.is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                !matches!(component, Component::Normal(_))
                    || component.as_os_str().to_str().is_none()
            })
            || !seen.insert(value.as_str())
        {
            return Err(format!(
                "worker_environment_kind '{kind}' has invalid or duplicate tool_process_bins entry '{value}'"
            ));
        }
    }
    Ok(())
}

fn valid_instance_config_file(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name.ends_with(".json")
        && !name.contains('/')
        && !name.contains('\\')
        && !matches!(
            name,
            "." | ".."
                | "interface.json"
                | "life.json"
                | "activation.json"
                | "policy.json"
                | "client.json"
        )
}

fn validate_root_refs(
    kind: &str,
    field: &str,
    roots: &[RootRef],
    required_access: Option<RootAccess>,
    forbid_write: bool,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for root in roots {
        validate_identifier(field, &root.path)?;
        if !seen.insert(root.path.as_str()) {
            return Err(format!(
                "worker_environment_kind '{kind}' repeats {field} root '{}'",
                root.path
            ));
        }
        let reads = root
            .access
            .iter()
            .filter(|access| **access == RootAccess::Read)
            .count();
        let writes = root
            .access
            .iter()
            .filter(|access| **access == RootAccess::Write)
            .count();
        if reads > 1 || writes > 1 {
            return Err(format!(
                "worker_environment_kind '{kind}' {field} root '{}' repeats an access value",
                root.path
            ));
        }
        if required_access
            .is_some_and(|required| !root.access.is_empty() && !root.access.contains(&required))
        {
            return Err(format!(
                "worker_environment_kind '{kind}' {field} root '{}' contradicts its access list",
                root.path
            ));
        }
        if forbid_write && writes != 0 {
            return Err(format!(
                "worker_environment_kind '{kind}' protected root '{}' grants write access",
                root.path
            ));
        }
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvCapabilityTag(pub String);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryDefaults {
    pub read_root_refs: Vec<RootRef>,
    pub write_root_refs: Vec<RootRef>,
    pub protected_read_only_subpaths: Vec<RootRef>,
    #[serde(default)]
    pub network_ceiling: NetworkCeiling,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkCeiling {
    #[default]
    Denied,
    Restricted,
    Full,
}

impl NetworkCeiling {
    pub fn min(self, other: Self) -> Self {
        if (self as u8) <= (other as u8) {
            self
        } else {
            other
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WekLifecycle {
    pub requires_release: bool,
}
