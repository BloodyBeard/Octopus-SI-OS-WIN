//! A worker is a System-interface: a unique pin descriptor
//! (`registry/interface/system_worker_<name>/interface.json`) plus a
//! `policy.json` ceiling sidecar — pure data, no Rust per worker. A new worker
//! is a copied folder with a new id and edited lists (the template convention),
//! never an engine, a registry of templates, or an execution-module field.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use octopus_app_server::{AuthoredBindings, SharedBlobs, SpineRootConfig, form_worker_run_policy};
use octopus_descriptors::{
    ArtifactId, DescriptorHash, DescriptorName, InterfaceId, ToolId, WorkerEnvironmentKindId,
    loader,
};
use octopus_interface_projection::InterfaceToolOwnerMap;
use octopus_interfaces::{InterfaceDescriptor, load_worker_interface_policies};
use octopus_ledger::InMemoryBlobs;
use octopus_policy::ArtifactAccess;
use octopus_runtime::RunId;

fn registry_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry")
}

const FLASH_WORKER: &str = "system_worker_flash";

fn load_real_worker_policies()
-> std::collections::BTreeMap<InterfaceId, octopus_policy::WorkerInterfacePolicy> {
    let interface_root = registry_root().join("interface");
    let interfaces = loader::load_kind::<InterfaceDescriptor>(&interface_root)
        .expect("registry/interface must parse through the strict loader");
    load_worker_interface_policies(&interface_root, &interfaces)
        .expect("every worker pin carries a valid policy sidecar")
}

#[test]
fn the_flash_worker_pin_is_pure_descriptor_data() {
    let policies = load_real_worker_policies();
    let flash = policies
        .get(&InterfaceId::new(FLASH_WORKER))
        .expect("the flash worker pin must be on disk");
    assert_eq!(flash.id.as_str(), FLASH_WORKER);
    assert!(
        !flash.ceiling.interfaces.is_empty(),
        "the pin's ceiling declares its interface surface as data"
    );
    assert!(
        flash
            .ceiling
            .interfaces
            .iter()
            .any(|interface| interface.as_str() == "system_worker_planner"),
        "run→run reach is per-worker pins in the ceiling, never a generic orchestration interface"
    );
    assert!(
        !policies.contains_key(&InterfaceId::new("system_orchestration")),
        "the generic worker interface is dead — every pin is unique"
    );
}

