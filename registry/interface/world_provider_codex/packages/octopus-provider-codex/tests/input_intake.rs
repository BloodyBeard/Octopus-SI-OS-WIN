#[path = "../../../../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs"]
mod support;

use octopus_descriptors::InterfaceId;
use octopus_ledger::{
    ArtifactRef, ContentHash, Endpoint, InvocationMetadata, Lamport, Movement, MovementKind, Seq,
    TraceId,
};
use octopus_operator_chat_client::{
    input_intake::InputIntakeConfig, run_composition::plan_run_composition,
};
use octopus_provider_codex::intake_state::{
    InputDelivery, IntakeState, load_state, prepare_input, save_state,
};
use octopus_provider_run_authoring::provider_context_bindings;
use octopus_provider_run_authoring::{CheckedRunTransition, validate_checked_owner_transition};
use octopus_runtime::run_record::RunRecord;
use serde_json::{Value, json};

#[test]
fn exact_intake_composition_rearms_only_owned_edges_and_retains_crash_intent() {
    let temp = tempfile::tempdir().unwrap();
    let request = support::fixture(temp.path());
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    for relative in [
        "interface/system_service_input/interface.json",
        "interface/system_service_input/life.json",
    ] {
        let value: Value =
            serde_json::from_slice(&std::fs::read(source.join("registry").join(relative)).unwrap())
                .unwrap();
        support::write(&request.roots.registry_root, relative, value);
    }
    let path = request
        .roots
        .registry_root
        .join("interface/world_provider_fixture/interface.json");
    let mut provider: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"]["input_intake"] =
        json!({"template_interface_id":"system_service_input"});
    std::fs::write(path, serde_json::to_vec(&provider).unwrap()).unwrap();
    let plan = plan_run_composition(&request).unwrap();
    assert_ne!(plan.receive_interface_id, plan.worker_interface_id);
    assert_eq!(plan.seed.interface_id, plan.receive_interface_id);
    assert!(
        plan.required_binding_updates.iter().all(|update| update
            .outbound_binding
            .interface
            .as_str()
            == plan.receive_interface_id)
    );
    assert!(
        plan.required_interface_ids
            .contains(&plan.receive_interface_id)
    );
    assert!(plan.required_policy_updates.iter().any(|update| {
        update
            .added_interface_ids
            .contains(&plan.receive_interface_id)
    }));
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    let id = InterfaceId::new(&plan.receive_interface_id);
    let config: InputIntakeConfig =
        serde_json::from_value(record.interface_state[&id]["input_intake"].clone()).unwrap();
    config.validate(&id).unwrap();
    let provider_state = record.interface_state[&config.provider_interface].clone();
    let worker: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/interface.json", plan.worker_interface_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(
        worker
            .get("interface_driver_ref")
            .is_none_or(|value| value == "")
    );
    let endpoint: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/interface.json", plan.receive_interface_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(endpoint["role"], "service");
    let mut current = plan.run_write.content.into_bytes();
    let mut previous_input = None;
    for sequence in [10, 11, 12] {
        let edit = prepare_input(
            &config,
            plan.run_write.path.clone(),
            &current,
            sequence,
            &ArtifactRef { id: format!("source:{}", ContentHash::of_bytes(b"source").to_hex()).into(), content: ContentHash::of_bytes(b"source") },
            sequence,
        )
        .unwrap();
        let replay = prepare_input(
            &config,
            plan.run_write.path.clone(),
            &current,
            sequence,
            &ArtifactRef { id: format!("source:{}", ContentHash::of_bytes(b"source").to_hex()).into(), content: ContentHash::of_bytes(b"source") },
            sequence,
        )
        .unwrap();
        assert_eq!(edit.edit.content, replay.edit.content);
        let next = RunRecord::from_bytes(edit.edit.content.as_bytes()).unwrap();
        let metadata: InvocationMetadata = serde_json::from_value(json!({"grant":{"role_artifacts":config.artifact_bindings.iter().map(|binding| binding.id.as_str()).collect::<Vec<_>>(),"artifact_access":config.artifact_bindings.iter().map(|binding| (binding.id.to_string(),json!({"read":true,"change":false,"create":false,"delete":false}))).collect::<serde_json::Map<_,_>>()}})).unwrap();
        let call = Movement {
            seq: Seq(sequence),
            lamport: Lamport(sequence),
            recorded_at_unix_ms: 0,
            trace: TraceId("fixture".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(id.clone()),
            to: Endpoint::Interface(config.writer.door.clone()),
            artifact: None,
            invocation_metadata: Some(metadata.into()),
        };
        let transition = CheckedRunTransition {
            writer: config.writer.clone(),
            before: current.clone(),
            after: edit.edit.content.as_bytes().to_vec(),
            call: call.clone(),
            result: call,
        };
        validate_checked_owner_transition(&transition).unwrap();
        let mut foreign = transition.clone();
        let mut changed: Value = serde_json::from_slice(&foreign.after).unwrap();
        changed["interface_state"][config.provider_interface.as_str()]["context_generation"] =
            json!(999);
        foreign.after = serde_json::to_vec(&changed).unwrap();
        assert!(validate_checked_owner_transition(&foreign).is_err());
        assert_eq!(
            next.interface_state[&config.provider_interface],
            provider_state
        );
        let owned: Vec<String> =
            serde_json::from_value(next.interface_state[&id]["owned_checkpoint_ids"].clone())
                .unwrap();
        assert_eq!(owned.len(), 3);
        let input = next
            .checkpoints
            .iter()
            .find(|cp| {
                cp.inbound_interface() == Some(&id)
                    && cp.outbound_interfaces().next() == Some(&config.provider_interface)
            })
            .unwrap();
        assert_eq!(
            input.artifact_bindings,
            octopus_provider_run_authoring::input_artifacts::read_bindings(&next, &id, provider_context_bindings(&next, &config.provider_interface).unwrap()).unwrap()
        );
        if let Some(previous) = previous_input {
            assert!(next.checkpoint(&previous).is_none());
        }
        previous_input = Some(input.id.clone());
        assert_eq!(next.checkpoints.len(), record.checkpoints.len());
        current = edit.edit.content.into_bytes();
    }
    let path = temp.path().join("state.json");
    let state = IntakeState {
        completed_through: Some(12),
        pending: None,
    };
    save_state(&path, &state, 1024).unwrap();
    assert_eq!(load_state(&path, 1024).unwrap().completed_through, Some(12));
    assert!(load_state(&path, 1).is_err());
    let hash = ContentHash::of_bytes(b"delivery");
    let input = RunRecord::from_bytes(&current)
        .unwrap()
        .checkpoints
        .into_iter()
        .find(|cp| {
            cp.inbound_interface() == Some(&id)
                && cp.outbound_interfaces().next() == Some(&config.provider_interface)
        })
        .unwrap();
    let metadata: InvocationMetadata = serde_json::from_value(json!({"runtime":{"run":config.run,"ee":record.ee,"worker":record.worker,"checkpoint":input.id,"interface":config.provider_interface,"epoch":0},"checkpoint_binding_policy":{"inbound":input.interface_binding,"outbound":input.outbound_interface_bindings},"checkpoint_admission":{"artifact_label":config.input_label,"required_artifact_labels":[],"complete":true}})).unwrap();
    let delivered = Movement {
        seq: Seq(40),
        lamport: Lamport(40),
        recorded_at_unix_ms: 0,
        trace: TraceId("fixture".into()),
        kind: MovementKind::InterfaceOutbound,
        from: Endpoint::Interface(id.clone()),
        to: Endpoint::Interface(config.provider_interface.clone()),
        artifact: Some(ArtifactRef {
            id: format!("delivery:{}", hash.to_hex()).into(),
            content: hash,
        }),
        invocation_metadata: Some(metadata.into()),
    };
    assert_eq!(
        octopus_provider_codex::intake_state::admitted_input(&[], &config, &hash, 12).unwrap(),
        None
    );
    assert_eq!(
        octopus_provider_codex::intake_state::admitted_input(
            &[delivered.clone()],
            &config,
            &hash,
            12
        )
        .unwrap(),
        Some(40)
    );
    assert!(
        octopus_provider_codex::intake_state::admitted_input(
            &[delivered.clone(), delivered.clone()],
            &config,
            &hash,
            12
        )
        .is_err()
    );
    let delivery: InputDelivery = serde_json::from_value(json!({"schema_version":1,"source_movement_seq":12,"source_artifact":delivered.artifact,"message":{"action":"followup","control_id":"retained","text":"resume from evidence"}})).unwrap();
    let source_ref = delivered.artifact.clone().unwrap();
    let pending = octopus_provider_codex::intake_state::PendingInput {
        source: delivered.clone(),
        delivery,
        edit: Some(prepare_input(&config, plan.run_write.path.clone(), &current, 13, &source_ref, 40).unwrap()),
    };
    let retained = IntakeState {
        completed_through: Some(11),
        pending: Some(pending),
    };
    save_state(&path, &retained, 1024 * 1024).unwrap();
    assert_eq!(
        serde_json::to_value(load_state(&path, 1024 * 1024).unwrap()).unwrap(),
        serde_json::to_value(retained).unwrap()
    );
    let mut corrupted = config.clone();
    corrupted.writer.door = id;
    assert!(prepare_input(&corrupted, plan.run_write.path, &current, 13, &source_ref, 13).is_err());
}

#[test]
fn input_delivery_identity_preserves_distinct_equal_messages_and_actions() {
    let reference =
        json!({"id":"source", "content":octopus_ledger::ContentHash::of_bytes(b"source")});
    let body = json!({"schema_version":1,"source_movement_seq":10,"source_artifact":reference,"message":{"action":"queue","control_id":"same","text":"same"}});
    let first: InputDelivery = serde_json::from_value(body.clone()).unwrap();
    first.validate().unwrap();
    let mut second = body.clone();
    second["source_movement_seq"] = json!(11);
    assert_ne!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&serde_json::from_value::<InputDelivery>(second).unwrap()).unwrap()
    );
    for action in ["queue", "followup", "interrupt"] {
        let mut value = body.clone();
        value["message"]["action"] = json!(action);
        serde_json::from_value::<InputDelivery>(value)
            .unwrap()
            .validate()
            .unwrap();
    }
    let mut invalid = body;
    invalid["message"]["action"] = json!("hidden_resume");
    assert!(serde_json::from_value::<InputDelivery>(invalid).is_err());
}
