use super::*;

#[tokio::test]
#[ignore = "requires an explicitly selected Home with a checked pending capability amendment"]
async fn selected_home_recovers_the_checked_capability_amendment_without_writing_or_replaying() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let descriptor_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(descriptor_dir.join("interface.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), descriptor_dir, Some(descriptor.clone()),
        home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        scratch.path().join("unused-index.sqlite"),
        BTreeMap::from([
            ("run_records_root".into(), home.join("state/runs")),
            ("interface_hand_in_root".into(), home.join("interface-hand-in")),
        ]), sender, Default::default(),
    ).with_host_io_limits(InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576, max_manifest_bytes: 1_048_576,
        max_payload_bytes: 268_435_456, max_stderr_bytes: 262_144,
    }).unwrap();
    let run_path = home.join("state/runs").join(format!("{run}.json"));
    let current = std::fs::read(&run_path).unwrap();
    let ledger = std::fs::read(&ctx.movements_path).unwrap();
    let mut replay = octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();
    let mut selected = None;
    while let Some(batch) = replay.read_batch(256).unwrap() {
        if let Some(own) = batch.into_iter().find(|own| own.movement.seq.0 == source) {
            selected = Some(replay.envelope(&own).unwrap());
            break;
        }
    }
    let input = selected.expect("the selected provider source must exist");
    let hash = input.runtime().unwrap().run_record_hash.as_ref().unwrap();
    let pinned = std::fs::read(ctx.blobs_dir.join(hash)).unwrap();
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    let handoff = session.from_invocation(&ctx, &descriptor, &run, &input, source, &pinned).unwrap();
    assert_eq!(handoff.config.checkpoints, topology(&RunRecord::from_bytes(&current).unwrap(), &provider).unwrap().checkpoints);
    let state = handoff.state.lock().await;
    assert_eq!(state.run_record_bytes, current);
    let pending = persisted_pending_handoff(&RunRecord::from_bytes(&current).unwrap(), &provider).unwrap().unwrap();
    assert_eq!(pending.source_movement_seq, source);
    assert!(!pending.terminal);
    assert_eq!(std::fs::read(run_path).unwrap(), current);
    assert_eq!(std::fs::read(&ctx.movements_path).unwrap(), ledger);
}
