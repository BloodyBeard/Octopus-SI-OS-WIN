use super::*;

#[test]
fn terminal_failure_seals_only_its_source_and_refuses_ambiguous_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let rules = LiveStreamRules { enabled: true, artifact_label: "turn.stream".into(), always_stream_labels: BTreeSet::new(), active_turn_stream_labels: BTreeSet::new(), phase_rules: BTreeMap::new() };
    let identity = || LiveTurnIdentity { provider_interface: "world_provider_codex_task".into(), run: "run-task".into(), thread_id: "thread-1".into(), turn_id: "turn-1".into() };
    let old = LiveTurnStream::open(temp.path(), &rules, identity(), 7).unwrap();
    let old_bytes = std::fs::read(&old.body_path).unwrap();
    let next = LiveTurnStream::open(temp.path(), &rules, LiveTurnIdentity {
        turn_id: "turn-next".into(), ..identity()
    }, 9).unwrap();
    let next_manifest = next.manifest_path.clone();
    drop(next);
    let seal = |source| seal_source_turn_stream(temp.path(), &rules,
        "world_provider_codex_task", "run-task", source, "turn.error", &"a".repeat(64));
    assert_eq!(seal(9).unwrap().unwrap().manifest_path(), next_manifest);
    assert_eq!(std::fs::read(&old.body_path).unwrap(), old_bytes);
    assert!(seal(10).unwrap().is_none());
    let duplicate = LiveTurnStream::open(temp.path(), &rules, LiveTurnIdentity {
        turn_id: "ambiguous-turn".into(), ..identity()
    }, 7).unwrap();
    assert!(seal(7).unwrap_err().contains("source movement 7 owns more than one"));
    assert_eq!(std::fs::read(&old.body_path).unwrap(), old_bytes);
    assert!(duplicate.sealed_manifest_hash.is_none());
}
