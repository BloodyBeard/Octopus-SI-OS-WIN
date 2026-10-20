use octopus_descriptors::{ArtifactId, InterfaceId, MarkId, ToolId, WorkerEnvironmentKindId};
use serde::{Deserialize, Serialize};

use crate::{ArtifactVerb, CapSet, ContextGrant, CopyLineage, DenySet, InterfacePolicyOverrideLayer};

fn default_artifact_verbs() -> Vec<ArtifactVerb> {
    vec![ArtifactVerb::Read]
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerInterfaceCeiling {
    #[serde(default)]
    pub interfaces: Vec<InterfaceId>,
    #[serde(default)]
    pub tools: Vec<ToolId>,
    #[serde(default)]
    pub wek: Vec<WorkerEnvironmentKindId>,
    #[serde(default)]
    pub mark_outcomes: Vec<MarkId>,
    #[serde(default)]
    pub hard_blocked_tools: Vec<ToolId>,
    #[serde(default)]
    pub interface_policy: InterfacePolicyOverrideLayer,
    /// POLICY.md §4/§3a — the object-level artifact verbs this worker may ever
    /// exercise. Absent ⇒ read-only: a worker may always read its bound context
    /// artifacts, but change/create/delete are fail-closed until the pin grants
    /// them explicitly.
    #[serde(default = "default_artifact_verbs")]
    pub artifact_verbs: Vec<ArtifactVerb>,
}

impl Default for WorkerInterfaceCeiling {
    fn default() -> Self {
        Self {
            interfaces: Vec::new(),
            tools: Vec::new(),
            wek: Vec::new(),
            mark_outcomes: Vec::new(),
            hard_blocked_tools: Vec::new(),
            interface_policy: Default::default(),
            artifact_verbs: default_artifact_verbs(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerInterfacePolicy {
    pub id: InterfaceId,
    pub ceiling: WorkerInterfaceCeiling,
    #[serde(default)]
    pub role_artifacts: Vec<ArtifactId>,
}

impl WorkerInterfacePolicy {
    /// A worker copy admitted by its declared original is never wider than it: each
    /// ceiling axis keeps only what the original's ceiling holds, by id or original.
    pub fn narrowed_by(&self, original: &WorkerInterfacePolicy, lineage: &CopyLineage) -> WorkerInterfacePolicy {
        let held = &original.ceiling;
        let interfaces = CapSet::from_allowed(held.interfaces.iter().cloned());
        let tools = CapSet::from_allowed(held.tools.iter().cloned());
        let mut ceiling = self.ceiling.clone();
        ceiling.interfaces.retain(|interface| lineage.admits_interface(&interfaces, interface));
        ceiling.tools.retain(|tool| lineage.admits_tool(&tools, tool));
        ceiling.wek.retain(|kind| held.wek.contains(kind));
        ceiling.mark_outcomes.retain(|mark| held.mark_outcomes.contains(mark));
        ceiling.artifact_verbs.retain(|verb| held.artifact_verbs.contains(verb));
        for tool in &held.hard_blocked_tools {
            if !ceiling.hard_blocked_tools.contains(tool) {
                ceiling.hard_blocked_tools.push(tool.clone());
            }
        }
        WorkerInterfacePolicy { id: self.id.clone(), ceiling, role_artifacts: self.role_artifacts.clone() }
    }

    pub fn ceiling_grant(&self) -> ContextGrant {
        ContextGrant {
            tools: CapSet::from_allowed(self.ceiling.tools.iter().cloned()),
            interfaces: CapSet::from_allowed(self.ceiling.interfaces.iter().cloned()),
            wek: CapSet::from_allowed(self.ceiling.wek.iter().cloned()),
            mark_outcomes: CapSet::from_allowed(self.ceiling.mark_outcomes.iter().cloned()),
            hard_blocks_tools: DenySet::from_denied(
                self.ceiling.hard_blocked_tools.iter().cloned(),
            ),
            interface_policy: Default::default(),
            artifact_verbs: CapSet::from_allowed(self.ceiling.artifact_verbs.iter().cloned()),
            artifact_access: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_interface_policy_is_data_and_fails_closed_on_unknown_fields() {
        let policy: WorkerInterfacePolicy = serde_json::from_value(serde_json::json!({
            "id": "system_worker_probe",
            "ceiling": {
                "interfaces": ["world_provider_probe", "system_worker_probe"],
                "tools": ["artifact.read"],
                "wek": [],
                "mark_outcomes": [],
                "hard_blocked_tools": [],
                "interface_policy": {
                    "world_provider_probe": {
                        "available_options": ["compact", "full"],
                        "max_attempts": 3
                    }
                }
            },
            "role_artifacts": ["artifact-probe-role"]
        }))
        .expect("a worker-interface policy sidecar parses");
        let grant = policy.ceiling_grant();
        assert!(grant.tools.contains(&ToolId::new("artifact.read")));
        assert!(grant
            .interfaces
            .contains(&InterfaceId::new("system_worker_probe")));
        assert_eq!(
            policy
                .ceiling
                .interface_policy
                .get(&InterfaceId::new("world_provider_probe"))
                .and_then(|values| values.get("available_options")),
            Some(&serde_json::json!(["compact", "full"]))
        );

        assert!(
            serde_json::from_value::<WorkerInterfacePolicy>(serde_json::json!({
                "id": "system_worker_probe",
                "ceiling": {"execution_module": "octopus.worker.fc_runner"}
            }))
            .is_err(),
            "an execution-module field is not descriptor data — it fails to parse"
        );
        assert!(
            serde_json::from_value::<WorkerInterfacePolicy>(serde_json::json!({
                "id": "system_worker_probe",
                "ceiling": {},
                "auto_bind_interfaces": []
            }))
            .is_err(),
            "the dead auto_bind axis fails closed"
        );
    }
}
