use std::path::Path;

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interface_transport::{InterfaceHostIoLimits, InterfaceLiftContext};
use octopus_interfaces::{GrantProjection, InvocationMetadata, RuntimeInvocationMetadata};
use octopus_ledger::{
    ArtifactRef, ContentHash, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId,
};
use octopus_ledger_index::AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET;
use octopus_world_pyecho_life_runtime::{LedgerReplayReader, LedgerTailReader};
use tokio_util::sync::CancellationToken;

fn limits() -> InterfaceHostIoLimits {
    InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576,
        max_stderr_bytes: 262_144,
        max_manifest_bytes: 1_048_576,
        max_payload_bytes: 268_435_456,
    }
}

fn reader_context(root: &Path, interface: &str) -> InterfaceLiftContext {
    let (arrivals, _receiver) = tokio::sync::mpsc::channel(4);
    InterfaceLiftContext::unprivileged(
        InterfaceId::new(interface),
        root.join("registry").join("interface").join(interface),
        None,
        root.join("home").join("interfaces").join(interface),
        root.join("durable").join("movements.jsonl"),
        root.join("durable").join("blobs"),
        root.join("durable").join("ledger-index.sqlite"),
        Default::default(),
        arrivals,
        CancellationToken::new(),
    )
    .with_host_io_limits(limits())
    .unwrap()
}

fn outbound_row(seq: u64, interface: &str, manifest_hash: ContentHash) -> String {
    outbound_row_from(
        seq,
        Endpoint::Interface(InterfaceId::new("system_context")),
        interface,
        manifest_hash,
    )
}

fn outbound_row_from(
    seq: u64,
    from: Endpoint,
    interface: &str,
    manifest_hash: ContentHash,
) -> String {
    let movement = Movement {
        seq: Seq(seq),
        lamport: Lamport(seq + 1),
        recorded_at_unix_ms: 1,
        trace: TraceId("trace".into()),
        kind: MovementKind::InterfaceOutbound,
        from,
        to: Endpoint::Interface(InterfaceId::new(interface)),
        artifact: Some(ArtifactRef {
            id: ArtifactId::new(format!("interface.output:{}", manifest_hash.to_hex())),
            content: manifest_hash,
        }),
        invocation_metadata: Some(
            InvocationMetadata::new(GrantProjection::default())
                .with_runtime(RuntimeInvocationMetadata {
                    run: RunId::new("run_r"),
                    ee: EeId::new("ee_root"),
                    worker: InterfaceId::new("system_worker_flash"),
                    checkpoint: CheckpointId::new("checkpoint"),
                    interface: InterfaceId::new(interface),
                    epoch: 0,
                    run_record_hash: None,
                })
                .into(),
        ),
    };
    serde_json::to_string(&movement).unwrap()
}

#[test]
fn legacy_run_to_interface_rows_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("durable/blobs")).unwrap();
    let hash = manifest(root, b"payload");
    let mut legacy: serde_json::Value = serde_json::from_str(&outbound_row(0, "world_pyecho", hash)).unwrap();
    legacy["from"] = serde_json::json!({"Run":"run_r"});
    let current = outbound_row(1, "world_pyecho", hash);
    std::fs::write(
        root.join("durable/movements.jsonl"),
        format!("{legacy}\n{current}\n"),
    )
    .unwrap();

    let context = reader_context(root, "world_pyecho");
    let mut reader = LedgerTailReader::open(&context).unwrap();
    assert!(reader.read_new().is_err());
}

fn manifest(root: &Path, payload: &[u8]) -> ContentHash {
    let payload_path = root.join("durable").join("payload.json");
    std::fs::write(&payload_path, payload).unwrap();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "label": "interface.output",
        "payload_path": payload_path,
    }))
    .unwrap();
    let hash = ContentHash::of_bytes(&bytes);
    std::fs::write(root.join("durable/blobs").join(hash.to_hex()), bytes).unwrap();
    hash
}

