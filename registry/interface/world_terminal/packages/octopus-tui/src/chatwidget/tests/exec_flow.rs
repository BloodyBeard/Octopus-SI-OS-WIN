use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn exec_approval_emits_proposed_command_and_decision_history() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let ev = ExecApprovalRequestEvent {
        request_id: "call-short".into(),
        command: vec!["bash".into(), "-lc".into(), "echo hello world".into()],
        cwd: AbsolutePathBuf::current_dir()
            .expect("current dir")
            .as_path()
            .display()
            .to_string(),
        reason: Some(
            "this is a test reason such as one that would be produced by the model".into(),
        ),
        environment_id: None,
        ..Default::default()
    };
    handle_exec_approval_request(&mut chat, "sub-short", ev);

    let proposed_cells = drain_insert_history(&mut rx);
    assert!(
        proposed_cells.is_empty(),
        "expected approval request to render via modal without emitting history cells"
    );

    let area = Rect::new(0, 0, 80, chat.desired_height(80));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    chat.render(area, &mut buf);
    assert_chatwidget_snapshot!("exec_approval_modal_exec", format!("{buf:?}"));

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let decision = drain_insert_history(&mut rx)
        .pop()
        .expect("expected decision cell in history");
    assert_chatwidget_snapshot!(
        "exec_approval_history_decision_approved_short",
        lines_to_single_string(&decision)
    );
}

#[tokio::test]
async fn exec_approval_decision_truncates_multiline_and_long_commands() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let ev_multi = ExecApprovalRequestEvent {
        request_id: "call-multi".into(),
        command: vec!["bash".into(), "-lc".into(), "echo line1\necho line2".into()],
        cwd: AbsolutePathBuf::current_dir()
            .expect("current dir")
            .as_path()
            .display()
            .to_string(),
        environment_id: None,
        ..Default::default()
    };
    handle_exec_approval_request(&mut chat, "sub-multi", ev_multi);
    let proposed_multi = drain_insert_history(&mut rx);
    assert!(
        proposed_multi.is_empty(),
        "expected multiline approval request to render via modal without emitting history cells"
    );

    let area = Rect::new(0, 0, 80, chat.desired_height(80));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    chat.render(area, &mut buf);
    let mut saw_first_line = false;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        if row.contains("echo line1") {
            saw_first_line = true;
            break;
        }
    }
    assert!(
        saw_first_line,
        "expected modal to show first line of multiline snippet"
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    let aborted_multi = drain_insert_history(&mut rx)
        .pop()
        .expect("expected aborted decision cell (multiline)");
    assert_chatwidget_snapshot!(
        "exec_approval_history_decision_aborted_multiline",
        lines_to_single_string(&aborted_multi)
    );

    let long = format!("echo {}", "a".repeat(200));
    let ev_long = ExecApprovalRequestEvent {
        request_id: "call-long".into(),
        command: vec!["bash".into(), "-lc".into(), long],
        cwd: AbsolutePathBuf::current_dir()
            .expect("current dir")
            .as_path()
            .display()
            .to_string(),
        reason: None,
        environment_id: None,
        ..Default::default()
    };
    handle_exec_approval_request(&mut chat, "sub-long", ev_long);
    let proposed_long = drain_insert_history(&mut rx);
    assert!(
        proposed_long.is_empty(),
        "expected long approval request to avoid emitting history cells before decision"
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    let aborted_long = drain_insert_history(&mut rx)
        .pop()
        .expect("expected aborted decision cell (long)");
    assert_chatwidget_snapshot!(
        "exec_approval_history_decision_aborted_long",
        lines_to_single_string(&aborted_long)
    );
}

#[tokio::test]
async fn preamble_keeps_working_status_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());

    chat.on_task_started();
    chat.on_agent_message_delta("Preamble line\n".to_string());
    chat.on_commit_tick();
    drain_insert_history(&mut rx);
    complete_assistant_message(
        &mut chat,
        "msg-commentary-snapshot",
        "Preamble line\n",
        Some(MessagePhase::Commentary),
    );

    let height = chat.desired_height(80);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height))
        .expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw preamble + status widget");
    assert_chatwidget_snapshot!(
        "preamble_keeps_working_status",
        normalized_backend_snapshot(terminal.backend())
    );
}

