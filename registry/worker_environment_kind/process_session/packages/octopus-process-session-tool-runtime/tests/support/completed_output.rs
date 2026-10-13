use crate::*;
use super::{test_formation, test_lift_context, test_process_client_with_script, test_process_envelope,
    store_recovery_artifact, write_recovery_ledger};
use octopus_descriptors::RunId;
use octopus_interfaces::GrantProjection;
use octopus_ledger::{Lamport, Seq, TraceId};

async fn completed_fixture(root: &Path, output_count: usize) -> (InterfaceLiftContext, ToolProcessEnvelope, ToolProcessCompletion,
    tokio::sync::mpsc::Receiver<InterfaceActivationArrival>, Movement) {
    let (door, formation) = test_formation("run_completed_output");
    let mut ctx = test_lift_context(root, door.clone());
    let (arrivals, receiver) = tokio::sync::mpsc::channel(4);
    ctx.arrivals = arrivals;
    std::fs::create_dir_all(ctx.interface_var_dir()).unwrap();
    let generation = tool_process_generation_root(&ctx).unwrap().join("a".repeat(64));
    let input_root = generation.join("state/invocations");
    let emitted = input_root.join("1/artifact/emitted");
    let result = InterfaceArtifactFile::write(&emitted, "test.tool.result", &serde_json::to_vec(
        &ToolResultArtifact::success("test.tool", Some("completed-0".into()), serde_json::json!({"changed":true}))).unwrap()).unwrap();
    let mut artifacts = vec![result];
    if output_count == 2 { artifacts.push(InterfaceArtifactFile::write(&emitted, "test.extra", b"extra output").unwrap()); }
    let complete = serde_json::json!({"kind":"complete", "request_id":1, "artifact_manifest_paths":artifacts.iter().map(|a| a.manifest_path()).collect::<Vec<_>>()});
    let counter = root.join("effects");
    let script = format!("IFS= read -r request; printf effect >> '{}'; printf '%s\\n' '{complete}'; while IFS= read -r request; do printf REPLAY >> '{}'; done", counter.display(), counter.display());
    let mut client = test_process_client_with_script(root, &formation, &script);
    client.generation_dir = generation.clone();
    client.generation_root = generation.parent().unwrap().to_path_buf();
    client.input_root = input_root;
    client.output_roots = vec![generation.clone()];
    ctx.host_io_limits = Some(client.host_io_limits);
    let mut input = test_process_envelope(&door, &formation, 0);
    input.payload = serde_json::to_vec(&ToolCallArtifact {
        tool:"test.tool".into(), arguments:serde_json::json!({}), correlation_id:Some("completed-0".into()),
        label:Some("tool.call".into()), source_interface_evidence:BTreeMap::new(),
    }).unwrap();
    input.artifact = store_recovery_artifact(root, "tool.call", &input.payload);
    input.metadata.as_mut().unwrap().runtime.as_mut().unwrap().run = RunId::new(&formation.run);
    input.metadata = Some(InvocationMetadata::new(GrantProjection {
        tools:vec!["test.tool".into()], worker_environment_kinds:vec!["test.room".into()], ..Default::default()
    }).with_runtime(input.metadata.as_ref().unwrap().runtime().unwrap().clone()));
    let source = Movement { seq:Seq(0), lamport:Lamport(1), recorded_at_unix_ms:1, trace:TraceId("completed".into()),
        kind:MovementKind::InterfaceOutbound, from:Endpoint::Interface(InterfaceId::new("provider")),
        to:Endpoint::Interface(door), artifact:Some(input.artifact.clone()), invocation_metadata:Some(input.metadata.clone().unwrap().into()) };
    write_recovery_ledger(root, std::slice::from_ref(&source));
    let reader = LedgerTailReader::open_named(&ctx, "fixture").unwrap();
    input.output_source = Some(ToolDoorOutputSource { movement:source.clone(), envelope:reader.envelope_for_movement(&source).unwrap(), formation:Some(formation.clone()) });
    let tool_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../tool");
    let descriptor = load_tool_descriptor_from_dir(&tool_root, "process.list").unwrap().unwrap();
    WorkerEnvironmentRecordStore::new(ctx.interface_var_dir()).activate(&formation, 0).unwrap();
    let pid = client.child.id().unwrap();
    write_tool_process_owner_record(&generation, &ToolProcessOwnerRecord {
        schema_version:1, generation:"a".repeat(64), formation_id:Some(formation.worker_environment_id.to_string()),
        process_id:pid, process_group_id:pid as i32,
    }).unwrap();
    let completion = client.invoke(&ctx, descriptor, input.clone(), false, false, None).await.unwrap();
    client.child.kill().await.unwrap();
    assert_eq!(std::fs::read(&counter).unwrap(), b"effect");
    (ctx.with_source_movement(source.clone()), input, completion, receiver, source)
}

