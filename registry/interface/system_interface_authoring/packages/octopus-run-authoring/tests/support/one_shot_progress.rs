async fn exercise_one_shot_progress(
    handoff: &mut ProviderOutputHandoff,
    arrivals: &mut tokio::sync::mpsc::Receiver<
        octopus_interface_transport::InterfaceActivationArrival,
    >,
) -> u64 {
    use std::io::Write as _;
    let HandoffDeliveryMode::OneShot(plan) = &mut handoff.delivery_mode else {
        panic!("one-shot fixture required")
    };
    for (label, checkpoint) in [("hook.catalog", "hooks"), ("turn.reasoning", "reasoning")] {
        plan.terminal.insert(
            label.into(),
            OneShotHandoffSlot {
                checkpoint: CheckpointId::new(checkpoint),
                epoch: 0,
                required_mark: "provider.output.admitted".into(),
                required_verifier: "artifact_exists".into(),
            },
        );
    }
    let root = handoff.var_dir.join("progress-inputs");
    let hooks = InterfaceArtifactFile::write(&root, "hook.catalog", br#"{"hooks":[]}"#).unwrap();
    let dispatch = InterfaceArtifactFile::write(
        &root,
        PROVIDER_TRANSPORT_DISPATCH_LABEL,
        &transport_dispatch_payload(
            "POST",
            "http",
            0,
            7,
            blake3::hash(b"request").to_hex().to_string(),
            None,
        ),
    )
    .unwrap();
    let reasoning =
        InterfaceArtifactFile::write(&root, "turn.reasoning", br#"{"summary":"reviewing"}"#)
            .unwrap();
    let delivery = async {
        for (index, (label, checkpoint, mark)) in [
            ("hook.catalog", "hooks", "provider.output.admitted"),
            (
                PROVIDER_TRANSPORT_DISPATCH_LABEL,
                "dispatch_1",
                "provider.dispatch.admitted",
            ),
            ("turn.reasoning", "reasoning", "provider.output.admitted"),
        ]
        .into_iter()
        .enumerate()
        {
            let delivery = arrivals.recv().await.unwrap();
            assert_eq!(delivery.artifact.read_manifest().unwrap().label, label);
            let payload = delivery.artifact.read_payload_bytes().unwrap();
            assert_eq!(handoff.is_one_shot_progress(&payload).unwrap(), index != 1);
            let artifact = pin_artifact(&delivery.artifact, &handoff.ctx.blobs_dir);
            let seq = 1 + index as u64 * 2;
            let mut ledger = std::fs::OpenOptions::new()
                .append(true)
                .open(&handoff.ctx.movements_path)
                .unwrap();
            for row in [
                one_shot_movement(seq, &artifact, checkpoint, MovementKind::InterfaceArrival),
                one_shot_movement(
                    seq + 1,
                    &artifact,
                    checkpoint,
                    MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(mark)),
                ),
            ] {
                writeln!(ledger, "{}", serde_json::to_string(&row).unwrap()).unwrap();
            }
            ledger.sync_all().unwrap();
            delivery
                .verdict_tx
                .unwrap()
                .send(DeliveryVerdict::Admitted { seq })
                .unwrap();
        }
    };
    let progress = async {
        let emitted = handoff
            .hand(ProviderEmission {
                artifact: hooks,
                route: ProviderEmissionRoute::ClientProjection,
            })
            .await
            .unwrap();
        assert!(matches!(
            emitted,
            ProviderHandoffOutcome::Admitted { seq: 1, .. }
        ));
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Process
        );
        assert!(matches!(
            handoff.hand_transport_dispatch(dispatch).await.unwrap(),
            ProviderHandoffOutcome::Admitted { seq: 3, .. }
        ));
        let outcomes = handoff
            .hand_progress_batch(vec![ProviderEmission {
                artifact: reasoning,
                route: ProviderEmissionRoute::ClientProjection,
            }])
            .await
            .unwrap();
        assert!(matches!(
            outcomes.as_slice(),
            [ProviderHandoffOutcome::Admitted { seq: 5, .. }]
        ));
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Interrupted {
                last_artifact_label: PROVIDER_TRANSPORT_DISPATCH_LABEL.into(),
            }
        );
        let replay =
            InterfaceArtifactFile::write(&root, "hook.catalog", br#"{"hooks":[]}"#).unwrap();
        assert_eq!(
            handoff
                .hand(ProviderEmission {
                    artifact: replay,
                    route: ProviderEmissionRoute::ClientProjection
                })
                .await
                .unwrap(),
            emitted
        );
        let conflicting =
            InterfaceArtifactFile::write(&root, "hook.catalog", br#"{"hooks":["changed"]}"#)
                .unwrap();
        assert!(
            handoff
                .hand(ProviderEmission {
                    artifact: conflicting,
                    route: ProviderEmissionRoute::ClientProjection
                })
                .await
                .unwrap_err()
                .contains("already spent")
        );
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(progress, delivery);
    })
    .await
    .unwrap();
    assert!(matches!(
        arrivals.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    assert!(!handoff.run_path.exists());
    assert_eq!(handoff.state.lock().await.run_record_bytes, b"not authored");
    7
}
