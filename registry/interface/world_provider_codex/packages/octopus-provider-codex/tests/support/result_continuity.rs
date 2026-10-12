#[test]
fn successive_tool_results_use_admitted_session_continuity() {
    let temp = tempfile::tempdir().unwrap();
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json"),
    ).unwrap()).unwrap();
    let mut input = model_policy_input(serde_json::json!(["gpt-5.5"]));
    let runtime = input.metadata.as_ref().unwrap().runtime.as_ref().unwrap().clone();
    let pending = |call: &str, tool: &str| CodexPendingContinuity::DynamicTool {
        native_operation: None, turn_id: "turn-a".into(), call_id: call.into(), tool: tool.into(),
    };
    let first = continuity(runtime.interface.as_str(), runtime.run.as_str(), "thread-a", 3,
        Some(pending("call-list", "artifact.list")));
    let second = continuity(runtime.interface.as_str(), runtime.run.as_str(), "thread-a", 5,
        Some(pending("call-read", "artifact.read")));
    let first_payload = serde_json::json!({"label":"tool.call", CODEX_CONTINUITY_FIELD:first});
    let first_bound = bound("first-call", first_payload.clone());
    input.metadata.as_mut().unwrap().artifact_binding_refs = vec![first_bound.artifact.clone()];
    input.artifact_binding_payloads = vec![first_bound];
    let result_payload = serde_json::json!({"label":"artifact.read.result", "tool":"artifact.read",
        "correlation_id":"call-read", "ok":true, "body":{"text":"current document"}});
    input.artifact_file = InterfaceArtifactFile::write(temp.path(), "artifact.read.result",
        &serde_json::to_vec(&result_payload).unwrap()).unwrap();
    input.artifact = input.artifact_file.pin_manifest_into_blob(
        &mut octopus_ledger::InMemoryBlobs::new()).unwrap().artifact;
    let record = serde_json::to_vec(&serde_json::json!({
        "id":runtime.run, "ee":runtime.ee, "worker":runtime.worker, "trace":"trace:successive-tools",
        "status":"running", "checkpoints":[], "active_checkpoints":[],
    })).unwrap();
    let session = vec![
        session_artifact("first-call", "tool.call", first_payload, 10),
        session_artifact("first-result", "artifact.list.result", serde_json::json!({
            "label":"artifact.list.result", "tool":"artifact.list", "correlation_id":"call-list",
            "ok":true,"body":{"artifacts":[]}}), 20),
        session_artifact("second-call", "tool.call", serde_json::json!({
            "label":"tool.call", CODEX_CONTINUITY_FIELD:second}), 30),
    ].into_iter().map(|entry| serde_json::from_value::<SessionContextArtifact>(entry).unwrap())
        .collect::<Vec<_>>();
    let recovered = recover_provider_result_continuity(&descriptor, &input, None, &session,
        &runtime.run, &record).unwrap().unwrap();
    assert_eq!(recovered, second);
    assert!(recover_provider_result_continuity(&descriptor, &input, None, &[],
        &runtime.run, &record).is_err());
    let mut wrong_thread = session.clone();
    wrong_thread[2].payload[CODEX_CONTINUITY_FIELD]["thread_id"] = serde_json::json!("unrelated-thread");
    assert!(recover_provider_result_continuity(&descriptor, &input, None, &wrong_thread,
        &runtime.run, &record).is_err());
    let mut wrong_call = session.clone();
    wrong_call[2].payload[CODEX_CONTINUITY_FIELD]["pending"]["call_id"] = serde_json::json!("unrelated-call");
    assert!(recover_provider_result_continuity(&descriptor, &input, None, &wrong_call,
        &runtime.run, &record).is_err());

    // The prior result has already produced a new call. Only its admitted handoff
    // may finish that old source; correlation mismatches alone never authorize it.
    let mut old_input = input.clone();
    old_input.artifact_file = InterfaceArtifactFile::write(temp.path(), "artifact.list.result",
        &serde_json::to_vec(&serde_json::json!({"label":"artifact.list.result", "tool":"artifact.list",
            "correlation_id":"call-list", "ok":true, "body":{"artifacts":[]}})).unwrap()).unwrap();
    old_input.artifact = old_input.artifact_file.pin_manifest_into_blob(
        &mut octopus_ledger::InMemoryBlobs::new()).unwrap().artifact;
    let mut handed = session;
    handed[2].payload["tool"] = serde_json::json!("artifact.read");
    handed[2].payload["correlation_id"] = serde_json::json!("call-read");
    handed[2].payload["arguments"] = serde_json::json!({"path":"document"});
    let outcomes = vec![octopus_provider_run_authoring::ProviderHandoffOutcome::Admitted {
        seq:30, artifact:handed[2].source_artifact.clone(),
    }];
    let mut current: serde_json::Value = serde_json::from_slice(&record).unwrap();
    current["interface_state"] = serde_json::json!({runtime.interface.as_str():{"context_generation":5}});
    let current = serde_json::to_vec(&current).unwrap();
    assert!(recover_provider_result_continuity(&descriptor, &old_input, None, &handed,
        &runtime.run, &record).is_err());
    let receipt = recovered_tool_handoff_receipt(&descriptor, &old_input, 20, &current,
        &handed, &outcomes, temp.path()).unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&receipt.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(value["label"], INTERFACE_CONTEXT_UPDATED_LABEL);
    assert_eq!(value["effect_artifact"], serde_json::to_value(&handed[2].source_artifact).unwrap());
    assert_eq!(value["source_artifact"], serde_json::to_value(&old_input.artifact).unwrap());
    assert_eq!(value[CODEX_CONTINUITY_FIELD]["context_generation"], 6);
    assert_eq!(value[CODEX_CONTINUITY_FIELD]["pending"], handed[2].payload[CODEX_CONTINUITY_FIELD]["pending"]);
    assert!(recovered_tool_handoff_receipt(&descriptor, &old_input, 20, &current,
        &handed, &[], temp.path()).unwrap().is_none());
    assert!(recovered_tool_handoff_receipt(&descriptor, &old_input, 30, &current,
        &handed, &outcomes, temp.path()).is_err());
    let mut forged = handed.clone();
    forged[2].payload["correlation_id"] = serde_json::json!("different-call");
    assert!(recovered_tool_handoff_receipt(&descriptor, &old_input, 20, &current,
        &forged, &outcomes, temp.path()).is_err());
    let mut forged = handed;
    forged[2].source_movement_seq = Some(31);
    assert!(recovered_tool_handoff_receipt(&descriptor, &old_input, 20, &current,
        &forged, &outcomes, temp.path()).is_err());

    // Two routes can deliver one immutable result. The second receipt must keep
    // the successor wait intact and retain the completed input's exact identity.
    let mut duplicate_context = forged;
    duplicate_context[2].source_movement_seq = Some(30);
    duplicate_context[1].source_artifact = old_input.artifact.clone();
    duplicate_context[1].payload = serde_json::from_slice(&old_input.read_artifact_payload().unwrap()).unwrap();
    let receipt = duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &duplicate_context, temp.path()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&receipt.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(value["code"], "duplicate_tool_result");
    assert_eq!(value["label"], TURN_INPUT_ACKNOWLEDGED_LABEL);
    assert!(!codex_context_labels().contains(TURN_INPUT_ACKNOWLEDGED_LABEL));
    assert_eq!(value["completed_source_movement_seq"], 20);
    assert!(value.get(CODEX_CONTINUITY_FIELD).is_none());
    assert_eq!(value["context_generation"], 5);
    assert!(duplicate_tool_result_receipt(&descriptor, &old_input, 20, 20,
        &current, &duplicate_context, temp.path()).is_err());
    let mut changed = duplicate_context.clone();
    changed[1].payload["body"] = serde_json::json!({"artifacts":["forged"]});
    assert!(duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &changed, temp.path()).is_err());
    let mut without_prior_context = duplicate_context.clone();
    without_prior_context.remove(1);
    duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &without_prior_context, temp.path()).unwrap();
    let mut changed = duplicate_context.clone();
    changed[1].source_movement_seq = Some(26);
    assert!(duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &changed, temp.path()).is_err());
    let mut interrupted = duplicate_context.clone();
    let pending = interrupted[2].payload[CODEX_CONTINUITY_FIELD]["pending"].take();
    interrupted[2].payload[CODEX_CONTINUITY_FIELD]["interrupted_pending"] = pending;
    let receipt = duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &interrupted, temp.path()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&receipt.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(value["context_generation"], 5);
    assert!(value.get(CODEX_CONTINUITY_FIELD).is_none());
    duplicate_context[2].payload[CODEX_CONTINUITY_FIELD]["pending"]["call_id"] = serde_json::json!("call-list");
    assert!(duplicate_tool_result_receipt(&descriptor, &old_input, 25, 20,
        &current, &duplicate_context, temp.path()).is_err());
}
