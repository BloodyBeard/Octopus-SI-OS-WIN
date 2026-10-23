use super::*;
use octopus_descriptors::{CheckpointId, DescriptorHash, DescriptorName, EeId, RunId};
use octopus_interfaces::GrantProjection;
use octopus_ledger::RuntimeInvocationMetadata;

// A long run must preserve every grant, bound reference and provenance coordinate
// without making its control frame proportional to its context. No unverified
// carrier, or carrier from another request/generation, may reach the handler.
#[test]
fn long_context_roundtrip_uses_checked_request_carrier_and_small_control_frame() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join(format!(
        "system_memory_{}/.tool-process-host-state/generations/{}/state/invocations",
        "a".repeat(32), "b".repeat(64),
    ));
    let limits = InterfaceHostIoLimits {
        max_control_frame_bytes: 1024 * 1024, max_stderr_bytes: 65536,
        max_manifest_bytes: 65536, max_payload_bytes: 4 * 1024 * 1024,
    };
    let bindings: Vec<_> = (0..1100).map(|index| {
        let payload = format!("exact bound experience {index}").into_bytes();
        let hash = ContentHash::of_bytes(&payload);
        ArtifactBindingPayload::new(ArtifactRef {
            id: ArtifactId::new(format!("tool.result:long-run-context-{index}:{}", hash.to_hex())),
            content: hash,
        }, payload)
    }).collect();
    let refs: Vec<_> = bindings.iter().map(|binding| binding.artifact.clone()).collect();
    let mut grant = GrantProjection {
        tools: vec!["memory.save".into()], hard_blocked_tools: vec!["file.delete".into()],
        interfaces: vec!["system_memory_long".into()],
        worker_environment_kinds: vec!["system_room".into()],
        artifact_verbs: vec!["read".into(), "create".into()],
        ee_root_slot_allowance: Some(vec!["memory_root".into()]),
        role_artifacts: vec![refs[0].id.as_str().into()],
        goal: Some("retain admitted experience within its exact scope".into()),
        ..Default::default()
    };
    grant.formation_pin.inputs.insert(
        DescriptorName::new("worker:long"), DescriptorHash::of_bytes(b"pinned worker ceiling"),
    );
    for (index, artifact) in refs.iter().enumerate() {
        let access = grant.artifact_access.entry(artifact.id.as_str().into()).or_default();
        access.change = index == 0;
    }
    let mut metadata = InvocationMetadata::new(grant).with_runtime(RuntimeInvocationMetadata {
        run: RunId::new("run_long"), ee: EeId::new("ee_scoped"),
        worker: InterfaceId::new("system_worker_long"), checkpoint: CheckpointId::new("memory_save"),
        interface: InterfaceId::new("system_memory_long"), epoch: 42,
        run_record_hash: Some(ContentHash::of_bytes(b"exact run record").to_hex()),
    });
    metadata.source_movement_seq = Some(45366);
    metadata.delivery_scope_seq = Some(45374);
    metadata.replaces_arrival_seq = Some(45365);
    metadata.outbound_interface_bindings = vec![InterfaceId::new("system_memory_long")];
    metadata.artifact_binding_refs = refs.clone();
    let payload = br#"{"tool":"memory.save","arguments":{"note":"drain closed process output"}}"#;
    let input = InterfaceInvocationEnvelope {
        artifact: ArtifactRef { id: ArtifactId::new("tool.call:memory"), content: ContentHash::of_bytes(payload) },
        artifact_file: InterfaceArtifactFile::write(temporary.path().join("call"), "tool.call", payload).unwrap(),
        artifact_binding_payloads: bindings.clone(), metadata: Some(metadata.clone()),
    };
    let captured = ToolProcessEnvelope::capture(&input, 45374, &limits).unwrap();
    let wire = captured.clone().materialize_for_process(&root, 7, &limits).unwrap();
    let context_bytes = wire.context.read_bytes(&root, 7, &limits).unwrap();
    assert!(context_bytes.len() > limits.max_control_frame_bytes as usize);
    assert!(serde_json::to_vec(&metadata).unwrap().len() > 8192);
    let repeated = captured.clone().materialize_for_process(&root, 7, &limits).unwrap();
    assert_eq!(repeated.context.path, wire.context.path);
    assert_eq!(repeated.context.read_bytes(&root, 7, &limits).unwrap(), context_bytes);

    let descriptor = octopus_tool_registry::load_tool_descriptor_from_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../tool"), "memory.save",
    ).unwrap().expect("memory.save descriptor");
    let message = ToolProcessInput::Invoke { request_id: 7, descriptor, envelope: wire.clone() };
    let frame = serialize_bounded_json_line(&message, limits.max_control_frame_bytes, "tool-process input frame").unwrap();
    assert!(frame.len() < 8192);
    println!("long-context bindings={} metadata_bytes={} carrier_bytes={} control_frame_bytes={}",
        refs.len(), serde_json::to_vec(&metadata).unwrap().len(), context_bytes.len(), frame.len());
    let ToolProcessInput::Invoke { request_id, envelope, .. } = serde_json::from_slice(&frame).unwrap() else { panic!("invoke frame"); };
    assert_eq!(envelope.source_movement_seq, 45374);
    let restored = envelope.materialize(&root, request_id, &limits).unwrap();
    assert_eq!(restored.artifact, input.artifact);
    assert_eq!(restored.read_artifact_payload_bounded(limits.max_payload_bytes).unwrap(), payload);
    assert_eq!(restored.metadata, Some(metadata));
    assert_eq!(restored.artifact_binding_payloads.len(), bindings.len());
    for (restored, original) in restored.artifact_binding_payloads.iter().zip(&bindings) {
        assert_eq!(restored.artifact, original.artifact);
        assert_eq!(restored.read_bytes().unwrap(), original.read_bytes().unwrap());
    }

    let mut legacy = serde_json::to_value(&wire).unwrap();
    legacy.as_object_mut().unwrap().remove("context");
    legacy["metadata"] = serde_json::to_value(&input.metadata).unwrap();
    legacy["artifact_bindings"] = serde_json::json!([]);
    assert!(serde_json::from_value::<ToolProcessWireEnvelope>(legacy).is_err());
    assert!(wire.clone().materialize(&root, 8, &limits).unwrap_err().contains("exact generation and request"));
    let other_generation = tempfile::tempdir().unwrap();
    assert!(wire.clone().materialize(other_generation.path(), 7, &limits).unwrap_err().contains("exact generation and request"));
    let mut escaped = wire.clone();
    escaped.context.path = root.join("7/../7/artifact").join(wire.context.path.file_name().unwrap());
    assert!(escaped.materialize(&root, 7, &limits).is_err());
    for byte_len in [wire.context.byte_len - 1, wire.context.byte_len + 1, limits.max_payload_bytes + 1] {
        let mut wrong_length = wire.clone();
        wrong_length.context.byte_len = byte_len;
        assert!(wrong_length.materialize(&root, 7, &limits).is_err());
    }
    let mut too_small = limits;
    too_small.max_payload_bytes = wire.context.byte_len - 1;
    assert!(wire.clone().materialize(&root, 7, &too_small).unwrap_err().contains("payload byte ceiling"));
    assert!(captured.clone().materialize_for_process(&root, 8, &too_small).unwrap_err().contains("payload byte ceiling"));

    // Each growing half is pinned independently of whether tampered JSON parses.
    let text = std::str::from_utf8(&context_bytes).unwrap();
    for needle in ["run_long", "bound-artifacts"] {
        let mut tampered = context_bytes.clone();
        tampered[text.find(needle).unwrap()] = b'x';
        std::fs::write(&wire.context.path, tampered).unwrap();
        assert!(wire.clone().materialize(&root, 7, &limits).unwrap_err().contains("pinned length or hash"));
        assert!(captured.clone().materialize_for_process(&root, 7, &limits).is_err());
    }
    std::fs::write(&wire.context.path, &context_bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::{fs::symlink, fs::MetadataExt};
        let published = std::fs::metadata(&wire.context.path).unwrap();
        assert_eq!(published.mode() & 0o777, 0o600);
        assert_eq!(published.nlink(), 1);
        let saved = wire.context.path.with_extension("saved");
        std::fs::rename(&wire.context.path, &saved).unwrap();
        symlink(&saved, &wire.context.path).unwrap();
        assert!(wire.clone().materialize(&root, 7, &limits).unwrap_err().contains("without following links"));
        std::fs::remove_file(&wire.context.path).unwrap();
        std::fs::rename(&saved, &wire.context.path).unwrap();
        let directory = wire.context.path.parent().unwrap();
        let moved = directory.with_extension("saved");
        std::fs::rename(directory, &moved).unwrap();
        symlink(&moved, directory).unwrap();
        assert!(wire.clone().materialize(&root, 7, &limits).unwrap_err().contains("without following links"));
        std::fs::remove_file(directory).unwrap();
        std::fs::rename(&moved, directory).unwrap();
        let input = ToolProcessPrimaryInput::capture_invocation(&root, 7, &wire, &limits).unwrap();
        std::fs::write(&wire.context.path, b"{}").unwrap();
        assert!(input.retire().is_err());
        assert!(wire.manifest_path.exists());
        assert!(wire.context.path.exists());
    }
}
