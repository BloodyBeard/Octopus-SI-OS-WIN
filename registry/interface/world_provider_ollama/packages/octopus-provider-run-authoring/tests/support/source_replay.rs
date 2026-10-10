use super::*;

#[test]
#[ignore = "requires explicitly selected read-only historical ledger evidence"]
fn selected_ledger_checked_authoring_history() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), home.join("registry/interface").join(provider.as_str()),
        None, home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"), BTreeMap::new(), arrivals, Default::default())
        .with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits { max_control_frame_bytes:1_048_576,
            max_stderr_bytes:262_144,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456 }).unwrap();
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let mut selected = BTreeMap::new();
    for (line, row) in std::io::BufReader::new(std::fs::File::open(&ctx.movements_path).unwrap()).lines().enumerate() {
        let row = row.unwrap(); let value: serde_json::Value = serde_json::from_str(&row).unwrap();
        let seq = value["seq"].as_u64().unwrap();
        if [1452,1456,1471,1473,1477,1478,1481].contains(&seq) {
            selected.insert(seq, decoder.decode(row.as_bytes(), line + 1).unwrap());
        }
    }
    let runtime = |seq| selected[&seq].runtime().unwrap();
    for seq in [1452,1456,1477,1481] {
        assert_eq!(runtime(seq).run, run);
        assert_eq!(selected[&seq].to, Endpoint::Interface(provider.clone()));
    }
    let snapshot = |seq| pinned_run_record(&ctx, &ContentHash::from_hex(
        runtime(seq).run_record_hash.as_ref().unwrap()).unwrap(), "historical ledger").unwrap();
    let (before, _) = snapshot(1452); let (after, record) = snapshot(1481);
    let (call_manifest, call_bytes) = read_pinned_artifact(&ctx, selected[&1471].artifact.as_ref().unwrap()).unwrap();
    let call: FileWriteCallEnvelope = serde_json::from_slice(&call_bytes).unwrap();
    let (result, _) = read_pinned_artifact(&ctx, selected[&1473].artifact.as_ref().unwrap()).unwrap();
    let profile = RunAuthoringProfile { file_write_tool:call.tool, file_write_label:call_manifest.label,
        file_write_result_label:result.label, result_timeout_ms:1000 };
    let config = config_from_persisted_profile(&record, &provider, &profile).unwrap();
    let evidence = CheckedRunEvidence { movements_path:&ctx.movements_path, ledger_index_path:&ctx.ledger_index_path,
        blobs_dir:&ctx.blobs_dir,max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456 };
    let writer = CheckedRunWriter { owner:provider.clone(),source:config.authoring_source_interface.clone(),
        door:config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap().interface().clone(),
        tool:profile.file_write_tool,call_label:profile.file_write_label,result_label:profile.file_write_result_label };
    let start = concurrent_writer_evidence_start(&evidence, &RunRecord::from_bytes(&before).unwrap(), &before, 1452).unwrap();
    let chain = checked_run_file_chain_state(&evidence, &call.arguments.path, &before, &after, start, &[writer]).unwrap();
    for transition in &chain { validate_checked_provider_transition(transition).unwrap(); }
    assert_eq!(chain.len(), 3);
    assert_eq!(record.interface_state[&provider]["authoring_generation"], serde_json::json!(68));
    assert_eq!(persisted_pending_handoff(&record, &provider).unwrap().unwrap().source_movement_seq, 1456);
    assert_eq!(runtime(1477).checkpoint, runtime(1478).checkpoint);
    assert_ne!(runtime(1477).checkpoint, runtime(1481).checkpoint);
    eprintln!("historical ledger: 3 checked writes through generation 68; source 1452 precedes Stop 1456; message 1477 and Stop 1481 use distinct checkpoints");
}