#[tokio::test]
async fn completed_outputs_survive_transient_refusal_and_cold_broker_recovery_without_effect_replay() {
    for (reason, cancelled_before_save) in [(RefusedReason::RegistryUnstable, true), (RefusedReason::RegistryUnstable, false),
        (RefusedReason::Policy, false), (RefusedReason::Malformed, false)] {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let (ctx, input, completion, mut arrivals, source) = completed_fixture(&root, 1).await;
        let expected_path = completion.artifacts[0].manifest_path().to_path_buf();
        let generation = completion.generation.clone();
        let expected_bytes = completion.artifacts[0].read_payload_bytes().unwrap();
        let journal_path = ctx.interface_var_dir().join(TOOL_DOOR_EFFECT_JOURNAL_FILE);
        let digest = tool_door_movement_digest(&source).unwrap();
        let mut journal = ToolDoorEffectJournal::open(journal_path.clone(), 0, 65536).unwrap();
        journal.prepare(0, &digest).unwrap(); journal.begin(0, &digest).unwrap();
        let mut retirement = None;
        if cancelled_before_save { ctx.cancel.cancel(); }
        let hot = hand_completed_tool_outputs(&ctx, &input, completion, None, &mut retirement);
        tokio::pin!(hot);
        if cancelled_before_save {
            assert!(matches!(hot.await, ToolProcessBrokerResult::OutputPending { .. }));
        } else {
            let first = tokio::select! { output=arrivals.recv()=>output.unwrap(), _=&mut hot=>panic!("completion ended before hand-in") };
            assert!(completed_output::CompletedOutputs::load(&ctx, 0).unwrap().is_some());
            first.verdict_tx.unwrap().send(DeliveryVerdict::Refused {reason, detail:serde_json::json!({"test":"refusal"})}).unwrap();
            if reason == RefusedReason::RegistryUnstable {
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    loop {
                        tokio::select! {
                            _ = &mut hot => panic!("transient refusal terminated"),
                            _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
                        }
                        let receipt = serde_json::to_value(completed_output::CompletedOutputs::load(&ctx, 0).unwrap().unwrap()).unwrap();
                        if receipt["outputs"][0]["admission"]["state"] == "settled" { break; }
                    }
                }).await.unwrap();
                ctx.cancel.cancel();
                assert!(matches!(hot.await, ToolProcessBrokerResult::OutputPending { .. }));
            } else {
                assert!(matches!(hot.await, ToolProcessBrokerResult::Refused {reason:observed,..} if observed==reason));
            }
        }
        drop(journal);
        let journal = ToolDoorEffectJournal::open(journal_path, 0, 65536).unwrap();
        assert_eq!(journal.state(0, &digest).unwrap(), Some(ToolDoorJournalState::InFlight));
        assert!(completed_output::retains_generation(&generation).unwrap());
        let mut cold_ctx = ctx.clone();
        cold_ctx.cancel = tokio_util::sync::CancellationToken::new();
        let ctx = cold_ctx;
        let owner = ToolDoorOwner { worker_environment_kind:WorkerEnvironmentKindId::new("test.room"),
            door_interface:ctx.interface.clone(), max_in_flight:1, private_read_max_bytes:4096, root_slot_transform:None };
        let host = Arc::new(ToolProcessBrokerHost::new(owner.clone()).unwrap());
        let cold = tool_door_loop(&ctx, &owner, host.clone(), default_tool_door_artifact_decoder);
        tokio::pin!(cold);
        if reason == RefusedReason::RegistryUnstable {
            let output = tokio::select! { output=arrivals.recv()=>output.unwrap(), result=&mut cold=>panic!("cold recovery failed: {result:?}") };
            assert_eq!(output.artifact.manifest_path(), expected_path);
            assert_eq!(output.artifact.read_payload_bytes().unwrap(), expected_bytes);
            output.verdict_tx.unwrap().send(DeliveryVerdict::Admitted {seq:1}).unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                tokio::select! {
                    result=&mut cold=>panic!("door ended before cursor commit: {result:?}"),
                    _=tokio::time::sleep(std::time::Duration::from_millis(5))=>{}
                }
                let cursor=ctx.interface_var_dir().join("ledger-cursor.tool-door.json");
                if cursor.exists() && serde_json::from_slice::<serde_json::Value>(&std::fs::read(cursor).unwrap()).unwrap()["seq"]==1 { break; }
            }
        }).await.unwrap();
        ctx.cancel.cancel();
        cold.await.unwrap();
        assert!(arrivals.try_recv().is_err());
        assert!(host.shards.lock().await.by_key.is_empty(), "recovery must not acquire an executable");
        assert_eq!(std::fs::read(root.join("effects")).unwrap(), b"effect");
        assert!(completed_output::CompletedOutputs::load(&ctx, 0).unwrap().is_none());
        assert!(!completed_output::retains_generation(&generation).unwrap());
    }
}

