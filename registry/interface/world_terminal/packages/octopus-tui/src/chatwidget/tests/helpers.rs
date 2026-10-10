use super::*;
use pretty_assertions::assert_eq;

pub(super) async fn test_config() -> TuiConfig {
    let codex_home = tempfile::Builder::new()
        .prefix("chatwidget-tests-")
        .tempdir()
        .expect("tempdir")
        .keep();
    write_test_model_provider_config(&codex_home);
    let mut snapshot = crate::test_support::test_session_snapshot(
        codex_home.clone().abs(),
        PathBuf::from(test_path_display("/tmp/project")).abs(),
    );
    snapshot.effective_config = toml::Value::Table(toml::map::Map::new());
    TuiConfig::with_default_terminal(snapshot)
}

pub(super) fn test_project_path() -> PathBuf {
    PathBuf::from(test_path_display("/tmp/project"))
}

pub(super) fn effective_config_from_kv(
    kv: Vec<(String, toml::Value)>,
    base: toml::Value,
) -> toml::Value {
    let mut root = match base {
        toml::Value::Table(table) => table,
        _ => toml::map::Map::new(),
    };
    for (path, value) in kv {
        let segments: Vec<&str> = path.split('.').collect();
        insert_toml_path(&mut root, &segments, value);
    }
    toml::Value::Table(root)
}

fn insert_toml_path(
    table: &mut toml::map::Map<String, toml::Value>,
    segments: &[&str],
    value: toml::Value,
) {
    match segments {
        [] => {}
        [leaf] => {
            table.insert((*leaf).to_string(), value);
        }
        [head, rest @ ..] => {
            let entry = table
                .entry((*head).to_string())
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
            if let toml::Value::Table(child) = entry {
                insert_toml_path(child, rest, value);
            }
        }
    }
}

pub(super) fn apply_feature_kv_to_snapshot(
    snapshot: &mut crate::provider_config_runtime::ProviderSessionSnapshot,
) {
    let Some(features) = snapshot
        .effective_config
        .get("features")
        .and_then(toml::Value::as_table)
        .cloned()
    else {
        return;
    };
    for (key, value) in features {
        if let Some(enabled) = value.as_bool() {
            let _ = snapshot.features.set_enabled_by_key(&key, enabled);
        }
    }
}

pub(super) fn terminal_text_element_value(element: TextElement) -> serde_json::Value {
    serde_json::to_value(element).expect("text element should serialize to opaque JSON")
}

pub(super) fn truncated_path_variants(path: &str) -> Vec<String> {
    let chars: Vec<char> = path.chars().collect();
    (1..chars.len())
        .map(|len| chars[..len].iter().collect::<String>())
        .collect()
}

pub(super) fn normalize_snapshot_paths(text: impl Into<String>) -> String {
    let mut text = text.into();

    for unix_path in ["/tmp/project", "/tmp/hooks.json"] {
        let platform_path = test_path_display(unix_path);
        if platform_path != unix_path {
            text = text.replace(&platform_path, unix_path);
        }
    }

    let platform_test_cwd = test_path_display("/tmp/project");
    if platform_test_cwd == "/tmp/project" {
        text
    } else {
        for platform_prefix in truncated_path_variants(&platform_test_cwd)
            .into_iter()
            .rev()
        {
            let unix_prefix: String = "/tmp/project"
                .chars()
                .take(platform_prefix.chars().count())
                .collect();
            text = text.replace(&format!("{platform_prefix}…"), &format!("{unix_prefix}…"));
        }

        text
    }
}