#[tokio::test]
#[ignore = "requires explicitly selected read-only replay evidence"]
async fn selected_home_independent_handoff_replays_unconsumed_source() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source_movement_seq = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let run_path = home.join("state/runs").join(format!("{run}.json"));
    let current = std::fs::read(&run_path).unwrap();
    let record = RunRecord::from_bytes(&current).unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let profile = run_authoring_profile(&descriptor).unwrap();
    let (config, bound_topology) = resolve_profile_invocation(&profile, &provider, &run, &record, None).unwrap();
    let (context_output_labels, session_snapshot_labels) = provider_context_label_sets(&descriptor).unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let mut ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor), home.join("interfaces").join(provider.as_str()),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"), BTreeMap::new(), arrivals,
        tokio_util::sync::CancellationToken::new(),
    ).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576, max_stderr_bytes: 262_144,
        max_manifest_bytes: 1_048_576, max_payload_bytes: 268_435_456,
    }).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    ctx.ledger_index_path = scratch.path().join("unused-index.sqlite");
    let mut reader_ctx = ctx.clone();
    reader_ctx.var_dir = scratch.path().to_path_buf();
    let result_reader = LedgerTailReader::open_named(&reader_ctx, "replay-audit").unwrap();
    let handoff = ProviderOutputHandoff {
        session: ProviderHandoffSession::new(&reader_ctx).unwrap(),
        hand_in_root: home.join("interface-hand-in").join(config.authoring_source_interface.as_str()),
        var_dir: ctx.var_dir.clone(), ctx, config, run_path, source_movement_seq,
        source_carries_context: true, failed_source: false,
        context_output_labels, session_snapshot_labels,
        granted_tools: BTreeSet::new(), granted_interfaces: BTreeSet::new(), granted_mark_outcomes: BTreeSet::new(),
        bound_topology, delivery_mode: HandoffDeliveryMode::Rearm, invocation_session_snapshot: None,
        result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            context_artifacts: Vec::new(), static_context_artifacts: Vec::new(),
            session_context: ResolvedSessionContext { provider_interface: provider.clone(), artifacts: Vec::new() },
            session_snapshot_head: None, run_record_bytes: current.clone(), run_record_evidence_seq: None,
            consumed_input: true, consumed_context_free_independent_input: None, consumed_verified_input: false,
        })),
    };
    let pending = persisted_pending_handoff(&record, &provider).unwrap().unwrap();
    assert!(pending.source_movement_seq > source_movement_seq);
    let recovered = handoff.recover_overtaken_source(&current, &pending).unwrap();
    assert_eq!((&recovered.0, &recovered.1), (&ProviderSourceRecovery::Process, &Vec::new()));
    assert!(pending.context_publication.is_none());
    assert!(handoff.recover_authoring_failure(&record, &current).unwrap().is_none());
    assert!(matches!(handoff.recover_authoring_attempt(&record, &current).unwrap(), AuthoringAttemptRecovery::None));
    let (_, source) = handoff.session.index().unwrap().exact_provider_source_outbound(
        &handoff.config.run, &handoff.config.worker, &provider, source_movement_seq,
    ).unwrap();
    let (manifest, payload) = read_pinned_artifact(&handoff.ctx, &source.artifact).unwrap();
    append_session_payload(&mut handoff.state.lock().await.session_context,
        &manifest.label, &payload, &source.artifact, Some(source_movement_seq), "replayed input").unwrap();
    assert_eq!(handoff.recover_source_with_outcomes().await.unwrap(), (recovered.0, recovered.1));
    let context = handoff.current_session_context_artifacts().await;
    assert_eq!(context.last().unwrap().source_artifact, source.artifact);
    assert_eq!(context.last().unwrap().source_movement_seq, Some(source_movement_seq));
    let mut unadmitted = pending.clone();
    unadmitted.items[0].checkpoint.epoch += 1;
    assert!(handoff.recover_overtaken_source(&current, &unadmitted).is_err());
    let mut unrelated = pending.clone();
    unrelated.source_movement_seq = source_movement_seq;
    assert!(handoff.recover_overtaken_source(&current, &unrelated).unwrap_err().contains("independent-input"));
    let mut incomplete = pending.clone();
    incomplete.terminal = false;
    assert!(handoff.recover_overtaken_source(&current, &incomplete).unwrap_err().contains("independent-input"));
    let mut changed: serde_json::Value = serde_json::from_slice(&current).unwrap();
    changed["status"] = serde_json::json!("running");
    assert!(handoff.recover_overtaken_source(&serde_json::to_vec(&changed).unwrap(), &pending).is_err());
    assert_eq!(std::fs::read(&handoff.run_path).unwrap(), current);
    eprintln!("verified source {source_movement_seq} remains Process behind admitted independent source {}", pending.source_movement_seq);
}

