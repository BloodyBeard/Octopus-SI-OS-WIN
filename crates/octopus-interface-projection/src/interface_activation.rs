use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use octopus_descriptors::{InterfaceId, loader};
use octopus_interfaces::{Direction, InterfaceDescriptor};

#[derive(Clone, Debug)]
pub struct LoadedInterfaceActivationFile {
    pub path: PathBuf,
    pub descriptor: InterfaceActivationFile,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceActivationFile {
    pub schema_version: u32,
    pub interface: String,
    pub activations: Vec<InterfaceActivation>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceActivation {
    pub activation_driver_ref: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub config: serde_json::Value,
}

fn default_true() -> bool {
    true
}

pub fn load_interface_activation_file_for_interface(
    registry_root: &Path,
    interface: &InterfaceId,
) -> Result<LoadedInterfaceActivationFile, String> {
    load_interface_activation_file_in_dir(
        &registry_root.join("interface").join(interface.as_str()),
        interface,
    )
}

pub fn load_interface_activation_file_in_dir(
    interface_dir: &Path,
    interface: &InterfaceId,
) -> Result<LoadedInterfaceActivationFile, String> {
    let path = interface_dir.join("activation.json");
    load_interface_activation_file_at(&path, interface.as_str())
}

pub fn load_interface_activation_files(
    interface_root: &Path,
) -> Result<Vec<LoadedInterfaceActivationFile>, String> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(interface_root)
        .map_err(|e| format!("reading registry/interface failed: {e}"))?
    {
        let entry = entry.map_err(|e| format!("reading registry/interface entry failed: {e}"))?;
        if !entry
            .file_type()
            .map_err(|e| {
                format!(
                    "reading file type for {} failed: {e}",
                    entry.path().display()
                )
            })?
            .is_dir()
        {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path().join("activation.json");
        if !path.exists() {
            continue;
        }
        files.push(load_interface_activation_file_at(&path, &dir_name)?);
    }
    Ok(files)
}

pub fn load_validated_interface_activation_files(
    registry_root: &Path,
) -> Result<Vec<LoadedInterfaceActivationFile>, String> {
    let interface_root = registry_root.join("interface");
    let loaded_interfaces =
        loader::load_kind::<InterfaceDescriptor>(&interface_root).map_err(|e| {
            format!("loading interface descriptors for activation validation failed: {e}")
        })?;
    let mut interface_ids: BTreeSet<String> = BTreeSet::new();
    let mut inbound_capable: BTreeSet<String> = BTreeSet::new();
    for (_name, desc) in &loaded_interfaces {
        interface_ids.insert(desc.id.as_str().to_string());
        if matches!(
            desc.direction,
            Direction::Inbound | Direction::Bidirectional
        ) {
            inbound_capable.insert(desc.id.as_str().to_string());
        }
    }

    let mut files = Vec::new();
    for loaded in load_interface_activation_files(&interface_root)
        .map_err(|e| format!("loading interface activation files failed: {e}"))?
    {
        if !interface_ids.contains(&loaded.descriptor.interface) {
            return Err(format!(
                "interface activation file {} points at missing interface '{}'",
                loaded.path.display(),
                loaded.descriptor.interface
            ));
        }
        if !inbound_capable.contains(&loaded.descriptor.interface) {
            return Err(format!(
                "interface activation file {} points at non-inbound interface '{}'",
                loaded.path.display(),
                loaded.descriptor.interface
            ));
        }
        files.push(loaded);
    }
    Ok(files)
}

fn load_interface_activation_file_at(
    path: &Path,
    expected_interface: &str,
) -> Result<LoadedInterfaceActivationFile, String> {
    let bytes = std::fs::read(path).map_err(|e| {
        format!(
            "{}: required interface activation is missing or unreadable: {e}",
            path.display()
        )
    })?;
    let descriptor: InterfaceActivationFile = serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "malformed interface activation file {}: {e}",
            path.display()
        )
    })?;
    validate_interface_activation_file(&descriptor, path, expected_interface)?;
    Ok(LoadedInterfaceActivationFile {
        path: path.to_path_buf(),
        descriptor,
    })
}

fn validate_interface_activation_file(
    file: &InterfaceActivationFile,
    path: &Path,
    expected_interface: &str,
) -> Result<(), String> {
    if file.schema_version != 1 {
        return Err(format!(
            "interface activation file {} has unsupported schema_version {}",
            path.display(),
            file.schema_version
        ));
    }
    if file.interface != expected_interface {
        return Err(format!(
            "interface activation file {} points at '{}', but lives under '{}'",
            path.display(),
            file.interface,
            expected_interface
        ));
    }
    if file.activations.is_empty() {
        return Err(format!(
            "interface activation file {} must declare at least one activation",
            path.display()
        ));
    }
    let mut enabled_activation_driver_refs = BTreeSet::new();
    for (idx, activation) in file.activations.iter().enumerate() {
        let activation_driver_ref = activation.activation_driver_ref.trim();
        if activation_driver_ref.is_empty() {
            return Err(format!(
                "interface activation file {} activation #{} has empty activation_driver_ref",
                path.display(),
                idx
            ));
        }
        if !activation.config.is_object() {
            return Err(format!(
                "interface activation file {} activation #{} config must be a JSON object",
                path.display(),
                idx
            ));
        }
        if activation.enabled
            && !enabled_activation_driver_refs.insert(activation_driver_ref.to_string())
        {
            return Err(format!(
                "interface activation file {} declares duplicate enabled activation for activation_driver_ref '{}'",
                path.display(),
                activation_driver_ref
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_run_demux_vocabulary_fails_to_parse() {
        let err = serde_json::from_value::<InterfaceActivationFile>(serde_json::json!({
            "schema_version": 1,
            "interface": "world_operator_chat",
            "activations": [{
                "activation_driver_ref": "http_json_poll",
                "keyed_run": {"key_path": ["origin_target"], "run_prefix": "run_chat_"},
                "config": {}
            }]
        }))
        .expect_err("the key→run map is exterminated; sessions are separate interface folders");
        assert!(
            err.to_string().contains("keyed_run"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn activation_cannot_address_a_run_or_install_run_topology() {
        let err = serde_json::from_value::<InterfaceActivationFile>(serde_json::json!({
            "schema_version": 1,
            "interface": "world_operator_chat",
            "activations": [{
                "activation_driver_ref": "app_server_transport",
                "run": "run_daemon",
                "activation_run": {
                    "id": "run_daemon",
                    "ee": "ee_existence",
                    "worker": "system_worker_coder_max",
                    "trace": null,
                    "status": "running",
                    "interface_state": {},
                    "checkpoints": [],
                    "active_checkpoints": []
                },
                "config": {}
            }]
        }))
        .expect_err(
            "run records are never app-installed topology: the interface's own code authors its \
             declared record lazily from its own template data, so a sidecar carrying a run body \
             for the host is descriptor drift",
        );
        assert!(err.to_string().contains("run"), "unexpected error: {err}");
    }
}
