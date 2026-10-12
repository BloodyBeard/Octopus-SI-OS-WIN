use std::collections::{BTreeMap, BTreeSet};

use crate::{WorkerConsent, WorkerEnvKindSpec};
use octopus_descriptors::{DescriptorName, InterfaceId};
use octopus_interfaces::{InterfaceDescriptor, InterfaceRuntimeDisposition, InterfaceType};

#[derive(Clone, Debug)]
pub struct WorkerEnvironmentCeilingView<'a> {
    pub worker: &'a InterfaceId,
    pub wek: Vec<&'a str>,
    pub tools: Vec<&'a str>,
}

#[derive(Clone, Debug)]
pub struct WorkerEnvironmentToolView<'a> {
    pub tool: &'a str,
    pub interface: &'a InterfaceId,
    pub process_bin: &'a str,
    pub worker_environment_kinds: Vec<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerEnvironmentRegistrySubject {
    Worker(InterfaceId),
    Tool(String),
    Kind(String),
    Interface(InterfaceId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerEnvironmentRegistryIssue {
    pub subject: WorkerEnvironmentRegistrySubject,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
struct WorkerEnvironmentToolSurface<'a> {
    kinds_by_tool: BTreeMap<&'a str, BTreeSet<&'a str>>,
}

impl<'a> WorkerEnvironmentToolSurface<'a> {
    fn from_tools(tools: &'a [WorkerEnvironmentToolView<'a>]) -> Self {
        let mut surface = Self::default();
        for tool in tools {
            surface.kinds_by_tool.insert(
                tool.tool,
                tool.worker_environment_kinds.iter().copied().collect(),
            );
        }
        surface
    }

    fn kinds_for(&self, tool: &str) -> Option<&BTreeSet<&'a str>> {
        self.kinds_by_tool.get(tool)
    }
}

pub fn worker_environment_kind_ids<'a>(
    worker_environment_kinds: &'a [(DescriptorName, WorkerEnvKindSpec)],
) -> BTreeSet<&'a str> {
    worker_environment_kinds
        .iter()
        .map(|(_, wek)| wek.worker_environment_kind_id.as_str())
        .collect()
}

