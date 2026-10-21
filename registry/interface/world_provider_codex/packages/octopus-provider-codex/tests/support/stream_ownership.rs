use super::*;

#[test]
fn terminal_failure_seals_every_stream_of_its_source_and_only_those() {
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
    let sealed_paths = |source| seal(source).unwrap().iter()
        .map(|stream| stream.manifest_path().to_path_buf()).collect::<BTreeSet<_>>();
    assert_eq!(sealed_paths(9), BTreeSet::from([next_manifest]));
    assert_eq!(std::fs::read(&old.body_path).unwrap(), old_bytes);
    assert!(seal(10).unwrap().is_empty());
    let successor = LiveTurnStream::open(temp.path(), &rules, LiveTurnIdentity {
        turn_id: "successor-turn".into(), ..identity()
    }, 7).unwrap();
    let successor_manifest = successor.manifest_path.clone();
    drop(successor);
    assert_eq!(sealed_paths(7), BTreeSet::from([old.manifest_path.clone(), successor_manifest.clone()]));
    for manifest in [&old.manifest_path, &successor_manifest] {
        let stream = InterfaceArtifactFile::new(manifest.clone()).unwrap();
        assert!(stream.read_manifest().unwrap().payload_content_hash.is_some());
        let body = String::from_utf8(stream.read_payload_bytes().unwrap()).unwrap();
        assert!(body.lines().last().unwrap().contains("\"terminal_label\":\"turn.error\""));
    }
}
