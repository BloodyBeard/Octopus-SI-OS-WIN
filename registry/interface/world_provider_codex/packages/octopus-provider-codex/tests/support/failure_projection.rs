use super::*;
use octopus_interfaces::InterfaceArtifactFile;

#[tokio::test]
async fn unavailable_failed_source_projection_retains_original_error_without_arrival() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = temp.path().join("movements.jsonl");
    std::fs::write(&ledger, b"").unwrap();
    let descriptor: InterfaceDescriptor = serde_json::from_value(serde_json::json!({
        "id":"world_provider_fixture", "interface_type":"world", "role":"provider",
        "direction":"bidirectional", "source_names":["fixture"],
        "interface_driver_ref":"fixture",
    }))
    .unwrap();
    let (arrivals, mut receiver) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(
        descriptor.id.clone(),
        temp.path().into(),
        Some(descriptor.clone()),
        temp.path().join("var"),
        ledger,
        temp.path().join("blobs"),
        temp.path().join("index.sqlite"),
        Default::default(),
        arrivals,
        tokio_util::sync::CancellationToken::new(),
    )
    .with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 65536,
        max_stderr_bytes: 1024,
        max_manifest_bytes: 65536,
        max_payload_bytes: 65536,
    })
    .unwrap();
    let input = InterfaceArtifactFile::write(temp.path().join("input"), "operator.input", br#"{}"#)
        .unwrap();
    let turn = QueuedTurn {
        seq: 7,
        envelope: InterfaceInvocationEnvelope {
            artifact: materialized_artifact_ref(&input).unwrap(),
            artifact_file: input,
            artifact_binding_payloads: Vec::new(),
            metadata: None,
        },
    };
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    let run = RunId::new("run_fixture");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        record_invalid_provider_context(
            &ctx,
            &descriptor,
            &session,
            &run,
            &turn,
            "invalid fixture".into(),
        ),
    ).await.expect("an unavailable authored route must not attempt an unproven arrival");
    assert_eq!(result.unwrap_err(), "codex turn stopped because its admitted provider context is invalid (movement seq 7): invalid fixture");
    assert!(matches!(receiver.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
    assert!(std::fs::read(&ctx.movements_path).unwrap().is_empty());
    assert!(!ctx.interface_var_dir().join("emitted").exists());
    assert!(!ctx.interface_var_dir().join("ledger-cursor.json").exists());
}