pub fn worker_environment_registry_issues(
    workers: &[WorkerEnvironmentCeilingView<'_>],
    worker_environment_kinds: &[(DescriptorName, WorkerEnvKindSpec)],
    tools: &[WorkerEnvironmentToolView<'_>],
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
    interface_dispositions: &BTreeMap<InterfaceId, InterfaceRuntimeDisposition>,
) -> Vec<WorkerEnvironmentRegistryIssue> {
    let kind_ids = worker_environment_kind_ids(worker_environment_kinds);
    let kinds_by_id = worker_environment_kinds
        .iter()
        .map(|(_, kind)| (kind.worker_environment_kind_id.as_str(), kind))
        .collect::<BTreeMap<_, _>>();
    let interfaces_by_id = interfaces
        .iter()
        .map(|(_, interface)| (interface.id.as_str(), interface))
        .collect::<BTreeMap<_, _>>();
    let environment_surface = WorkerEnvironmentToolSurface::from_tools(tools);
    let mut issues = Vec::new();

    for tool in tools {
        for kind in &tool.worker_environment_kinds {
            if !kind_ids.contains(kind) {
                issues.push(WorkerEnvironmentRegistryIssue {
                    subject: WorkerEnvironmentRegistrySubject::Tool(tool.tool.to_string()),
                    reason: format!(
                        "tool '{}' names missing worker_environment_kind '{}'",
                        tool.tool, kind
                    ),
                });
                continue;
            }
            let kind_spec = kinds_by_id
                .get(kind)
                .expect("worker_environment_kind id set and map must agree");
            if !kind_spec
                .tool_process_bins
                .iter()
                .any(|bin| bin == tool.process_bin)
            {
                issues.push(WorkerEnvironmentRegistryIssue {
                    subject: WorkerEnvironmentRegistrySubject::Tool(tool.tool.to_string()),
                    reason: format!(
                        "tool '{}' process.bin '{}' is not declared by worker_environment_kind '{}'",
                        tool.tool, tool.process_bin, kind
                    ),
                });
            }
            let Some(door) = interfaces_by_id.get(tool.interface.as_str()).copied() else {
                issues.push(WorkerEnvironmentRegistryIssue {
                    subject: WorkerEnvironmentRegistrySubject::Tool(tool.tool.to_string()),
                    reason: format!(
                        "tool '{}' names missing exact door interface '{}'",
                        tool.tool, tool.interface
                    ),
                });
                continue;
            };
            if door.interface_type != InterfaceType::System
                || interface_dispositions.get(&door.id)
                    != Some(&InterfaceRuntimeDisposition::ExternalReader)
            {
                issues.push(WorkerEnvironmentRegistryIssue {
                    subject: WorkerEnvironmentRegistrySubject::Interface(door.id.clone()),
                    reason: format!(
                        "worker-environment door '{}' for tool '{}' must be an executable System interface",
                        door.id, tool.tool
                    ),
                });
            }
            if !kind_spec
                .door_driver_refs
                .iter()
                .any(|driver_ref| driver_ref == &door.interface_driver_ref)
            {
                issues.push(WorkerEnvironmentRegistryIssue {
                    subject: WorkerEnvironmentRegistrySubject::Tool(tool.tool.to_string()),
                    reason: format!(
                        "tool '{}' exact door '{}' driver ref '{}' is not declared by worker_environment_kind '{}'",
                        tool.tool, door.id, door.interface_driver_ref, kind
                    ),
                });
            }
        }
    }

    for worker in workers {
        let ceiling_weks = worker.wek.iter().copied().collect::<BTreeSet<_>>();
        for kind in &ceiling_weks {
            if !kind_ids.contains(kind) {
                issues.push(WorkerEnvironmentRegistryIssue {
                    subject: WorkerEnvironmentRegistrySubject::Worker(worker.worker.clone()),
                    reason: format!(
                        "worker interface '{}' ceiling.wek names missing worker_environment_kind '{}'",
                        worker.worker, kind
                    ),
                });
                continue;
            }
            let kind_spec = kinds_by_id
                .get(kind)
                .expect("worker_environment_kind id set and map must agree");
            if kind_spec.worker_consent != WorkerConsent::ExplicitCeiling {
                issues.push(WorkerEnvironmentRegistryIssue {
                    subject: WorkerEnvironmentRegistrySubject::Worker(worker.worker.clone()),
                    reason: format!(
                        "worker_environment_kind '{}' does not consent to worker interface '{}' through its explicit ceiling",
                        kind, worker.worker
                    ),
                });
            }
        }
        for tool in &worker.tools {
            let Some(tool_kinds) = environment_surface.kinds_for(tool) else {
                continue;
            };
            if tool_kinds.is_empty() {
                continue;
            }
            if tool_kinds.is_disjoint(&ceiling_weks) {
                issues.push(WorkerEnvironmentRegistryIssue {
                    subject: WorkerEnvironmentRegistrySubject::Worker(worker.worker.clone()),
                    reason: format!(
                        "worker interface '{}' grants worker-environment tool '{}' but ceiling.wek intersects none of {:?}",
                        worker.worker, tool, tool_kinds
                    ),
                });
            }
        }
    }
    issues
}

pub fn validate_worker_environment_registry(
    workers: &[WorkerEnvironmentCeilingView<'_>],
    worker_environment_kinds: &[(DescriptorName, WorkerEnvKindSpec)],
    tools: &[WorkerEnvironmentToolView<'_>],
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
    interface_dispositions: &BTreeMap<InterfaceId, InterfaceRuntimeDisposition>,
) -> Result<(), String> {
    let issues = worker_environment_registry_issues(
        workers,
        worker_environment_kinds,
        tools,
        interfaces,
        interface_dispositions,
    );
    if issues.is_empty() {
        return Ok(());
    }
    Err(issues
        .into_iter()
        .map(|issue| issue.reason)
        .collect::<Vec<_>>()
        .join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kind() -> WorkerEnvKindSpec {
        serde_json::from_value(json!({
            "worker_environment_kind_id": "git_worktree",
            "substrate_family": "process_exec",
            "door_driver_refs": ["system_coder_env"],
            "tool_process_bins": ["target/debug/octopus-coder-fs-tool"],
            "worker_consent": "explicit_ceiling",
            "supported_executor_kinds": ["exec"],
            "capabilities": ["filesystem"],
            "boundary_defaults": {
                "read_root_refs": [],
                "write_root_refs": [],
                "protected_read_only_subpaths": []
            },
            "lifecycle": {"requires_release": true}
        }))
        .expect("worker_environment_kind fixture should parse")
    }

    fn door() -> InterfaceDescriptor {
        serde_json::from_value(json!({
            "id": "system_coder_env_exact",
            "interface_type": "system",
            "role": "system",
            "direction": "bidirectional",
            "source_names": ["coder_env"],
            "interface_driver_ref": "system_coder_env"
        }))
        .expect("door fixture should parse")
    }

    #[test]
    fn exact_worker_consent_and_code_world_are_descriptor_owned() {
        let worker_id = InterfaceId::new("system_worker_exact");
        let door = door();
        let worker = WorkerEnvironmentCeilingView {
            worker: &worker_id,
            wek: vec!["git_worktree"],
            tools: vec!["coder.write_file", "artifact.read"],
        };
        let tool = WorkerEnvironmentToolView {
            tool: "coder.write_file",
            interface: &door.id,
            process_bin: "target/debug/octopus-coder-fs-tool",
            worker_environment_kinds: vec!["git_worktree"],
        };
        let interfaces = [(
            DescriptorName::new("interface/system_coder_env_exact"),
            door.clone(),
        )];
        let dispositions =
            BTreeMap::from([(door.id.clone(), InterfaceRuntimeDisposition::ExternalReader)]);
        validate_worker_environment_registry(
            &[worker],
            &[(
                DescriptorName::new("worker_environment_kind/git_worktree"),
                kind(),
            )],
            &[tool],
            &interfaces,
            &dispositions,
        )
        .expect("exact worker, exact door, tool process, and code kind form bidirectionally");
    }

    #[test]
    fn declared_bins_can_precede_tools_without_weakening_actual_ownership_edges() {
        let mut spec = kind();
        spec.tool_process_bins.push("owner://packages/authored-tool".into());
        spec.door_driver_refs.push("authored_door".into());
        let kinds = [(DescriptorName::new("worker_environment_kind/git_worktree"), spec)];
        let door = door();
        let interfaces = [(DescriptorName::new("interface/system_coder_env_exact"), door.clone())];
        let dispositions = BTreeMap::from([(door.id.clone(), InterfaceRuntimeDisposition::ExternalReader)]);
        validate_worker_environment_registry(&[], &kinds, &[], &interfaces, &dispositions).unwrap();
        let worker = InterfaceId::new("system_worker_repair");
        let workers = [WorkerEnvironmentCeilingView {worker: &worker, wek: vec!["git_worktree"], tools: vec!["repair"]}];
        let mut tool = WorkerEnvironmentToolView {tool: "repair", interface: &door.id,
            process_bin: "target/debug/octopus-coder-fs-tool", worker_environment_kinds: vec!["git_worktree"]};
        validate_worker_environment_registry(&workers, &kinds, &[tool.clone()], &interfaces, &dispositions).unwrap();
        tool.process_bin = "owner://packages/undeclared";
        let issues = worker_environment_registry_issues(&workers, &kinds, &[tool], &interfaces, &dispositions);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].subject, WorkerEnvironmentRegistrySubject::Tool("repair".into()));
    }

    #[test]
    fn missing_or_ungranted_code_kind_fails_closed() {
        let worker_id = InterfaceId::new("system_worker_exact");
        let door = door();
        let worker = WorkerEnvironmentCeilingView {
            worker: &worker_id,
            wek: vec![],
            tools: vec!["coder.write_file"],
        };
        let tool = WorkerEnvironmentToolView {
            tool: "coder.write_file",
            interface: &door.id,
            process_bin: "target/debug/octopus-coder-fs-tool",
            worker_environment_kinds: vec!["git_worktree"],
        };
        let interfaces = [(
            DescriptorName::new("interface/system_coder_env_exact"),
            door.clone(),
        )];
        let dispositions =
            BTreeMap::from([(door.id.clone(), InterfaceRuntimeDisposition::ExternalReader)]);
        assert!(validate_worker_environment_registry(
            &[worker],
            &[(
                DescriptorName::new("worker_environment_kind/git_worktree"),
                kind()
            )],
            &[tool],
            &interfaces,
            &dispositions,
        )
        .is_err());

        let missing = WorkerEnvironmentToolView {
            tool: "remote.exec",
            interface: &door.id,
            process_bin: "target/debug/octopus-remote-exec-tool",
            worker_environment_kinds: vec!["remote_exec_server"],
        };
        assert!(validate_worker_environment_registry(
            &[],
            &[(
                DescriptorName::new("worker_environment_kind/git_worktree"),
                kind()
            )],
            &[missing],
            &interfaces,
            &dispositions,
        )
        .is_err());
    }
}