#[tokio::test]
async fn explicit_completed_output_import_replays_only_selected_receipt_behind_consumed_cursor() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (ctx, input, completion, mut arrivals, source) = completed_fixture(&root, 1).await;
    let owner = ToolDoorOwner { worker_environment_kind:WorkerEnvironmentKindId::new("test.room"), door_interface:ctx.interface.clone(),
        max_in_flight:1, private_read_max_bytes:4096, root_slot_transform:None };
    let before = std::fs::read(&ctx.movements_path).unwrap();
    let prepare = || prepare_tool_output_recovery(&ctx, &owner, 0, &completion.invocation_manifest,
        completion.artifacts[0].manifest_path(), default_tool_door_artifact_decoder);
    let prepared = prepare().unwrap();
    assert!(completed_output::CompletedOutputs::load(&ctx, 0).unwrap().is_none(), "preparation is read-only");
    let mut forged = source.clone(); forged.artifact.as_mut().unwrap().content=ContentHash::of_bytes(b"wrong");
    write_recovery_ledger(&root, &[forged]);
    assert!(prepare().is_err());
    write_recovery_ledger(&root, &[source]);
    prepared.import(&ctx).unwrap();
    prepare().unwrap().import(&ctx).unwrap();
    let mut reader = LedgerTailReader::open_named(&ctx, "tool-door").unwrap();
    let consumed = reader.read_new_uncommitted_limit(1).unwrap();
    reader.commit_through(&consumed[0]).unwrap();
    drop(reader);
    let cursor_path = ctx.interface_var_dir().join("ledger-cursor.tool-door.json");
    let cursor_before = std::fs::read(&cursor_path).unwrap();
    let (ctx, ready) = ctx.with_readiness_channel();
    let host = Arc::new(ToolProcessBrokerHost::new(owner.clone()).unwrap());
    let resumed = tool_door_loop(&ctx, &owner, host.clone(), default_tool_door_artifact_decoder);
    tokio::pin!(resumed);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::select! {
            ready = ready => ready.unwrap().unwrap(),
            result = &mut resumed => panic!("door ended before readiness: {result:?}"),
        }
    }).await.expect("pending result admission must not block door readiness");
    let output = tokio::select! { output=arrivals.recv()=>output.unwrap(), result=&mut resumed=>panic!("import did not hand its result: {result:?}") };
    assert_eq!(output.artifact.manifest_path(), completion.artifacts[0].manifest_path());
    assert!(completed_output::CompletedOutputs::load(&ctx, 0).unwrap().is_some());
    output.verdict_tx.unwrap().send(DeliveryVerdict::Admitted {seq:1}).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            tokio::select! {
                result = &mut resumed => panic!("door ended before completion receipt retired: {result:?}"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
            }
            if completed_output::CompletedOutputs::load(&ctx, 0).unwrap().is_none() { break; }
        }
    }).await.unwrap();
    ctx.cancel.cancel();
    resumed.await.unwrap();
    assert_eq!(std::fs::read(cursor_path).unwrap(), cursor_before);
    assert_eq!(std::fs::read(&ctx.movements_path).unwrap(), before);
    assert_eq!(std::fs::read(root.join("effects")).unwrap(), b"effect");
    assert!(host.shards.lock().await.by_key.is_empty());
    assert!(completed_output::CompletedOutputs::load(&ctx, input.source_movement_seq).unwrap().is_none());
}