#[test]
fn a_new_worker_pin_is_two_json_files_zero_rust() {
    let base = std::env::temp_dir().join(format!(
        "octopus-worker-pin-data-{}-{}",
        std::process::id(),
        line!()
    ));
    let dir = base.join("interface").join("system_worker_scout");
    std::fs::create_dir_all(&dir).expect("scratch pin dir writes");
    std::fs::write(
        dir.join("interface.json"),
        br#"{"id":"system_worker_scout","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["scout"]}"#,
    )
    .expect("scratch interface.json writes");
    std::fs::write(
        dir.join("policy.json"),
        br#"{"id":"system_worker_scout","ceiling":{"interfaces":["system_worker_scout"],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]},"role_artifacts":[]}"#,
    )
    .expect("scratch policy.json writes");

    let interface_root = base.join("interface");
    let interfaces = loader::load_kind::<InterfaceDescriptor>(&interface_root)
        .expect("the freshly-dropped pin's interface.json loads with NO Rust change");
    let policies = load_worker_interface_policies(&interface_root, &interfaces)
        .expect("the freshly-dropped pin's policy.json loads with NO Rust change");
    assert!(policies.contains_key(&InterfaceId::new("system_worker_scout")));

    std::fs::write(
        dir.join("policy.json"),
        br#"{"id":"system_worker_scout","ceiling":{"execution_module":"octopus.worker.fc_runner"}}"#,
    )
    .expect("scratch policy.json rewrites");
    assert!(
        load_worker_interface_policies(&interface_root, &interfaces).is_err(),
        "an execution-module field is not descriptor data — the pin fails to parse"
    );

    std::fs::remove_file(dir.join("policy.json")).expect("scratch policy.json removes");
    assert!(
        load_worker_interface_policies(&interface_root, &interfaces).is_err(),
        "a worker pin without a ceiling sidecar forms no grant — fail-closed at load"
    );

    let stray = base.join("interface").join("world_not_a_worker");
    std::fs::create_dir_all(&stray).expect("stray interface dir writes");
    std::fs::write(
        stray.join("interface.json"),
        br#"{"id":"world_not_a_worker","interface_type":"world","role":"probe","direction":"outbound","source_names":["probe"]}"#,
    )
    .expect("stray interface.json writes");
    std::fs::write(
        stray.join("policy.json"),
        br#"{"id":"world_not_a_worker","ceiling":{},"role_artifacts":[]}"#,
    )
    .expect("stray policy.json writes");
    std::fs::write(
        dir.join("policy.json"),
        br#"{"id":"system_worker_scout","ceiling":{},"role_artifacts":[]}"#,
    )
    .expect("scratch policy.json restores");
    let interfaces = loader::load_kind::<InterfaceDescriptor>(&interface_root)
        .expect("the stray descriptor itself parses");
    assert!(
        load_worker_interface_policies(&interface_root, &interfaces).is_err(),
        "a policy sidecar on a non-worker interface is descriptor drift — fail-closed"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Pure transformers carry no code-room (constitution §Worker Environments: a run
/// whose interface only transforms artifacts needs NO worker environment beyond the
/// system room for its own record edits). The boot over the REAL registry must
/// hold with non-coder ceilings free of `git_worktree` and the coder door.
#[tokio::test]
async fn non_coder_pins_hold_no_worktree_and_the_real_registry_boots() {
    let policies = load_real_worker_policies();
    for pin in [
        "system_worker_flash",
        "system_worker_memory",
        "system_worker_planner",
        "system_worker_sovereign",
        "system_worker_startup_manager",
    ] {
        let policy = policies
            .get(&InterfaceId::new(pin))
            .unwrap_or_else(|| panic!("worker pin '{pin}' must be on disk"));
        assert!(
            !policy
                .ceiling
                .wek
                .iter()
                .any(|wek| wek.as_str() == "git_worktree"),
            "'{pin}' is not a coder — a git worktree does not belong in its ceiling"
        );
        assert!(
            !policy
                .ceiling
                .interfaces
                .iter()
                .any(|interface| interface.as_str() == "system_coder_env"),
            "'{pin}' holds no coder door — record edits travel the system room only"
        );
        assert!(
            !policy
                .ceiling
                .tools
                .iter()
                .any(|tool| tool.as_str() == "coder.write_file"),
            "'{pin}' authors files through the generic file.write, never the coder family"
        );
        assert!(
            policy
                .ceiling
                .wek
                .iter()
                .any(|wek| wek.as_str() == "system_room")
                && policy
                    .ceiling
                    .tools
                    .iter()
                    .any(|tool| tool.as_str() == "file.write"),
            "'{pin}' holds the system room + file.write — the one record-edit path"
        );
    }

    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = std::env::temp_dir().join(format!(
        "octopus-non-coder-boot-{}-{}",
        std::process::id(),
        line!()
    ));
    support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_non_coder_boot"),
        blobs,
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("boot validation holds with worktree-free non-coder ceilings");
}

#[tokio::test]
async fn the_formation_pin_carries_the_worker_pin_ceiling_hash() {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root = std::env::temp_dir().join(format!(
        "octopus-worker-pin-boot-{}-{}",
        std::process::id(),
        line!()
    ));
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new("run_worker_pin_boot"),
        blobs,
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded spine boots over the worker pins");

    let flash = InterfaceId::new(FLASH_WORKER);
    let policy = embedded
        .worker_interface_policies
        .get(&flash)
        .expect("the flash worker pin is loaded in the embedded registry data");
    let no_interfaces: Vec<InterfaceId> = Vec::new();
    let tool_ownership =
        InterfaceToolOwnerMap::from_pairs(embedded.tool_descriptors_loaded.iter().map(
            |(_, descriptor)| {
                (
                    ToolId::new(descriptor.id.as_str()),
                    descriptor.interface.clone(),
                )
            },
        ));
    let tool_supported_weks: BTreeMap<ToolId, BTreeSet<WorkerEnvironmentKindId>> = embedded
        .tool_descriptors_loaded
        .iter()
        .map(|(_, descriptor)| {
            (
                ToolId::new(descriptor.id.as_str()),
                descriptor
                    .supported_worker_environment_kinds
                    .iter()
                    .cloned()
                    .collect(),
            )
        })
        .collect();
    let no_artifact_access: BTreeMap<ArtifactId, ArtifactAccess> = BTreeMap::new();
    let authored = AuthoredBindings {
        interfaces: &no_interfaces,
        tool_ownership: &tool_ownership,
        tool_supported_weks: &tool_supported_weks,
        artifact_access: &no_artifact_access,
    };
    let formed = form_worker_run_policy(
        &flash,
        policy,
        &embedded.ee_tree,
        &embedded.existence_ee_id,
        &embedded.formation_source_hashes,
        &authored,
        &octopus_policy::CopyLineage::default(),
    )
    .expect("the flash grant forms at the bootstrap root");

    let pinned = formed
        .pin
        .inputs
        .get(&DescriptorName::new(format!(
            "registry_interface_root/{FLASH_WORKER}/policy.json"
        )))
        .expect("the formation pin names the worker pin descriptor");
    assert_eq!(
        pinned,
        &DescriptorHash::of_canonical_json(
            &serde_json::to_value(policy).expect("the policy sidecar serializes")
        ),
        "the pin carries the worker pin ceiling's REAL content hash (the boundary the replay re-forms against)"
    );
    assert!(
        formed.pin.inputs.len() >= 2,
        "the worker-run fold pins at least the worker pin and the EE-template primitives; got {:?}",
        formed.pin.inputs.keys().collect::<Vec<_>>()
    );
}
