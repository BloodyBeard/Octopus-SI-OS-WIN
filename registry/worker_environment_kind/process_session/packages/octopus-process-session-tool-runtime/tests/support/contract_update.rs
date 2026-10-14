use crate::*;
use super::{store_recovery_artifact, write_recovery_ledger};
use octopus_descriptors::{CheckpointId, EeId, RunId};
use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};
use octopus_ledger::{Lamport, Seq, TraceId};

#[test]
fn contract_update_requires_exact_no_reinvoke_evidence_for_the_whole_grant() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let door = InterfaceId::new("system_contract_door");
    let private = root.join("interfaces");
    let var = private.join(door.as_str());
    std::fs::create_dir_all(&var).unwrap();
    let limits = InterfaceHostIoLimits { max_control_frame_bytes:65536, max_stderr_bytes:65536,
        max_manifest_bytes:65536, max_payload_bytes:65536 };
    let affected = BTreeMap::from([("test.updated".to_string(), door.clone())]);
    let call = ToolCallArtifact { tool:"test.sibling".into(), arguments:serde_json::json!({}),
        correlation_id:Some("once".into()), label:Some("tool.call".into()), source_interface_evidence:BTreeMap::new() };
    let metadata = InvocationMetadata::new(GrantProjection {
        tools:vec!["test.sibling".into(), "test.updated".into()], ..Default::default()
    }).with_runtime(RuntimeInvocationMetadata {
        run:RunId::new("run_contract"), ee:EeId::new("ee_contract"), worker:InterfaceId::new("system_worker_contract"),
        checkpoint:CheckpointId::new("source"), interface:door.clone(), epoch:0, run_record_hash:None,
    });
    let source = Movement { seq:Seq(0), lamport:Lamport(1), recorded_at_unix_ms:1, trace:TraceId("contract".into()),
        kind:MovementKind::InterfaceOutbound, from:Endpoint::Interface(InterfaceId::new("world_provider_contract")),
        to:Endpoint::Interface(door.clone()),
        artifact:Some(store_recovery_artifact(root, "tool.call", &serde_json::to_vec(&call).unwrap())),
        invocation_metadata:Some(metadata.clone().into()),
    };
    let result = Movement { seq:Seq(1), lamport:Lamport(2), recorded_at_unix_ms:2, trace:source.trace.clone(),
        kind:MovementKind::InterfaceArrival, from:Endpoint::Interface(door.clone()),
        to:Endpoint::Interface(metadata.runtime().unwrap().worker.clone()),
        artifact:Some(store_recovery_artifact(root, "test.sibling.result", &serde_json::to_vec(
            &ToolResultArtifact::success("test.sibling", Some("once".into()), serde_json::json!({"changed":true}))).unwrap())),
        invocation_metadata:Some(metadata.into()),
    };
    let movements = root.join("movements.jsonl");
    let index = root.join("ledger-index.sqlite");
    let blobs = root.join("blobs");
    let journal_path = var.join(TOOL_DOOR_EFFECT_JOURNAL_FILE);
    let cursor_path = var.join("ledger-cursor.tool-door.json");
    let inspect = || tool_contract_update_quiescent(ToolContractUpdateRequest {
        movements_path:&movements, ledger_index_path:&index, blobs_dir:&blobs,
        interface_private_state_root:&private, affected_tools:&affected, limits:&limits,
    });
    let retire = |run| tool_retirement_quiescent(ToolRetirementRequest {
        movements_path:&movements, ledger_index_path:&index, blobs_dir:&blobs,
        interface_private_state_root:&private, runs:&BTreeSet::from([RunId::new(run)]), limits:&limits,
        tool_doors: &BTreeSet::from([door.clone()]),
    });
    write_recovery_ledger(root, &[source.clone(), result.clone()]);
    assert!(!inspect().unwrap().quiescent);
    assert!(!journal_path.exists() && !cursor_path.exists() && !index.exists());
    let digest = tool_door_movement_digest(&source).unwrap();
    let mut journal = ToolDoorEffectJournal::open(journal_path.clone(), 0, 65536).unwrap();
    journal.prepare(0, &digest).unwrap();
    assert!(!inspect().unwrap().quiescent);
    journal.begin(0, &digest).unwrap();
    let before = std::fs::read(&journal_path).unwrap();
    let proof = inspect().unwrap();
    assert!(proof.quiescent);
    assert!(!retire("run_contract").unwrap().quiescent);
    assert!(retire("run_unrelated").unwrap().quiescent);
    for other_tools in [BTreeMap::new(), BTreeMap::from([("test.updated".to_string(), InterfaceId::new("other_door"))])] {
        assert!(tool_contract_update_quiescent(ToolContractUpdateRequest {
            movements_path:&movements, ledger_index_path:&index, blobs_dir:&blobs,
            interface_private_state_root:&private, affected_tools:&other_tools, limits:&limits,
        }).unwrap().quiescent);
        let proof = retire("run_contract").unwrap();
        assert!(!proof.quiescent);
        assert!(proof.preimages.iter().any(|(path, _)| path == &journal_path));
    }
    assert_eq!(std::fs::read(&journal_path).unwrap(), before);
    assert!(proof.preimages.contains(&(cursor_path.clone(), None)));
    use sha2::Digest;
    assert!(proof.preimages.contains(&(movements.clone(), Some(format!("{:x}", sha2::Sha256::digest(std::fs::read(&movements).unwrap()))))));
    let mut stopped = result.clone();
    stopped.to = Endpoint::Interface(InterfaceId::new("world_provider_contract"));
    stopped.from = Endpoint::Interface(InterfaceId::new("world_control_contract"));
    stopped.artifact = Some(store_recovery_artifact(root, "turn.stopped", b"{}"));
    write_recovery_ledger(root, &[source.clone(), stopped]);
    assert!(!inspect().unwrap().quiescent);
    journal.finish(0, &digest, ToolDoorJournalState::Completed).unwrap();
    let before = std::fs::read(&journal_path).unwrap();
    assert!(inspect().unwrap().quiescent);
    assert!(retire("run_contract").unwrap().quiescent);
    assert_eq!(std::fs::read(&journal_path).unwrap(), before);
    std::fs::remove_file(&journal_path).unwrap();
    let mut journal = ToolDoorEffectJournal::open(journal_path.clone(), 0, 65536).unwrap();
    journal.prepare(0, &digest).unwrap();
    journal.begin(0, &digest).unwrap();
    journal.finish(0, &digest, ToolDoorJournalState::Indeterminate).unwrap();
    assert!(inspect().unwrap().quiescent);
    assert!(!retire("run_contract").unwrap().quiescent);
    let retained_indeterminate = std::fs::read(&journal_path).unwrap();
    let mut custom = source.clone();
    custom.artifact = Some(store_recovery_artifact(root, "operator.approval.response", br#"{"resume":"exact accepted operation"}"#));
    let custom_digest = tool_door_movement_digest(&custom).unwrap();
    std::fs::remove_file(&journal_path).unwrap();
    let mut custom_journal = ToolDoorEffectJournal::open(journal_path.clone(), 0, 65536).unwrap();
    write_recovery_ledger(root, std::slice::from_ref(&custom));
    assert!(!retire("run_contract").unwrap().quiescent);
    custom_journal.prepare(0, &custom_digest).unwrap();
    assert!(!retire("run_contract").unwrap().quiescent);
    custom_journal.begin(0, &custom_digest).unwrap();
    assert!(!retire("run_contract").unwrap().quiescent);
    custom_journal.finish(0, &custom_digest, ToolDoorJournalState::Completed).unwrap();
    let before = std::fs::read(&journal_path).unwrap();
    custom_journal.prune_before(1).unwrap();
    let cold = ToolDoorEffectJournal::open(journal_path.clone(), 1, 65536).unwrap();
    assert_eq!(cold.state(0, &custom_digest).unwrap(), Some(ToolDoorJournalState::Completed));
    assert_eq!(std::fs::read(&journal_path).unwrap(), before);
    let mut projection = source.clone();
    projection.seq = Seq(1);
    projection.to = Endpoint::Interface(InterfaceId::new("client_projection"));
    let mut projection_metadata = projection.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    projection_metadata.runtime.as_mut().unwrap().interface = InterfaceId::new("client_projection");
    projection.invocation_metadata = Some(projection_metadata.into());
    write_recovery_ledger(root, &[custom, projection]);
    assert!(retire("run_contract").unwrap().quiescent);
    assert!(tool_retirement_quiescent(ToolRetirementRequest {
        movements_path:&movements, ledger_index_path:&index, blobs_dir:&blobs,
        interface_private_state_root:&private, runs:&BTreeSet::from([RunId::new("run_contract")]),
        tool_doors:&BTreeSet::new(), limits:&limits,
    }).unwrap().quiescent);
    std::fs::write(&journal_path, retained_indeterminate).unwrap();
    let mut changed = source.clone(); changed.recorded_at_unix_ms += 1;
    write_recovery_ledger(root, &[changed]);
    assert!(inspect().unwrap_err().contains("current movement"));
    std::fs::remove_file(&journal_path).unwrap();
    write_recovery_ledger(root, std::slice::from_ref(&source));
    let cursor = |seq| std::fs::write(&cursor_path, serde_json::to_vec(&serde_json::json!({
        "seq":seq,"offset":std::fs::metadata(&movements).unwrap().len()
    })).unwrap()).unwrap();
    cursor(1);
    assert!(!inspect().unwrap().quiescent);
    write_recovery_ledger(root, &[source.clone(), result.clone()]);
    cursor(2);
    assert!(inspect().unwrap().quiescent);
    assert!(!journal_path.exists());
    assert!(retire("run_contract").unwrap().quiescent);
    let mut uncertain = result.clone();
    uncertain.artifact = Some(store_recovery_artifact(root, "test.sibling.result", &serde_json::to_vec(
        &ToolResultArtifact::failure("test.sibling", Some("once".into()),
            bounded_tool_failure_error(0, TOOL_EFFECT_INDETERMINATE, &serde_json::json!({"effect_state":"in_flight_outcome_unknown"})).unwrap())).unwrap()));
    write_recovery_ledger(root, &[source.clone(), uncertain.clone()]);
    cursor(2);
    assert!(inspect().unwrap().quiescent);
    assert!(!retire("run_contract").unwrap().quiescent);
    for state in [ToolDoorJournalState::Completed, ToolDoorJournalState::Refused] {
        let mut journal = ToolDoorEffectJournal::open(journal_path.clone(), 0, 65536).unwrap();
        journal.prepare(0, &digest).unwrap();
        journal.begin(0, &digest).unwrap();
        journal.finish(0, &digest, state).unwrap();
        write_recovery_ledger(root, std::slice::from_ref(&source));
        cursor(1);
        assert!(retire("run_contract").unwrap().quiescent);
        let mut old = Vec::new();
        for row in std::fs::read_to_string(&journal_path).unwrap().lines() {
            let mut receipt: ToolDoorJournalReceipt = serde_json::from_str(row).unwrap();
            receipt.schema_version = 2;
            serde_json::to_writer(&mut old, &receipt).unwrap();
            old.push(b'\n');
        }
        std::fs::write(&journal_path, &old).unwrap();
        assert!(inspect().unwrap().quiescent);
        assert!(!retire("run_contract").unwrap().quiescent);
        write_recovery_ledger(root, &[source.clone(), uncertain.clone()]);
        cursor(2);
        assert!(!retire("run_contract").unwrap().quiescent);
        write_recovery_ledger(root, &[source.clone(), result.clone()]);
        cursor(2);
        assert!(retire("run_contract").unwrap().quiescent);
        assert_eq!(std::fs::read(&journal_path).unwrap(), old);
        std::fs::remove_file(&journal_path).unwrap();
    }
    std::fs::write(&cursor_path, b"{\"seq\":2,\"offset\":1}").unwrap();
    assert!(inspect().unwrap_err().contains("exact ledger boundary"));
    cursor(2);
    let mut malformed = source.clone(); malformed.invocation_metadata = None;
    write_recovery_ledger(root, &[malformed, result.clone()]);
    assert!(inspect().unwrap_err().contains("invocation metadata"));
    write_recovery_ledger(root, &[source.clone(), result.clone()]);
    cursor(2);
    let mut duplicate = result; duplicate.seq = Seq(2);
    write_recovery_ledger(root, &[source, duplicate.clone(), duplicate]);
    assert!(inspect().is_err());
}

#[tokio::test]
async fn failure_output_refusal_preserves_native_effect_uncertainty() {
    let temp = tempfile::tempdir().unwrap();
    let mut ctx = super::test_lift_context(temp.path(), InterfaceId::new("system_failure_door"));
    ctx.host_io_limits = Some(InterfaceHostIoLimits { max_control_frame_bytes:65536, max_stderr_bytes:65536,
        max_manifest_bytes:65536, max_payload_bytes:65536 });
    let (arrivals, mut receiver) = tokio::sync::mpsc::channel(1);
    ctx.arrivals = arrivals;
    for target in [None, Some(ToolResultTarget { tool:"test.effect".into(), correlation_id:Some("uncertain".into()) })] {
        let failure = hand_tool_effect_indeterminate(&ctx, 0, target.as_ref(), None, None);
        let refusal = async {
            let arrival = receiver.recv().await.unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&arrival.artifact.read_payload_bytes().unwrap()).unwrap();
            if target.is_some() { assert_eq!(payload["error"]["code"], TOOL_EFFECT_INDETERMINATE); }
            arrival.verdict_tx.unwrap().send(DeliveryVerdict::refused(RefusedReason::Policy, serde_json::json!({"test":"refused"}))).unwrap();
        };
        let (state, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(failure, refusal)
        }).await.unwrap();
        assert_eq!(state.unwrap(), ToolDoorJournalState::Indeterminate);
    }
}