#[tokio::test]
async fn completed_output_recovers_partial_admission_and_refuses_changed_evidence() {
    for (acknowledged, corrupt) in [(true,false), (false,false), (false,true)] {
        let root=tempfile::tempdir().unwrap();
        let root=root.path().canonicalize().unwrap();
        let (ctx,input,completion,mut arrivals,source)=completed_fixture(&root,2).await;
        let second=completion.artifacts[1].clone();
        let mut retirement=None;
        let hot=hand_completed_tool_outputs(&ctx,&input,completion,None,&mut retirement);
        tokio::pin!(hot);
        let first=tokio::select! { output=arrivals.recv()=>output.unwrap(), result=&mut hot=>panic!("unexpected completion: {result:?}") };
        let captured=first.artifact.read_bounded(65536,65536).unwrap();
        let hash=ContentHash::of_bytes(&captured.manifest_bytes);
        std::fs::write(ctx.blobs_dir.join(hash.to_hex()),&captured.manifest_bytes).unwrap();
        std::fs::write(ctx.blobs_dir.join(ContentHash::of_bytes(&captured.payload_bytes).to_hex()),&captured.payload_bytes).unwrap();
        let mut metadata=input.metadata.clone().unwrap();
        metadata.runtime.as_mut().unwrap().interface=InterfaceId::new("provider");
        let admitted=Movement {seq:Seq(1),lamport:Lamport(2),recorded_at_unix_ms:2,trace:source.trace.clone(),
            kind:MovementKind::InterfaceOutbound,from:Endpoint::Interface(ctx.interface.clone()),to:Endpoint::Interface(InterfaceId::new("provider")),
            artifact:Some(ArtifactRef {id:ArtifactId::new(format!("{}:{}",captured.manifest.label,hash.to_hex())),content:hash}),invocation_metadata:Some(metadata.into())};
        let mut ledger = vec![source,admitted];
        write_recovery_ledger(&root,&ledger);
        if acknowledged {
            first.verdict_tx.unwrap().send(DeliveryVerdict::Admitted {seq:1}).unwrap();
            let next=tokio::select! { output=arrivals.recv()=>output.unwrap(), result=&mut hot=>panic!("unexpected completion: {result:?}") };
            assert_eq!(next.artifact.manifest_path(),second.manifest_path());
            ledger.push(completed_refusal(&ctx, &second, 2, RefusedReason::RegistryUnstable));
            write_recovery_ledger(&root, &ledger);
        }
        ctx.cancel.cancel();
        assert!(matches!(hot.await,ToolProcessBrokerResult::OutputPending {..}));
        let mut cold_ctx=ctx.clone(); cold_ctx.cancel=tokio_util::sync::CancellationToken::new();
        let host=ToolProcessBrokerHost::new(ToolDoorOwner {worker_environment_kind:WorkerEnvironmentKindId::new("test.room"),door_interface:ctx.interface.clone(),
            max_in_flight:1,private_read_max_bytes:4096,root_slot_transform:None}).unwrap();
        let mut forged=input.clone(); forged.artifact.content=ContentHash::of_bytes(b"changed input");
        assert!(matches!(host.invoke(&cold_ctx,"test.tool".into(),serde_json::json!({}),forged,true).await,
            ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::OutputPending {..})));
        if corrupt { std::fs::write(second.read_manifest().unwrap().payload_path,b"changed output").unwrap(); }
        let recovered=host.invoke(&cold_ctx,"test.tool".into(),serde_json::json!({}),input.clone(),true);
        tokio::pin!(recovered);
        if corrupt {
            assert!(matches!(recovered.await,ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::OutputPending {..})));
        } else {
            let output=tokio::select! { output=arrivals.recv()=>output.unwrap(), _=&mut recovered=>panic!("second output was not rehanded") };
            assert_eq!(output.artifact.manifest_path(),second.manifest_path());
            output.verdict_tx.unwrap().send(DeliveryVerdict::Admitted {seq:2}).unwrap();
            assert!(matches!(recovered.await,ToolProcessBrokerInvocation::Completed(_)));
        }
        assert!(arrivals.try_recv().is_err());
        assert!(host.shards.lock().await.by_key.is_empty());
        assert_eq!(std::fs::read(root.join("effects")).unwrap(),b"effect");
    }
}

