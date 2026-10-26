mod model_catalog;
mod session_summary;
mod startup;

use super::*;
use crate::app_backtrack::BacktrackSelection;
use crate::app_backtrack::BacktrackState;
use crate::app_backtrack::user_count;
use crate::chatwidget::tests::helpers::render_bottom_popup;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use crate::chatwidget::tests::set_chatgpt_auth;
use crate::chatwidget::tests::set_fast_mode_test_catalog;
use crate::file_search::FileSearchManager;
use crate::goal_files;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::history_cell::new_session_info;
use crate::session_state::ThreadSessionState;
use crate::test_support::get_model_offline_for_tests;
use crate::test_support::test_config_contents;
use crate::test_support::write_test_model_provider_config;

use crate::app_command::AppCommand as Op;
use crate::session_telemetry::SessionTelemetry;
use assert_matches::assert_matches;
use insta::assert_snapshot;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_world_terminal_client::TerminalUserInput as UserInput;
use octopus_world_terminal_client::TextElement;
use octopus_world_terminal_config::TerminalResizeReflowMaxRows;
use pretty_assertions::assert_eq;
use ratatui::prelude::Line;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

macro_rules! assert_app_snapshot {
    ($name:expr, $value:expr $(,)?) => {
        insta::with_settings!({snapshot_path => "../snapshots"}, {
            assert_snapshot!($name, $value);
        });
    };
}

fn test_absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::try_from(PathBuf::from(path)).expect("absolute test path")
}

fn test_thread_session(thread_id: String, cwd: &str) -> ThreadSessionState {
    ThreadSessionState {
        thread_id,
        forked_from_id: None,
        fork_parent_title: None,
        thread_name: None,
        model: "gpt-test".to_string(),
        model_provider_id: "test-provider".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::Never,
        approvals_reviewer: ApprovalsReviewer::User,
        permission_profile: PermissionProfile::read_only(),
        active_permission_profile: None,
        cwd: test_path_buf(cwd).abs(),
        runtime_workspace_roots: Vec::new(),
        instruction_source_paths: Vec::new(),
        reasoning_effort: None,
        collaboration_mode: None,
        personality: None,
        network_proxy: None,
        rollout_path: Some(PathBuf::new()),
    }
}

#[test]
fn bypass_hook_trust_startup_warning_snapshot() {
    let rendered = lines_to_single_string(
        &history_cell::new_warning_event(
            "`--dangerously-bypass-hook-trust` is enabled. Enabled hooks may run without review for this invocation."
                .to_string(),
        )
        .display_lines( 80),
    );

    assert_app_snapshot!("bypass_hook_trust_startup_warning", rendered);
}

