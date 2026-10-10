async fn exercise_interrupted_source_order(
    original_ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor,
    initial: &[u8], input: &InterfaceInvocationEnvelope,
) {
    fn append(h: &ProviderOutputHandoff, rows: &mut Vec<Movement>, row: Movement) {
        rows.push(row);
        std::fs::write(&h.ctx.movements_path, rows.iter().map(|row|
            format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    }
    fn edge(h: &ProviderOutputHandoff, rows: &mut Vec<Movement>, bytes: &[u8], cp: &CheckpointId,
        file: &InterfaceArtifactFile, target: InterfaceId, kind: MovementKind) {
        let record = RunRecord::from_bytes(bytes).unwrap();
        let checkpoint = record.checkpoint(cp).unwrap();
        let hash = ContentHash::of_bytes(bytes).to_hex();
        std::fs::write(h.ctx.blobs_dir.join(&hash), bytes).unwrap();
        let mut metadata = InvocationMetadata::new(GrantProjection {
            tools: vec![h.config.file_write_tool.clone()], interfaces: vec![target.to_string()], ..Default::default()
        }).with_runtime(RuntimeInvocationMetadata {
            run: record.id.clone(), ee: record.ee.clone(), worker: record.worker.clone(), checkpoint: cp.clone(),
            interface: if kind == MovementKind::InterfaceArrival { h.ctx.interface.clone() } else { target.clone() },
            epoch: checkpoint.epoch.0, run_record_hash: Some(hash),
        }).with_outbound_interface_bindings(checkpoint.outbound_interface_bindings.iter().map(|b| b.interface.clone()).collect())
            .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {
                inbound: checkpoint.interface_binding.clone().unwrap(), outbound: checkpoint.outbound_interface_bindings.clone(),
            }).with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {
                artifact_label: file.read_manifest().unwrap().label, required_artifact_labels: vec![], complete: true,
            });
        metadata.artifact_binding_refs = checkpoint.artifact_bindings.iter().map(|binding| {
            let hash = binding.id.as_str().rsplit(':').next().and_then(ContentHash::from_hex).unwrap();
            ArtifactRef { id: binding.id.clone(), content: hash }
        }).collect();
        let mut row = ledger_row(rows.len() as u64);
        row.kind = kind; row.from = Endpoint::Interface(checkpoint.interface_binding.as_ref().unwrap().interface.clone());
        row.to = Endpoint::Interface(target); row.artifact = Some(pin_artifact(file, &h.ctx.blobs_dir));
        row.invocation_metadata = Some(metadata.into()); append(h, rows, row);
    }
    fn install(h: &ProviderOutputHandoff, rows: &mut Vec<Movement>, authored: &AuthoredRun) {
        let before = std::fs::read(&h.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        let call = h.materialize_file_write_call(authored).unwrap();
        let cp = active_checkpoint_for_role(&record, &h.config, RunAuthoringCheckpointRole::AuthoringCall).unwrap();
        edge(h, rows, &before, &cp.id, &call, authored.authoring_result_interface.clone(), MovementKind::InterfaceOutbound);
        std::fs::write(&h.run_path, &authored.content).unwrap();
        let result = InterfaceArtifactFile::write(h.var_dir.join("results"), &h.config.file_write_result_label,
            &serde_json::to_vec(&serde_json::json!({"label":h.config.file_write_result_label,"tool":h.config.file_write_tool,
                "correlation_id":authored.correlation_id,"ok":true,"body":{"result":"files_written","writes":[{
                    "path":h.run_path,"before_hash":authored.before_hash,"after_hash":authored.after_hash,"bytes":authored.content.len()}]}})).unwrap()).unwrap();
        edge(h, rows, authored.content.as_bytes(), &authored.authoring_result_checkpoint, &result, h.ctx.interface.clone(), MovementKind::InterfaceOutbound);
    }
    let mut ctx = original_ctx.clone();
    ctx.movements_path = ctx.var_dir.join("ordered-replay.jsonl");
    ctx.ledger_index_path = ctx.var_dir.join("ordered-replay.sqlite");
    ctx.root_slots.insert("interface_hand_in_root".into(), ctx.var_dir.join("ordered-hand-in"));
    std::fs::write(&ctx.movements_path, b"").unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(initial).unwrap();
    value["checkpoints"][0]["artifact_bindings"] = serde_json::json!([]);
    let run = RunRecord::from_bytes(initial).unwrap().id;
    let mut topology: BoundRunAuthoringTopology = serde_json::from_value(value["interface_state"][ctx.interface.as_str()][RUN_AUTHORING_BINDING].clone()).unwrap();
    let control: RunAuthoringCheckpoint = serde_json::from_value(serde_json::json!({"name":"control","role":"independent_input",
        "interface_binding":names_binding("system_worker_widget_return", &["client.surface.document"]),"outbound_interface_bindings":[names_binding(ctx.interface.as_str(), &["client.surface.document"]), names_binding("world_home_client", &["client.surface.document"])],
        "include_context":false,"required_verified_artifacts":[]})).unwrap();
    topology.checkpoints.push(control.clone());
    value["interface_state"][ctx.interface.as_str()][RUN_AUTHORING_BINDING] = serde_json::to_value(topology).unwrap();
    value["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&control, "control", &run, &[]));
    value["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("control"));
    let initial = serde_json::to_vec(&value).unwrap();
    std::fs::write(ctx.root_slot("run_records_root").unwrap().join(format!("{run}.json")), &initial).unwrap();
    let mut input = input.clone();
    input.metadata.as_mut().unwrap().runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(&initial).to_hex());
    let mut h = ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx, descriptor, &run, &input, 1452, &initial).unwrap();
    h.session_snapshot_labels.clear(); h.context_output_labels = BTreeSet::from(["interface.artifact.output".into()]);
    let widget = InterfaceArtifactFile::write(h.var_dir.join("widget"), "client.surface.document", br#"{"id":"task-widget","version":1}"#).unwrap();
    let second_widget = InterfaceArtifactFile::write(h.var_dir.join("widget"), "client.surface.document", br#"{"id":"another-task-widget","version":1}"#).unwrap();
    let queued = InterfaceArtifactFile::write(h.var_dir.join("queued"), "operator.message", br#"{"text":"queued new message"}"#).unwrap();
    let queued_ref = pin_artifact(&queued, &h.ctx.blobs_dir);
    let mut rows = (0..1452).map(ledger_row).collect::<Vec<_>>();
    for (seq, role) in [(1452, RunAuthoringCheckpointRole::Stable), (1456, RunAuthoringCheckpointRole::IndependentInput)] {
        while rows.len() < seq { rows.push(ledger_row(rows.len() as u64)); }
        let record = RunRecord::from_bytes(&initial).unwrap();
        let cp = record.checkpoints.iter().find(|cp| record.active_checkpoints.contains(&cp.id)
            && checkpoint_record_matches_template(cp, h.config.checkpoint_for_role(role).unwrap(), &run)).unwrap();
        edge(&h, &mut rows, &initial, &cp.id, if role == RunAuthoringCheckpointRole::IndependentInput { &widget } else { &input.artifact_file }, ctx.interface.clone(), MovementKind::InterfaceOutbound);
    }
    let mut closure = None;
    for source in [1456, 1452, 1481] {
        h.source_movement_seq = source;
        let current = std::fs::read(&h.run_path).unwrap();
        let file = InterfaceArtifactFile::write(h.var_dir.join("emitted"), "interface.artifact.output",
            &serde_json::to_vec(&serde_json::json!({"text":format!("source {source}")})).unwrap()).unwrap();
        let identity = artifact_identity(&file, &h.var_dir).unwrap();
        let mut context = current_context_artifacts(&RunRecord::from_bytes(&current).unwrap(), &h.config).unwrap();
        if source != 1481 { context.push(identity.artifact.id.clone()); }
        let authored = h.author_run(&current, &context, &[PreparedHandoff { identity: identity.clone(),
            output_checkpoint:h.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),verified_inputs:vec![] }],
            true, (source != 1452).then_some(&control), false, true).unwrap();
        if source == 1452 {
            h.materialize_file_write_call(&authored).unwrap();
            let mut stop = h.clone(); stop.source_movement_seq = 1481;
            stop.state.lock().await.run_record_bytes = current.clone();
            assert!(stop.recover_source().await.unwrap_err().contains("private attempt belongs to source movement 1452, current movement is 1481"));
        }
        if source != 1452 {
            let before = RunRecord::from_bytes(&current).unwrap();
            let after = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
            let old = before.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &control, &run)).unwrap();
            let new = after.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &control, &run)).unwrap();
            assert_ne!(old.id, new.id, "each widget receipt authors its own next receiving checkpoint");
            assert_eq!(new.outbound_interface_bindings, control.outbound_interface_bindings);
            assert_eq!(after.status, serde_json::json!("idle"));
        }
        install(&h, &mut rows, &authored);
        edge(&h, &mut rows, authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id, &file,
            h.config.worker.clone(), MovementKind::InterfaceArrival);
        if source == 1452 { closure = Some(identity.artifact); }
        let published = if authored.pending_handoff.context_publication.is_some() {
            let publication = h.author_context_publication(authored.content.as_bytes(), &authored.pending_handoff).unwrap();
            install(&h, &mut rows, &publication); publication
        } else { authored };
        if source == 1456 {
            for (seq, role) in [(1477, RunAuthoringCheckpointRole::Stable), (1481, RunAuthoringCheckpointRole::IndependentInput)] {
                while rows.len() < seq { rows.push(ledger_row(rows.len() as u64)); }
                let record = RunRecord::from_bytes(published.content.as_bytes()).unwrap();
                let cp = record.checkpoints.iter().find(|cp| record.active_checkpoints.contains(&cp.id)
                    && checkpoint_record_matches_template(cp, h.config.checkpoint_for_role(role).unwrap(), &run)).unwrap().id.clone();
                edge(&h, &mut rows, published.content.as_bytes(), &cp, if seq == 1477 { &queued } else { &second_widget }, ctx.interface.clone(), MovementKind::InterfaceOutbound);
            }
        }
    }
    let current = std::fs::read(&h.run_path).unwrap();
    h.session = ProviderHandoffSession::new(&h.ctx).unwrap();
    for (seq, expected) in [(1452, ProviderSourceRecovery::Completed), (1456, ProviderSourceRecovery::Completed), (1481, ProviderSourceRecovery::Completed), (1477, ProviderSourceRecovery::Process)] {
        h.source_movement_seq = seq;
        let mut state = h.state.lock().await; state.run_record_bytes = current.clone(); state.session_context.artifacts.clear();
        if seq == 1477 { append_session_payload(&mut state.session_context, "operator.message",
            &queued.read_payload_bytes().unwrap(), &queued_ref, Some(seq), "queued source").unwrap(); }
        drop(state);
        let (recovered, outcomes) = h.recover_source_with_outcomes().await.unwrap();
        assert_eq!(recovered, expected, "source {seq}");
        if seq == 1452 { assert!(outcomes.iter().any(|o| matches!(o, ProviderHandoffOutcome::Admitted {artifact,..} if Some(artifact) == closure.as_ref()))); }
        if seq == 1477 {
            let context = h.current_session_context_artifacts().await;
            assert!(context.iter().any(|entry| Some(&entry.source_artifact) == closure.as_ref()));
            assert_eq!(context.last().unwrap().source_artifact, queued_ref);
        }
    }
    let closure_row = rows.iter_mut().find(|row| row.artifact.as_ref() == closure.as_ref()).unwrap();
    *closure_row = ledger_row(closure_row.seq.0);
    std::fs::write(&h.ctx.movements_path, rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    h.session = ProviderHandoffSession::new(&h.ctx).unwrap();
    let error = h.recover_source().await.unwrap_err();
    assert!(error.contains("admitted ledger evidence"), "{error}");
}
