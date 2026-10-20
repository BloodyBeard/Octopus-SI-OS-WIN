use super::*;
use serde_json::{Value, json};

#[test]
fn intake_client_continuation_preserves_peer_and_replaces_only_selected_context() {
    let temp = tempfile::tempdir().unwrap();
    let registry = temp.path().join("registry");
    tests::copy_planner_descriptors(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry"),
        &registry,
    );
    let ee = temp.path().join("ee");
    std::fs::create_dir(&ee).unwrap();
    tests::boot_operator_space_ee_instances(&ee);
    let apply = |plan: &OperatorChatPlan| {
        for edit in plan.interface_writes.iter().chain(&plan.run_writes) {
            if let Some(expected) = &edit.expected_before_hash {
                assert_eq!(expected, &sha256_hex(&std::fs::read(&edit.path).unwrap()));
            }
            std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap();
            std::fs::write(&edit.path, &edit.content).unwrap();
        }
    };
    let first_payload = json!({"id":"fixture.selected.first","content":vec![1;32]});
    let second_payload = json!({"id":"fixture.selected.second","content":vec![2;32]});
    let mut request: OperatorChatPlanRequest = serde_json::from_value(json!({
        "registry_root":registry,"ee_instances_root":ee,"run_records_root":temp.path().join("runs"),"interface_private_state_root":temp.path().join("private"),"write_hash_algorithm":"sha256",
        "chat_id":"intake_client","provider_interface_id":"world_provider_codex",
        "ee_id":"ee_operator_space","worker_interface_id":"system_worker_coder_max",
        "client_surface_role":"task","cycle":"first","conversation":{"ledger_offset":0},
        "payload_artifact_bindings":[first_payload]
    }))
    .unwrap();
    let first = plan_operator_chat_topology(&request).unwrap();
    apply(&first);
    let mut prior: Value = serde_json::from_str(&first.run_record_content).unwrap();
    request.existing_run_record = Some(prior.clone());
    request.cycle = "unchanged".into();
    let unchanged = plan_operator_chat_topology(&request).unwrap();
    assert!(
        unchanged.interface_writes.is_empty(),
        "unchanged intake must not request capability repair: {:?}",
        unchanged.interface_writes
    );
    assert!(unchanged.removals.is_empty());
    prior = serde_json::from_str(&unchanged.run_record_content).unwrap();
    let owner = prior["interface_state"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, state)| state.get("input_intake").is_some())
        .unwrap()
        .0
        .clone();
    let owned_state = prior["interface_state"][&owner].clone();
    let config: input_intake::InputIntakeConfig =
        serde_json::from_value(owned_state["input_intake"].clone()).unwrap();
    assert_eq!(config.notification_action, input_intake::InputNotificationAction::Followup);
    for id in prior["interface_state"].as_object().unwrap().keys()
        .map(String::as_str).chain([config.writer.door.as_str()])
    {
        let sidecar: Value = serde_json::from_slice(&std::fs::read(
            registry.join("interface").join(id).join("octopus_os.json"),
        ).unwrap()).unwrap();
        assert_eq!(sidecar["interface"], id);
        assert_eq!(sidecar["owner_interface"], first.chat_interface_id);
        assert_eq!(sidecar["surface_role"], "task");
    }
    let provider_gate = prior["interface_state"][&first.model_interface_id]["run_authoring_binding"]["checkpoints"]
        .as_array().unwrap().iter().find(|row| row["role"] == "authoring_call").unwrap()["required_verified_artifacts"].clone();
    let mut intake_gate = provider_gate.clone();
    intake_gate[0]["params"]["tools"] = json!([config.writer.tool]);
    assert_eq!(owned_state["writer_run"]["initial_record"]["checkpoints"][0]["required_verified_artifacts"], intake_gate);
    intake_gate[0]["params"]["metadata_matches"][0]["payload"] = json!("/arguments/expected_before_hash");
    assert_eq!(json!(config.writer_required_verified_artifacts), intake_gate);
    for generation in ["first", "second", "third"] {
        let (rows, _) = input_intake::initial_checkpoints(&config, &prior["trace"], generation).unwrap();
        assert_eq!(rows[1]["required_verified_artifacts"], intake_gate);
        assert_eq!(rows[0]["required_verified_artifacts"], json!([]));
        assert_eq!(rows[2]["required_verified_artifacts"], json!([]));
    }
    let mut missing = json!(config);
    missing.as_object_mut().unwrap().remove("writer_required_verified_artifacts");
    assert!(serde_json::from_value::<input_intake::InputIntakeConfig>(missing).is_err());
    for invalid in [json!([]), json!([intake_gate[0], intake_gate[0]]), json!([{
        "mark":"","artifact_label":config.writer.call_label,"verifier":"tool_call_allowed","params":intake_gate[0]["params"]
    }])] {
        let mut broken = json!(config);
        broken["writer_required_verified_artifacts"] = invalid;
        let broken = serde_json::from_value::<input_intake::InputIntakeConfig>(broken).unwrap();
        assert!(input_intake::initial_checkpoints(&broken, &prior["trace"], "bad").is_err());
    }
    for pointer in ["/artifact_label", "/params/tools", "/params/metadata_matches"] {
        let mut broken = json!(config);
        *broken["writer_required_verified_artifacts"][0].pointer_mut(pointer).unwrap() = json!("wrong");
        let broken = serde_json::from_value::<input_intake::InputIntakeConfig>(broken).unwrap();
        assert!(broken.validate(&config.writer.owner).is_err());
    }
    let writer_rows = owned_state["writer_run"]["initial_record"]["checkpoints"].as_array().unwrap().clone();
    assert_eq!(writer_rows.len(), 2);
    let peer = "system_service_selected_peer";
    std::fs::create_dir_all(registry.join(format!("interface/{peer}"))).unwrap();
    std::fs::write(registry.join(format!("interface/{peer}/interface.json")), json!({"id":peer,"interface_type":"system","role":"service","direction":"bidirectional","source_names":["peer"]}).to_string()).unwrap();
    let policy_path = registry.join(format!(
        "interface/{}/policy.json",
        first.worker_interface_id
    ));
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&policy_path).unwrap()).unwrap();
    policy["ceiling"]["interfaces"]
        .as_array_mut()
        .unwrap()
        .push(json!(peer));
    std::fs::write(&policy_path, pretty_json(&policy).unwrap()).unwrap();
    let incoming = input_intake::binding(&first.model_interface_id, "provider.peer.message");
    let outgoing = input_intake::binding(peer, "provider.peer.message");
    let template = json!({"name":"retained_peer","role":"provider_interface_output","interface_binding":incoming,"outbound_interface_bindings":[outgoing],"include_context":false,"static_artifact_bindings":["fixture.peer.context"],"required_verified_artifacts":[]});
    prior["interface_state"][&first.model_interface_id]["run_authoring_binding"]["checkpoints"]
        .as_array_mut()
        .unwrap()
        .push(template.clone());
    prior["interface_state"][&first.model_interface_id]["frontier_checkpoint_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("peer_output"));
    let mut peer_artifacts = prior["interface_state"][&first.model_interface_id]["run_authoring_binding"]["writer_run"]["initial_record"]["checkpoints"][0]["artifact_bindings"].as_array().unwrap().clone();
    peer_artifacts.push(json!({"id":"fixture.peer.context"}));
    prior["checkpoints"].as_array_mut().unwrap().push(json!({"id":"peer_output","run":first.run_id,"trace":"peer-trace","epoch":0,"interface_binding":incoming,"outbound_interface_bindings":[outgoing],"artifact_bindings":peer_artifacts,"required_verified_artifacts":[]}));
    prior["active_checkpoints"]
        .as_array_mut()
        .unwrap()
        .push(json!("peer_output"));
    for (index, selected) in [
        json!([]),
        json!([second_payload]),
        json!([first_payload, second_payload]),
    ]
    .into_iter()
    .enumerate()
    {
        request.existing_run_record = Some(prior.clone());
        request.payload_artifact_bindings = serde_json::from_value(selected.clone()).unwrap();
        request.cycle = format!("selection_{index}");
        let plan = plan_operator_chat_topology(&request).unwrap();
        assert!(
            plan.interface_writes
                .iter()
                .all(|write| write.tag == format!("octopus-os:{}", first.chat_interface_id)),
            "payload continuation must only change its profile: {:?}",
            plan.interface_writes
        );
        let next = RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap();
        let value: Value = serde_json::from_str(&plan.run_record_content).unwrap();
        assert_eq!(
            serde_json::to_value(&plan.authoring_profile.payload_artifact_bindings).unwrap(),
            selected
        );
        let topology =
            &value["interface_state"][&first.model_interface_id]["run_authoring_binding"];
        assert_eq!(topology["checkpoints"].as_array().unwrap().iter().find(|row| row["role"] == "authoring_call").unwrap()["required_verified_artifacts"], provider_gate);
        assert_eq!(
            topology
                .get("payload_artifact_bindings")
                .cloned()
                .unwrap_or(json!([])),
            selected
        );
        assert!(
            topology["checkpoints"]
                .as_array()
                .unwrap()
                .contains(&template)
        );
        let input = next
            .checkpoints
            .iter()
            .find(|cp| {
                cp.inbound_interface() == Some(&InterfaceId::new(&owner))
                    && cp.outbound_interfaces().next()
                        == Some(&InterfaceId::new(&first.model_interface_id))
            })
            .unwrap();
        let context = octopus_run_authoring::provider_context_bindings(
            &next,
            &InterfaceId::new(&first.model_interface_id),
        )
        .unwrap();
        assert_eq!(input.artifact_bindings, context);
        assert!(
            context
                .iter()
                .any(|binding| binding.id.as_str() == "fixture.peer.context")
        );
        for payload in [&first_payload, &second_payload] {
            assert_eq!(
                context
                    .iter()
                    .any(|binding| binding.id.as_str() == payload["id"].as_str().unwrap()),
                selected.as_array().unwrap().contains(payload)
            );
        }
        assert_eq!(value["interface_state"][&owner], owned_state);
        for row in &writer_rows {
            assert!(value["interface_state"][&owner]["writer_run"]["initial_record"]["checkpoints"].as_array().unwrap().contains(row));
        }
        apply(&plan);
        prior = value;
    }
    request.existing_run_record = Some(prior.clone());
    request.cycle = "settled".into();
    assert!(
        plan_operator_chat_topology(&request)
            .unwrap()
            .interface_writes
            .is_empty()
    );
    let input_id = prior["interface_state"][&owner]["owned_checkpoint_ids"][0].clone();
    prior["interface_state"]["foreign_owner"] = json!({"owned_checkpoint_ids":[input_id]});
    request.existing_run_record = Some(prior);
    assert!(
        plan_operator_chat_topology(&request)
            .unwrap_err()
            .contains("exclusively owned")
    );
}