pub(super) fn normalized_backend_snapshot<T: std::fmt::Display>(value: &T) -> String {
    let platform_test_cwd = test_path_display("/tmp/project");
    let rendered = format!("{value}");

    if platform_test_cwd == "/tmp/project" {
        return rendered;
    }

    rendered
        .lines()
        .map(|line| {
            if let Some(content) = line
                .strip_prefix('"')
                .and_then(|line| line.strip_suffix('"'))
            {
                let width = content.chars().count();
                let normalized = normalize_snapshot_paths(content);
                format!("\"{normalized:width$}\"")
            } else {
                normalize_snapshot_paths(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn snapshot(percent: f64) -> RateLimitSnapshotItem {
    RateLimitSnapshotItem {
        primary: Some(octopus_world_terminal_client::RateLimitWindowItem {
            used_percent: percent,
            window_minutes: Some(60),
            resets_at: None,
        }),
        ..Default::default()
    }
}

pub(super) fn test_session_telemetry(_config: &TuiConfig, _model: &str) -> SessionTelemetry {
    SessionTelemetry::disabled()
}

pub(super) fn test_model_catalog(_config: &TuiConfig) -> Arc<ModelCatalog> {
    Arc::new(ModelCatalog::new(
        crate::test_support::TEST_MODEL_PRESETS.clone(),
        crate::test_support::test_collaboration_modes(),
    ))
}

pub(super) async fn make_chatwidget_manual(
    model_override: Option<&str>,
) -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    make_chatwidget_manual_with_auth(model_override, false, false).await
}

pub(super) async fn make_chatwidget_manual_with_auth(
    model_override: Option<&str>,
    has_chatgpt_account: bool,
    has_codex_backend_auth: bool,
) -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (tx_raw, rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(tx_raw);
    let (op_tx, op_rx) = unbounded_channel::<Op>();
    let mut cfg = test_config().await;
    let resolved_model = model_override
        .map(str::to_owned)
        .unwrap_or_else(|| get_model_offline_for_tests(cfg.model.as_deref()));
    if let Some(model) = model_override {
        cfg.model = Some(model.to_string());
    }
    let session_telemetry = test_session_telemetry(&cfg, resolved_model.as_str());
    let model_catalog = test_model_catalog(&cfg);
    let common = ChatWidgetInit {
        config: cfg,
        config_runtime: crate::test_support::TestProviderConfigRuntime::new(),
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx,
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: false,
        has_chatgpt_account,
        has_codex_backend_auth,
        model_catalog,
        is_first_run: true,
        status_account_display: None,
        runtime_model_provider_base_url: None,
        initial_plan_type: None,
        model: Some(resolved_model.clone()),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        session_telemetry,
    };
    let mut widget = ChatWidget::new_with_op_target(common, super::CodexOpTarget::Direct(op_tx));
    widget.transcript.active_cell = None;
    widget.transcript.active_cell_revision = 0;
    widget.normal_placeholder_text = "Ask Octopus to do anything".to_string();
    widget.side_placeholder_text =
        "Check recently modified functions for compatibility".to_string();
    widget
        .bottom_pane
        .set_placeholder_text(widget.normal_placeholder_text.clone());
    widget.set_model(&resolved_model);
    (widget, rx, op_rx)
}

pub(super) fn next_submit_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Op {
    loop {
        match op_rx.try_recv() {
            Ok(op @ Op::UserTurn { .. }) => return op,
            Ok(_) => continue,
            Err(TryRecvError::Empty) => panic!("expected a submit op but queue was empty"),
            Err(TryRecvError::Disconnected) => panic!("expected submit op but channel closed"),
        }
    }
}

pub(super) fn next_interrupt_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) {
    loop {
        match op_rx.try_recv() {
            Ok(Op::Interrupt { .. }) => return,
            Ok(_) => continue,
            Err(TryRecvError::Empty) => panic!("expected interrupt op but queue was empty"),
            Err(TryRecvError::Disconnected) => panic!("expected interrupt op but channel closed"),
        }
    }
}

pub(super) fn assert_no_submit_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) {
    while let Ok(op) = op_rx.try_recv() {
        assert!(
            !matches!(op, Op::UserTurn { .. }),
            "unexpected submit op: {op:?}"
        );
    }
}

pub(crate) fn set_chatgpt_auth(chat: &mut ChatWidget) {
    chat.has_chatgpt_account = true;
    chat.has_codex_backend_auth = true;
    chat.config.model_provider.requires_openai_auth = true;
    chat.model_catalog = test_model_catalog(&chat.config);
}

fn test_model_info(slug: &str, priority: i32, supports_fast_mode: bool) -> ModelInfo {
    let mut service_tiers = Vec::new();
    if supports_fast_mode {
        service_tiers.push(json!({
            "id": ServiceTier::Fast.request_value(),
            "name": "fast",
            "description": "Fastest inference with increased plan usage"
        }));
    }
    serde_json::from_value(json!({
        "slug": slug,
        "display_name": slug,
        "description": format!("{slug} description"),
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [{"effort": "medium", "description": "medium"}],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": priority,
        "additional_speed_tiers": [],
        "service_tiers": service_tiers,
        "default_service_tier": null,
        "availability_nux": null,
        "upgrade": null,
        "base_instructions": "base instructions",
        "supports_reasoning_summaries": false,
        "default_reasoning_summary": "none",
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10_000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 272_000,
        "experimental_supported_tools": [],
    }))
    .expect("valid model info")
}

pub(crate) fn set_fast_mode_test_catalog(chat: &mut ChatWidget) {
    let models: Vec<ModelPreset> = ModelsResponse {
        models: vec![
            test_model_info("gpt-5.4", 0, true),
            test_model_info("gpt-5.3-codex", 1, false),
        ],
    }
    .models
    .into_iter()
    .map(Into::into)
    .collect();

    chat.model_catalog = Arc::new(ModelCatalog::new(
        models,
        chat.model_catalog.collaboration_modes().to_vec(),
    ));
}

pub(crate) async fn make_chatwidget_manual_with_sender() -> (
    ChatWidget,
    AppEventSender,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (widget, rx, op_rx) = make_chatwidget_manual(None).await;
    let app_event_tx = widget.app_event_tx.clone();
    (widget, app_event_tx, rx, op_rx)
}

pub(super) fn drain_insert_history(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> Vec<Vec<ratatui::text::Line<'static>>> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = ev {
            let mut lines = cell.display_lines(80);
            if !cell.is_stream_continuation() && !out.is_empty() && !lines.is_empty() {
                lines.insert(0, "".into());
            }
            out.push(lines)
        }
    }
    out
}

