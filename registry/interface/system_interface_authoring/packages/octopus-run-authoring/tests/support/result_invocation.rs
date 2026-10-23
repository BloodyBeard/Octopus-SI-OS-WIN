use super::*;

#[test]
#[ignore = "requires explicitly selected read-only result invocation evidence"]
fn selected_result_invocation_rebases_peer_advance() {
    let root = PathBuf::from(std::env::var("OCTOPUS_RESULT_EVIDENCE").unwrap());
    let before = std::fs::read(root.join("result-invocation-retained-before.json")).unwrap();
    let after = std::fs::read(root.join("result-invocation-retained-after.json")).unwrap();
    let current = std::fs::read(root.join("result-invocation-before.json")).unwrap();
    let evidence: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("result-invocation-identity.json")).unwrap()).unwrap();
    let owner: InterfaceId = serde_json::from_value(evidence["provider"].clone()).unwrap();
    let profile: RunAuthoringProfile = serde_json::from_value(evidence["profile"].clone()).unwrap();
    let original = RunRecord::from_bytes(&before).unwrap();
    let candidate = RunRecord::from_bytes(&after).unwrap();
    let writer = declared_provider_run_writer(&original, &owner, &profile).unwrap();
    let retained = definition(&before, &after, &writer).unwrap().unwrap();
    assert_eq!(definition(&current, &after, &writer).err().unwrap(),
        "result invocation changed pending work, future context or a neighboring contract");
    let config = validate_declared_provider_writer(&original, &writer).unwrap().config(&profile);
    let generation = candidate.interface_state[&owner]["authoring_generation"].as_u64().unwrap();
    let edit = CheckedRunEdit::prepare(root.join("run.json"), &before, serde_json::from_slice(&after).unwrap(),
        format!("provider-run-authoring:{}:{generation}", original.id)).unwrap();
    let authored = AuthoredRun { json_format:edit.json_format, content:edit.content, patch:edit.patch,
        before_hash:edit.before_hash, after_hash:edit.after_hash, correlation_id:edit.correlation_id,
        authoring_result_checkpoint:"writer-result".into(), authoring_result_interface:writer.door.clone(),
        pending_handoff:persisted_pending_handoff(&candidate, &owner).unwrap().unwrap() };
    let rebased = authoring_retry::rebase_authored_run(&before, &current, &authored, &config, Some(JsonFormat::Compact)).unwrap();
    let recovered = definition(&current, rebased.content.as_bytes(), &writer).unwrap().unwrap();
    assert_eq!(recovered.invocation, retained.invocation);
    assert_eq!(recovered.checkpoint, retained.checkpoint);
}
