use octopus_operator_chat_client::run_composition::*;
use octopus_runtime::run_record::RunRecord;
use serde_json::{Value, json};

#[path = "support/composition_fixture.rs"]
mod support;
use support::{fixture, write};

#[test]
fn selected_model_and_reasoning_effort_preserve_service_tier() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    for tier in ["priority", "ultrafast", "default"] {
        request.service_tier = Some(tier.into());
        let plan = plan_run_composition(&request).unwrap();
        assert_eq!(plan.model, request.model);
        assert_eq!(plan.reasoning_effort, request.reasoning_effort);
        assert_eq!(plan.service_tier.as_deref(), Some(tier));
        let provider: Value = serde_json::from_str(&plan.descriptor_writes.iter()
            .find(|write| write.tag == format!("interface:{}", plan.provider_interface_id)).unwrap().content).unwrap();
        assert_eq!(provider["interface_driver_config"]["private_config"]["service_tier"], tier);
        let retained: RunCompositionPlan = serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
        assert_eq!(retained.service_tier, plan.service_tier);
    }
    for tier in ["", " priority", "priority\t"] {
        request.service_tier = Some(tier.into());
        assert!(plan_run_composition(&request).unwrap_err().contains("service tier"));
    }
    request.service_tier = None;
    let plan = plan_run_composition(&request).unwrap();
    assert_eq!(plan.service_tier, None);
}

#[test]
fn private_system_composition_is_fresh_exact_and_checked_under_policy() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    let parent_before = request.admitted_parent_record.clone();
    let plan = plan_run_composition(&request).unwrap();
    assert!(
        plan.required_binding_updates[0]
            .required_artifact_bindings
            .iter()
            .any(|binding| binding.id == "role-system_worker_template")
    );
    let retained: RunCompositionPlan =
        serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    assert_eq!(retained.run_write.content, plan.run_write.content);
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        serde_json::to_value(plan_run_composition(&request).unwrap()).unwrap()
    );
    assert_eq!(request.admitted_parent_record, parent_before);
    assert!(!request.roots.run_records_root.exists());
    assert!(!request.roots.interface_private_state_root.exists());
    assert_eq!(
        plan.receive_interface_id,
        request.communication.receive_interface_id
    );
    assert_ne!(
        plan.worker_interface_id,
        request.worker_template_interface_id
    );
    assert!(
        plan.descriptor_writes
            .iter()
            .all(|write| write.expected_before_hash.is_none() && !write.path.exists())
    );
    assert!(
        plan.descriptor_writes
            .iter()
            .all(|write| write.path.file_name().unwrap() != "octopus_os.json")
    );
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    assert_eq!(record.interface_state.len(), 1);
    let templates =
        record.interface_state[&plan.provider_interface_id.clone().into()]["run_authoring_binding"]
            ["checkpoints"]
            .as_array()
            .unwrap();
    let call = templates.iter().find(|row| row["role"] == "authoring_call").unwrap();
    assert_eq!(call["required_verified_artifacts"], json!([{
        "mark":request.authoring.tool_call_mark,
        "artifact_label":request.authoring.tool_call_label,
        "verifier":request.authoring.tool_call_verifier,
        "params":{"tools":[plan.authoring.tool_id],"metadata_matches":[{
            "payload":"/arguments/expected_before_hash","metadata":"/runtime/run_record_hash"
        }]}
    }]));
    assert!(record.checkpoints.iter().any(|cp| serde_json::to_value(&cp.required_verified_artifacts).unwrap() == call["required_verified_artifacts"]));
    assert!(templates.iter().filter(|row| row["role"] == "provider_tool_output")
        .all(|row| row["required_verified_artifacts"][0]["params"].get("metadata_matches").is_none()));
    assert!(
        templates
            .iter()
            .any(
                |row| row["interface_binding"]["interface"] == plan.receive_interface_id
                    && row["role"] == "stable"
                    && row["include_context"] == true
            )
    );
    assert_eq!(
        record
            .checkpoints
            .iter()
            .filter(|cp| cp
                .inbound_interface()
                .is_some_and(|id| id.as_str() == plan.receive_interface_id))
            .count(),
        1
    );
    assert!(!plan.run_write.content.contains("MUST NOT COPY"));
    assert!(!plan.run_write.content.contains("private-transcript"));
    assert!(!plan.run_write.content.contains("conversation"));
    assert!(!plan.run_write.content.contains("world_operator_chat"));
    assert!(
        !plan
            .run_write
            .content
            .contains("role-system_worker_unselected")
    );
    let input = record
        .checkpoints
        .iter()
        .find(|cp| {
            cp.inbound_interface()
                .is_some_and(|id| id.as_str() == plan.receive_interface_id)
        })
        .unwrap();
    assert!(
        input
            .artifact_bindings
            .iter()
            .any(
                |binding| binding.id.as_str() == "role-system_worker_template"
                    && binding.access.read
            )
    );
    assert!(
        input
            .artifact_bindings
            .iter()
            .any(
                |binding| binding.id.as_str() == "role-system_worker_parent" && binding.access.read
            )
    );
    assert_eq!(plan.seed.payload, request.seed.payload);
    assert_ne!(
        plan.authoring.capability_tools["file.write"],
        plan.authoring.tool_id
    );
    let writer: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/tool.json", plan.authoring.tool_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        writer["process"]["authority"]["write_argument_paths"][0]["within_root_slots"],
        json!(["run_records_root"])
    );
    assert_eq!(
        plan.required_binding_updates[0]
            .outbound_binding
            .interface
            .as_str(),
        plan.receive_interface_id
    );
    assert!(
        plan.required_policy_updates
            .iter()
            .any(|update| update.write.path.ends_with("ee_existence.json"))
    );
    for update in &plan.required_policy_updates {
        assert_eq!(
            update.write.expected_before_hash.as_ref().unwrap(),
            &blake3::hash(&std::fs::read(&update.write.path).unwrap())
                .to_hex()
                .to_string()
        );
    }
    let provider: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/interface.json", plan.provider_interface_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        provider["interface_policy"]["values"]["models"],
        json!(["fixture-a"])
    );
    for update in &plan.required_policy_updates {
        std::fs::write(&update.write.path, &update.write.content).unwrap();
    }
    for write in &plan.descriptor_writes {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(&write.path, &write.content).unwrap();
    }
    let topology = octopus_world_terminal_client::form_capability_topology(
        &request.roots.registry_root,
        &octopus_world_terminal_client::CapabilityFormationRequest::from_live_chain(
            plan.worker_interface_id.clone(),
            octopus_world_terminal_client::resolve_live_ee_policy_chain(
                &request.roots.ee_instances_root,
                &request.ee_id.clone().into(),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(
        topology.interface_policies[&plan.provider_interface_id.clone().into()].context["models"],
        json!(["fixture-a"])
    );
    let retry = plan_run_composition(&request).unwrap();
    assert_eq!(retry.run_id, plan.run_id);
    assert_eq!(retry.run_write.content, plan.run_write.content);
    request.model = "fixture-b".into();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("selected model")
    );
    request.model = "fixture-a".into();
    request.identity_seed = "call/second".into();
    request.communication.receive_interface_id = "system_worker_private_second".into();
    let second = plan_run_composition(&request).unwrap();
    assert_ne!(plan.run_id, second.run_id);
    assert_ne!(plan.provider_interface_id, second.provider_interface_id);
    assert_ne!(plan.worker_interface_id, second.worker_interface_id);
    request.communication.return_interface_ids = vec![request.provider_interface_id.clone()];
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("System interface")
    );
}