fn completed_refusal(ctx: &InterfaceLiftContext, artifact: &InterfaceArtifactFile, sequence: u64, reason: RefusedReason) -> Movement {
    let captured = artifact.read_bounded(65536, 65536).unwrap();
    let hash = ContentHash::of_bytes(&captured.manifest_bytes);
    Movement {
        seq: Seq(sequence), lamport: Lamport(sequence + 1), recorded_at_unix_ms: sequence + 1,
        trace: TraceId("completed-refusal".into()), kind: MovementKind::DeliveryRefused,
        from: Endpoint::Interface(ctx.interface.clone()), to: Endpoint::Interface(ctx.interface.clone()),
        artifact: Some(ArtifactRef { id: ArtifactId::new(format!("{}:{}", captured.manifest.label, hash.to_hex())), content: hash }),
        invocation_metadata: Some(DeliveryVerdict::refused(reason, serde_json::json!({"test":"lost acknowledgement"})).into()),
    }
}

#[tokio::test]
async fn completed_output_lost_refusal_ack_never_guesses_a_retry() {
    for (reason, evidence, prior_transient) in [
        (RefusedReason::Policy, "exact", false), (RefusedReason::Malformed, "exact", false),
        (RefusedReason::Policy, "boundary", false), (RefusedReason::Malformed, "boundary", false),
        (RefusedReason::RegistryUnstable, "boundary", false), (RefusedReason::RegistryUnstable, "exact", false),
        (RefusedReason::Policy, "exact", true), (RefusedReason::RegistryUnstable, "wrong_artifact", false),
        (RefusedReason::RegistryUnstable, "wrong_scope", false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let (ctx, input, completion, mut arrivals, source) = completed_fixture(&root, 1).await;
        let mut ledger = vec![source];
        let mut retirement = None;
        let hot = hand_completed_tool_outputs(&ctx, &input, completion, None, &mut retirement);
        tokio::pin!(hot);
        let mut output = tokio::select! { output=arrivals.recv()=>output.unwrap(), _=&mut hot=>panic!("missing first hand-in") };
        if prior_transient {
            let refusal = completed_refusal(&ctx, &output.artifact, 1, RefusedReason::RegistryUnstable);
            output.verdict_tx.take().unwrap().send(refusal.invocation_metadata.as_ref().unwrap().delivery_verdict().unwrap().clone()).unwrap();
            ledger.push(refusal);
            write_recovery_ledger(&root, &ledger);
            output = tokio::select! { output=arrivals.recv()=>output.unwrap(), _=&mut hot=>panic!("missing proven transient retry") };
        }
        let mut refusal = completed_refusal(&ctx, &output.artifact, ledger.len() as u64, reason);
        match evidence {
            "boundary" => refusal.artifact = None,
            "wrong_artifact" => refusal.artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"another output"),
            "wrong_scope" => {
                let octopus_ledger::MovementMetadata::Delivery(metadata) = refusal.invocation_metadata.as_mut().unwrap() else { unreachable!() };
                let mut runtime = input.metadata.as_ref().unwrap().runtime().unwrap().clone();
                runtime.run = RunId::new("another_run");
                metadata.runtime = Some(runtime);
            }
            _ => {}
        }
        let verdict = refusal.invocation_metadata.as_ref().unwrap().delivery_verdict().unwrap().clone();
        ledger.push(refusal);
        write_recovery_ledger(&root, &ledger);
        output.verdict_tx.take().unwrap().send(verdict).unwrap();
        ctx.cancel.cancel();
        assert!(matches!(hot.await, ToolProcessBrokerResult::OutputPending { .. }));
        let receipt = serde_json::to_value(completed_output::CompletedOutputs::load(&ctx, 0).unwrap().unwrap()).unwrap();
        assert_eq!(receipt["outputs"][0]["admission"]["state"], "awaiting_verdict");
        let mut cold = ctx.clone(); cold.cancel = tokio_util::sync::CancellationToken::new();
        let host = ToolProcessBrokerHost::new(ToolDoorOwner { worker_environment_kind:WorkerEnvironmentKindId::new("test.room"),
            door_interface:ctx.interface.clone(), max_in_flight:1, private_read_max_bytes:4096, root_slot_transform:None }).unwrap();
        let resumed = host.invoke(&cold, "test.tool".into(), serde_json::json!({}), input.clone(), true);
        tokio::pin!(resumed);
        if evidence != "exact" {
            assert!(matches!(resumed.await, ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::OutputPending { error })
                if error.contains("no exact verdict")), "{reason:?} {evidence}");
        } else if reason != RefusedReason::RegistryUnstable {
            assert!(matches!(resumed.await, ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Refused { reason: observed, .. }) if observed == reason));
        } else {
            let retry = tokio::select! { output=arrivals.recv()=>output.unwrap(), _=&mut resumed=>panic!("exact transient proof must permit retry") };
            assert_eq!(retry.artifact.manifest_path(), output.artifact.manifest_path());
            retry.verdict_tx.unwrap().send(DeliveryVerdict::Admitted { seq: 2 }).unwrap();
            assert!(matches!(resumed.await, ToolProcessBrokerInvocation::Completed(_)));
        }
        assert!(arrivals.try_recv().is_err());
        assert!(host.shards.lock().await.by_key.is_empty());
        assert_eq!(std::fs::read(root.join("effects")).unwrap(), b"effect");
    }
}