pub(super) fn lines_to_single_string(lines: &[ratatui::text::Line<'static>]) -> String {
    let mut s = String::new();
    for line in lines {
        for span in &line.spans {
            s.push_str(&span.content);
        }
        s.push('\n');
    }
    s
}

pub(super) fn status_line_text(chat: &ChatWidget) -> Option<String> {
    chat.status_line_text()
}

pub(super) fn make_token_info(total_tokens: i64, context_window: i64) -> TokenUsageInfo {
    fn usage(total_tokens: i64) -> TokenUsage {
        TokenUsage {
            total_tokens,
            ..TokenUsage::default()
        }
    }

    TokenUsageInfo {
        total_token_usage: usage(total_tokens),
        last_token_usage: usage(total_tokens),
        model_context_window: Some(context_window),
    }
}

pub(super) fn handle_token_count(chat: &mut ChatWidget, info: Option<TokenUsageInfo>) {
    chat.set_token_info(info);
}

pub(super) fn handle_error(chat: &mut ChatWidget, message: impl Into<String>) {
    chat.on_error(message.into());
}

pub(super) fn handle_stream_error(
    chat: &mut ChatWidget,
    message: impl Into<String>,
    additional_details: Option<String>,
) {
    chat.on_stream_error(message.into(), additional_details);
}

pub(super) fn handle_warning(chat: &mut ChatWidget, message: impl Into<String>) {
    chat.on_warning(message);
}

pub(super) fn handle_agent_message_delta(chat: &mut ChatWidget, delta: impl Into<String>) {
    chat.on_agent_message_delta(delta.into());
}

pub(super) fn handle_agent_reasoning_delta(chat: &mut ChatWidget, delta: impl Into<String>) {
    chat.on_agent_reasoning_delta(delta.into());
}

pub(super) fn handle_agent_reasoning_final(chat: &mut ChatWidget) {
    chat.on_agent_reasoning_final();
}

pub(super) fn handle_entered_review_mode(chat: &mut ChatWidget, review: impl Into<String>) {
    chat.enter_review_mode_with_hint(review.into(), false);
}

pub(super) fn replay_entered_review_mode(chat: &mut ChatWidget, review: impl Into<String>) {
    chat.enter_review_mode_with_hint(review.into(), true);
}

pub(super) fn handle_exited_review_mode(chat: &mut ChatWidget) {
    chat.exit_review_mode_after_item();
}

pub(super) fn handle_exec_approval_request(
    chat: &mut ChatWidget,
    id: impl Into<String>,
    event: ExecApprovalRequestEvent,
) {
    chat.on_exec_approval_request(id.into(), event);
}

pub(super) fn handle_apply_patch_approval_request(
    chat: &mut ChatWidget,
    id: impl Into<String>,
    event: ApplyPatchApprovalRequestEvent,
) {
    chat.on_apply_patch_approval_request(id.into(), event);
}

pub(super) fn handle_patch_apply_begin(
    chat: &mut ChatWidget,
    _call_id: impl Into<String>,
    _turn_id: impl Into<String>,
    changes: HashMap<PathBuf, FileChange>,
) {
    chat.on_patch_apply_begin(changes);
}

pub(super) fn handle_patch_apply_end(
    chat: &mut ChatWidget,
    _call_id: impl Into<String>,
    _turn_id: impl Into<String>,
    _changes: HashMap<PathBuf, FileChange>,
    success: bool,
) {
    chat.on_patch_apply_completed(success);
}

pub(super) fn handle_view_image_tool_call(
    chat: &mut ChatWidget,
    _call_id: impl Into<String>,
    path: AbsolutePathBuf,
) {
    chat.on_view_image_tool_call(path);
}

pub(super) fn handle_image_generation_end(
    chat: &mut ChatWidget,
    call_id: impl Into<String>,
    status: impl Into<String>,
    revised_prompt: Option<String>,
    saved_path: Option<AbsolutePathBuf>,
) {
    chat.on_image_generation_end(call_id.into(), status.into(), revised_prompt, saved_path);
}

pub(super) fn replay_user_message_text(
    chat: &mut ChatWidget,
    _item_id: &str,
    text: impl Into<String>,
    _replay_kind: ReplayKind,
) {
    chat.replay_transcript_entries(&[octopus_world_terminal_client::TranscriptEntry {
        side: octopus_world_terminal_client::TranscriptSide::Inbound,
        label: "terminal.input".to_string(),
        text: text.into(),
        recorded_at_unix_ms: 0,
    }]);
}

pub(super) fn replay_agent_message(
    chat: &mut ChatWidget,
    _item_id: &str,
    text: impl Into<String>,
    _replay_kind: ReplayKind,
) {
    chat.replay_transcript_entries(&[octopus_world_terminal_client::TranscriptEntry {
        side: octopus_world_terminal_client::TranscriptSide::Outbound,
        label: octopus_app_server_interface::ACT_OBSERVATION_REPLY_LABEL.to_string(),
        text: text.into(),
        recorded_at_unix_ms: 0,
    }]);
}

fn command_actions_from_parsed(
    parsed: Vec<ParsedCommand>,
) -> Vec<octopus_world_terminal_client::CommandActionItem> {
    use ParsedCommand as Core;
    use octopus_world_terminal_client::CommandActionItem as Item;
    parsed
        .into_iter()
        .map(|action| match action {
            Core::Read { cmd, name, path } => Item::Read {
                cmd,
                name,
                path: path.display().to_string(),
            },
            Core::ListFiles { cmd, path } => Item::ListFiles { cmd, path },
            Core::Search { cmd, query, path } => Item::Search { cmd, query, path },
            Core::Unknown { cmd } => Item::Unknown { cmd },
        })
        .collect()
}

pub(super) fn begin_exec_with_source(
    chat: &mut ChatWidget,
    call_id: &str,
    raw_cmd: &str,
    source: ExecCommandSource,
) -> ExecCommandItem {
    let command = vec!["bash".to_string(), "-lc".to_string(), raw_cmd.to_string()];
    let command_actions = command_actions_from_parsed(
        octopus_shell_command::parse_command::parse_command(&command),
    );
    let item = ExecCommandItem {
        item_id: call_id.to_string(),
        command: octopus_shell_command::parse_command::shlex_join(&command),
        cwd: chat.config.cwd.to_string_lossy().to_string(),
        process_id: None,
        command_actions,
        source,
        status: octopus_world_terminal_client::ExecItemStatus::InProgress,
        exit_code: None,
        duration_ms: None,
        aggregated_output: None,
    };
    handle_exec_begin(chat, item.clone());
    item
}

pub(super) fn begin_unified_exec_startup(
    chat: &mut ChatWidget,
    call_id: &str,
    process_id: &str,
    raw_cmd: &str,
) -> ExecCommandItem {
    let command = vec!["bash".to_string(), "-lc".to_string(), raw_cmd.to_string()];
    let item = ExecCommandItem {
        item_id: call_id.to_string(),
        command: octopus_shell_command::parse_command::shlex_join(&command),
        cwd: chat.config.cwd.to_string_lossy().to_string(),
        process_id: Some(process_id.to_string()),
        command_actions: Vec::new(),
        source: ExecCommandSource::UnifiedExecStartup,
        status: octopus_world_terminal_client::ExecItemStatus::InProgress,
        exit_code: None,
        duration_ms: None,
        aggregated_output: None,
    };
    handle_exec_begin(chat, item.clone());
    item
}

pub(super) fn handle_exec_begin(chat: &mut ChatWidget, item: ExecCommandItem) {
    chat.on_command_execution_started(item);
}

pub(super) fn handle_exec_end(chat: &mut ChatWidget, item: ExecCommandItem) {
    chat.on_command_execution_completed(item);
}

pub(super) fn terminal_interaction(
    chat: &mut ChatWidget,
    _call_id: &str,
    process_id: &str,
    stdin: &str,
) {
    chat.on_terminal_interaction(process_id.to_string(), stdin.to_string());
}

pub(super) fn complete_assistant_message(
    chat: &mut ChatWidget,
    item_id: &str,
    text: &str,
    phase: Option<MessagePhase>,
) {
    chat.on_agent_message_item_completed(
        AgentMessageItem {
            id: item_id.to_string(),
            content: vec![AgentMessageContent::Text {
                text: text.to_string(),
            }],
            phase,
        },
        false,
    );
}

pub(super) fn pending_steer(text: &str) -> PendingSteer {
    PendingSteer {
        user_message: UserMessage::from(text),
        history_record: UserMessageHistoryRecord::UserMessageText,
        compare_key: PendingSteerCompareKey {
            message: text.to_string(),
            image_count: 0,
        },
    }
}

pub(super) fn complete_user_message(chat: &mut ChatWidget, item_id: &str, text: &str) {
    complete_user_message_for_inputs(
        chat,
        item_id,
        vec![TerminalUserInput::text(text.to_string())],
    );
}

pub(super) fn complete_user_message_for_inputs(
    chat: &mut ChatWidget,
    _item_id: &str,
    content: Vec<TerminalUserInput>,
) {
    chat.on_committed_user_message(&content, false);
}

pub(super) fn handle_turn_started(chat: &mut ChatWidget, turn_id: &str) {
    chat.turn_lifecycle.last_turn_id = Some(turn_id.to_string());
    chat.on_task_started();
}

pub(super) fn handle_turn_completed(
    chat: &mut ChatWidget,
    _turn_id: &str,
    duration_ms: Option<i64>,
) {
    chat.on_task_complete(None, duration_ms, false);
}

pub(super) fn handle_turn_interrupted(chat: &mut ChatWidget, turn_id: &str) {
    let reason = if chat.turn_lifecycle.take_budget_limited(turn_id) {
        TurnAbortReason::BudgetLimited
    } else {
        TurnAbortReason::Interrupted
    };
    chat.on_interrupted_turn(reason);
}

pub(super) fn handle_budget_limited_turn(chat: &mut ChatWidget, turn_id: &str) {
    chat.turn_lifecycle.mark_budget_limited(turn_id.to_string());
    handle_turn_interrupted(chat, turn_id);
}

pub(super) fn begin_exec(chat: &mut ChatWidget, call_id: &str, raw_cmd: &str) -> ExecCommandItem {
    begin_exec_with_source(chat, call_id, raw_cmd, ExecCommandSource::Agent)
}

pub(super) fn end_exec(
    chat: &mut ChatWidget,
    begin_item: ExecCommandItem,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) {
    let aggregated = if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{stdout}{stderr}")
    };
    let status = if exit_code == 0 {
        octopus_world_terminal_client::ExecItemStatus::Completed
    } else {
        octopus_world_terminal_client::ExecItemStatus::Failed
    };
    handle_exec_end(
        chat,
        ExecCommandItem {
            status,
            exit_code: Some(i64::from(exit_code)),
            duration_ms: Some(5),
            aggregated_output: (!aggregated.is_empty()).then_some(aggregated),
            ..begin_item
        },
    );
}