#[tokio::test]
async fn update_memory_settings_persists_and_updates_widget_config() -> Result<()> {
    let (mut app, runtime, _app_event_rx, _op_rx) =
        Box::pin(make_test_app_with_channels_and_runtime()).await;
    let codex_home = tempdir()?;
    write_test_model_provider_config(codex_home.path());
    app.config
        .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

    Box::pin(app.update_memory_settings_with_app_server(&mut app_server, false, false)).await;

    assert!(!app.config.memories.use_memories);
    assert!(!app.config.memories.generate_memories);
    assert!(!app.chat_widget.config_ref().memories.use_memories);
    assert!(!app.chat_widget.config_ref().memories.generate_memories);

    assert_eq!(
        persisted_edit_value(&runtime, "memories.use_memories"),
        Some(serde_json::json!(false))
    );
    assert_eq!(
        persisted_edit_value(&runtime, "memories.generate_memories"),
        Some(serde_json::json!(false))
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn reset_memories_clears_local_memory_directories() -> Result<()> {
    Box::pin(async {
        let (mut app, _app_event_rx, _op_rx) = Box::pin(make_test_app_with_channels()).await;
        let codex_home = tempdir()?;
        app.config
            .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());

        let memory_root = codex_home.path().join("memories");
        let extensions_root = memory_root.join("extensions");
        std::fs::create_dir_all(memory_root.join("rollout_summaries"))?;
        std::fs::create_dir_all(&extensions_root)?;
        std::fs::write(memory_root.join("MEMORY.md"), "stale memory\n")?;
        std::fs::write(
            memory_root.join("rollout_summaries").join("stale.md"),
            "stale summary\n",
        )?;
        std::fs::write(extensions_root.join("stale.txt"), "stale extension\n")?;

        let mut app_server =
            Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

        Box::pin(app.reset_memories_with_app_server(&mut app_server)).await;

        assert_eq!(std::fs::read_dir(&memory_root)?.count(), 0);

        app_server.shutdown().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn apply_permission_profile_selection_installs_the_runtime_resolved_profile() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    app.config
        .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
    let runtime = crate::test_support::TestProviderConfigRuntime::new();
    runtime.set_snapshot_tweak(|snapshot| {
        snapshot.permissions = crate::session_posture::SessionPermissions::new(
            crate::session_posture::ConstrainedValue::unconstrained(AskForApproval::OnRequest),
            crate::session_posture::SessionPermissionSnapshot::active(
                PermissionProfile::read_only(),
                octopus_substrate_contract::permission_profile::ActivePermissionProfile::new(
                    "locked-down",
                ),
            ),
            crate::session_posture::ProfileClassVerdicts::default(),
            Vec::new(),
            None,
        );
    });
    app.config_runtime = runtime;

    assert!(
        app.apply_permission_profile_selection(PermissionProfileSelection {
            profile_id: "locked-down".to_string(),
            approval_policy: None,
            approvals_reviewer: None,
            display_label: "locked-down".to_string(),
        })
        .await
    );

    assert_eq!(
        app.config
            .permissions
            .active_permission_profile()
            .as_ref()
            .map(|profile| profile.id.as_str()),
        Some("locked-down")
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .active_permission_profile()
            .as_ref()
            .map(|profile| profile.id.as_str()),
        Some("locked-down")
    );
    assert_eq!(
        app.runtime_permission_profile_override,
        Some(RuntimePermissionProfileOverride::from_config(&app.config))
    );
    let op = match app_event_rx.try_recv() {
        Ok(AppEvent::CodexOp(op)) => op,
        other => panic!("expected CodexOp event, got {other:?}"),
    };
    assert_eq!(
        op,
        Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            permission_profile: Some(app.config.permissions.permission_profile().clone()),
            active_permission_profile: app.config.permissions.active_permission_profile(),
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        }
    );
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = cell
        .display_lines(120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Permissions updated to locked-down"));
    Ok(())
}

#[tokio::test]
async fn update_feature_flags_enabling_guardian_selects_auto_review() -> Result<()> {
    let (mut app, runtime, mut app_event_rx, mut op_rx) =
        make_test_app_with_channels_and_runtime().await;
    let codex_home = tempdir()?;
    app.config
        .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
    let auto_review = auto_review_mode();
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

    app.update_feature_flags(
        &mut app_server,
        vec![("guardian_approval".to_string(), true)],
    )
    .await;

    assert!(app.config.features.enabled_key("guardian_approval"));
    assert!(
        app.chat_widget
            .config_ref()
            .features
            .enabled_key("guardian_approval")
    );
    assert_eq!(
        app.config.approvals_reviewer,
        auto_review.approvals_reviewer
    );
    assert_eq!(
        AskForApproval::from(app.config.permissions.approval_policy.value()),
        auto_review.approval_policy
    );
    assert_eq!(
        AskForApproval::from(
            app.chat_widget
                .config_ref()
                .permissions
                .approval_policy
                .value(),
        ),
        auto_review.approval_policy
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .permission_profile(),
        &auto_review.permission_profile()
    );
    assert_eq!(
        app.config.permissions.active_permission_profile(),
        Some(auto_review.active_permission_profile.clone())
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .active_permission_profile(),
        Some(auto_review.active_permission_profile.clone())
    );
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        auto_review.approvals_reviewer
    );
    assert_eq!(app.runtime_approval_policy_override, None);
    assert_eq!(
        app.runtime_permission_profile_override,
        Some(RuntimePermissionProfileOverride::from_config(&app.config))
    );
    assert_eq!(
        op_rx.try_recv(),
        Ok(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: Some(auto_review.approval_policy),
            approvals_reviewer: Some(auto_review.approvals_reviewer),
            permission_profile: Some(auto_review.permission_profile()),
            active_permission_profile: Some(auto_review.active_permission_profile.clone()),
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
    );
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = cell
        .display_lines(120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Permissions updated to Approve for me"));

    assert_eq!(
        persisted_edit_value(&runtime, "features.guardian_approval"),
        Some(serde_json::json!(true))
    );
    assert_eq!(
        persisted_edit_value(&runtime, "approvals_reviewer"),
        Some(serde_json::json!("auto_review"))
    );
    assert_eq!(
        persisted_edit_value(&runtime, "approval_policy"),
        Some(serde_json::json!("on-request"))
    );
    assert_eq!(
        persisted_edit_value(&runtime, "sandbox_mode"),
        Some(serde_json::json!("workspace-write"))
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn update_feature_flags_disabling_guardian_clears_review_policy_and_restores_default()
-> Result<()> {
    let (mut app, runtime, mut app_event_rx, mut op_rx) =
        make_test_app_with_channels_and_runtime().await;
    let codex_home = tempdir()?;
    app.config
        .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
    let config_toml_path = codex_home.path().join("config.toml").abs();
    let config_toml = test_config_contents(
        "approvals_reviewer = \"guardian_subagent\"\napproval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n\n[features]\nguardian_approval = true\n",
    );
    std::fs::write(config_toml_path.as_path(), &config_toml)?;
    app.config
        .features
        .set_enabled_by_key("guardian_approval", true)?;
    app.chat_widget
        .set_feature_enabled("guardian_approval", true);
    app.config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    app.chat_widget
        .set_approvals_reviewer(ApprovalsReviewer::AutoReview);
    app.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest)?;
    app.config
        .permissions
        .set_permission_profile(PermissionProfile::workspace_write())?;
    app.chat_widget
        .set_approval_policy(AskForApproval::OnRequest);
    app.chat_widget
        .set_permission_profile_from_session_snapshot(
            crate::session_posture::SessionPermissionSnapshot::legacy(
                PermissionProfile::workspace_write(),
            ),
        )?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

    app.update_feature_flags(
        &mut app_server,
        vec![("guardian_approval".to_string(), false)],
    )
    .await;

    assert!(!app.config.features.enabled_key("guardian_approval"));
    assert!(
        !app.chat_widget
            .config_ref()
            .features
            .enabled_key("guardian_approval")
    );
    assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
    assert_eq!(
        AskForApproval::from(app.config.permissions.approval_policy.value()),
        AskForApproval::OnRequest
    );
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        ApprovalsReviewer::User
    );
    assert_eq!(app.runtime_approval_policy_override, None);
    assert_eq!(
        op_rx.try_recv(),
        Ok(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: Some(ApprovalsReviewer::User),
            permission_profile: None,
            active_permission_profile: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
    );
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = cell
        .display_lines(120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Permissions updated to Ask for approval"));

    assert_ne!(
        persisted_edit_value(&runtime, "features.guardian_approval"),
        Some(serde_json::json!(true))
    );
    let reviewer_edit = persisted_edit_value(&runtime, "approvals_reviewer");
    assert!(
        reviewer_edit.is_none() || reviewer_edit == Some(serde_json::Value::Null),
        "disabling guardian must not persist a reviewer: {reviewer_edit:?}"
    );
    assert_eq!(persisted_edit_value(&runtime, "approval_policy"), None);
    assert_eq!(persisted_edit_value(&runtime, "sandbox_mode"), None);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn update_feature_flags_enabling_guardian_overrides_explicit_manual_review_policy()
-> Result<()> {
    let (mut app, runtime, _app_event_rx, mut op_rx) =
        make_test_app_with_channels_and_runtime().await;
    let codex_home = tempdir()?;
    app.config
        .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
    let auto_review = auto_review_mode();
    let config_toml_path = codex_home.path().join("config.toml").abs();
    let config_toml = test_config_contents("approvals_reviewer = \"user\"\n");
    std::fs::write(config_toml_path.as_path(), &config_toml)?;
    app.config.approvals_reviewer = ApprovalsReviewer::User;
    app.chat_widget
        .set_approvals_reviewer(ApprovalsReviewer::User);
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

    app.update_feature_flags(
        &mut app_server,
        vec![("guardian_approval".to_string(), true)],
    )
    .await;

    assert!(app.config.features.enabled_key("guardian_approval"));
    assert_eq!(
        app.config.approvals_reviewer,
        auto_review.approvals_reviewer
    );
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        auto_review.approvals_reviewer
    );
    assert_eq!(
        AskForApproval::from(app.config.permissions.approval_policy.value()),
        auto_review.approval_policy
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .permission_profile(),
        &auto_review.permission_profile()
    );
    assert_eq!(
        op_rx.try_recv(),
        Ok(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: Some(auto_review.approval_policy),
            approvals_reviewer: Some(auto_review.approvals_reviewer),
            permission_profile: Some(auto_review.permission_profile()),
            active_permission_profile: Some(auto_review.active_permission_profile.clone()),
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
    );

    assert_eq!(
        persisted_edit_value(&runtime, "approvals_reviewer"),
        Some(serde_json::json!("auto_review"))
    );
    assert_eq!(
        persisted_edit_value(&runtime, "features.guardian_approval"),
        Some(serde_json::json!(true))
    );
    assert_eq!(
        persisted_edit_value(&runtime, "approval_policy"),
        Some(serde_json::json!("on-request"))
    );
    assert_eq!(
        persisted_edit_value(&runtime, "sandbox_mode"),
        Some(serde_json::json!("workspace-write"))
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn update_feature_flags_disabling_guardian_clears_manual_review_policy_without_history()
-> Result<()> {
    let (mut app, runtime, mut app_event_rx, mut op_rx) =
        make_test_app_with_channels_and_runtime().await;
    let codex_home = tempdir()?;
    app.config
        .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
    let config_toml_path = codex_home.path().join("config.toml").abs();
    let config_toml = test_config_contents(
        "approvals_reviewer = \"user\"\napproval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n\n[features]\nguardian_approval = true\n",
    );
    std::fs::write(config_toml_path.as_path(), &config_toml)?;
    app.config
        .features
        .set_enabled_by_key("guardian_approval", true)?;
    app.chat_widget
        .set_feature_enabled("guardian_approval", true);
    app.config.approvals_reviewer = ApprovalsReviewer::User;
    app.chat_widget
        .set_approvals_reviewer(ApprovalsReviewer::User);
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

    app.update_feature_flags(
        &mut app_server,
        vec![("guardian_approval".to_string(), false)],
    )
    .await;

    assert!(!app.config.features.enabled_key("guardian_approval"));
    assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        ApprovalsReviewer::User
    );
    assert_eq!(
        op_rx.try_recv(),
        Ok(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: Some(ApprovalsReviewer::User),
            permission_profile: None,
            active_permission_profile: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
    );
    assert!(
        app_event_rx.try_recv().is_err(),
        "manual review should not emit a permissions history update when the effective state stays default"
    );

    assert_ne!(
        persisted_edit_value(&runtime, "features.guardian_approval"),
        Some(serde_json::json!(true))
    );
    let reviewer_edit = persisted_edit_value(&runtime, "approvals_reviewer");
    assert!(
        reviewer_edit.is_none() || reviewer_edit == Some(serde_json::Value::Null),
        "disabling guardian must not persist a reviewer: {reviewer_edit:?}"
    );
    app_server.shutdown().await?;
    Ok(())
}

async fn render_clear_ui_header_after_long_transcript_for_snapshot() -> String {
    let mut app = make_test_app().await;
    app.config.cwd = test_path_buf("/tmp/project").abs();
    app.chat_widget.set_model("gpt-test");
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    let story_part_one = "In the cliffside town of Bracken Ferry, the lighthouse had been dark for \
            nineteen years, and the children were told it was because the sea no longer wanted a \
            guide. Mara, who repaired clocks for a living, found that hard to believe. Every dawn she \
            heard the gulls circling the empty tower, and every dusk she watched ships hesitate at the \
            mouth of the bay as if listening for a signal that never came. When an old brass key fell \
            out of a cracked parcel in her workshop, tagged only with the words 'for the lamp room,' \
            she decided to climb the hill and see what the town had forgotten.";
    let story_part_two = "Inside the lighthouse she found gears wrapped in oilcloth, logbooks filled \
            with weather notes, and a lens shrouded beneath salt-stiff canvas. The mechanism was not \
            broken, only unfinished. Someone had removed the governor spring and hidden it in a false \
            drawer, along with a letter from the last keeper admitting he had darkened the light on \
            purpose after smugglers threatened his family. Mara spent the night rebuilding the clockwork \
            from spare watch parts, her fingers blackened with soot and grease, while a storm gathered \
            over the water and the harbor bells began to ring.";
    let story_part_three = "At midnight the first squall hit, and the fishing boats returned early, \
            blind in sheets of rain. Mara wound the mechanism, set the teeth by hand, and watched the \
            great lens begin to turn in slow, certain arcs. The beam swept across the bay, caught the \
            whitecaps, and reached the boats just as they were drifting toward the rocks below the \
            eastern cliffs. In the morning the town square was crowded with wet sailors, angry elders, \
            and wide-eyed children, but when the oldest captain placed the keeper's log on the fountain \
            and thanked Mara for relighting the coast, nobody argued. By sunset, Bracken Ferry had a \
            lighthouse again, and Mara had more clocks to mend than ever because everyone wanted \
            something in town to keep better time.";

    let user_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(UserHistoryCell {
            message: text.to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>
    };
    let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(AgentMessageCell::new(
            vec![Line::from(text.to_string())],
            true,
        )) as Arc<dyn HistoryCell>
    };
    let make_header = |is_first| -> Arc<dyn HistoryCell> {
        let session = ThreadSessionState {
            thread_id: uuid::Uuid::now_v7().to_string(),
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/tmp/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: Some(ReasoningEffortConfig::High),
            collaboration_mode: None,
            personality: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
        };
        Arc::new(new_session_info(
            app.chat_widget.config_ref(),
            app.chat_widget.current_model(),
            &session,
            is_first,
            None,
            None,
            false,
        )) as Arc<dyn HistoryCell>
    };

    app.transcript_cells = vec![
        make_header(true),
        Arc::new(crate::history_cell::new_info_event(
            "startup tip that used to replay".to_string(),
            None,
        )) as Arc<dyn HistoryCell>,
        user_cell("Tell me a long story about a town with a dark lighthouse."),
        agent_cell(story_part_one),
        user_cell("Continue the story and reveal why the light went out."),
        agent_cell(story_part_two),
        user_cell("Finish the story with a storm and a resolution."),
        agent_cell(story_part_three),
    ];
    app.has_emitted_history_lines = true;

    let rendered = app
        .clear_ui_header_lines_with_version(80, "<VERSION>")
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !rendered.contains("startup tip that used to replay"),
        "clear header should not replay startup notices"
    );
    assert!(
        !rendered.contains("Bracken Ferry"),
        "clear header should not replay prior conversation turns"
    );
    rendered
}

#[tokio::test]
#[cfg_attr(
    target_os = "windows",
    ignore = "snapshot path rendering differs on Windows"
)]

async fn clear_ui_after_long_transcript_snapshots_fresh_header_only() {
    let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
    assert_app_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
}

#[tokio::test]
#[cfg_attr(
    target_os = "windows",
    ignore = "snapshot path rendering differs on Windows"
)]

async fn ctrl_l_clear_ui_after_long_transcript_reuses_clear_header_snapshot() {
    let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
    assert_app_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
}

#[tokio::test]
#[cfg_attr(
    target_os = "windows",
    ignore = "snapshot path rendering differs on Windows"
)]

async fn clear_ui_header_shows_fast_status_for_fast_capable_models() {
    let mut app = make_test_app().await;
    app.config.cwd = test_path_buf("/tmp/project").abs();
    app.chat_widget.set_model("gpt-5.4");
    set_fast_mode_test_catalog(&mut app.chat_widget);
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));
    app.chat_widget.set_service_tier(Some(
        crate::session_vocab::config_types::ServiceTier::Fast
            .request_value()
            .to_string(),
    ));
    set_chatgpt_auth(&mut app.chat_widget);
    set_fast_mode_test_catalog(&mut app.chat_widget);

    let rendered = app
        .clear_ui_header_lines_with_version(80, "<VERSION>")
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert_app_snapshot!("clear_ui_header_fast_status_fast_capable_models", rendered);
}