#[tokio::test]
async fn unified_exec_begin_restores_status_indicator_after_preamble() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_task_started();
    assert_eq!(chat.bottom_pane.status_indicator_visible(), true);

    chat.bottom_pane.hide_status_indicator();
    assert_eq!(chat.bottom_pane.status_indicator_visible(), false);
    assert_eq!(chat.bottom_pane.is_task_running(), true);

    begin_unified_exec_startup(&mut chat, "call-1", "proc-1", "sleep 2");

    assert_eq!(chat.bottom_pane.status_indicator_visible(), true);
}

#[tokio::test]
async fn unified_exec_begin_restores_working_status_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_task_started();
    chat.on_agent_message_delta("Preamble line\n".to_string());
    chat.on_commit_tick();
    drain_insert_history(&mut rx);

    begin_unified_exec_startup(&mut chat, "call-1", "proc-1", "sleep 2");

    let width: u16 = 80;
    let height = chat.desired_height(width);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
        .expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw chatwidget");
    assert_chatwidget_snapshot!(
        "unified_exec_begin_restores_working_status",
        normalized_backend_snapshot(terminal.backend())
    );
}

#[tokio::test]
async fn exec_history_cell_shows_working_then_completed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let begin = begin_exec(&mut chat, "call-1", "echo done");

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 0, "no exec cell should have been flushed yet");

    end_exec(&mut chat, begin, "done", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected finalized exec cell to flush");
    let lines = &cells[0];
    let blob = lines_to_single_string(lines);
    assert!(
        blob.contains("• Ran"),
        "expected summary header present: {blob:?}"
    );
    assert!(
        blob.contains("echo done"),
        "expected command text to be present: {blob:?}"
    );
}

#[tokio::test]
async fn exec_history_cell_shows_working_then_failed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let begin = begin_exec(&mut chat, "call-2", "false");
    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 0, "no exec cell should have been flushed yet");

    end_exec(&mut chat, begin, "", "Bloop", 2);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected finalized exec cell to flush");
    let lines = &cells[0];
    let blob = lines_to_single_string(lines);
    assert!(
        blob.contains("• Ran false"),
        "expected command and header text present: {blob:?}"
    );
    assert!(blob.to_lowercase().contains("bloop"), "expected error text");
}

#[tokio::test]
async fn exec_end_without_begin_does_not_flush_unrelated_running_exploring_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    begin_exec(&mut chat, "call-exploring", "cat /dev/null");
    assert!(drain_insert_history(&mut rx).is_empty());
    assert!(active_blob(&chat).contains("Read null"));

    let orphan =
        begin_unified_exec_startup(&mut chat, "call-orphan", "proc-1", "echo repro-marker");
    assert!(drain_insert_history(&mut rx).is_empty());

    end_exec(&mut chat, orphan, "repro-marker\n", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "only the orphan end should be inserted");
    let orphan_blob = lines_to_single_string(&cells[0]);
    assert!(
        orphan_blob.contains("• Ran echo repro-marker"),
        "expected orphan end to render a standalone entry: {orphan_blob:?}"
    );
    let active = active_blob(&chat);
    assert!(
        active.contains("• Exploring"),
        "expected unrelated exploring call to remain active: {active:?}"
    );
    assert!(
        active.contains("Read null"),
        "expected active exploring command to remain visible: {active:?}"
    );
    assert!(
        !active.contains("echo repro-marker"),
        "orphaned end should not replace the active exploring cell: {active:?}"
    );
}