pub(super) fn active_blob(chat: &ChatWidget) -> String {
    let lines = chat
        .transcript
        .active_cell
        .as_ref()
        .expect("active cell present")
        .display_lines(80);
    lines_to_single_string(&lines)
}

pub(super) fn active_hook_blob(chat: &ChatWidget) -> String {
    let Some(cell) = chat.active_hook_cell.as_ref() else {
        return "<empty>\n".to_string();
    };
    let lines = cell.display_lines(80);
    lines_to_single_string(&lines)
}

pub(super) fn expire_quiet_hook_linger(chat: &mut ChatWidget) {
    if let Some(cell) = chat.active_hook_cell.as_mut() {
        cell.expire_quiet_runs_now_for_test();
    }
    chat.pre_draw_tick();
}

pub(super) fn reveal_running_hooks(chat: &mut ChatWidget) {
    if let Some(cell) = chat.active_hook_cell.as_mut() {
        cell.reveal_running_runs_now_for_test();
    }
    chat.pre_draw_tick();
}

pub(super) fn reveal_running_hooks_after_delayed_redraw(chat: &mut ChatWidget) {
    if let Some(cell) = chat.active_hook_cell.as_mut() {
        cell.reveal_running_runs_after_delayed_redraw_for_test();
    }
    chat.pre_draw_tick();
}