fn build_test_app(chat_widget: crate::chatwidget::ChatWidget, app_event_tx: AppEventSender) -> App {
    let config = chat_widget.config_ref().clone();
    let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
    let model = get_model_offline_for_tests(config.model.as_deref());
    let session_telemetry = test_session_telemetry(&config, model.as_str());

    App {
        model_catalog: chat_widget.model_catalog(),
        session_telemetry,
        app_event_tx,
        chat_widget,
        workspace_command_runner: None,
        config,
        config_runtime: crate::test_support::TestProviderConfigRuntime::new(),
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
        environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
        app_server_target: crate::AppServerTarget::Embedded,
        windows_sandbox: WindowsSandboxState::default(),
        prompt_history: None,
        pending_hook_enabled_writes: HashMap::new(),
        side_parent_run: None,
        side_parent_was_running: false,
    }
}

async fn make_test_app() -> App {
    let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    build_test_app(chat_widget, app_event_tx)
}

async fn make_test_app_with_channels() -> (
    App,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (chat_widget, app_event_tx, rx, op_rx) = make_chatwidget_manual_with_sender().await;
    (build_test_app(chat_widget, app_event_tx), rx, op_rx)
}

async fn make_test_app_with_channels_and_runtime() -> (
    App,
    std::sync::Arc<crate::test_support::TestProviderConfigRuntime>,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut app, rx, op_rx) = make_test_app_with_channels().await;
    let runtime = crate::test_support::TestProviderConfigRuntime::new();
    app.config_runtime = runtime.clone();
    (app, runtime, rx, op_rx)
}