#[tokio::test]
async fn exec_end_without_begin_flushes_completed_unrelated_exploring_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    let begin_ls = begin_exec(&mut chat, "call-ls", "ls -la");
    end_exec(&mut chat, begin_ls, "", "", 0);
    assert!(drain_insert_history(&mut rx).is_empty());
    assert!(active_blob(&chat).contains("ls -la"));

    let orphan = begin_unified_exec_startup(&mut chat, "call-after", "proc-1", "echo after");
    end_exec(&mut chat, orphan, "after\n", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(
        cells.len(),
        2,
        "completed exploring cell should flush before the orphan entry"
    );
    let first = lines_to_single_string(&cells[0]);
    let second = lines_to_single_string(&cells[1]);
    assert!(
        first.contains("• Explored"),
        "expected flushed exploring cell: {first:?}"
    );
    assert!(
        first.contains("List ls -la"),
        "expected flushed exploring cell: {first:?}"
    );
    assert!(
        second.contains("• Ran echo after"),
        "expected orphan end entry after flush: {second:?}"
    );
    assert!(
        chat.transcript.active_cell.is_none(),
        "both entries should be finalized"
    );
}

#[tokio::test]
async fn overlapping_exploring_exec_end_is_not_misclassified_as_orphan() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let begin_ls = begin_exec(&mut chat, "call-ls", "ls -la");
    let begin_cat = begin_exec(&mut chat, "call-cat", "cat foo.txt");
    assert!(drain_insert_history(&mut rx).is_empty());

    end_exec(&mut chat, begin_ls, "foo.txt\n", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "tracked end inside an exploring cell should not render as an orphan"
    );
    let active = active_blob(&chat);
    assert!(
        active.contains("List ls -la"),
        "expected first command still grouped: {active:?}"
    );
    assert!(
        active.contains("Read foo.txt"),
        "expected second running command to stay in the same active cell: {active:?}"
    );
    assert!(
        active.contains("• Exploring"),
        "expected grouped exploring header to remain active: {active:?}"
    );

    end_exec(&mut chat, begin_cat, "hello\n", "", 0);
}

#[tokio::test]
async fn exec_history_shows_unified_exec_startup_commands() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    let begin = begin_exec_with_source(
        &mut chat,
        "call-startup",
        "echo unified exec startup",
        ExecCommandSource::UnifiedExecStartup,
    );
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "exec begin should not flush until completion"
    );

    end_exec(&mut chat, begin, "echo unified exec startup\n", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected finalized exec cell to flush");
    let blob = lines_to_single_string(&cells[0]);
    assert!(
        blob.contains("• Ran echo unified exec startup"),
        "expected startup command to render: {blob:?}"
    );
}

#[tokio::test]
async fn exec_history_shows_unified_exec_tool_calls() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    let begin = begin_exec_with_source(
        &mut chat,
        "call-startup",
        "ls",
        ExecCommandSource::UnifiedExecStartup,
    );
    end_exec(&mut chat, begin, "", "", 0);

    let blob = active_blob(&chat);
    assert_eq!(blob, "• Explored\n  └ List ls\n");
}

#[tokio::test]
async fn unified_exec_unknown_end_with_active_exploring_cell_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    begin_exec(&mut chat, "call-exploring", "cat /dev/null");
    let orphan =
        begin_unified_exec_startup(&mut chat, "call-orphan", "proc-1", "echo repro-marker");
    end_exec(&mut chat, orphan, "repro-marker\n", "", 0);

    let cells = drain_insert_history(&mut rx);
    let history = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    let active = active_blob(&chat);
    let snapshot = format!("History:\n{history}\nActive:\n{active}");
    assert_chatwidget_snapshot!(
        "unified_exec_unknown_end_with_active_exploring_cell",
        snapshot
    );
}

#[tokio::test]
async fn unified_exec_end_after_task_complete_is_suppressed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    let begin = begin_exec_with_source(
        &mut chat,
        "call-startup",
        "echo unified exec startup",
        ExecCommandSource::UnifiedExecStartup,
    );
    drain_insert_history(&mut rx);

    chat.on_task_complete(None, None, false);
    end_exec(&mut chat, begin, "", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected unified exec end after task complete to be suppressed"
    );
}

