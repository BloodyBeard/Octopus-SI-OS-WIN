use super::*;

#[test]
fn media_connectivity_requires_connected_peer_state() {
    for state in [
        PeerConnectionState::New,
        PeerConnectionState::Connecting,
        PeerConnectionState::Disconnected,
        PeerConnectionState::Closed,
    ] {
        assert_eq!(connection_event(state), None);
    }
    assert_eq!(
        connection_event(PeerConnectionState::Connected),
        Some(RealtimeWebrtcEvent::Connected)
    );
    assert!(matches!(
        connection_event(PeerConnectionState::Failed),
        Some(RealtimeWebrtcEvent::Failed(_))
    ));
}

#[tokio::test]
async fn local_close_interrupts_pending_native_operation_and_drop_closes_worker() {
    let (command_tx, command_rx) = mpsc::channel();
    let close = Arc::new(tokio::sync::Notify::new());
    let handle = SessionHandle {
        command_tx,
        close: close.clone(),
    };
    handle.close();
    let cancelled = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = close.notified() => true,
            _ = std::future::pending::<()>() => false,
        }
    })
    .await
    .unwrap();
    assert!(cancelled);
    assert!(matches!(command_rx.try_recv(), Ok(Command::Close)));
    drop(handle);
    assert!(matches!(command_rx.try_recv(), Ok(Command::Close)));
    assert!(
        matches!(response_error(mpsc::RecvTimeoutError::Timeout), RealtimeWebrtcError::Message(message) if message.contains("timed out"))
    );
}