fn persisted_edit_value(
    runtime: &crate::test_support::TestProviderConfigRuntime,
    key_path: &str,
) -> Option<serde_json::Value> {
    runtime
        .persisted_edits
        .lock()
        .expect("test runtime lock")
        .iter()
        .rev()
        .find(|edit| edit.key_path == key_path)
        .map(|edit| edit.value.clone())
}

#[tokio::test]
async fn set_thread_goal_draft_fails_closed_without_descriptor_owned_goal_projection() -> Result<()>
{
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let mut app_server =
        crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
    let thread_id = uuid::Uuid::now_v7().to_string();
    app.chat_widget
        .handle_thread_session(test_thread_session(thread_id.clone(), "/tmp/project"));
    while app_event_rx.try_recv().is_ok() {}

    app.set_thread_goal_draft(
        &mut app_server,
        thread_id.clone(),
        crate::goal_files::GoalDraft {
            objective: "small goal".to_string(),
            ..Default::default()
        },
        crate::app_event::ThreadGoalSetMode::ConfirmIfExists,
    )
    .await;

    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected goal read failure history cell, got {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(120));
    assert!(rendered.contains("Failed to read thread goal"));
    assert!(rendered.contains("thread goal requires an attached private terminal run"));
    assert!(
        !app.chat_widget
            .config_ref()
            .codex_home
            .join("attachments")
            .exists()
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn replace_goal_confirmation_snapshot() {
    let mut app = make_test_app().await;
    app.show_replace_thread_goal_confirmation(
        uuid::Uuid::now_v7().to_string(),
        goal_files::GoalDraft {
            objective: "New goal".to_string(),
            ..Default::default()
        },
    );
    assert_app_snapshot!(
        "replace_goal_confirmation",
        render_bottom_popup(&app.chat_widget, 80)
    );
}

fn plain_line_cell(text: impl Into<String>) -> Arc<dyn HistoryCell> {
    Arc::new(PlainHistoryCell::new(vec![Line::from(text.into())])) as Arc<dyn HistoryCell>
}

fn rendered_line_text(line: &crate::terminal_hyperlinks::HyperlinkLine) -> String {
    line.line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[tokio::test]
async fn capped_resize_reflow_renders_recent_suffix_only() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal.resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(5);
    app.transcript_cells = (0..20)
        .map(|i| plain_line_cell(format!("cell {i}")))
        .collect();

    let rendered = app.render_transcript_lines_for_reflow(80);

    assert_eq!(rendered.lines.len(), 5);
    assert_eq!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>(),
        vec![
            "cell 17".to_string(),
            String::new(),
            "cell 18".to_string(),
            String::new(),
            "cell 19".to_string(),
        ]
    );
}

#[tokio::test]
async fn uncapped_resize_reflow_renders_all_cells_when_row_cap_absent() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal.resize_reflow.max_rows = TerminalResizeReflowMaxRows::Disabled;
    app.transcript_cells = (0..20)
        .map(|i| plain_line_cell(format!("cell {i}")))
        .collect();

    let rendered = app.render_transcript_lines_for_reflow(80);

    assert_eq!(rendered.lines.len(), 39);
    assert_eq!(rendered_line_text(&rendered.lines[0]), "cell 0");
    assert_eq!(rendered_line_text(&rendered.lines[38]), "cell 19");
}