#[tokio::test]
async fn unified_exec_interaction_after_task_complete_is_suppressed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    chat.on_task_complete(None, None, false);

    terminal_interaction(&mut chat, "call-1", "proc-1", "ls\n");

    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected unified exec interaction after task complete to be suppressed"
    );
}

#[tokio::test]
async fn unified_exec_wait_after_final_agent_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    handle_turn_started(&mut chat, "turn-1");

    begin_unified_exec_startup(&mut chat, "call-wait", "proc-1", "cargo test -p codex-core");
    terminal_interaction(&mut chat, "call-wait-stdin", "proc-1", "");

    complete_assistant_message(&mut chat, "msg-1", "Final response.", None);
    handle_turn_completed(&mut chat, "turn-1", None);

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!("unified_exec_wait_after_final_agent_message", combined);
}

#[tokio::test]
async fn unified_exec_wait_before_streamed_agent_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    handle_turn_started(&mut chat, "turn-1");

    begin_unified_exec_startup(
        &mut chat,
        "call-wait-stream",
        "proc-1",
        "cargo test -p codex-core",
    );
    terminal_interaction(&mut chat, "call-wait-stream-stdin", "proc-1", "");

    handle_agent_message_delta(&mut chat, "Streaming response.");
    handle_turn_completed(&mut chat, "turn-wait-1", None);

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!("unified_exec_wait_before_streamed_agent_message", combined);
}

#[tokio::test]
async fn final_worked_for_uses_cumulative_turn_duration_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    handle_turn_started(&mut chat, "turn-1");

    let exec = begin_exec_with_source(
        &mut chat,
        "call-1",
        "echo preparing",
        ExecCommandSource::Agent,
    );
    end_exec(&mut chat, exec, "preparing\n", "", 0);

    complete_assistant_message(
        &mut chat,
        "msg-final",
        "Final response.",
        Some(MessagePhase::FinalAnswer),
    );
    handle_turn_completed(&mut chat, "turn-1", Some(125_000));

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(
        combined.contains("Worked for 2m 05s"),
        "expected final separator to use cumulative turn duration, got:\n{combined}"
    );
    assert_chatwidget_snapshot!("final_worked_for_uses_cumulative_turn_duration", combined);
}

#[tokio::test]
async fn unified_exec_wait_status_header_updates_on_late_command_display() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    chat.unified_exec_processes.push(UnifiedExecProcessSummary {
        key: "proc-1".to_string(),
        call_id: "call-1".to_string(),
        command_display: "sleep 5".to_string(),
        recent_chunks: Vec::new(),
    });

    terminal_interaction(&mut chat, "call-1", "proc-1", "");

    assert!(chat.transcript.active_cell.is_none());
    assert_eq!(
        chat.status_state.current_status.header,
        "Waiting for background terminal"
    );
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), "Waiting for background terminal");
    assert_eq!(status.details(), Some("sleep 5"));
}

#[tokio::test]
async fn unified_exec_empty_poll_for_finished_process_does_not_show_waiting_status() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    terminal_interaction(&mut chat, "call-finished", "proc-finished", "");

    assert_eq!(chat.status_state.current_status.header, "Working");
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("task status indicator should remain visible");
    assert_eq!(status.header(), "Working");
    assert!(chat.unified_exec_wait_streak.is_none());
}

#[tokio::test]
async fn unified_exec_waiting_multiple_empty_snapshots() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(&mut chat, "call-wait-1", "proc-1", "just fix");

    terminal_interaction(&mut chat, "call-wait-1a", "proc-1", "");
    terminal_interaction(&mut chat, "call-wait-1b", "proc-1", "");
    assert_eq!(
        chat.status_state.current_status.header,
        "Waiting for background terminal"
    );
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), "Waiting for background terminal");
    assert_eq!(status.details(), Some("just fix"));

    handle_turn_completed(&mut chat, "turn-wait-3", None);

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!("unified_exec_waiting_multiple_empty_after", combined);
}