#[test]
fn selected_outputs_keep_recorded_activity_out_of_peer_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    request.communication.output_selection = Some(CompositionOutputSelection {
        return_labels: vec!["interface.artifact.output".into(), "turn.error".into()],
        record_labels: vec!["turn.stream".into(), "turn.completed".into()],
    });
    assert!(plan_run_composition(&request).unwrap_err().contains("must declare client_projection_output_labels"));
    let provider_path = request.roots.registry_root.join("interface/world_provider_fixture/interface.json");
    let mut provider: serde_json::Value = serde_json::from_slice(&std::fs::read(&provider_path).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"]["client_projection_output_labels"] =
        json!(["interface.artifact.output", "turn.error", "turn.stream", "turn.completed", "turn.input.acknowledged"]);
    std::fs::write(provider_path, serde_json::to_vec(&provider).unwrap()).unwrap();
    assert!(plan_run_composition(&request).unwrap_err().contains("turn.input.acknowledged"));
    request.communication.output_selection.as_mut().unwrap().record_labels.push("turn.input.acknowledged".into());
    let plan = plan_run_composition(&request).unwrap();
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    for (label, destinations) in [("interface.artifact.output", vec!["system_worker_parent"]), ("turn.stream", vec![]), ("turn.input.acknowledged", vec![])] {
        let matches = record.active_checkpoints.iter().filter_map(|id| record.checkpoint(id))
            .filter(|cp| cp.inbound_interface().is_some_and(|id| id.as_str() == plan.provider_interface_id))
            .filter(|cp| cp.required_verified_artifacts.is_empty())
            .filter(|cp| cp.interface_binding.as_ref().unwrap().admits(label)).collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].outbound_interfaces().map(|id| id.as_str()).collect::<Vec<_>>(), destinations);
    }
    request.communication.output_selection.as_mut().unwrap().record_labels.push("turn.error".into());
    assert!(plan_run_composition(&request).unwrap_err().contains("disjoint"));
}