#[tokio::test]
async fn resize_reflow_wraps_transcript_early_when_pet_is_enabled() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal.resize_reflow.max_rows = TerminalResizeReflowMaxRows::Disabled;
    app.transcript_cells = vec![Arc::new(AgentMarkdownCell::new(
        "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda".to_string(),
        Path::new("/tmp"),
    ))];

    let without_pet = app.render_transcript_lines_for_reflow(40);
    app.chat_widget
        .set_pet_image_support_for_tests(crate::pets::PetImageSupport::Supported(
            crate::pets::ImageProtocol::Kitty,
        ));
    app.chat_widget.install_test_ambient_pet_for_tests(false);
    let width = app.chat_widget.history_wrap_width(40);
    assert!(width < 40);
    let with_pet = app.render_transcript_lines_for_reflow(width);

    assert!(
        with_pet.lines.len() > without_pet.lines.len(),
        "expected pet-enabled transcript reflow to wrap earlier"
    );
}

#[tokio::test]
async fn uncapped_resize_reflow_renders_all_cells_under_row_limit() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal.resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(100);
    app.transcript_cells = (0..3)
        .map(|i| plain_line_cell(format!("cell {i}")))
        .collect();

    let rendered = app.render_transcript_lines_for_reflow(80);

    assert_eq!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>(),
        vec![
            "cell 0".to_string(),
            String::new(),
            "cell 1".to_string(),
            String::new(),
            "cell 2".to_string(),
        ]
    );
}