#[tokio::test]
async fn unified_exec_wait_status_renders_command_in_single_details_row_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(
        &mut chat,
        "call-wait-ui",
        "proc-ui",
        "cargo test -p codex-core -- --exact some::very::long::test::name",
    );

    terminal_interaction(&mut chat, "call-wait-ui-stdin", "proc-ui", "");

    let rendered = render_bottom_popup(&chat, 48);
    assert_chatwidget_snapshot!(
        "unified_exec_wait_status_renders_command_in_single_details_row",
        normalize_snapshot_paths(rendered)
    );
}

#[tokio::test]
async fn unified_exec_empty_then_non_empty_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(&mut chat, "call-wait-2", "proc-2", "just fix");

    terminal_interaction(&mut chat, "call-wait-2a", "proc-2", "");
    terminal_interaction(&mut chat, "call-wait-2b", "proc-2", "ls\n");

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!("unified_exec_empty_then_non_empty_after", combined);
}

#[tokio::test]
async fn unified_exec_non_empty_then_empty_snapshots() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(&mut chat, "call-wait-3", "proc-3", "just fix");

    terminal_interaction(&mut chat, "call-wait-3a", "proc-3", "pwd\n");
    terminal_interaction(&mut chat, "call-wait-3b", "proc-3", "");
    assert_eq!(
        chat.status_state.current_status.header,
        "Waiting for background terminal"
    );
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), "Waiting for background terminal");
    assert_eq!(status.details(), Some("just fix"));
    let pre_cells = drain_insert_history(&mut rx);
    let active_combined = pre_cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!("unified_exec_non_empty_then_empty_active", active_combined);

    handle_turn_completed(&mut chat, "turn-1", None);

    let post_cells = drain_insert_history(&mut rx);
    let mut combined = pre_cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    let post = post_cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    if !combined.is_empty() && !post.is_empty() {
        combined.push('\n');
    }
    combined.push_str(&post);
    assert_chatwidget_snapshot!("unified_exec_non_empty_then_empty_after", combined);
}

#[tokio::test]
async fn view_image_tool_call_adds_history_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let image_path = chat.config.cwd.join("example.png");

    handle_view_image_tool_call(&mut chat, "call-image", image_path);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected a single history cell");
    let combined = lines_to_single_string(&cells[0]);
    assert_chatwidget_snapshot!("local_image_attachment_history_snapshot", combined);
}

#[tokio::test]
async fn image_generation_call_adds_history_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    handle_image_generation_end(
        &mut chat,
        "call-image-generation",
        "completed",
        Some("A tiny blue square".into()),
        Some(test_path_buf("/tmp/ig-1.png").abs()),
    );

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected a single history cell");
    let platform_file_url = url::Url::from_file_path(test_path_buf("/tmp/ig-1.png"))
        .expect("test path should convert to file URL")
        .to_string();
    let combined =
        lines_to_single_string(&cells[0]).replace(&platform_file_url, "file:///tmp/ig-1.png");
    assert_chatwidget_snapshot!("image_generation_call_history_snapshot", combined);

    handle_image_generation_end(
        &mut chat,
        "call-image-generation-failed",
        "failed",
        Some("A tiny blue square".into()),
        None,
    );

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected a single failure history cell");
    assert_chatwidget_snapshot!(
        "failed_image_generation_call_history_snapshot",
        lines_to_single_string(&cells[0])
    );
}

