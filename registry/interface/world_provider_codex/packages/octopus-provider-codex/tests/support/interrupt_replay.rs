use super::*;

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
