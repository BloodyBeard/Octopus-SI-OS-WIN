use super::*;

#[tokio::test]
#[ignore = "requires explicitly selected read-only replay evidence"]
async fn selected_home_deferred_continuity_preserves_exact_evidence() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let seq = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse::<u64>().unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let life: serde_json::Value = serde_json::from_slice(
        &std::fs::read(interface_dir.join("life.json")).unwrap(),
    ).unwrap();
    let limits = serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let emitted = scratch.path().join("provider/emitted");
    std::fs::create_dir_all(&emitted).unwrap();
    for entry in std::fs::read_dir(home.join("interfaces").join(provider.as_str()).join("emitted")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().ends_with(".artifact.json") {
            std::fs::copy(entry.path(), emitted.join(entry.file_name())).unwrap();
        }
    }
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor.clone()), scratch.path().join("provider"),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"),
        BTreeMap::from([
            ("run_records_root".into(), home.join("state/runs")),
            ("interface_hand_in_root".into(), scratch.path().join("hand-in")),
        ]), arrivals, Default::default(),
    ).with_host_io_limits(limits).unwrap();
    let path = home.join("interfaces").join(provider.as_str())
        .join("deferred-provider-turn").join(format!("{seq}.json"));
    let journal_bytes = std::fs::read(&path).unwrap();
    let staged = deferred_provider_turn_path(&ctx, seq);
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    std::fs::write(&staged, &journal_bytes).unwrap();
    upgrade_deferred_provider_turn_journals(&ctx, &descriptor).await.unwrap();
    let turn = load_deferred_provider_turns(&ctx, &descriptor).unwrap().remove(&seq).unwrap().turn;
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope).unwrap();
    let snapshot = bound_operator_session_snapshot(&ctx, &descriptor, &turn).unwrap();
    let bound = recover_operator_turn_continuity(
        &descriptor, &turn.envelope, snapshot.as_ref().map(|(_, snapshot)| snapshot), &run, &pinned,
    ).unwrap();
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    let handoff = session.from_invocation_when_ready(
        &ctx, &descriptor, &run, &turn.envelope, seq, &pinned,
    ).await.unwrap();
    let current = handoff.current_session_context_artifacts().await;
    let recovered = reconcile_operator_session_continuity(
        &descriptor, &turn.envelope, bound.clone(), &current,
    ).unwrap();
    let labels = codex_session_snapshot_labels(&descriptor).unwrap();
    let scan = latest_provider_session_carrier(&ctx, &run, seq, &labels, None).unwrap();
    assert!(scan.complete_boundary);
    assert!(scan.latest.is_none());
    let interrupted_path = interrupted_effect_journal_path(&ctx, &run);
    let interrupted_bytes = std::fs::read(home.join("interfaces").join(provider.as_str())
        .join("interrupted-effect-journal").join(interrupted_path.file_name().unwrap())).unwrap();
    std::fs::create_dir_all(interrupted_path.parent().unwrap()).unwrap();
    std::fs::write(&interrupted_path, &interrupted_bytes).unwrap();
    let (release, private) = checked_deferred_interrupted_result(
        &ctx, &descriptor, &run, &turn, bound.as_ref(), None, None,
    ).await.unwrap().expect("the exact reconciled result must settle private continuity");
    assert!(codex_continuity_successor_is_settled(bound.as_ref(), Some(&private)).unwrap());
    assert_eq!(serde_json::to_value(&private).unwrap()["context_generation"], 32);
    assert!(matches!(&release, DeferredProviderTurnRelease::InterruptedResult {
        result_source, closure, ..
    } if result_source.movement_seq == 896 && closure.movement_seq == 912));
    let (_, replayed) = checked_deferred_interrupted_result(
        &ctx, &descriptor, &run, &turn, bound.as_ref(), Some(&release), None,
    ).await.unwrap().unwrap();
    assert_eq!(replayed, private);
    let deferred = DeferredProviderTurn { run:run.clone(), turn:turn.clone(), release:None };
    let DeferredTurnReadiness::Ready(ready) = prepare_deferred_provider_turn(
        &ctx, &descriptor, &run, &deferred, &ActiveTurnControls::default(),
    ).await.unwrap() else { panic!("exact interruption must release the original queued input"); };
    assert_eq!(capture_deferred_provider_turn(&ctx, &run, &ready).unwrap(),
        capture_deferred_provider_turn(&ctx, &run, &turn).unwrap());
    let loaded = load_deferred_provider_turns(&ctx, &descriptor).unwrap();
    assert_eq!(loaded[&seq].release.as_ref(), Some(&release));
    assert!(matches!(prepare_deferred_provider_turn(&ctx, &descriptor, &run, &loaded[&seq],
        &ActiveTurnControls::default()).await.unwrap(), DeferredTurnReadiness::Ready(_)));
    let unchanged = handoff.current_session_context_artifacts().await;
    assert_eq!(unchanged.len(), current.len());
    assert!(unchanged.iter().zip(current.iter()).all(|(left, right)|
        left.source_artifact == right.source_artifact && left.source_movement_seq == right.source_movement_seq
            && left.label == right.label && left.payload == right.payload));
    assert_eq!(std::fs::read(interrupted_path).unwrap(), interrupted_bytes);
    assert_eq!(std::fs::read(path).unwrap(), journal_bytes);
    eprintln!("read-only source {seq}: bound generation {}; ordinary checked generation {}; private exact-result generation {}; model-readable context unchanged",
        serde_json::to_value(bound.unwrap()).unwrap()["context_generation"],
        serde_json::to_value(recovered.unwrap()).unwrap()["context_generation"],
        serde_json::to_value(private).unwrap()["context_generation"]);
}