#[tokio::test]
async fn exec_history_extends_previous_when_consecutive() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    let begin_ls = begin_exec(&mut chat, "call-ls", "ls -la");
    assert_chatwidget_snapshot!("exploring_step1_start_ls", active_blob(&chat));

    end_exec(&mut chat, begin_ls, "", "", 0);
    assert_chatwidget_snapshot!("exploring_step2_finish_ls", active_blob(&chat));

    let begin_cat_foo = begin_exec(&mut chat, "call-cat-foo", "cat foo.txt");
    assert_chatwidget_snapshot!("exploring_step3_start_cat_foo", active_blob(&chat));

    end_exec(&mut chat, begin_cat_foo, "hello from foo", "", 0);
    assert_chatwidget_snapshot!("exploring_step4_finish_cat_foo", active_blob(&chat));

    let begin_sed_range = begin_exec(&mut chat, "call-sed-range", "sed -n 100,200p foo.txt");
    end_exec(&mut chat, begin_sed_range, "chunk", "", 0);
    assert_chatwidget_snapshot!("exploring_step5_finish_sed_range", active_blob(&chat));

    let begin_cat_bar = begin_exec(&mut chat, "call-cat-bar", "cat bar.txt");
    end_exec(&mut chat, begin_cat_bar, "hello from bar", "", 0);
    assert_chatwidget_snapshot!("exploring_step6_finish_cat_bar", active_blob(&chat));
}

#[tokio::test]
async fn user_shell_command_renders_output_not_exploring() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let begin_ls = begin_exec_with_source(
        &mut chat,
        "user-shell-ls",
        "ls",
        ExecCommandSource::UserShell,
    );
    end_exec(&mut chat, begin_ls, "file1\nfile2\n", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(
        cells.len(),
        1,
        "expected a single history cell for the user command"
    );
    let blob = lines_to_single_string(cells.first().unwrap());
    assert_chatwidget_snapshot!("user_shell_ls_output", blob);
}

#[tokio::test]
async fn bang_shell_enter_while_task_running_submits_run_user_shell_command() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();
    let rollout_file = NamedTempFile::new().unwrap();
    let configured = crate::session_state::ThreadSessionState {
        thread_id,
        forked_from_id: None,
        fork_parent_title: None,
        thread_name: None,
        model: "test-model".to_string(),
        model_provider_id: "test-provider".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::Never,
        approvals_reviewer: ApprovalsReviewer::User,
        permission_profile: PermissionProfile::read_only(),
        active_permission_profile: None,
        cwd: test_path_buf("/home/user/project").abs(),
        runtime_workspace_roots: Vec::new(),
        instruction_source_paths: Vec::new(),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        collaboration_mode: None,
        personality: None,
        network_proxy: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };
    chat.handle_thread_session(configured);
    drain_insert_history(&mut rx);
    while op_rx.try_recv().is_ok() {}
    handle_turn_started(&mut chat, "turn-1");

    chat.bottom_pane
        .set_composer_text("!echo hi".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match op_rx.try_recv() {
        Ok(Op::RunUserShellCommand { command }) => assert_eq!(command, "echo hi"),
        other => panic!("expected RunUserShellCommand op, got {other:?}"),
    }
    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn user_message_during_user_shell_command_is_queued_not_steered() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());
    handle_turn_started(&mut chat, "turn-1");
    let begin = begin_exec_with_source(
        &mut chat,
        "user-shell-sleep",
        "sleep 10",
        ExecCommandSource::UserShell,
    );

    assert!(chat.only_user_shell_commands_running());
    chat.bottom_pane
        .set_composer_text("hi".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(chat.queued_user_message_texts(), vec!["hi".to_string()]);

    end_exec(&mut chat, begin, "", "", 0);
    complete_assistant_message(
        &mut chat,
        "msg-done",
        "done",
        Some(MessagePhase::FinalAnswer),
    );
    handle_turn_completed(&mut chat, "turn-1", None);

    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "hi".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected queued user message after shell completion, got {other:?}"),
    }
    assert!(chat.input_queue.queued_user_messages.is_empty());
}

#[tokio::test]
async fn disabled_slash_command_while_task_running_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    handle_turn_started(&mut chat, "turn-1");

    chat.dispatch_command(SlashCommand::Resume);

    let cells = drain_insert_history(&mut rx);
    assert!(
        !cells.is_empty(),
        "expected an error message history cell to be emitted",
    );
    let blob = lines_to_single_string(cells.last().unwrap());
    assert_chatwidget_snapshot!("disabled_slash_command_while_task_running_snapshot", blob);
}