#[test]
fn cursor_filters_by_owner_and_resumes_after_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("durable/blobs")).unwrap();
    let hash = manifest(root, br#"{"text":"hello"}"#);
    let mine = outbound_row(0, "world_pyecho", hash);
    let foreign = outbound_row(1, "world_other", hash);
    let torn = outbound_row(2, "world_pyecho", hash);
    let ledger = root.join("durable/movements.jsonl");
    std::fs::write(&ledger, format!("{mine}\n{foreign}\n{}", &torn[..40])).unwrap();

    let context = reader_context(root, "world_pyecho");
    let mut reader = LedgerTailReader::open(&context).unwrap();
    let own = reader.read_new().unwrap();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].movement.seq.0, 0);
    assert_eq!(
        reader
            .envelope(&own[0])
            .unwrap()
            .read_artifact_payload()
            .unwrap(),
        br#"{"text":"hello"}"#
    );
    reader.commit().unwrap();

    std::fs::write(&ledger, format!("{mine}\n{foreign}\n{torn}\n")).unwrap();
    let mut resumed = LedgerTailReader::open(&context).unwrap();
    assert_eq!(resumed.next_seq(), 2);
    assert_eq!(resumed.read_new().unwrap()[0].movement.seq.0, 2);
}

#[test]
fn commit_through_preserves_the_uncommitted_tail_for_replay() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("durable/blobs")).unwrap();
    let hash = manifest(root, b"payload");
    let rows = (0..3)
        .map(|seq| outbound_row(seq, "world_pyecho", hash))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(root.join("durable/movements.jsonl"), format!("{rows}\n")).unwrap();
    let context = reader_context(root, "world_pyecho");

    {
        let mut reader = LedgerTailReader::open(&context).unwrap();
        let batch = reader.read_new().unwrap();
        reader.commit_through(&batch[0]).unwrap();
    }

    let mut resumed = LedgerTailReader::open(&context).unwrap();
    assert_eq!(
        resumed
            .read_new()
            .unwrap()
            .iter()
            .map(|movement| movement.movement.seq.0)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn unaddressed_scan_persists_only_after_bounded_catch_up() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("durable/blobs")).unwrap();
    let hash = manifest(root, b"payload");
    let budget = AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET as u64;
    let foreign = (0..budget + 1)
        .map(|seq| outbound_row(seq, "world_other", hash))
        .collect::<Vec<_>>()
        .join("\n");
    let ledger = root.join("durable/movements.jsonl");
    std::fs::write(&ledger, format!("{foreign}\n")).unwrap();
    let context = reader_context(root, "world_pyecho");
    let cursor = root
        .join("home/interfaces/world_pyecho")
        .join("ledger-cursor.json");

    let mut reader = LedgerTailReader::open(&context).unwrap();
    assert!(reader.read_new().unwrap().is_empty());
    assert_eq!(reader.next_seq(), budget);
    assert!(!cursor.exists());
    assert!(reader.read_new().unwrap().is_empty());
    assert_eq!(
        LedgerTailReader::open(&context).unwrap().next_seq(),
        budget + 1
    );
}

#[test]
fn history_replay_has_a_fixed_boundary_and_never_changes_delivery_cursors() {
    let dir = tempfile::tempdir().unwrap(); let root = dir.path();
    std::fs::create_dir_all(root.join("durable/blobs")).unwrap();
    let hash = manifest(root, b"payload"); let ctx = reader_context(root, "world_pyecho");
    let budget = AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET as u64;
    let mut rows = (0..budget).map(|seq| outbound_row(seq, "world_other", hash)).collect::<Vec<_>>();
    rows.push(outbound_row(budget, "world_pyecho", hash));
    let ledger = root.join("durable/movements.jsonl");
    std::fs::write(&ledger, format!("{}\n", rows.join("\n"))).unwrap();
    let mut delivery = LedgerTailReader::open(&ctx).unwrap();
    assert!(delivery.read_new().unwrap().is_empty()); delivery.read_new().unwrap(); delivery.commit().unwrap();
    let cursor = ctx.var_dir.join("ledger-cursor.json"); let before = std::fs::read(&cursor).unwrap();
    let mut replay = LedgerReplayReader::open(&ctx).unwrap();
    assert!(replay.read_batch(1).unwrap().unwrap().is_empty());
    rows.push(outbound_row(budget + 1, "world_pyecho", hash));
    std::fs::write(&ledger, format!("{}\n", rows.join("\n"))).unwrap();
    let batch = replay.read_batch(1).unwrap().unwrap(); assert_eq!(batch[0].movement.seq.0, budget);
    assert_eq!(replay.envelope(&batch[0]).unwrap().read_artifact_payload().unwrap(), b"payload");
    assert!(replay.read_batch(1).unwrap().is_none());
    let mut replay = LedgerReplayReader::open(&ctx).unwrap();
    std::fs::write(&ledger, b"").unwrap(); assert!(replay.read_batch(1).is_err());
    assert_eq!(std::fs::read(cursor).unwrap(), before);
}
