#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_descriptors as desc;
use octopus_descriptors::{DescriptorName, InterfaceId, ToolDescriptorId, WorkerEnvironmentKindId};
pub use octopus_descriptors::{
    ProcessArgumentPathSpec, ProcessAuthoritySpec, ProcessEnvironmentSubpathSpec,
};

pub const TOOL_DESCRIPTOR_STORAGE_KIND: &str = "tool";

pub struct ToolArgumentValidator {
    schema_hash: String,
    validator: jsonschema::Validator,
}

/// Runs JSON Schema compilation on a thread with a large stack.
///
/// `jsonschema` builds its meta-schema validators lazily: each draft, and
/// each part of it, the first time a schema needs it. That build recurses
/// deeply, well over 1 MiB of stack in debug builds. Windows gives a main
/// thread only 1 MiB and test threads get 2 MiB everywhere, so compiling
/// wherever a descriptor happens to load can overflow the stack. A short-lived
/// thread costs microseconds next to loading a descriptor or a tool call.
fn on_large_stack<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    const STACK_BYTES: usize = 16 * 1024 * 1024;
    let mut work = Some(work);
    let result = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("octopus-json-schema".into())
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || (work.take().expect("work runs once"))())
            .map(|thread| thread.join())
    });
    match result {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        // The thread could not start: compile here instead.
        Err(_) => (work.take().expect("work did not run"))(),
    }
}

impl ToolArgumentValidator {
    pub fn compile(schema: &serde_json::Value) -> Result<Self, String> {
        let schema_hash = Self::hash(schema);
        let validator = on_large_stack(|| {
            jsonschema::options()
                .should_validate_formats(true)
                .should_ignore_unknown_formats(false)
                .build(schema)
        })
        .map_err(|error| format!("args_schema is invalid: {error}"))?;
        Ok(Self {
            schema_hash,
            validator,
        })
    }

    pub fn hash(schema: &serde_json::Value) -> String {
        blake3::hash(&octopus_descriptors::canonical_bytes(schema))
            .to_hex()
            .to_string()
    }

    pub fn schema_hash(&self) -> &str {
        &self.schema_hash
    }