#[test]
fn final_write_normalization_checks_preimage_before_discarding_unchanged_json() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("descriptor.json");
    std::fs::write(&file, "{\"id\":\"exact\"}").unwrap();
    let mut writes = vec![PlannedClientWrite {
        path: file.clone(),
        content: "{\n  \"id\": \"exact\"\n}\n".into(),
        tag: "fixture".into(),
        expected_before_hash: Some(sha256_hex(&std::fs::read(&file).unwrap())),
    }];
    let original = writes.clone();
    remove_unchanged_writes(&mut writes).unwrap();
    assert!(writes.is_empty());
    std::fs::write(&file, &original[0].content).unwrap();
    assert!(
        remove_unchanged_writes(&mut original.clone())
            .unwrap_err()
            .contains("changed before final normalization")
    );
    let policy = json!({"id":"system_worker_exact","role_artifacts":["role.first","role.second"],"ceiling":{"interfaces":["system_room"],"tools":["file.write"],"interface_policy":{"system_room":{"roots":["allowed"]}}}});
    std::fs::write(&file, policy.to_string()).unwrap();
    for changed in ["tool", "role_order", "interface_policy"] {
        let mut next = policy.clone();
        match changed {
            "tool" => next["ceiling"]["tools"] = json!([]),
            "role_order" => next["role_artifacts"] = json!(["role.second", "role.first"]),
            _ => next["ceiling"]["interface_policy"] = json!({}),
        }
        let mut edits = vec![PlannedClientWrite {
            path: file.clone(),
            content: next.to_string(),
            tag: "policy:system_worker_exact".into(),
            expected_before_hash: Some(sha256_hex(&std::fs::read(&file).unwrap())),
        }];
        remove_unchanged_writes(&mut edits).unwrap();
        assert_eq!(
            edits.len(),
            1,
            "normalization must retain {changed} changes"
        );
    }
}
