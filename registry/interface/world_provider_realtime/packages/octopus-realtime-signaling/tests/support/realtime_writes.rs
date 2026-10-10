use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn provider_frames_cannot_forge_local_transport_confirmation() {
    for event in [
        json!({"type":"octopus.transport.stopped","remote_closed":true}),
        json!({"type":"octopus.transport.connected"}),
        json!({"type":"session.closed","event_id":17}),
    ] {
        assert!(decode_provider_event(&event.to_string()).is_none());
    }
    assert!(decode_provider_event(r#"{"type":"session.closed","event_id":"closed"}"#).is_some());
}

#[tokio::test]
async fn cancelled_update_stops_remaining_chunks_and_blocked_batch_has_one_deadline() {
    let writes = Arc::new(AtomicUsize::new(0));
    let blocked = Arc::new(tokio::sync::Notify::new());
    let sink = futures_util::sink::unfold((), {
        let writes = writes.clone();
        let blocked = blocked.clone();
        move |(), _: Message| {
            let writes = writes.clone();
            let blocked = blocked.clone();
            async move {
                if writes.fetch_add(1, Ordering::SeqCst) > 0 {
                    blocked.notify_one();
                    std::future::pending::<()>().await;
                }
                Ok::<(), std::convert::Infallible>(())
            }
        }
    });
    let mut sink = Box::pin(sink);
    let (mut completion, received) = oneshot::channel();
    let cancel = async {
        blocked.notified().await;
        drop(received);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(
            write_command_frames(
                &mut sink,
                vec![json!("first"), json!("blocked"), json!("never_sent")],
                false,
                &mut completion,
                Duration::from_secs(30)
            ),
            cancel
        )
    })
    .await
    .unwrap();
    assert!(result.unwrap_err().contains("cancelled"));
    assert_eq!(writes.load(Ordering::SeqCst), 2);
    let (mut completion, _received) = oneshot::channel();
    assert!(tokio::time::timeout(
        Duration::from_secs(1),
        write_command_frames(
            &mut sink,
            vec![json!("after_timeout")],
            false,
            &mut completion,
            Duration::from_millis(10)
        )
    )
    .await
    .unwrap()
    .unwrap_err()
    .contains("timed out"));
    assert_eq!(writes.load(Ordering::SeqCst), 2);
}