#[test]
#[ignore = "requires explicitly selected read-only replay evidence"]
fn selected_home_sealed_interruption_preserves_exact_evidence() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let seq = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse::<u64>().unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor.clone()),
        home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"),
        home.join("ledger/blobs"), home.join("ledger/ledger-index.sqlite"),
        BTreeMap::new(), arrivals, Default::default(),
    );
    let path = interrupted_effect_journal_path(&ctx, &run);
    let journal_bytes = std::fs::read(&path).unwrap();
    let journal: InterruptedEffectJournal = serde_json::from_slice(&journal_bytes).unwrap();
    let InterruptedEffectPhase::Resolving { result_source_movement_seq, source_artifact } = journal.phase else {
        panic!("selected evidence is not an interrupted result awaiting handoff");
    };
    assert_eq!(result_source_movement_seq, seq);
    let turn = QueuedTurn { seq, envelope: InterfaceInvocationEnvelope {
        artifact_file: octopus_interfaces::InterfaceArtifactFile::new(ctx.blobs_dir.join(source_artifact.content.to_hex())).unwrap(),
        artifact: source_artifact, artifact_binding_payloads: Vec::new(), metadata: None,
    } };
    let (emissions, closure) = recover_sealed_interrupted_result(&ctx, &descriptor, &turn, &journal.continuity)
        .unwrap().expect("selected interrupted source must have its sealed terminal pair");
    assert_eq!(emissions.len(), 2);
    assert_eq!(materialized_artifact_ref(&emissions[1].artifact).unwrap(), closure);
    let body = emissions[0].artifact.read_payload_bytes().unwrap();
    let rows = String::from_utf8(body.clone()).unwrap().lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()).collect::<Vec<_>>();
    let mut labels = BTreeMap::<String, usize>::new();
    for row in &rows {
        if row["type"] == "event" { *labels.entry(row["label"].as_str().unwrap().to_string()).or_default() += 1; }
    }
    assert_eq!(rows.last().unwrap()["source_movement_seq"], seq);
    assert_eq!(emissions[0].artifact.read_payload_bytes().unwrap(), body);
    assert_eq!(std::fs::read(path).unwrap(), journal_bytes);
    eprintln!("verified sealed source {seq}: {} rows, events {labels:?}, terminal {}", rows.len(), emissions[1].artifact.read_manifest().unwrap().payload_content_hash.unwrap());
}