#[tokio::test]
async fn initial_replay_buffer_keeps_recent_rows_when_row_cap_present() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal.resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(3);

    app.begin_initial_history_replay_buffer();
    for index in 0..5 {
        App::buffer_initial_history_replay_display_lines(
            app.initial_history_replay_buffer
                .as_mut()
                .expect("initial replay buffer active"),
            vec![Line::from(format!("line {index}")).into()],
            3,
        );
    }

    let buffer = app
        .initial_history_replay_buffer
        .as_ref()
        .expect("initial replay buffer should remain active");
    assert_eq!(
        buffer
            .retained_lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>(),
        vec![
            "line 2".to_string(),
            "line 3".to_string(),
            "line 4".to_string(),
        ]
    );
}

#[tokio::test]
async fn thread_switch_replay_buffer_uses_transcript_tail_mode_when_row_cap_present() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal.resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(3);

    app.begin_thread_switch_history_replay_buffer();

    let buffer = app
        .initial_history_replay_buffer
        .as_ref()
        .expect("thread switch replay buffer should be active");
    assert!(buffer.render_from_transcript_tail);
    assert!(buffer.retained_lines.is_empty());
}

#[tokio::test]
async fn thread_switch_replay_buffer_is_disabled_without_row_cap() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal.resize_reflow.max_rows = TerminalResizeReflowMaxRows::Disabled;

    app.begin_thread_switch_history_replay_buffer();

    assert!(app.initial_history_replay_buffer.is_none());
}

#[tokio::test]
async fn height_shrink_schedules_resize_reflow() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    let frame_requester = crate::tui::FrameRequester::test_dummy();

    assert!(!app.handle_draw_size_change(
        ratatui::layout::Size::new(118, 35),
        ratatui::layout::Size::new(118, 35),
        &frame_requester,
    ));

    assert!(app.handle_draw_size_change(
        ratatui::layout::Size::new(118, 24),
        ratatui::layout::Size::new(118, 35),
        &frame_requester,
    ));
    assert!(app.transcript_reflow.has_pending_reflow());
}

fn next_user_turn_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Op {
    let mut seen = Vec::new();
    while let Ok(op) = op_rx.try_recv() {
        if matches!(op, Op::UserTurn { .. }) {
            return op;
        }
        seen.push(format!("{op:?}"));
    }
    panic!("expected UserTurn op, saw: {seen:?}");
}