#[tokio::test]
#[ignore = "requires explicitly selected read-only replay evidence"]
async fn selected_home_failed_source_rearms_its_consumed_input() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source_movement_seq: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor.clone()), scratch.path().join("var"),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        scratch.path().join("unused-index.sqlite"),
        BTreeMap::from([
            ("run_records_root".to_string(), home.join("state/runs")),
            ("interface_hand_in_root".to_string(), scratch.path().join("hand-in")),
        ]),
        arrivals, tokio_util::sync::CancellationToken::new(),
    ).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576, max_stderr_bytes: 262_144,
        max_manifest_bytes: 1_048_576, max_payload_bytes: 268_435_456,
    }).unwrap();
    std::fs::create_dir_all(&ctx.var_dir).unwrap();
    let mut replay = octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();
    let mut input = None;
    while input.is_none() {
        let Some(batch) = replay.read_batch(256).unwrap() else { break; };
        for own in batch {
            if own.movement.seq.0 == source_movement_seq {
                input = Some(replay.envelope(&own).unwrap());
            }
        }
    }
    let input = input.expect("selected source is not an outbound movement of this provider");
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    eprintln!(
        "consumed context input sources: {:?}",
        session.consumed_context_input_sources(&ctx, &descriptor).unwrap()
    );
    let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    let canonical = session
        .from_invocation(&ctx, &descriptor, &run, &input, source_movement_seq, &pinned)
        .err();
    eprintln!("canonical handoff for source {source_movement_seq}: {}", canonical.as_deref().unwrap_or("formed"));
    let failed = session
        .failed_source_handoff(&ctx, &descriptor, &run, &input, source_movement_seq)
        .unwrap();
    let error = InterfaceArtifactFile::write(
        ctx.var_dir.join("emitted"), "turn.error",
        br#"{"label":"turn.error","message":"replayed failed source"}"#,
    ).unwrap();
    let state = failed.state.lock().await;
    let before = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
    let identity = artifact_identity(&error, &failed.var_dir).unwrap();
    let output_checkpoint = failed.config.output_checkpoint(
        &ProviderHandoffRoute::ClientProjection, &failed.granted_tools, &failed.granted_interfaces, &identity.label,
    ).unwrap();
    let authored = failed.author_run_with_bindings(
        &state.run_record_bytes, &state.context_artifacts,
        &[PreparedHandoff { identity, output_checkpoint, verified_inputs: Vec::new() }],
        state.consumed_input, state.consumed_context_free_independent_input.as_ref(),
        state.consumed_verified_input, true, &[],
    ).unwrap();
    let after = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    let checkpoint = &input.runtime().unwrap().checkpoint;
    eprintln!(
        "failed source {source_movement_seq}: consumed_input={} checkpoint '{}' active before={} after={}",
        state.consumed_input, checkpoint.as_str(),
        before.active_checkpoints.contains(checkpoint), after.active_checkpoints.contains(checkpoint),
    );
    assert!(authored.pending_handoff.terminal && authored.pending_handoff.source_without_context);
    assert_eq!(after.active_checkpoints.contains(checkpoint), !state.consumed_input && before.active_checkpoints.contains(checkpoint));
    assert_eq!(current_context_artifacts(&after, &failed.config).unwrap(), state.context_artifacts);
    assert_eq!(std::fs::read(home.join("state/runs").join(format!("{run}.json"))).unwrap(), state.run_record_bytes);
}