#[tokio::test]
async fn approval_modal_patch_snapshot() -> anyhow::Result<()> {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)?;

    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("README.md"),
        FileChange::Add {
            content: "hello\nworld\n".into(),
        },
    );
    let ev = ApplyPatchApprovalRequestEvent {
        request_id: "call-approve-patch".into(),
        changes,
        reason: Some("The model wants to apply changes".into()),
        grant_root: Some(PathBuf::from("/tmp")),
    };
    handle_apply_patch_approval_request(&mut chat, "sub-approve-patch", ev);

    let height = chat.desired_height(80);
    let mut terminal =
        crate::custom_terminal::Terminal::with_options(VT100Backend::new(80, height))
            .expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, 80, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw patch approval modal");
    let contents = terminal.backend().vt100().screen().contents();
    assert!(!contents.contains("$ apply_patch"));
    assert_chatwidget_snapshot!("approval_modal_patch", contents);

    Ok(())
}

#[tokio::test]
async fn interrupt_preserves_unified_exec_processes() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    begin_unified_exec_startup(&mut chat, "call-1", "process-1", "sleep 5");
    begin_unified_exec_startup(&mut chat, "call-2", "process-2", "sleep 6");
    assert_eq!(chat.unified_exec_processes.len(), 2);

    handle_turn_interrupted(&mut chat, "turn-1");

    assert_eq!(chat.unified_exec_processes.len(), 2);

    chat.add_ps_output();
    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        combined.contains("Background terminals"),
        "expected /ps to remain available after interrupt; got {combined:?}"
    );
    assert!(
        combined.contains("sleep 5") && combined.contains("sleep 6"),
        "expected /ps to list running unified exec processes; got {combined:?}"
    );

    let _ = drain_insert_history(&mut rx);
}

#[tokio::test]
async fn interrupt_preserves_unified_exec_wait_streak_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    handle_turn_started(&mut chat, "turn-1");

    let begin = begin_unified_exec_startup(&mut chat, "call-1", "process-1", "just fix");
    terminal_interaction(&mut chat, "call-1a", "process-1", "");

    handle_turn_interrupted(&mut chat, "turn-1");

    end_exec(&mut chat, begin, "", "", 0);
    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    let snapshot = format!("cells={}\n{combined}", cells.len());
    assert_chatwidget_snapshot!("interrupt_preserves_unified_exec_wait_streak", snapshot);
}

#[tokio::test]
async fn turn_complete_keeps_unified_exec_processes() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    begin_unified_exec_startup(&mut chat, "call-1", "process-1", "sleep 5");
    begin_unified_exec_startup(&mut chat, "call-2", "process-2", "sleep 6");
    assert_eq!(chat.unified_exec_processes.len(), 2);

    handle_turn_completed(&mut chat, "turn-1", None);

    assert_eq!(chat.unified_exec_processes.len(), 2);

    chat.add_ps_output();
    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        combined.contains("Background terminals"),
        "expected /ps to remain available after turn complete; got {combined:?}"
    );
    assert!(
        combined.contains("sleep 5") && combined.contains("sleep 6"),
        "expected /ps to list running unified exec processes; got {combined:?}"
    );

    let _ = drain_insert_history(&mut rx);
}

#[tokio::test]
async fn apply_patch_events_emit_history_cells() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    let ev = ApplyPatchApprovalRequestEvent {
        request_id: "c1".into(),
        changes,
        reason: None,
        grant_root: None,
    };
    handle_apply_patch_approval_request(&mut chat, "s1", ev);
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "expected approval request to surface via modal without emitting history cells"
    );

    let mut changes2 = HashMap::new();
    changes2.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    handle_patch_apply_begin(&mut chat, "c1", "turn-c1", changes2);
    let cells = drain_insert_history(&mut rx);
    assert!(!cells.is_empty(), "expected apply block cell to be sent");
    let blob = lines_to_single_string(cells.last().unwrap());
    assert!(
        blob.contains("Added foo.txt") || blob.contains("Edited foo.txt"),
        "expected single-file header with filename (Added/Edited): {blob:?}"
    );

    let mut end_changes = HashMap::new();
    end_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    handle_patch_apply_end(&mut chat, "c1", "turn-c1", end_changes, true);
    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "no success cell should be emitted anymore"
    );
}