    pub fn validate(&self, arguments: &serde_json::Value) -> Result<(), String> {
        let errors = self
            .validator
            .iter_errors(arguments)
            .take(8)
            .map(|error| {
                format!(
                    "{} at instance '{}' against schema '{}'",
                    error, error.instance_path, error.schema_path
                )
            })
            .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

pub fn tool_descriptor_registry_path(registry_root: &Path) -> PathBuf {
    registry_root.join(TOOL_DESCRIPTOR_STORAGE_KIND)
}

pub fn load_tool_descriptors(
    registry_root: &Path,
) -> desc::Result<Vec<(DescriptorName, ToolDescriptor)>> {
    load_tool_descriptors_from_dir(&tool_descriptor_registry_path(registry_root))
}

pub fn load_tool_descriptors_from_dir(
    tool_root: &Path,
) -> desc::Result<Vec<(DescriptorName, ToolDescriptor)>> {
    desc::loader::load_kind::<ToolDescriptor>(tool_root)
}

pub fn load_tool_descriptor_from_dir(
    tool_root: &Path,
    id: &str,
) -> desc::Result<Option<ToolDescriptor>> {
    let mut components = Path::new(id).components();
    let safe = matches!(components.next(), Some(std::path::Component::Normal(name)) if name.to_str() == Some(id))
        && components.next().is_none();
    if !safe || id.trim().is_empty() {
        return Err(desc::DescriptorError::Io(format!(
            "tool descriptor id '{id}' is not a registry directory name"
        )));
    }
    let path = tool_root.join(id).join("tool.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(desc::DescriptorError::Io(format!(
                "{}: {error}",
                path.display()
            )));
        }
    };
    let descriptor = desc::parse_strict::<ToolDescriptor>(&bytes)?;
    if descriptor.id.as_str() != id {
        return Err(desc::DescriptorError::Io(format!(
            "{}: dir/body identity mismatch — directory is {:?} but descriptor declares {:?}",
            path.display(),
            id,
            descriptor.id.as_str()
        )));
    }
    Ok(Some(descriptor))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProcessDeclaration {
    pub bin: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub required_files: BTreeSet<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub inherit_env: BTreeSet<String>,
    #[serde(default)]
    pub inherit_env_from_interface: BTreeSet<String>,
    pub authority: ProcessAuthoritySpec,
    #[serde(default)]
    pub coordinate_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub private_read_subpaths: BTreeSet<ToolProcessPrivateReadSubpath>,
    #[serde(default)]
    pub interrupted_effect_recovery: ToolInterruptedEffectRecovery,
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolInterruptedEffectRecovery {
    #[default]
    Indeterminate,
    ReinvokeExactArtifact,
}

#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ToolProcessPrivateReadSubpath {
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDescriptor {
    pub id: ToolDescriptorId,
    pub interface: InterfaceId,
    pub process: ToolProcessDeclaration,
    #[serde(default)]
    pub handler_config: serde_json::Value,
    pub supported_worker_environment_kinds: Vec<WorkerEnvironmentKindId>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_tool_args_schema")]
    pub args_schema: serde_json::Value,
    #[serde(default)]
    pub is_world_effect: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_of: Option<ToolDescriptorId>,
}

impl ToolDescriptor {
    /// A copy is never wider than its original: the same executable, handler and
    /// schema, run under an authority the original's authority covers.
    pub fn never_wider_than(&self, original: &ToolDescriptor) -> bool {
        let (copy, held) = (&self.process, &original.process);
        copy.bin == held.bin
            && copy.args == held.args
            && copy.env == held.env
            && copy.interrupted_effect_recovery == held.interrupted_effect_recovery
            && copy.required_files.is_subset(&held.required_files)
            && copy.inherit_env.is_subset(&held.inherit_env)
            && copy.inherit_env_from_interface.is_subset(&held.inherit_env_from_interface)
            && copy.coordinate_root_slots.is_subset(&held.coordinate_root_slots)
            && copy.private_read_subpaths.is_subset(&held.private_read_subpaths)
            && copy.authority.narrows(&held.authority)
            && self.handler_config == original.handler_config
            && self.args_schema == original.args_schema
            && self.is_world_effect == original.is_world_effect
            && self.supported_worker_environment_kinds.iter().all(|kind| original.supported_worker_environment_kinds.contains(kind))
    }
}

fn default_tool_args_schema() -> serde_json::Value {
    serde_json::json!({ "type": "object", "properties": {} })
}

impl desc::Descriptor for ToolDescriptor {
    const KIND: &'static str = TOOL_DESCRIPTOR_STORAGE_KIND;
    const SCHEMA_VERSION: u32 = 1;
    const CONTRACT_VERSION: u32 = 3;

    fn declared_name(&self) -> Option<&str> {
        Some(self.id.as_str())
    }

    fn validate(&self) -> Result<(), String> {
        if self.process.bin.trim().is_empty() {
            return Err(format!(
                "tool descriptor '{}' has an empty process.bin",
                self.id
            ));
        }
        if let Some(original) = &self.copy_of {
            if original.as_str().trim().is_empty()
                || original.as_str().trim() != original.as_str()
                || original == &self.id
            {
                return Err(format!(
                    "tool descriptor '{}' declares an invalid copy_of '{}'",
                    self.id, original
                ));
            }
        }
        self.process.authority.validate().map_err(|error| {
            format!(
                "tool descriptor '{}' has invalid process authority: {error}",
                self.id
            )
        })?;
        for slot in &self.process.coordinate_root_slots {
            if slot.is_empty() || slot.trim() != slot {
                return Err(format!(
                    "tool descriptor '{}' contains invalid process coordinate root slot {:?}",
                    self.id, slot
                ));
            }
        }
        let mut private_read_handles = BTreeSet::new();
        for declaration in &self.process.private_read_subpaths {
            let mut components = std::path::Path::new(&declaration.path).components();
            if declaration.path.trim() != declaration.path
                || declaration.path.is_empty()
                || components.any(|component| !matches!(component, std::path::Component::Normal(value) if !value.is_empty()))
            {
                return Err(format!(
                    "tool descriptor '{}' contains invalid process private_read_subpath {:?}",
                    self.id, declaration.path
                ));
            }
            if !private_read_handles.insert(&declaration.path) {
                return Err(format!(
                    "tool descriptor '{}' repeats process private_read_subpath {:?}",
                    self.id, declaration.path
                ));
            }
        }
        for name in &self.process.inherit_env {
            if name.trim().is_empty() || is_reserved_tool_process_env(name) {
                return Err(format!(
                    "tool descriptor '{}' process.inherit_env contains forbidden name '{}'",
                    self.id, name
                ));
            }
            if self.process.env.contains_key(name) {
                return Err(format!(
                    "tool descriptor '{}' process env '{}' is both literal and inherited",
                    self.id, name
                ));
            }
        }
        if self
            .process
            .inherit_env_from_interface
            .iter()
            .any(|pointer| !pointer.starts_with('/') || pointer.trim() == "/")
        {
            return Err(format!(
                "tool descriptor '{}' process.inherit_env_from_interface contains an invalid JSON pointer",
                self.id
            ));
        }
        if self
            .process
            .env
            .keys()
            .any(|name| name.trim().is_empty() || is_reserved_tool_process_env(name))
        {
            return Err(format!(
                "tool descriptor '{}' process.env contains an empty or reserved name",
                self.id
            ));
        }
        for name in self
            .process
            .authority
            .read_env_paths
            .iter()
            .chain(&self.process.authority.write_env_paths)
            .chain(&self.process.authority.executable_env_paths)
            .chain(&self.process.authority.optional_read_env_paths)
            .chain(&self.process.authority.optional_write_env_paths)
        {
            if is_reserved_tool_process_env(name) {
                return Err(format!(
                    "tool descriptor '{}' process authority names reserved env '{}'",
                    self.id, name
                ));
            }
        }
        for declaration in self
            .process
            .authority
            .read_env_subpaths
            .iter()
            .chain(&self.process.authority.write_env_subpaths)
            .chain(&self.process.authority.executable_env_subpaths)
        {
            if is_reserved_tool_process_env(&declaration.env) {
                return Err(format!(
                    "tool descriptor '{}' process authority names reserved env '{}'",
                    self.id, declaration.env
                ));
            }
        }
        if self.supported_worker_environment_kinds.len() != 1 {
            return Err(format!(
                "tool descriptor '{}' must name exactly one worker-environment code kind",
                self.id
            ));
        }
        let mut supported_weks = BTreeSet::new();
        for wek in &self.supported_worker_environment_kinds {
            if wek.as_str().trim().is_empty() {
                return Err(format!(
                    "tool descriptor '{}' supported_worker_environment_kinds contains an empty id",
                    self.id
                ));
            }
            if !supported_weks.insert(wek.as_str()) {
                return Err(format!(
                    "tool descriptor '{}' repeats supported_worker_environment_kind '{}'",
                    self.id, wek
                ));
            }
        }
        on_large_stack(|| jsonschema::meta::try_validate(&self.args_schema).map(|valid| valid.map_err(|error| error.to_string())))
            .map_err(|error| {
                format!(
                    "tool descriptor '{}' args_schema draft cannot be resolved: {error}",
                    self.id
                )
            })?
            .map_err(|error| {
                format!(
                    "tool descriptor '{}' carries malformed args_schema: {error}",
                    self.id
                )
            })?;
        Ok(())
    }
}

fn is_reserved_tool_process_env(name: &str) -> bool {
    matches!(
        name,
        "OCTOPUS_INTERFACE_ID"
            | "OCTOPUS_INTERFACE_DIR"
            | "OCTOPUS_INTERFACE_DESCRIPTOR_JSON"
            | "OCTOPUS_VAR_DIR"
            | "OCTOPUS_INTERFACE_VAR_DIR"
            | "OCTOPUS_TOOL_PROCESS_DURABLE_VAR_DIR"
            | "OCTOPUS_PRIVATE_READS_JSON"
            | "OCTOPUS_ROOT_SLOTS_JSON"
            | "OCTOPUS_MAX_IN_FLIGHT_TOOL_PROCESSES"
            | "OCTOPUS_TOOL_PROCESS_PRIVATE_READ_MAX_BYTES"
            | "OCTOPUS_MOVEMENTS_JSONL"
            | "OCTOPUS_BLOBS_DIR"
            | "OCTOPUS_LEDGER_INDEX_SQLITE"
            | octopus_descriptors::UNSANDBOXED_PROCESSES_ENV
            | "TMPDIR"
    )
}

#[cfg(test)]
mod tests {
    use super::{ProcessAuthoritySpec, ProcessEnvironmentSubpathSpec};
    use super::{
        ToolArgumentValidator, ToolDescriptor, ToolProcessDeclaration,
        ToolProcessPrivateReadSubpath,
    };
    use octopus_descriptors::{Descriptor, InterfaceId, ToolDescriptorId, WorkerEnvironmentKindId};
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    fn descriptor_with_code_kind() -> ToolDescriptor {
        ToolDescriptor {
            id: ToolDescriptorId::new("test.private"),
            interface: InterfaceId::new("system_test"),
            process: ToolProcessDeclaration {
                bin: "/usr/bin/true".to_string(),
                args: Vec::new(),
                required_files: BTreeSet::new(),
                env: BTreeMap::new(),
                inherit_env: BTreeSet::new(),
                inherit_env_from_interface: BTreeSet::new(),
                authority: ProcessAuthoritySpec::default(),
                coordinate_root_slots: BTreeSet::new(),
                private_read_subpaths: BTreeSet::new(),
                interrupted_effect_recovery: Default::default(),
            },
            handler_config: serde_json::Value::Null,
            supported_worker_environment_kinds: vec![WorkerEnvironmentKindId::new("test_room")],
            description: String::new(),
            args_schema: json!({"type": "object", "properties": {}}),
            is_world_effect: false,
            copy_of: None,
        }
    }

    #[test]
    fn tool_code_world_field_is_required() {
        let mut missing = serde_json::to_value(descriptor_with_code_kind()).unwrap();
        missing
            .as_object_mut()
            .unwrap()
            .remove("supported_worker_environment_kinds");
        assert!(serde_json::from_value::<ToolDescriptor>(missing).is_err());
    }

    #[test]
    fn tool_code_world_is_exactly_one() {
        descriptor_with_code_kind().validate().unwrap();

        let mut empty = descriptor_with_code_kind();
        empty.supported_worker_environment_kinds.clear();
        assert!(empty
            .validate()
            .unwrap_err()
            .contains("must name exactly one worker-environment code kind"));
        assert!(octopus_descriptors::parse_strict::<ToolDescriptor>(
            &serde_json::to_vec(&empty).unwrap()
        )
        .is_err());

        let mut multiple = descriptor_with_code_kind();
        multiple
            .supported_worker_environment_kinds
            .push(WorkerEnvironmentKindId::new("other_room"));
        assert!(multiple
            .validate()
            .unwrap_err()
            .contains("must name exactly one worker-environment code kind"));
        assert!(octopus_descriptors::parse_strict::<ToolDescriptor>(
            &serde_json::to_vec(&multiple).unwrap()
        )
        .is_err());
    }

    #[test]
    fn tool_argument_validator_enforces_nested_refs_and_closed_objects() {
        let validator = ToolArgumentValidator::compile(&json!({
            "type": "object",
            "properties": {
                "event": { "$ref": "#/definitions/event" }
            },
            "required": ["event"],
            "additionalProperties": false,
            "definitions": {
                "event": {
                    "type": "object",
                    "properties": { "title": { "type": "string", "minLength": 1 } },
                    "required": ["title"],
                    "additionalProperties": false
                }
            }
        }))
        .unwrap();
        validator
            .validate(&json!({ "event": { "title": "Octopus" } }))
            .unwrap();
        assert!(validator
            .validate(&json!({ "event": { "title": "", "extra": true } }))
            .is_err());
    }

    #[test]
    fn legacy_interface_var_coordinate_cannot_be_redeclared() {
        let mut descriptor = descriptor_with_code_kind();
        descriptor.process.inherit_env = BTreeSet::from(["OCTOPUS_INTERFACE_VAR_DIR".to_string()]);
        assert!(descriptor
            .validate()
            .unwrap_err()
            .contains("forbidden name"));

        descriptor.process.inherit_env.clear();
        descriptor.process.env.insert(
            "OCTOPUS_INTERFACE_VAR_DIR".to_string(),
            "/tmp/ambient".to_string(),
        );
        assert!(descriptor.validate().unwrap_err().contains("reserved name"));

        descriptor.process.env.clear();
        descriptor.process.env.insert(
            "OCTOPUS_TOOL_PROCESS_DURABLE_VAR_DIR".to_string(),
            "/tmp/ambient".to_string(),
        );
        assert!(descriptor.validate().unwrap_err().contains("reserved name"));

        descriptor.process.env.clear();
        descriptor
            .process
            .authority
            .read_env_subpaths
            .push(ProcessEnvironmentSubpathSpec {
                env: "OCTOPUS_INTERFACE_VAR_DIR".to_string(),
                relative_path: "secret".to_string(),
            });
        assert!(descriptor.validate().unwrap_err().contains("reserved env"));
    }

    #[test]
    fn private_reads_are_strict_leaf_declarations() {
        let authority = serde_json::to_value(ProcessAuthoritySpec::default()).unwrap();
        assert!(serde_json::from_value::<ToolProcessDeclaration>(json!({
            "bin": "/usr/bin/true",
            "authority": authority,
            "private_read_subpaths": ["secrets/auth"]
        }))
        .is_err());
        let declaration = ToolProcessPrivateReadSubpath {
            path: "secrets/auth".to_string(),
        };
        let mut descriptor = descriptor_with_code_kind();
        descriptor.process.private_read_subpaths = BTreeSet::from([declaration]);
        descriptor.validate().unwrap();
        descriptor.process.private_read_subpaths =
            BTreeSet::from([ToolProcessPrivateReadSubpath {
                path: "../secrets/auth".to_string(),
            }]);
        assert!(descriptor
            .validate()
            .unwrap_err()
            .contains("invalid process private_read_subpath"));

        let authority = serde_json::to_value(ProcessAuthoritySpec::default()).unwrap();
        assert!(serde_json::from_value::<ToolProcessDeclaration>(json!({
            "bin": "/usr/bin/true",
            "authority": authority,
            "private_read_subpaths": [{"path": "secrets/auth", "max_bytes": 4096}]
        }))
        .is_err());
    }
}