#[tokio::test]
async fn completed_output_recovery_waits_for_partial_tail_but_rejects_complete_corruption() {
    for case in ["partial", "irrelevant_tail", "malformed", "changed_source"] {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let (ctx, input, completion, mut arrivals, mut source) = completed_fixture(&root, 1).await;
        let mut retirement = None;
        let hot = hand_completed_tool_outputs(&ctx, &input, completion, None, &mut retirement);
        tokio::pin!(hot);
        let output = tokio::select! { output=arrivals.recv()=>output.unwrap(), _=&mut hot=>panic!("missing hand-in") };
        let mut admitted = completed_refusal(&ctx, &output.artifact, 1, RefusedReason::Policy);
        admitted.kind = MovementKind::InterfaceOutbound;
        admitted.to = Endpoint::Interface(InterfaceId::new("provider"));
        let mut metadata = input.metadata.clone().unwrap();
        metadata.runtime.as_mut().unwrap().interface = InterfaceId::new("provider");
        admitted.invocation_metadata = Some(metadata.into());
        output.verdict_tx.unwrap().send(DeliveryVerdict::Admitted {seq:1}).unwrap();
        ctx.cancel.cancel();
        assert!(matches!(hot.await, ToolProcessBrokerResult::OutputPending { .. }));
        if case == "changed_source" { source.trace = TraceId("changed original source".into()); }
        write_recovery_ledger(&root, &[source, admitted]);
        let complete = std::fs::read(&ctx.movements_path).unwrap();
        let source_end = complete.iter().position(|b| *b == b'\n').unwrap() + 1;
        let split = source_end + (complete.len() - source_end) / 2;
        match case {
            "partial" => std::fs::write(&ctx.movements_path, &complete[..split]).unwrap(),
            "irrelevant_tail" => std::fs::OpenOptions::new().append(true).open(&ctx.movements_path).unwrap().write_all(b"{\"seq\":2").unwrap(),
            "malformed" => std::fs::write(&ctx.movements_path, [&complete[..source_end], b"broken-json\n"].concat()).unwrap(),
            _ => {}
        }
        let mut cold = ctx.clone(); cold.cancel = tokio_util::sync::CancellationToken::new();
        let owner = ToolDoorOwner { worker_environment_kind:WorkerEnvironmentKindId::new("test.room"), door_interface:ctx.interface.clone(),
            max_in_flight:1, private_read_max_bytes:4096, root_slot_transform:None };
        let host = Arc::new(ToolProcessBrokerHost::new(owner.clone()).unwrap());
        if case != "partial" {
            let result = host.invoke(&cold, "test.tool".into(), serde_json::json!({}), input.clone(), true).await;
            if case == "irrelevant_tail" { assert!(matches!(result, ToolProcessBrokerInvocation::Completed(_))); }
            else { assert!(matches!(result, ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::OutputPending { .. }))); }
        } else {
            let mut reader = LedgerTailReader::open_named(&cold, "tool-door").unwrap();
            let consumed = reader.read_new_uncommitted_limit(1).unwrap(); reader.commit_through(&consumed[0]).unwrap(); drop(reader);
            let (cold, ready) = cold.with_readiness_channel();
            let resumed = tool_door_loop(&cold, &owner, host.clone(), default_tool_door_artifact_decoder);
            tokio::pin!(resumed);
            tokio::select! { ready=ready=>ready.unwrap().unwrap(), result=&mut resumed=>panic!("door ended before ready: {result:?}") }
            tokio::select! {
                result=&mut resumed=>panic!("growing tail took the ready door down: {result:?}"),
                _=tokio::time::sleep(std::time::Duration::from_millis(25))=>{}
            }
            assert!(arrivals.try_recv().is_err());
            let mut ledger = std::fs::OpenOptions::new().append(true).open(&cold.movements_path).unwrap();
            ledger.write_all(&complete[split..]).unwrap(); ledger.sync_all().unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    tokio::select! {
                        result=&mut resumed=>panic!("door failed to recover completed append: {result:?}"),
                        _=tokio::time::sleep(std::time::Duration::from_millis(5))=>{}
                    }
                    if completed_output::CompletedOutputs::load(&cold, 0).unwrap().is_none() { break; }
                }
            }).await.unwrap();
            cold.cancel.cancel(); resumed.await.unwrap();
        }
        assert!(arrivals.try_recv().is_err());
        assert!(host.shards.lock().await.by_key.is_empty());
        assert_eq!(std::fs::read(root.join("effects")).unwrap(), b"effect");
    }
}

