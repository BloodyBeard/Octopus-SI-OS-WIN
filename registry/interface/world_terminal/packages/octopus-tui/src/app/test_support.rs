use super::*;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;

pub(super) async fn make_test_app() -> App {
    let (app, _runtime) = make_test_app_with_runtime().await;
    app
}

pub(super) async fn make_test_app_with_runtime() -> (
    App,
    std::sync::Arc<crate::test_support::TestProviderConfigRuntime>,
) {
    let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    let config = chat_widget.config_ref().clone();
    let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
    let session_telemetry = SessionTelemetry::disabled();
    let runtime = crate::test_support::TestProviderConfigRuntime::new();

    let app = App {
        model_catalog: chat_widget.model_catalog(),
        session_telemetry,
        app_event_tx,
        chat_widget,
        workspace_command_runner: None,
        config,
        config_runtime: runtime.clone(),
        load_request: crate::provider_config_runtime::SessionLoadRequest::default(),
        runtime_approval_policy_override: None,
        runtime_permission_profile_override: None,
        file_search,
        transcript_cells: Vec::new(),
        overlay: None,
        deferred_history_lines: Vec::new(),
        has_emitted_history_lines: false,
        transcript_reflow: TranscriptReflowState::default(),
        initial_history_replay_buffer: None,
        enhanced_keys_supported: false,
        keymap: crate::keymap::RuntimeKeymap::defaults(),
        commit_anim_running: Arc::new(AtomicBool::new(false)),
        status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        backtrack: BacktrackState::default(),
        backtrack_render_pending: false,
        app_server_target: crate::AppServerTarget::Embedded,
        windows_sandbox: WindowsSandboxState::default(),
        prompt_history: None,
        pending_hook_enabled_writes: HashMap::new(),
        side_parent_run: None,
        side_parent_was_running: false,
    };
    (app, runtime)
}

pub(super) fn app_enabled_in_effective_config(config: &TuiConfig, app_id: &str) -> Option<bool> {
    config
        .effective_config
        .as_table()
        .and_then(|table| table.get("apps"))
        .and_then(toml::Value::as_table)
        .and_then(|apps| apps.get(app_id))
        .and_then(toml::Value::as_table)
        .and_then(|app| app.get("enabled"))
        .and_then(toml::Value::as_bool)
}