#[test]
fn client_and_return_bindings_are_only_the_callers_explicit_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    request.communication.return_interface_ids.clear();
    let plan = plan_run_composition(&request).unwrap();
    assert!(plan.return_interface_ids.is_empty());
    assert!(!plan.run_write.content.contains("role-system_worker_parent"));
    write(
        dir.path(),
        "registry/interface/world_client/interface.json",
        json!({"id":"world_client","interface_type":"world","role":"admin","direction":"bidirectional","source_names":["client"],"interface_driver_ref":"app_server"}),
    );
    request.communication.client_interface_id = Some("world_client".into());
    let plan = plan_run_composition(&request).unwrap();
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    assert!(record.checkpoints.iter().any(|cp| {
        cp.inbound_interface()
            .is_some_and(|id| id.as_str() == "world_client")
    }));
    assert!(record.checkpoints.iter().any(|cp| {
        cp.outbound_interfaces()
            .any(|id| id.as_str() == "world_client")
    }));
    assert!(plan.descriptor_writes.iter().all(|write| {
        !write
            .path
            .starts_with(request.roots.registry_root.join("interface/world_client"))
    }));
    let state = request
        .roots
        .interface_private_state_root
        .join(&plan.provider_interface_id);
    std::fs::create_dir_all(&state).unwrap();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("existing private state")
    );
    std::fs::remove_dir(&state).unwrap();
    request.communication.receive_interface_id = request.worker_template_interface_id.clone();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("private worker pin")
    );
}

#[test]
fn selected_client_template_authors_a_private_client_without_reusing_the_callers_chat() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    for name in ["interface.json", "life.json", "client.json"] {
        let value: Value = serde_json::from_slice(&std::fs::read(source.join("registry/interface/world_operator_chat").join(name)).unwrap()).unwrap();
        write(dir.path(), &format!("registry/interface/world_operator_chat/{name}"), value);
    }
    for (file, pointer) in [
        ("registry/interface/system_worker_template/policy.json", "/ceiling/interfaces"),
        ("registry/ee_template/existence/ee_template.json", "/allowed_interfaces"),
        ("ee/ee_existence.json", "/policy_narrowing/allowed_interfaces"),
    ] {
        let mut value: Value = serde_json::from_slice(&std::fs::read(dir.path().join(file)).unwrap()).unwrap();
        value.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!("world_operator_chat"));
        write(dir.path(), file, value);
    }
    request.communication.client_template_interface_id = Some("world_operator_chat".into());
    request.communication.client_title = Some("Context check".into());
    let plan = plan_run_composition(&request).unwrap();
    let record: Value = serde_json::from_str(&plan.run_write.content).unwrap();
    let (client, state) = record["interface_state"].as_object().unwrap().iter().find(|(_, state)| state["client_surface_role"] == "task").unwrap();
    assert_ne!(client, "world_operator_chat");
    assert_ne!(client, &plan.receive_interface_id);
    assert_eq!(state["provider_runtime_interface"], plan.provider_interface_id);
    assert!(plan.required_interface_ids.contains(client));
    let sidecar: Value = serde_json::from_str(&plan.descriptor_writes.iter().find(|write| write.path.ends_with(format!("{client}/octopus_os.json"))).unwrap().content).unwrap();
    assert_eq!(sidecar["owner_interface"], *client);
    assert_eq!(sidecar["presentation"], json!({"name":"Context check", "projects":[]}));
    assert_eq!(sidecar["authoring_profile"]["worker_choice_interface"], request.worker_template_interface_id);
    assert!(record["checkpoints"].as_array().unwrap().iter().any(|cp| cp["interface_binding"]["interface"] == *client));
    assert!(!plan.run_write.content.contains("MUST NOT COPY"));
    for invalid in ["", " name", "name\ninside", &"x".repeat(81)] {
        request.communication.client_title = Some(invalid.into());
        assert!(plan_run_composition(&request).unwrap_err().contains("client_title"));
    }
    request.communication.client_title = Some("Context check".into());
    request.communication.client_template_interface_id = None;
    assert!(plan_run_composition(&request).unwrap_err().contains("client_title"));
    request.communication.client_template_interface_id = Some("world_operator_chat".into());
    request.communication.client_interface_id = Some("world_operator_chat".into());
    assert!(plan_run_composition(&request).unwrap_err().contains("not both"));
}
