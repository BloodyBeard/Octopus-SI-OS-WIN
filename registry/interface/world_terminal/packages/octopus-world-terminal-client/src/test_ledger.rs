#![cfg(test)]

use std::path::Path;

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{InvocationMetadata, RuntimeInvocationMetadata};
use octopus_ledger::{
    ArtifactRef, ContentHash, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId,
};

pub(crate) fn write_run_record(runs_dir: &Path, run: &str, ee: &str, worker: &str) {
    std::fs::create_dir_all(runs_dir).unwrap();
    let record = serde_json::json!({
        "id": run,
        "ee": ee,
        "worker": worker,
        "trace": serde_json::Value::Null,
        "status": "running",
        "interface_state": {},
        "checkpoints": [],
        "active_checkpoints": [],
    });
    std::fs::write(
        runs_dir.join(format!("{run}.json")),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
}

pub(crate) fn seed_movements(durable_root: &Path, movements: &[(&str, MovementKind, u64, &str)]) {
    let labeled: Vec<(&str, MovementKind, u64, &str, &str)> = movements
        .iter()
        .map(|(run, kind, ts, text)| (*run, kind.clone(), *ts, "interface.artifact.output", *text))
        .collect();
    seed_labeled_movements(durable_root, &labeled);
}

pub(crate) fn seed_labeled_movements(
    durable_root: &Path,
    movements: &[(&str, MovementKind, u64, &str, &str)],
) {
    let blobs = durable_root.join("blobs");
    std::fs::create_dir_all(&blobs).unwrap();
    let payloads = durable_root.join("payloads");
    std::fs::create_dir_all(&payloads).unwrap();

    let mut lines = String::new();
    for (seq, (run, kind, recorded_at_unix_ms, label, text)) in movements.iter().enumerate() {
        let payload_bytes = serde_json::to_vec(&serde_json::json!({ "text": text })).unwrap();
        let payload_path = payloads.join(format!("{run}-{seq}.payload"));
        std::fs::write(&payload_path, &payload_bytes).unwrap();
        let manifest = serde_json::json!({
            "label": label,
            "payload_path": payload_path,
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let hash = ContentHash::of_bytes(&manifest_bytes);
        std::fs::write(blobs.join(hash.to_hex()), &manifest_bytes).unwrap();

        let terminal_endpoint = Endpoint::Interface(InterfaceId::new("world_terminal"));
        let worker_endpoint = Endpoint::Interface(InterfaceId::new("system_worker_flash"));
        let (from, to) = match kind {
            MovementKind::InterfaceArrival => (terminal_endpoint, worker_endpoint),
            MovementKind::InterfaceOutbound => (
                Endpoint::Interface(InterfaceId::new("world_provider_test")),
                terminal_endpoint,
            ),
            _ => (terminal_endpoint, worker_endpoint),
        };
        let movement = Movement {
            seq: Seq(seq as u64),
            lamport: Lamport(seq as u64 + 1),
            recorded_at_unix_ms: *recorded_at_unix_ms,
            trace: TraceId("t".into()),
            kind: kind.clone(),
            from,
            to,
            artifact: Some(ArtifactRef {
                id: ArtifactId::new(format!("{label}:{}", hash.to_hex())),
                content: hash,
            }),
            invocation_metadata: Some(
                InvocationMetadata::new(octopus_interfaces::GrantProjection::default())
                    .with_runtime(RuntimeInvocationMetadata {
                        run: RunId::new(*run),
                        ee: EeId::new("ee_operator"),
                        worker: InterfaceId::new("system_worker_flash"),
                        checkpoint: CheckpointId::new("c1"),
                        interface: InterfaceId::new("world_terminal"),
                        epoch: 0,
                        run_record_hash: None,
                    })
                    .into(),
            ),
        };
        lines.push_str(&serde_json::to_string(&movement).unwrap());
        lines.push('\n');
    }
    std::fs::write(durable_root.join("movements.jsonl"), lines).unwrap();
}

pub(crate) fn rewrite_movements(durable_root: &Path, mut rewrite: impl FnMut(&mut Movement)) {
    let path = durable_root.join("movements.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut rewritten = String::new();
    for line in text.lines() {
        let mut movement: Movement = serde_json::from_str(line).unwrap();
        rewrite(&mut movement);
        rewritten.push_str(&serde_json::to_string(&movement).unwrap());
        rewritten.push('\n');
    }
    std::fs::write(path, rewritten).unwrap();
}
