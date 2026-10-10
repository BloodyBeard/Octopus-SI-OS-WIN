#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use octopus_descriptors::InterfaceId;
use octopus_ee::{EeInstance, EeTemplateDescriptor};
use octopus_interface_transport::spawn_life_config_of;
use octopus_interfaces::InterfaceDescriptor;
use octopus_policy::{form_policy, FormationPin, WorkerInterfacePolicy};
use octopus_runtime::run_record::RunRecord;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..")
}

fn read_json(path: impl AsRef<Path>) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).expect("JSON file reads"))
        .expect("JSON file parses")
}

fn install_interface_owner(
    repository_root: &Path,
    interface_root: &Path,
    owner: &str,
    files: &[&str],
) {
    let source = repository_root.join("registry/interface").join(owner);
    let target = interface_root.join(owner);
    std::fs::create_dir_all(&target).expect("interface owner directory creates");
    for file in files {
        std::fs::copy(source.join(file), target.join(file)).expect("interface owner file installs");
    }
}

fn assert_exact_descriptor_copy(
    actual: &serde_json::Value,
    owner: &serde_json::Value,
    exact_id: &str,
) {
    assert_eq!(actual["id"], exact_id);
    assert!(actual.get("reference").is_none());
    let mut normalized = actual.clone();
    normalized["id"] = owner["id"].clone();
    assert_eq!(&normalized, owner);
}

#[test]
fn telegram_gateway_authors_one_private_interface_topology_per_chat() {
    let dir = tempfile::tempdir().expect("temp root");
    let registry_root = dir.path().join("registry");
    let interface_root = registry_root.join("interface");
    let ee_template_root = registry_root.join("ee_template");
    let tool_root = registry_root.join("tool");
    let worker_environment_kind_root = registry_root.join("worker_environment_kind");
    let runs_root = dir.path().join("runs");
    let ee_root = dir.path().join("ee-instances");
    let var_dir = dir.path().join("home/interfaces/world_telegram");
    let blobs_dir = dir.path().join("blobs");
    let movements = dir.path().join("movements.jsonl");
    let index = dir.path().join("ledger-index.sqlite");
    for path in [
        &interface_root,
        &ee_template_root,
        &tool_root,
        &worker_environment_kind_root,
        &runs_root,
        &ee_root,
        &var_dir,
        &blobs_dir,
    ] {
        std::fs::create_dir_all(path).expect("test directory creates");
    }
    std::fs::set_permissions(&var_dir, std::fs::Permissions::from_mode(0o700))
        .expect("interface private state directory mode sets");
    std::fs::write(&movements, b"").expect("movement log creates");

    let repository_root = repo_root();
    install_interface_owner(
        &repository_root,
        &interface_root,
        "world_telegram",
        &[
            "interface.json",
            "life.json",
            "octopus_python_boot.py",
            "telegram_life.py",
            "telegram_rendering.py",
            "octopus_content_hash.py",
            "requirements-content-hash.lock",
        ],
    );
    for owner in [
        "world_provider_codex",
        "system_interface_authoring",
        "system_room",
    ] {
        install_interface_owner(
            &repository_root,
            &interface_root,
            owner,
            &["interface.json", "life.json"],
        );
    }
    install_interface_owner(
        &repository_root,
        &interface_root,
        "system_worker_telegram",
        &["interface.json"],
    );
    for template in ["operator_interaction", "existence"] {
        let source = repository_root
            .join("registry/ee_template")
            .join(template)
            .join("ee_template.json");
        let target = ee_template_root.join(template);
        std::fs::create_dir_all(&target).expect("EE template owner directory creates");
        std::fs::copy(source, target.join("ee_template.json")).expect("EE template owner installs");
    }
    let kind_target = worker_environment_kind_root.join("system_room");
    std::fs::create_dir_all(&kind_target).expect("worker environment kind directory creates");
    std::fs::copy(
        repository_root
            .join("registry/worker_environment_kind/system_room/worker_environment_kind.json"),
        kind_target.join("worker_environment_kind.json"),
    )
    .expect("worker environment kind installs");
    let tool_target = tool_root.join("file.write");
    std::fs::create_dir_all(&tool_target).expect("tool owner directory creates");
    std::fs::copy(
        repository_root.join("registry/tool/file.write/tool.json"),
        tool_target.join("tool.json"),
    )
    .expect("tool owner installs");
    std::fs::write(
        ee_root.join("ee_existence.json"),
        serde_json::to_vec(&serde_json::json!({
            "ee_id": "ee_existence",
            "template_name": "existence",
            "parent_ee_id": null,
            "goal": "Exist"
        }))
        .expect("parent EE encodes"),
    )
    .expect("parent EE installs");

    let roots = serde_json::json!({
        "registry_interface_root": interface_root,
        "registry_ee_template_root": ee_template_root,
        "registry_tool_root": tool_root,
        "registry_worker_environment_kind_root": worker_environment_kind_root,
        "run_records_root": runs_root,
        "ee_instances_root": ee_root,
    });
    let script = r#"
import importlib.util, json, os, sys
p = 'registry/interface/world_telegram/telegram_life.py'
sys.path.insert(0, os.path.dirname(p))
s = importlib.util.spec_from_file_location('telegram_life', p)
m = importlib.util.module_from_spec(s)
s.loader.exec_module(m)
from octopus_content_hash import octopus_content_hash_reference
m.octopus_content_hash = octopus_content_hash_reference
c = m.load_config()
o = '91b7d302531ff85c1205f40a'
b = {
  'chat_id': -1004242424242,
  'thread_id': 77,
  'opaque_id': o,
  'interface': 'world_telegram_' + o,
  'provider_interface': 'world_provider_codex_telegram_' + o,
  'interface_authoring_interface': 'system_interface_authoring_telegram_' + o,
  'worker_interface': 'system_worker_telegram_' + o,
  'worker_environment_interface': c['private_run']['id_prefixes']['worker_environment'] + o,
  'file_write_tool': c['private_run']['id_prefixes']['file_write_tool'] + o,
  'run': 'run_telegram_' + o,
  'ee': 'ee_telegram_' + o,
  'secret': 'c' * 64,
  'host': '127.0.0.1',
  'port': 31337,
  'status': 'planned',
}
for write in m._binding_policy_writes(c, b):
    with open(write['path'], 'rb') as handle:
        current = handle.read()
    assert m.octopus_content_hash(current) == write['expected_before_hash']
    with open(write['path'], 'w', encoding='utf-8') as handle:
        handle.write(write['content'])
assert m._binding_policy_writes(c, b) == []
for path, body in m._binding_topology(c, b).items():
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'x', encoding='utf-8') as handle:
        handle.write(body)
