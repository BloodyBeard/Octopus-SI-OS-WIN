use std::collections::BTreeMap;
use std::path::Path;

use octopus_descriptors::InterfaceId;
use octopus_policy::WorkerInterfacePolicy;

use crate::{InterfaceDescriptor, InterfaceType};

/// A worker System-interface's ceiling reference to an interface that has no
/// registry folder. A typed dangling FACT, never boot poison — the boot lives,
/// and any emission toward the absent interface refuses at send time through
/// the delivery protocol.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DanglingWorkerInterfaceRef {
    pub worker: InterfaceId,
    pub interface: InterfaceId,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerInterfacePolicyScan {
    pub policies: BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    pub refusals: BTreeMap<InterfaceId, String>,
}

pub fn interface_is_worker_pin(descriptor: &InterfaceDescriptor) -> bool {
    descriptor.interface_type == InterfaceType::System && descriptor.role.0 == "worker"
}

/// Load every worker System-interface pin's `policy.json` sidecar, fail-closed:
/// - every `system` interface with `role = "worker"` MUST carry a policy sidecar
///   (a worker pin without a ceiling can form no grant);
/// - a policy sidecar on a non-worker interface is descriptor drift;
/// - the sidecar's `id` must match its folder's interface id.
pub fn load_worker_interface_policies(
    interface_root: &Path,
    interfaces: &[(octopus_descriptors::DescriptorName, InterfaceDescriptor)],
) -> Result<BTreeMap<InterfaceId, WorkerInterfacePolicy>, String> {
    let scan = scan_worker_interface_policies(interface_root, interfaces);
    if scan.refusals.is_empty() {
        return Ok(scan.policies);
    }
    Err(scan
        .refusals
        .iter()
        .map(|(interface, reason)| format!("{}: {reason}", interface.as_str()))
        .collect::<Vec<_>>()
        .join("; "))
}

pub fn scan_worker_interface_policies(
    interface_root: &Path,
    interfaces: &[(octopus_descriptors::DescriptorName, InterfaceDescriptor)],
) -> WorkerInterfacePolicyScan {
    let mut policies = BTreeMap::new();
    let mut refusals = BTreeMap::new();
    for (_name, descriptor) in interfaces {
        let sidecar = interface_root
            .join(descriptor.id.as_str())
            .join("policy.json");
        let is_worker = interface_is_worker_pin(descriptor);
        let bytes = match std::fs::read(&sidecar) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if is_worker {
                    refusals.insert(
                        descriptor.id.clone(),
                        format!(
                            "worker interface has no policy.json sidecar at {}; a worker pin without a ceiling forms no grant",
                            sidecar.display()
                        ),
                    );
                }
                continue;
            }
            Err(e) => {
                refusals.insert(
                    descriptor.id.clone(),
                    format!(
                        "worker interface policy sidecar {} is unreadable: {e}",
                        sidecar.display()
                    ),
                );
                continue;
            }
        };
        if !is_worker {
            refusals.insert(
                descriptor.id.clone(),
                "interface carries a policy.json worker sidecar but is not a system worker interface (interface_type=system, role=worker)".to_string(),
            );
            continue;
        }
        let policy: WorkerInterfacePolicy = match serde_json::from_slice(&bytes) {
            Ok(policy) => policy,
            Err(error) => {
                refusals.insert(
                    descriptor.id.clone(),
                    format!("{}: {error}", sidecar.display()),
                );
                continue;
            }
        };
        if policy.id != descriptor.id {
            refusals.insert(
                descriptor.id.clone(),
                format!(
                    "{}: identity mismatch — the folder pins interface '{}' but the policy declares '{}'",
                    sidecar.display(),
                    descriptor.id.as_str(),
                    policy.id.as_str()
                ),
            );
            continue;
        }
        if policies.insert(policy.id.clone(), policy).is_some() {
            refusals.insert(
                descriptor.id.clone(),
                "duplicate worker interface policy".to_string(),
            );
        }
    }
    WorkerInterfacePolicyScan { policies, refusals }
}

/// Surface every ceiling reference to an interface absent from the loaded
/// registry as typed dangling boot evidence.
pub fn dangling_worker_interface_refs(
    policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    interfaces: &[(octopus_descriptors::DescriptorName, InterfaceDescriptor)],
) -> Vec<DanglingWorkerInterfaceRef> {
    let known: std::collections::BTreeSet<&str> = interfaces
        .iter()
        .map(|(_, descriptor)| descriptor.id.as_str())
        .collect();
    let mut dangling = Vec::new();
    for (worker, policy) in policies {
        for interface in &policy.ceiling.interfaces {
            if !known.contains(interface.as_str()) {
                dangling.push(DanglingWorkerInterfaceRef {
                    worker: worker.clone(),
                    interface: interface.clone(),
                });
            }
        }
    }
    dangling
}
