use octopus_descriptors::ArtifactId;
use octopus_interfaces::{ArtifactBindingPayload, GrantProjection, InvocationMetadata};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_model_context_render::resolve_instruction_context;
use serde_json::json;

fn payload(id: &str, bytes: &[u8]) -> ArtifactBindingPayload {
    ArtifactBindingPayload::new(
        ArtifactRef {
            id: ArtifactId::new(id),
            content: ContentHash::of_bytes(bytes),
        },
        bytes.to_vec(),
    )
}

#[test]
fn explicit_instruction_sources_preserve_order_deduplicate_and_refuse_leaks() {
    let file = payload(
        "file",
        b"# Universal instructions\nAct on verified evidence.",
    );
    let role = payload(
        "role",
        &serde_json::to_vec(&json!({"label":"context.reference","sources":[file.artifact]}))
            .unwrap(),
    );
    let capability = payload("capability", &serde_json::to_vec(&json!({"label":"context.reference","text":"Use the declared room.","sources":[file.artifact]})).unwrap());
    let task = payload("task", br#"{"text":"task data"}"#);
    let payloads = vec![capability, file, task, role];
    let mut metadata = InvocationMetadata::new(GrantProjection::default());
    metadata.grant.role_artifacts = vec!["role".into()];
    metadata.artifact_binding_refs = payloads.iter().map(|p| p.artifact.clone()).collect();
    for p in &payloads {
        metadata
            .grant
            .artifact_access
            .insert(p.artifact.id.as_str().into(), Default::default());
    }
    let resolved = resolve_instruction_context(&metadata, &payloads).unwrap();
    assert_eq!(
        resolved.render(&["role".into(), "capability".into()]).unwrap(),
        [
            "# Universal instructions\nAct on verified evidence.",
            "Use the declared room."
        ]
    );
    assert_eq!(resolved.remaining_payloads, vec![payloads[2].clone()]);
    assert_eq!(resolved.source_artifacts.len(), 3);
    let own_role = resolved.render(&["role".into()]).unwrap().join("\n");
    assert!(own_role.contains("Act on verified evidence."));
    assert!(own_role.contains("- capability"));
    assert!(!own_role.contains("Use the declared room."));
    assert!(resolved.render(&["unbound".into()]).is_err());
    assert!(resolved.render(&["role".into(), "role".into()]).is_err());
    assert_eq!(
        resolved.fingerprint_bytes,
        resolve_instruction_context(&metadata, &payloads)
            .unwrap()
            .fingerprint_bytes
    );
    let mut duplicate = payloads.clone();
    duplicate.push(payloads[0].clone());
    assert!(resolve_instruction_context(&metadata, &duplicate).is_err());
    let mut missing = payloads.clone();
    missing.remove(1);
    assert!(
        resolve_instruction_context(&metadata, &missing)
            .unwrap_err()
            .contains("not explicitly bound")
    );
    let mut denied = metadata.clone();
    denied.grant.artifact_access.get_mut("file").unwrap().read = false;
    assert!(resolve_instruction_context(&denied, &payloads).is_err());
    let mut foreign = metadata.clone();
    foreign.artifact_binding_refs[1].content = ContentHash::of_bytes(b"foreign");
    assert!(resolve_instruction_context(&foreign, &payloads).is_err());
    let mut missing_role = metadata.clone();
    missing_role.grant.role_artifacts.push("other-run".into());
    assert!(resolve_instruction_context(&missing_role, &payloads).is_err());
    for bytes in [b"Previous instruction".as_slice(), br#"{"label":"context.reference","text":"Previous instruction"}"#, b" ", b"\xff"] {
        let old = payload(&format!("instruction.text:{}", ContentHash::of_bytes(bytes).to_hex()), bytes);
        let mut admitted = metadata.clone();
        admitted.artifact_binding_refs.push(old.artifact.clone());
        admitted.grant.artifact_access.insert(old.artifact.id.to_string(), Default::default());
        let all = [payloads.clone(), vec![old.clone()]].concat();
        let result = resolve_instruction_context(&admitted, &all);
        if bytes == b" " || bytes == b"\xff" { assert!(result.is_err()); continue; }
        let result = result.unwrap();
        assert_eq!(result.remaining_payloads, resolved.remaining_payloads);
        assert_eq!(result.render(&["role".into()]).unwrap(), resolved.render(&["role".into()]).unwrap());
        admitted.grant.artifact_access.get_mut(old.artifact.id.as_str()).unwrap().read = false;
        assert!(resolve_instruction_context(&admitted, &all).is_err());
    }
}
