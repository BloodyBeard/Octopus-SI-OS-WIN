#[test]
fn native_input_correlation_conflicts_fail_before_append_and_after_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let input = |item_id| InputRecordedObservation {
        thread_id: "thread-1",
        turn_id: "turn-1",
        item_id,
        client_id: "client-steer-1",
        source_event: InputRecordedSource::ItemStarted,
        native_event_at_ms: Some(100),
    };
    let mut stream = LiveTurnStream::open(temp.path(), &rules(), identity(), 7).unwrap();
    assert!(stream.append_input_recorded(7, input("native-1")).unwrap());
    let body = std::fs::read(&stream.body_path).unwrap();
    assert!(stream.append_input_recorded(7, input("native-2")).is_err());
    assert_eq!(std::fs::read(&stream.body_path).unwrap(), body);
    drop(stream);
    let mut replay = LiveTurnStream::open(temp.path(), &rules(), identity(), 8).unwrap();
    assert!(replay.append_input_recorded(8, input("native-2")).is_err());
    assert!(
        !replay
            .append_input_recorded(
                8,
                InputRecordedObservation {
                    source_event: InputRecordedSource::ItemCompleted,
                    native_event_at_ms: Some(101),
                    ..input("native-1")
                }
            )
            .unwrap()
    );
    assert_eq!(std::fs::read(&replay.body_path).unwrap(), body);
}