fn lines_to_single_string(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn test_session_telemetry(_config: &TuiConfig, _model: &str) -> SessionTelemetry {
    SessionTelemetry::disabled()
}

#[tokio::test]
async fn fresh_session_config_uses_current_service_tier() {
    let mut app = make_test_app().await;
    app.chat_widget.set_service_tier(Some(
        crate::session_vocab::config_types::ServiceTier::Fast
            .request_value()
            .to_string(),
    ));

    let config = app.fresh_session_config();

    assert_eq!(
        config.service_tier,
        Some(
            crate::session_vocab::config_types::ServiceTier::Fast
                .request_value()
                .to_string()
        )
    );
}

#[tokio::test]
async fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

    let user_cell = |text: &str,
                     text_elements: Vec<TextElement>,
                     local_image_paths: Vec<PathBuf>,
                     remote_image_urls: Vec<String>|
     -> Arc<dyn HistoryCell> {
        Arc::new(UserHistoryCell {
            message: text.to_string(),
            text_elements,
            local_image_paths,
            remote_image_urls,
        }) as Arc<dyn HistoryCell>
    };
    let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(AgentMessageCell::new(
            vec![Line::from(text.to_string())],
            true,
        )) as Arc<dyn HistoryCell>
    };

    let make_header = |is_first| {
        let session = ThreadSessionState {
            thread_id: uuid::Uuid::now_v7().to_string(),
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/home/user/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            personality: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
        };
        Arc::new(new_session_info(
            app.chat_widget.config_ref(),
            app.chat_widget.current_model(),
            &session,
            is_first,
            None,
            None,
            false,
        )) as Arc<dyn HistoryCell>
    };

    let placeholder = "[Image #1]";
    let edited_text = format!("follow-up (edited) {placeholder}");
    let edited_range = edited_text.len().saturating_sub(placeholder.len())..edited_text.len();
    let edited_text_elements = vec![TextElement::new(edited_range.into(), None)];
    let edited_local_image_paths = vec![PathBuf::from("/tmp/fake-image.png")];

    app.transcript_cells = vec![
        make_header(true),
        user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer first"),
        user_cell("follow-up", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer follow-up"),
        make_header(false),
        user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer first"),
        user_cell(
            &edited_text,
            edited_text_elements.clone(),
            edited_local_image_paths.clone(),
            vec!["https://example.com/backtrack.png".to_string()],
        ),
        agent_cell("answer edited"),
    ];

    assert_eq!(user_count(&app.transcript_cells), 2);

    let base_id = uuid::Uuid::now_v7().to_string();
    app.chat_widget
        .handle_thread_session(crate::session_state::ThreadSessionState {
            thread_id: base_id.clone(),
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/home/user/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            personality: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
        });

    app.backtrack.base_id = Some(base_id.clone());
    app.backtrack.primed = true;
    app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

    let selection = app
        .confirm_backtrack_from_main()
        .expect("backtrack selection");
    assert_eq!(selection.nth_user_message, 1);
    assert_eq!(selection.prefill, edited_text);
    assert_eq!(selection.text_elements, edited_text_elements);
    assert_eq!(selection.local_image_paths, edited_local_image_paths);
    assert_eq!(
        selection.remote_image_urls,
        vec!["https://example.com/backtrack.png".to_string()]
    );

    app.apply_backtrack_rollback(selection);
    assert_eq!(
        app.chat_widget.remote_image_urls(),
        vec!["https://example.com/backtrack.png".to_string()]
    );

    let mut rollback_turns = None;
    while let Ok(op) = op_rx.try_recv() {
        if let Op::ThreadRollback { num_turns } = op {
            rollback_turns = Some(num_turns);
        }
    }

    assert_eq!(rollback_turns, Some(1));
}

#[tokio::test]
async fn backtrack_remote_image_only_selection_clears_existing_composer_draft() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: "original".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    }) as Arc<dyn HistoryCell>];
    app.chat_widget
        .set_composer_text("stale draft".to_string(), Vec::new(), Vec::new());

    let remote_image_url = "https://example.com/remote-only.png".to_string();
    app.apply_backtrack_rollback(BacktrackSelection {
        nth_user_message: 0,
        prefill: String::new(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: vec![remote_image_url.clone()],
    });

    assert_eq!(app.chat_widget.composer_text_with_pending(), "");
    assert_eq!(app.chat_widget.remote_image_urls(), vec![remote_image_url]);

    let mut rollback_turns = None;
    while let Ok(op) = op_rx.try_recv() {
        if let Op::ThreadRollback { num_turns } = op {
            rollback_turns = Some(num_turns);
        }
    }
    assert_eq!(rollback_turns, Some(1));
}