pub(super) fn get_available_model(chat: &ChatWidget, model: &str) -> ModelPreset {
    let models = chat
        .model_catalog
        .try_list_models()
        .expect("models lock available");
    models
        .iter()
        .find(|&preset| preset.model == model)
        .cloned()
        .unwrap_or_else(|| panic!("{model} preset not found"))
}

pub(super) async fn assert_shift_left_edits_most_recent_queued_message_for_terminal(
    terminal_info: TerminalInfo,
) {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.queued_message_edit_hint_binding =
        Some(queued_message_edit_binding_for_terminal(terminal_info));
    chat.bottom_pane
        .set_queued_message_edit_binding(chat.queued_message_edit_hint_binding);

    chat.bottom_pane.set_task_running(true);

    chat.input_queue
        .queued_user_messages
        .push_back(UserMessage::from("first queued".to_string()).into());
    chat.input_queue
        .queued_user_messages
        .push_back(UserMessage::from("second queued".to_string()).into());
    chat.refresh_pending_input_preview();

    chat.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));

    assert_eq!(
        chat.bottom_pane.composer_text(),
        "second queued".to_string()
    );
    assert_eq!(chat.input_queue.queued_user_messages.len(), 1);
    assert_eq!(
        chat.input_queue.queued_user_messages.front().unwrap().text,
        "first queued"
    );
}