#[test]
#[ignore = "explicit selected Home read-only completion recovery plan"]
fn selected_home_completed_output_recovery_plan() {
    let home=PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let door=InterfaceId::new(std::env::var("OCTOPUS_REPLAY_DOOR").unwrap());
    let source=std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let input=PathBuf::from(std::env::var("OCTOPUS_REPLAY_INVOCATION_MANIFEST").unwrap());
    let result=PathBuf::from(std::env::var("OCTOPUS_REPLAY_RESULT_MANIFEST").unwrap());
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let mut ctx=InterfaceLiftContext::unprivileged(door.clone(), home.join("registry/interface").join(door.as_str()), None,
        home.join("interfaces").join(door.as_str()), home.join("ledger/movements.jsonl"),home.join("ledger/blobs"),home.join("ledger/ledger-index.sqlite"),
        BTreeMap::new(),arrivals,tokio_util::sync::CancellationToken::new());
    ctx.host_io_limits=Some(InterfaceHostIoLimits {max_control_frame_bytes:1_048_576,max_stderr_bytes:262_144,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456});
    let owner=ToolDoorOwner { worker_environment_kind:WorkerEnvironmentKindId::new(std::env::var("OCTOPUS_REPLAY_KIND").unwrap()),
        door_interface:door,max_in_flight:1,private_read_max_bytes:4096,root_slot_transform:None };
    let prepared=prepare_tool_output_recovery(&ctx,&owner,source,&input,&result,default_tool_door_artifact_decoder).unwrap();
    let call:ToolCallArtifact=serde_json::from_slice(&InterfaceArtifactFile::new(input).unwrap().read_payload_bytes().unwrap()).unwrap();
    let result:ToolResultArtifact=serde_json::from_slice(&InterfaceArtifactFile::new(result).unwrap().read_payload_bytes().unwrap()).unwrap();
    assert!(result.ok);
    let writes=call.arguments["writes"].as_array().unwrap();
    let body=result.body.unwrap();
    let outcomes=body["writes"].as_array().unwrap();
    assert_eq!(writes.len(),outcomes.len());
    for (write,outcome) in writes.iter().zip(outcomes) {
        assert_eq!(write["path"],outcome["path"]);
        assert_eq!(write["expected_before_hash"],outcome["before_hash"]);
        assert_eq!(outcome["outcome"],"applied");
        assert!(outcome["error"].is_null());
        let bytes=std::fs::read(outcome["path"].as_str().unwrap()).unwrap();
        if let Some(content) = write.get("content") {
            assert!(write.get("patch").is_none());
            let expected = content.as_str().unwrap().as_bytes();
            assert_eq!(bytes, expected);
            assert_eq!(ContentHash::of_bytes(expected).to_hex(), outcome["after_hash"].as_str().unwrap());
            assert_eq!(expected.len() as u64, outcome["bytes"].as_u64().unwrap());
        } else {
            assert!(write["patch"].is_array());
            assert_eq!(write["expected_after_hash"].as_str().unwrap(), outcome["after_hash"].as_str().unwrap());
        }
        assert_eq!(ContentHash::of_bytes(&bytes).to_hex(),outcome["after_hash"].as_str().unwrap());
        assert_eq!(bytes.len() as u64,outcome["bytes"].as_u64().unwrap());
    }
    eprintln!("{}",serde_json::to_string_pretty(&prepared.evidence()).unwrap());
    if let Ok(requested) = std::env::var("OCTOPUS_REPLAY_IMPORT_SOURCE") {
        assert_eq!(requested, source.to_string(), "import requires the exact explicitly selected source");
        eprintln!("imported completion receipt: {}", prepared.import(&ctx).unwrap().display());
    }
}