#[tokio::test]
async fn apply_patch_manual_approval_adjusts_header() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let mut proposed_changes = HashMap::new();
    proposed_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    handle_apply_patch_approval_request(
        &mut chat,
        "s1",
        ApplyPatchApprovalRequestEvent {
            request_id: "c1".into(),
            changes: proposed_changes,
            reason: None,
            grant_root: None,
        },
    );
    drain_insert_history(&mut rx);

    let mut apply_changes = HashMap::new();
    apply_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    handle_patch_apply_begin(&mut chat, "c1", "turn-c1", apply_changes);

    let cells = drain_insert_history(&mut rx);
    assert!(!cells.is_empty(), "expected apply block cell to be sent");
    let blob = lines_to_single_string(cells.last().unwrap());
    assert!(
        blob.contains("Added foo.txt") || blob.contains("Edited foo.txt"),
        "expected apply summary header for foo.txt: {blob:?}"
    );
}

#[tokio::test]
async fn apply_patch_manual_flow_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let mut proposed_changes = HashMap::new();
    proposed_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    handle_apply_patch_approval_request(
        &mut chat,
        "s1",
        ApplyPatchApprovalRequestEvent {
            request_id: "c1".into(),
            changes: proposed_changes,
            reason: Some("Manual review required".into()),
            grant_root: None,
        },
    );
    let history_before_apply = drain_insert_history(&mut rx);
    assert!(
        history_before_apply.is_empty(),
        "expected approval modal to defer history emission"
    );

    let mut apply_changes = HashMap::new();
    apply_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    handle_patch_apply_begin(&mut chat, "c1", "turn-c1", apply_changes);
    let approved_lines = drain_insert_history(&mut rx)
        .pop()
        .expect("approved patch cell");

    assert_chatwidget_snapshot!(
        "apply_patch_manual_flow_history_approved",
        lines_to_single_string(&approved_lines)
    );
}

#[tokio::test]
async fn apply_patch_untrusted_shows_approval_modal() -> anyhow::Result<()> {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)?;

    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("a.rs"),
        FileChange::Add { content: "".into() },
    );
    handle_apply_patch_approval_request(
        &mut chat,
        "sub-1",
        ApplyPatchApprovalRequestEvent {
            request_id: "call-1".into(),
            changes,
            reason: None,
            grant_root: None,
        },
    );

    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    let mut contains_title = false;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        if row.contains("Would you like to make the following edits?") {
            contains_title = true;
            break;
        }
    }
    assert!(
        contains_title,
        "expected approval modal to be visible with title 'Would you like to make the following edits?'"
    );

    Ok(())
}

#[tokio::test]
async fn apply_patch_request_omits_diff_summary_from_modal() -> anyhow::Result<()> {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)?;

    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("README.md"),
        FileChange::Add {
            content: "line one\nline two\n".into(),
        },
    );
    handle_apply_patch_approval_request(
        &mut chat,
        "sub-apply",
        ApplyPatchApprovalRequestEvent {
            request_id: "call-apply".into(),
            changes,
            reason: None,
            grant_root: None,
        },
    );

    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "expected approval request to render via modal instead of history"
    );

    let area = Rect::new(0, 0, 80, chat.desired_height(80));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    chat.render(area, &mut buf);
    let mut contents = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            contents.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        contents.push('\n');
    }
    assert!(!contents.contains("README.md (+2 -0)"));
    assert!(!contents.contains("+line one"));
    assert!(!contents.contains("+line two"));

    Ok(())
}