pub(super) fn render_bottom_first_row(chat: &ChatWidget, width: u16) -> String {
    let height = chat.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            let s = buf[(x, y)].symbol();
            if s.is_empty() {
                row.push(' ');
            } else {
                row.push_str(s);
            }
        }
        if !row.trim().is_empty() {
            return row;
        }
    }
    String::new()
}

pub(crate) fn render_bottom_popup(chat: &ChatWidget, width: u16) -> String {
    let height = chat.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    let mut lines: Vec<String> = (0..area.height)
        .map(|row| {
            let mut line = String::new();
            for col in 0..area.width {
                let symbol = buf[(area.x + col, area.y + row)].symbol();
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(symbol);
                }
            }
            line.trim_end().to_string()
        })
        .collect();

    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

pub(super) fn strip_osc8_for_snapshot(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut stripped = String::with_capacity(text.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i..].starts_with(b"\x1B]8;;") {
            i += 5;
            while i < bytes.len() {
                if bytes[i] == b'\x07' {
                    i += 1;
                    break;
                }
                if i + 1 < bytes.len() && bytes[i] == b'\x1B' && bytes[i + 1] == b'\\' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }

        let ch = text[i..]
            .chars()
            .next()
            .expect("slice should always contain a char");
        stripped.push(ch);
        i += ch.len_utf8();
    }

    stripped
}

