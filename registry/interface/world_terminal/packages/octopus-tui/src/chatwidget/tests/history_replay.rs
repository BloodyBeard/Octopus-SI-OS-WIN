use super::*;
use octopus_world_terminal_client::TranscriptEntry;
use octopus_world_terminal_client::TranscriptSide;

fn inbound(text: &str) -> TranscriptEntry {
    TranscriptEntry {
        side: TranscriptSide::Inbound,
        label: "terminal.input".to_string(),
        text: text.to_string(),
        recorded_at_unix_ms: 0,
    }
}

fn outbound(label: &str, text: &str) -> TranscriptEntry {
    TranscriptEntry {
        side: TranscriptSide::Outbound,
        label: label.to_string(),
        text: text.to_string(),
        recorded_at_unix_ms: 0,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn replays_prompt_and_reply_from_ledger_entries() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.replay_transcript_entries(&[
        inbound("hello there"),
        outbound("interface.artifact.output", "hi — resumed reply"),
    ]);

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(
        combined.contains("hello there"),
        "user prompt replays: {combined}"
    );
    assert!(
        combined.contains("hi — resumed reply"),
        "terminal reply replays: {combined}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn streamed_deltas_flush_once_on_reply() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.replay_transcript_entries(&[
        outbound("agent.message.delta", "chunk one "),
        outbound("agent.message.delta", "chunk two"),
        outbound("interface.artifact.output", "chunk one chunk two"),
    ]);

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_eq!(
        combined.matches("chunk one").count(),
        1,
        "streamed reply flushes once: {combined}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn rich_labels_replay_as_honest_info_lines() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.replay_transcript_entries(&[outbound("goal.status", "objective: ship it (active)")]);

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(
        combined.contains("objective: ship it (active)"),
        "rich label text replays: {combined}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn blank_inbound_entries_are_skipped() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.replay_transcript_entries(&[inbound("   "), inbound("real prompt")]);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "only the real prompt paints");
}
