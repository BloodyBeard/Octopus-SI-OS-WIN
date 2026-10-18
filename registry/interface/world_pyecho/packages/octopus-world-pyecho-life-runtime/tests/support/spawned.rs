use super::*;

#[tokio::test]
async fn provider_bridge_preserves_retained_delivery_and_reply_contract() {
    let root = tempfile::tempdir().unwrap();
    let (arrivals, mut incoming) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(
        InterfaceId::new("provider"), root.path().join("interface"), None,
        root.path().to_path_buf(), root.path().join("movements.jsonl"),
        root.path().join("blobs"), root.path().join("index.sqlite"), BTreeMap::new(),
        arrivals, CancellationToken::new(),
    ).with_host_io_limits(InterfaceHostIoLimits {
        max_control_frame_bytes: 65536, max_stderr_bytes: 65536,
        max_manifest_bytes: 65536, max_payload_bytes: 65536,
    }).unwrap();
    let artifact = InterfaceArtifactFile::write(root.path(), "provider.output", b"payload").unwrap();
    let pending: PendingVerdicts = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let mut next = 1;
    for expected in [
        Ok(octopus_ledger::ArtifactDelivery::Pending { scope_seq: 5 }),
        Ok(octopus_ledger::ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { seq: 6 } }),
        Err("exact scope refused".to_string()),
    ] {
        let caller = ctx.hand_arrival_retained(artifact.clone(), Vec::new(), "provider-original-output");
        let host = async {
            let output = spawned_arrival_output(incoming.recv().await.unwrap(), &pending, &mut next).await.unwrap();
            let frame = serde_json::to_value(output).unwrap();
            assert_eq!(frame["kind"], "retained_arrival");
            assert_eq!(frame["delivery_id"], "provider-original-output");
            assert_eq!(frame["artifact_manifest_path"], artifact.manifest_path().to_str().unwrap());
            let response = serde_json::json!({"kind":"retained_delivery", "request_id":frame["request_id"], "delivery":expected});
            accept_spawned_life_input(&pending, serde_json::from_value(response).unwrap()).await.unwrap();
        };
        let (result, ()) = tokio::join!(caller, host);
        assert_eq!(result, expected);
        assert!(pending.lock().await.is_empty());
    }
    let caller = ctx.hand_arrival(artifact.clone(), Vec::new());
    let host = async {
        let frame = serde_json::to_value(spawned_arrival_output(incoming.recv().await.unwrap(), &pending, &mut next).await.unwrap()).unwrap();
        assert_eq!(frame["kind"], "arrival");
        accept_spawned_life_input(&pending, serde_json::from_value(serde_json::json!({
            "kind":"delivery_verdict", "request_id":frame["request_id"], "verdict":{"admitted":{"seq":7}}
        })).unwrap()).await.unwrap();
    };
    let (result, ()) = tokio::join!(caller, host);
    assert_eq!(result.unwrap(), DeliveryVerdict::Admitted { seq: 7 });
    let (reply, result) = tokio::sync::oneshot::channel();
    let request_id = register_spawned_reply(&pending, &mut next, SpawnedReply::Retained(reply)).await.unwrap();
    let wrong = SpawnedLifeInput::DeliveryVerdict { request_id, verdict: DeliveryVerdict::Admitted { seq: 8 } };
    assert!(accept_spawned_life_input(&pending, wrong).await.unwrap_err().contains("acknowledgement contract"));
    assert!(result.await.is_err());
    let (verdict_tx, _) = tokio::sync::oneshot::channel();
    let (reply, _) = tokio::sync::oneshot::channel();
    let invalid = InterfaceActivationArrival {
        artifact, interface: InterfaceId::new("provider"), evidence: Vec::new(), verdict_tx: Some(verdict_tx),
        retained: Some(octopus_interface_transport::RetainedInterfaceArrival { id: "original".into(), reply }),
    };
    assert!(spawned_arrival_output(invalid, &pending, &mut next).await.is_err());
    assert!(pending.lock().await.is_empty());
}