#[tokio::test]
async fn cancelled_turn_edit_restores_prompt_and_rolls_back_latest_turn() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: "original".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    }) as Arc<dyn HistoryCell>];
    let prompt = crate::chatwidget::UserMessage {
        text: "edit me".to_string(),
        local_images: Vec::new(),
        remote_image_urls: vec!["https://example.com/edit.png".to_string()],
        text_elements: Vec::new(),
        mention_bindings: Vec::new(),
    };

    app.apply_cancelled_turn_edit(prompt);

    assert_eq!(app.chat_widget.composer_text_with_pending(), "edit me");
    assert_snapshot!(
        "cancelled_turn_edit_restores_composer",
        app.chat_widget.composer_text_with_pending()
    );
    assert_eq!(
        app.chat_widget.remote_image_urls(),
        vec!["https://example.com/edit.png".to_string()]
    );
    assert_matches!(op_rx.try_recv(), Ok(Op::ThreadRollback { num_turns: 1 }));
}

#[tokio::test]
async fn first_cancelled_turn_edit_restores_prompt_without_local_history() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let prompt = crate::chatwidget::UserMessage {
        text: "edit first prompt".to_string(),
        local_images: Vec::new(),
        remote_image_urls: vec!["https://example.com/edit.png".to_string()],
        text_elements: Vec::new(),
        mention_bindings: Vec::new(),
    };

    app.apply_cancelled_turn_edit(prompt);

    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "edit first prompt"
    );
    assert_eq!(
        app.chat_widget.remote_image_urls(),
        vec!["https://example.com/edit.png".to_string()]
    );
    assert_matches!(op_rx.try_recv(), Ok(Op::ThreadRollback { num_turns: 1 }));
}

#[tokio::test]
async fn backtrack_resubmit_preserves_data_image_urls_in_user_turn() {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

    let thread_id = uuid::Uuid::now_v7().to_string();
    app.chat_widget
        .handle_thread_session(crate::session_state::ThreadSessionState {
            thread_id,
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/home/user/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            personality: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
        });

    let data_image_url = "data:image/png;base64,abc123".to_string();
    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: "please inspect this".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: vec![data_image_url.clone()],
    }) as Arc<dyn HistoryCell>];

    app.apply_backtrack_rollback(BacktrackSelection {
        nth_user_message: 0,
        prefill: "please inspect this".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: vec![data_image_url.clone()],
    });

    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let mut saw_rollback = false;
    let mut submitted_items: Option<Vec<UserInput>> = None;
    while let Ok(op) = op_rx.try_recv() {
        match op {
            Op::ThreadRollback { .. } => saw_rollback = true,
            Op::UserTurn { items, .. } => submitted_items = Some(items),
            _ => {}
        }
    }

    assert!(saw_rollback);
    let items = submitted_items.expect("expected user turn after backtrack resubmit");
    assert!(items.iter().any(|item| {
        matches!(
            item,
            UserInput::Image { url, .. } if url == &data_image_url
        )
    }));
}

#[tokio::test]
async fn clear_only_ui_reset_preserves_chat_session_state() {
    let mut app = make_test_app().await;
    let thread_id = uuid::Uuid::now_v7().to_string();
    app.chat_widget
        .handle_thread_session(crate::session_state::ThreadSessionState {
            thread_id: thread_id.clone(),
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: Some("keep me".to_string()),
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/tmp/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            personality: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
        });
    app.chat_widget
        .apply_external_edit("draft prompt".to_string());
    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: "old message".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    }) as Arc<dyn HistoryCell>];
    app.overlay = Some(Overlay::new_transcript(
        app.transcript_cells.clone(),
        crate::keymap::RuntimeKeymap::defaults().pager,
    ));
    app.deferred_history_lines = vec![Line::from("stale buffered line").into()];
    app.has_emitted_history_lines = true;
    app.backtrack.primed = true;
    app.backtrack.overlay_preview_active = true;
    app.backtrack.nth_user_message = 0;
    app.backtrack_render_pending = true;

    app.reset_app_ui_state_after_clear();

    assert!(app.overlay.is_none());
    assert!(app.transcript_cells.is_empty());
    assert!(app.deferred_history_lines.is_empty());
    assert!(!app.has_emitted_history_lines);
    assert!(!app.backtrack.primed);
    assert!(!app.backtrack.overlay_preview_active);
    assert!(app.backtrack.pending_rollback.is_none());
    assert!(!app.backtrack_render_pending);
    assert_eq!(app.chat_widget.thread_id(), Some(thread_id.clone()));
    assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
}

#[tokio::test]
async fn backtrack_esc_does_not_steal_empty_vim_insert_escape() {
    let mut app = make_test_app().await;
    let esc = crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Esc, KeyModifiers::NONE);

    assert!(app.chat_widget.composer_is_empty());
    assert!(app.should_handle_backtrack_esc(esc));

    app.chat_widget.toggle_vim_mode_and_notify();
    assert!(app.should_handle_backtrack_esc(esc));

    app.chat_widget
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            KeyModifiers::NONE,
        ));
    assert!(app.chat_widget.should_handle_vim_insert_escape(esc));
    assert!(!app.should_handle_backtrack_esc(esc));

    app.chat_widget.handle_key_event(esc);

    assert!(!app.backtrack.primed);
    assert!(!app.chat_widget.should_handle_vim_insert_escape(esc));
    assert!(app.should_handle_backtrack_esc(esc));
}
