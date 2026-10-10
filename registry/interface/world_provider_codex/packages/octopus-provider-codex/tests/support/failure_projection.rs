use super::*;
use octopus_interfaces::InterfaceArtifactFile;

#[tokio::test]
async fn refused_failed_source_projection_retains_its_exact_source() {
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
    let refusal = async {
        let arrival = receiver.recv().await.unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&arrival.artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(body["additional_details"]["source_movement_seq"], 7);
        arrival
            .verdict_tx
            .unwrap()
            .send(DeliveryVerdict::Refused {
                reason: octopus_ledger::RefusedReason::CheckpointSpent,
                detail: "spent fixture".into(),
            })
            .unwrap();
    };
    let (result, ()) = tokio::join!(
        record_invalid_provider_context(
            &ctx,
            &descriptor,
            &session,
            &run,
            &turn,
            "invalid fixture".into(),
        ),
        refusal
    );
    assert!(
        result
            .unwrap_err()
            .contains("failed provider source 7 has no admitted error projection")
    );
}