pub(super) fn handle_hook_started(
    chat: &mut ChatWidget,
    run: octopus_world_terminal_client::HookRunItem,
) {
    chat.on_hook_started(run);
}

pub(super) fn handle_hook_completed(
    chat: &mut ChatWidget,
    run: octopus_world_terminal_client::HookRunItem,
) {
    chat.on_hook_completed(run);
}

/// Build one `hook.result` artifact item (the terminal-owned hook model).
pub(super) fn hook_run(
    run_id: &str,
    event: &str,
    status: octopus_world_terminal_client::HookItemStatus,
    status_message: &str,
    outputs: Vec<octopus_world_terminal_client::HookOutputItem>,
) -> octopus_world_terminal_client::HookRunItem {
    octopus_world_terminal_client::HookRunItem {
        id: run_id.to_string(),
        event: event.to_string(),
        status,
        status_message: Some(status_message.to_string()),
        outputs,
    }
}

pub(super) async fn assert_hook_events_snapshot(
    event: &str,
    run_id: &str,
    status_message: &str,
    snapshot_name: &str,
) {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    handle_hook_started(
        &mut chat,
        hook_run(
            run_id,
            event,
            octopus_world_terminal_client::HookItemStatus::Running,
            status_message,
            Vec::new(),
        ),
    );
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "hook start should update the live hook cell instead of writing history"
    );
    reveal_running_hooks(&mut chat);
    assert!(
        active_hook_blob(&chat).contains(&format!("Running {event} hook: {status_message}")),
        "hook start should render in the live hook cell"
    );

    handle_hook_completed(
        &mut chat,
        hook_run(
            run_id,
            event,
            octopus_world_terminal_client::HookItemStatus::Completed,
            status_message,
            vec![
                octopus_world_terminal_client::HookOutputItem {
                    kind: octopus_world_terminal_client::HookOutputKind::Warning,
                    text: "Heads up from the hook".to_string(),
                },
                octopus_world_terminal_client::HookOutputItem {
                    kind: octopus_world_terminal_client::HookOutputKind::Context,
                    text: "Remember the startup checklist.".to_string(),
                },
            ],
        ),
    );

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!(snapshot_name, combined);
}
