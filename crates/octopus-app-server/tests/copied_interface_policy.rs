use serde_json::{json, Value};
use std::path::Path;
fn put(root: &Path, path: &str, value: Value) {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
}
#[test]
fn copied_interface_must_keep_the_original_policy_reducer_in_a_real_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let registry = root.join("registry");
    let ees = root.join("ees");
    std::fs::create_dir(&ees).unwrap();
    for kind in [
        "interface",
        "artifact",
        "mark",
        "verifier",
        "tool",
        "worker_environment_kind",
        "ee_template",
    ] {
        std::fs::create_dir_all(registry.join(kind)).unwrap();
    }
    put(
        &registry,
        "interface/world_original/interface.json",
        json!({"id":"world_original","interface_type":"world","role":"provider","interface_driver_ref":"audit","direction":"bidirectional","source_names":["probe"],"interface_policy":{"variables":{"web_search_modes":{"reduce":"intersection","default":["disabled","live"]}},"values":{"web_search_modes":["disabled"]}}}),
    );
    put(
        &registry,
        "interface/world_copy/interface.json",
        json!({"id":"world_copy","copy_of":"world_original","interface_type":"world","role":"provider","interface_driver_ref":"audit","direction":"bidirectional","source_names":["probe"],"interface_policy":{"variables":{"web_search_modes":{"reduce":"deny_union","default":["live"]}},"values":{}}}),
    );
    put(
        &registry,
        "interface/system_worker_probe/interface.json",
        json!({"id":"system_worker_probe","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["probe"]}),
    );
    put(
        &registry,
        "interface/system_worker_probe/policy.json",
        json!({"id":"system_worker_probe","ceiling":{"interfaces":["world_copy"],"tools":[],"wek":[],"mark_outcomes":[],"artifact_verbs":[]},"role_artifacts":[]}),
    );
    put(
        &registry,
        "ee_template/existence/ee_template.json",
        json!({"id":"existence","goal_class":"existence","bootstrap_root":true,"allowed_interfaces":["system_worker_probe","world_original"],"allowed_worker_environment_kinds":[],"allowed_child_ee_templates":[],"allowed_root_slots":[]}),
    );
    put(
        &ees,
        "ee_existence.json",
        json!({"ee_id":"ee_existence","template_name":"existence","parent_ee_id":null,"goal":"audit"}),
    );
    let record=serde_json::from_value(json!({"id":"audit","ee":"ee_existence","worker":"system_worker_probe","trace":"audit","status":"idle","interface_state":{},"checkpoints":[{"id":"in","run":"audit","epoch":0,"interface_binding":{"interface":"world_copy","config":{"artifact_policy":{"mode":"all"}}},"outbound_interface_bindings":[],"artifact_bindings":[],"required_verified_artifacts":[],"trace":"audit"}],"active_checkpoints":["in"]})).unwrap();
    let error = octopus_app_server::spine_root::inspect_registry_run_grant(
        &registry,
        &ees,
        &record,
        &octopus_descriptors::CheckpointId::new("in"),
    )
    .unwrap_err();
    assert!(error.contains("changes the reducer"), "{error}");
    let copy_path = "interface/world_copy/interface.json";
    let mut copy: Value =
        serde_json::from_slice(&std::fs::read(registry.join(copy_path)).unwrap()).unwrap();
    copy["interface_policy"]["variables"]["web_search_modes"]["reduce"] = json!("intersection");
    copy["interface_policy"]["variables"]["web_search_modes"]["default"] =
        json!(["disabled", "live"]);
    put(&registry, copy_path, copy.clone());
    let grant = octopus_app_server::spine_root::inspect_registry_run_grant(
        &registry,
        &ees,
        &record,
        &octopus_descriptors::CheckpointId::new("in"),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(grant).unwrap()["interface_policy"]["world_copy"]["web_search_modes"],
        json!(["disabled"])
    );
    let original_path = "interface/world_original/interface.json";
    let mut original: Value =
        serde_json::from_slice(&std::fs::read(registry.join(original_path)).unwrap()).unwrap();
    original["interface_policy"]["values"]["web_search_modes"] = json!(["disabled", "live"]);
    put(&registry, original_path, original);
    let worker_path = "interface/system_worker_probe/policy.json";
    let mut worker: Value =
        serde_json::from_slice(&std::fs::read(registry.join(worker_path)).unwrap()).unwrap();
    worker["ceiling"]["interfaces"] = json!(["world_copy", "world_original"]);
    worker["ceiling"]["interface_policy"] = json!({"world_copy":{"web_search_modes":["live"]}, "world_original":{"web_search_modes":["disabled"]}});
    put(&registry, worker_path, worker);
    let grant = octopus_app_server::spine_root::inspect_registry_run_grant(
        &registry,
        &ees,
        &record,
        &octopus_descriptors::CheckpointId::new("in"),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(grant).unwrap()["interface_policy"]["world_copy"]["web_search_modes"],
        json!([])
    );
    copy["interface_policy"]["variables"] = json!({});
    put(&registry, copy_path, copy);
    let error = octopus_app_server::spine_root::inspect_registry_run_grant(
        &registry,
        &ees,
        &record,
        &octopus_descriptors::CheckpointId::new("in"),
    )
    .unwrap_err();
    assert!(error.contains("omits original policy variable"), "{error}");
}