print(json.dumps(b, sort_keys=True))
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .current_dir(repo_root())
        .env("OCTOPUS_INTERFACE_ID", "world_telegram")
        .env(
            "OCTOPUS_INTERFACE_DIR",
            repo_root().join("registry/interface/world_telegram"),
        )
        .env("OCTOPUS_VAR_DIR", &var_dir)
        .env("OCTOPUS_MOVEMENTS_JSONL", &movements)
        .env("OCTOPUS_BLOBS_DIR", &blobs_dir)
        .env("OCTOPUS_LEDGER_INDEX_SQLITE", &index)
        .env("OCTOPUS_ROOT_SLOTS_JSON", roots.to_string())
        .env(
            "OCTOPUS_CONTENT_HASH_RUNTIME",
            std::env::current_exe().expect("current test executable resolves"),
        )
        .output()
        .expect("telegram topology author runs");
    assert!(
        output.status.success(),
        "telegram topology author failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let binding: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("binding output is JSON");
    let child_id = binding["interface"].as_str().expect("child interface id");
    let provider_id = binding["provider_interface"]
        .as_str()
        .expect("provider interface id");
    let authoring_source_id = binding["interface_authoring_interface"]
        .as_str()
        .expect("authoring source interface id");
    let authoring_id = binding["worker_environment_interface"]
        .as_str()
        .expect("worker environment interface id");
    let worker_id = binding["worker_interface"]
        .as_str()
        .expect("worker interface id");
    let run_id = binding["run"].as_str().expect("run id");
    let ee_id = binding["ee"].as_str().expect("ee id");

    let child_path = interface_root.join(child_id).join("interface.json");
    let child_raw = read_json(&child_path);
    let telegram_owner =
        read_json(repository_root.join("registry/interface/world_telegram/interface.json"));
    assert_exact_descriptor_copy(&child_raw, &telegram_owner, child_id);
    let child: InterfaceDescriptor = serde_json::from_value(child_raw)
        .expect("exact child interface parses without a reference");
    assert_eq!(child.interface_driver_ref, "telegram_life");
    assert!(
        spawn_life_config_of(&registry_root, &InterfaceId::new(child_id))
            .expect("child life resolves")
            .is_some()
    );

    let child_config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(interface_root.join(child_id).join("config.json"))
            .expect("child config reads"),
    )
    .expect("child config parses");
    assert_eq!(child_config["mode"], "binding");
    assert_eq!(child_config["binding"]["interface"], child_id);
    assert_eq!(child_config["binding"]["run"], run_id);
    assert_ne!(child_id, "world_telegram");
    assert!(!child_id.contains("4242424242"));

    let provider_path = interface_root.join(provider_id).join("interface.json");
    let provider_raw = read_json(&provider_path);
    assert!(provider_raw.get("reference").is_none());
    assert_eq!(provider_raw["id"], provider_id);
    let provider_owner =
        read_json(repository_root.join("registry/interface/world_provider_codex/interface.json"));
    let mut normalized_provider = provider_raw.clone();
    normalized_provider["id"] = provider_owner["id"].clone();
    let normalized_metadata = normalized_provider["interface_driver_config"]["private_config"]
        .as_object_mut()
        .expect("provider invocation metadata object");
    let owner_metadata = provider_owner["interface_driver_config"]["private_config"]
        .as_object()
        .expect("provider owner invocation metadata object");
    for key in [
        "model",
        "reasoning_effort",
        "context_output_labels",
        "run_authoring_profile",
    ] {
        normalized_metadata.insert(
            key.to_string(),
            owner_metadata
                .get(key)
                .expect("provider owner override field")
                .clone(),
        );
    }
    assert_eq!(normalized_provider, provider_owner);
    let provider: InterfaceDescriptor = serde_json::from_value(provider_raw)
        .expect("exact provider interface parses without a reference");
    let provider_config = provider.interface_driver_config.expect("provider config");
    assert!(!provider_config.private_config.contains_key("run_authoring"));
    let run_authoring_profile = provider_config.private_config["run_authoring_profile"]
        .as_object()
        .expect("provider run_authoring_profile object");
    assert_eq!(
        run_authoring_profile
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "file_write_label",
            "file_write_result_label",
            "file_write_tool",
            "result_timeout_ms",
        ])
    );

    let worker_policy: WorkerInterfacePolicy = serde_json::from_slice(
        &std::fs::read(interface_root.join(worker_id).join("policy.json"))
            .expect("worker policy reads"),
    )
    .expect("worker policy parses");
    assert_eq!(worker_policy.id.as_str(), worker_id);
    assert_eq!(
        worker_policy
            .ceiling
            .interfaces
            .iter()
            .map(InterfaceId::as_str)
            .collect::<Vec<_>>(),
        vec![
            child_id,
            provider_id,
            authoring_source_id,
            authoring_id,
            worker_id,
        ]
    );

    for (exact_id, owner_id) in [
        (authoring_source_id, "system_interface_authoring"),
        (authoring_id, "system_room"),
        (worker_id, "system_worker_telegram"),
    ] {
        let actual = read_json(interface_root.join(exact_id).join("interface.json"));
        let owner = read_json(
            repository_root
                .join("registry/interface")
                .join(owner_id)
                .join("interface.json"),
        );
        assert_exact_descriptor_copy(&actual, &owner, exact_id);
    }

    for (exact_id, owner_id) in [
        (provider_id, "world_provider_codex"),
        (authoring_source_id, "system_interface_authoring"),
        (authoring_id, "system_room"),
    ] {
        assert_eq!(
            read_json(interface_root.join(exact_id).join("life.json")),
            read_json(
                repository_root
                    .join("registry/interface")
                    .join(owner_id)
                    .join("life.json"),
            )
        );
        assert!(
            spawn_life_config_of(&registry_root, &InterfaceId::new(exact_id))
                .expect("exact interface life resolves")
                .is_some()
        );
    }
    assert!(
        spawn_life_config_of(&registry_root, &InterfaceId::new(worker_id))
            .expect("worker exact interface resolves")
            .is_none()
    );

    let opaque_id = binding["opaque_id"].as_str().expect("binding opaque id");
    let exact_template_id = format!("operator_interaction_telegram_{opaque_id}");
    let exact_template_path = ee_template_root
        .join(&exact_template_id)
        .join("ee_template.json");
    let exact_template_raw = read_json(&exact_template_path);
    let exact_template: EeTemplateDescriptor =
        serde_json::from_value(exact_template_raw.clone()).expect("exact EE template parses");
    assert_eq!(exact_template.id.as_str(), exact_template_id);
    let exact_allowed = exact_template
        .allowed_interfaces
        .as_ref()
        .expect("exact EE interface allowance");
    for interface in [
        child_id,
        provider_id,
        authoring_source_id,
        authoring_id,
        worker_id,
    ] {
        assert!(exact_allowed.iter().any(|allowed| allowed == interface));
    }
    let operator_owner = read_json(
        repository_root.join("registry/ee_template/operator_interaction/ee_template.json"),
    );
    let mut normalized_template = exact_template_raw;
    normalized_template["id"] = operator_owner["id"].clone();
    normalized_template["allowed_interfaces"] = operator_owner["allowed_interfaces"].clone();
    assert_eq!(normalized_template, operator_owner);

    let parent_template_raw = read_json(ee_template_root.join("existence/ee_template.json"));
    let parent_owner =
        read_json(repository_root.join("registry/ee_template/existence/ee_template.json"));
    let parent_owner_allowed = parent_owner["allowed_interfaces"]
        .as_array()
        .expect("parent owner interface allowance");
    assert_eq!(
        &parent_template_raw["allowed_interfaces"]
            .as_array()
            .expect("merged parent interface allowance")[..parent_owner_allowed.len()],
        parent_owner_allowed
    );
    let parent_template: EeTemplateDescriptor = serde_json::from_value(parent_template_raw)
        .expect("parent EE template parses after exact policy merge");
    let parent_allowed = parent_template
        .allowed_interfaces
        .as_ref()
        .expect("parent EE interface allowance");
    for interface in [
        child_id,
        provider_id,
        authoring_source_id,
        authoring_id,
        worker_id,
    ] {
        assert!(parent_allowed.iter().any(|allowed| allowed == interface));
    }
    assert!(parent_template
        .allowed_child_ee_templates
        .iter()
        .any(|template| template == &exact_template_id));
    let formed = form_policy(
        worker_policy.ceiling_grant(),
        &[
            parent_template.as_policy_layer(),
            exact_template.as_policy_layer(),
        ],
        &[],
        FormationPin::default(),
    );
    for interface in [
        child_id,
        provider_id,
        authoring_source_id,
        authoring_id,
        worker_id,
    ] {
        assert!(formed
            .context
            .interfaces
            .contains(&InterfaceId::new(interface)));
    }
    assert_eq!(
        std::fs::read(
            worker_environment_kind_root.join("system_room/worker_environment_kind.json")
        )
        .expect("installed system_room descriptor reads"),
        std::fs::read(
            repository_root
                .join("registry/worker_environment_kind/system_room/worker_environment_kind.json")
        )
        .expect("system_room owner descriptor reads")
    );

    let run_bytes =
        std::fs::read(runs_root.join(format!("{run_id}.json"))).expect("private run reads");
    let run = RunRecord::from_bytes(&run_bytes).expect("private run parses");
    assert_eq!(run.worker.as_str(), worker_id);
    assert_eq!(run.ee.as_str(), ee_id);
    assert_eq!(run.active_checkpoints.len(), 4);
    assert_eq!(
        run.interface_state[&InterfaceId::new(provider_id)]["frontier_checkpoint_ids"]
            .as_array()
            .expect("provider frontier ids")
            .len(),
        3
    );

    let ee: EeInstance = serde_json::from_slice(
        &std::fs::read(ee_root.join(format!("{ee_id}.json"))).expect("private EE reads"),
    )
    .expect("private EE parses");
    assert_eq!(ee.ee_id.as_str(), ee_id);
    assert_eq!(ee.template.as_str(), exact_template_id);
    assert_eq!(
        ee.parent_ee.as_ref().map(|id| id.as_str()),
        Some("ee_existence")
    );
}
