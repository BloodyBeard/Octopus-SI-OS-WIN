use std::collections::BTreeSet;
use std::path::{Component, Path};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSandboxMode {
    #[default]
    Enforced,
    Disabled,
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessAuthoritySpec {
    #[serde(default)]
    pub sandbox_mode: ProcessSandboxMode,
    pub read_root_slots: BTreeSet<String>,
    pub write_root_slots: BTreeSet<String>,
    pub executable_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub optional_read_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub optional_write_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub optional_executable_root_slots: BTreeSet<String>,
    pub read_paths: BTreeSet<String>,
    pub write_paths: BTreeSet<String>,
    pub executable_paths: BTreeSet<String>,
    #[serde(default)]
    pub executable_root_subpaths: BTreeSet<ProcessRootSubpathSpec>,
    pub executable_commands: BTreeSet<String>,
    pub read_env_paths: BTreeSet<String>,
    pub write_env_paths: BTreeSet<String>,
    pub executable_env_paths: BTreeSet<String>,
    #[serde(default)]
    pub optional_read_env_paths: BTreeSet<String>,
    #[serde(default)]
    pub optional_write_env_paths: BTreeSet<String>,
    #[serde(default)]
    pub read_env_subpaths: Vec<ProcessEnvironmentSubpathSpec>,
    #[serde(default)]
    pub write_env_subpaths: Vec<ProcessEnvironmentSubpathSpec>,
    #[serde(default)]
    pub executable_env_subpaths: Vec<ProcessEnvironmentSubpathSpec>,
    #[serde(default)]
    pub read_argument_paths: Vec<ProcessArgumentPathSpec>,
    #[serde(default)]
    pub write_argument_paths: Vec<ProcessArgumentPathSpec>,
    #[serde(default)]
    pub executable_argument_paths: Vec<ProcessArgumentPathSpec>,
    pub allow_network: bool,
    pub allow_local_ipc: bool,
    #[serde(default)]
    pub allow_application_launch: bool,
    #[serde(default)]
    pub allow_sandbox_delegation: bool,
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessArgumentPathSpec {
    pub pointer: String,
    pub within_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub include_parent: bool,
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessEnvironmentSubpathSpec {
    pub env: String,
    pub relative_path: String,
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessRootSubpathSpec {
    pub root_slot: String,
    pub relative_path: String,
}

impl ProcessAuthoritySpec {
    pub fn has_argument_paths(&self) -> bool {
        !self.read_argument_paths.is_empty()
            || !self.write_argument_paths.is_empty()
            || !self.executable_argument_paths.is_empty()
    }

    pub fn validate(&self) -> Result<(), String> {
        for (field, values) in [
            ("read_root_slots", &self.read_root_slots),
            ("write_root_slots", &self.write_root_slots),
            ("executable_root_slots", &self.executable_root_slots),
            ("optional_read_root_slots", &self.optional_read_root_slots),
            ("optional_write_root_slots", &self.optional_write_root_slots),
            (
                "optional_executable_root_slots",
                &self.optional_executable_root_slots,
            ),
        ] {
            for value in values {
                validate_atom(field, value)?;
            }
        }
        for (required, optional, field) in [
            (
                &self.read_root_slots,
                &self.optional_read_root_slots,
                "read_root_slots",
            ),
            (
                &self.write_root_slots,
                &self.optional_write_root_slots,
                "write_root_slots",
            ),
            (
                &self.executable_root_slots,
                &self.optional_executable_root_slots,
                "executable_root_slots",
            ),
        ] {
            if !required.is_disjoint(optional) {
                return Err(format!(
                    "process authority repeats a root slot across {field} and optional_{field}"
                ));
            }
        }
        for (field, values) in [
            ("read_paths", &self.read_paths),
            ("write_paths", &self.write_paths),
            ("executable_paths", &self.executable_paths),
        ] {
            for value in values {
                validate_declared_path(field, value)?;
            }
        }
        for value in &self.executable_commands {
            validate_declared_executable("executable_commands", value)?;
        }
        for value in &self.executable_root_subpaths {
            value.validate("executable_root_subpaths")?;
        }
        for (field, values) in [
            ("read_env_paths", &self.read_env_paths),
            ("write_env_paths", &self.write_env_paths),
            ("executable_env_paths", &self.executable_env_paths),
            ("optional_read_env_paths", &self.optional_read_env_paths),
            ("optional_write_env_paths", &self.optional_write_env_paths),
        ] {
            for value in values {
                validate_env_name(field, value)?;
            }
        }
        if !self
            .read_env_paths
            .is_disjoint(&self.optional_read_env_paths)
        {
            return Err(
                "process authority repeats an env name across read_env_paths and optional_read_env_paths"
                    .to_string(),
            );
        }
        if !self
            .write_env_paths
            .is_disjoint(&self.optional_write_env_paths)
        {
            return Err(
                "process authority repeats an env name across write_env_paths and optional_write_env_paths"
                    .to_string(),
            );
        }
        for (field, values) in [
            ("read_env_subpaths", &self.read_env_subpaths),
            ("write_env_subpaths", &self.write_env_subpaths),
            ("executable_env_subpaths", &self.executable_env_subpaths),
        ] {
            for value in values {
                value.validate(field)?;
            }
        }
        for (field, values) in [
            ("read_argument_paths", &self.read_argument_paths),
            ("write_argument_paths", &self.write_argument_paths),
            ("executable_argument_paths", &self.executable_argument_paths),
        ] {
            for value in values {
                value.validate(field)?;
            }
        }
        Ok(())
    }
}

impl ProcessRootSubpathSpec {
    fn validate(&self, field: &str) -> Result<(), String> {
        validate_atom(&format!("{field}.root_slot"), &self.root_slot)?;
        validate_relative_subpath(&format!("{field}.relative_path"), &self.relative_path)
    }
}

impl ProcessArgumentPathSpec {
    fn validate(&self, field: &str) -> Result<(), String> {
        if !self.pointer.starts_with('/') || self.pointer == "/" {
            return Err(format!(
                "process authority {field} contains invalid pointer {:?}",
                self.pointer
            ));
        }
        if self.within_root_slots.is_empty() {
            return Err(format!(
                "process authority {field} pointer {:?} names no root slots",
                self.pointer
            ));
        }
        for slot in &self.within_root_slots {
            validate_atom(field, slot)?;
        }
        Ok(())
    }
}

impl ProcessEnvironmentSubpathSpec {
    fn validate(&self, field: &str) -> Result<(), String> {
        validate_env_name(field, &self.env)?;
        validate_relative_subpath(field, &self.relative_path)
    }
}

fn validate_atom(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!(
            "process authority {field} contains invalid value {value:?}"
        ));
    }
    Ok(())
}

fn validate_env_name(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value)?;
    if value.contains('=') || value.contains('\0') {
        return Err(format!(
            "process authority {field} contains invalid environment name {value:?}"
        ));
    }
    Ok(())
}

fn validate_declared_executable(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value)?;
    let path = Path::new(value);
    if path.is_relative()
        && path.components().count() > 1
        && path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(format!(
            "process authority {field} contains parent traversal {value:?}"
        ));
    }
    Ok(())
}

fn validate_declared_path(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value)?;
    let path = Path::new(value);
    if path.is_relative()
        && path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(format!(
            "process authority {field} contains parent traversal {value:?}"
        ));
    }
    Ok(())
}

fn validate_relative_subpath(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value)?;
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "process authority {field} contains invalid relative subpath {value:?}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_root_subpaths_reject_parent_traversal() {
        let authority = ProcessAuthoritySpec {
            executable_root_subpaths: BTreeSet::from([ProcessRootSubpathSpec {
                root_slot: "registry_worker_environment_kind_root".to_string(),
                relative_path: "../foreign/packages/bin/tool".to_string(),
            }]),
            ..Default::default()
        };

        assert!(authority
            .validate()
            .unwrap_err()
            .contains("invalid relative subpath"));
    }
}
