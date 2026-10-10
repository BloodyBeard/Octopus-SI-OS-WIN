use super::*;
use crate::bottom_pane::goal_status_indicator_line;
use crate::chatwidget::rate_limits::NUDGE_MODEL_SLUG;
use crate::chatwidget::rate_limits::get_limits_duration;
use pretty_assertions::assert_eq;
use ratatui::backend::TestBackend;
use serial_test::serial;

fn enable_test_ambient_pet(chat: &mut ChatWidget) {
    chat.set_pet_image_support_for_tests(crate::pets::PetImageSupport::Supported(
        crate::pets::ImageProtocol::Kitty,
    ));
    chat.install_test_ambient_pet_for_tests(false);
}

#[tokio::test]
async fn token_count_none_resets_context_indicator() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(None).await;

    let context_window = 13_000;
    let pre_compact_tokens = 12_700;

    handle_token_count(
        &mut chat,
        Some(make_token_info(pre_compact_tokens, context_window)),
    );
    assert_eq!(chat.bottom_pane.context_window_percent(), Some(30));

    handle_token_count(&mut chat, None);
    assert_eq!(chat.bottom_pane.context_window_percent(), None);
}

#[tokio::test]
async fn context_indicator_shows_used_tokens_when_window_unknown() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(Some("unknown-model")).await;

    chat.config.model_context_window = None;

    let total_tokens = 106_000;
    let token_usage = TokenUsage {
        total_tokens,
        ..TokenUsage::default()
    };
    let token_info = TokenUsageInfo {
        total_token_usage: token_usage.clone(),
        last_token_usage: token_usage,
        model_context_window: None,
    };

    handle_token_count(&mut chat, Some(token_info));

    assert_eq!(chat.bottom_pane.context_window_percent(), None);
    assert_eq!(
        chat.bottom_pane.context_window_used_tokens(),
        Some(total_tokens)
    );
}

#[tokio::test]
async fn token_usage_update_uses_runtime_context_window() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    chat.config.model_context_window = Some(1_000_000);

    handle_token_count(&mut chat, Some(make_token_info(0, 950_000)));

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::ContextWindowSize),
        Some("950K window".to_string())
    );
    assert_eq!(chat.bottom_pane.context_window_percent(), Some(100));

    chat.add_status_output(false, None);

    let cells = drain_insert_history(&mut rx);
    let context_line = cells
        .last()
        .expect("status output inserted")
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .find(|line| line.contains("Context window"))
        .expect("context window line");

    assert!(
        context_line.contains("950K"),
        "expected /status to use runtime context window, got: {context_line}"
    );
    assert!(
        !context_line.contains("1M"),
        "expected /status to avoid raw config context window, got: {context_line}"
    );
}

#[tokio::test]
async fn status_line_git_summary_items_render_values() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(None).await;
    chat.status_line_git_summary = Some(StatusLineGitSummary {
        pull_request: Some(crate::branch_summary::StatusLinePullRequest {
            number: 20_252,
            url: "https://github.com/openai/codex/pull/20252".to_string(),
        }),
        branch_change_stats: Some(crate::branch_summary::GitBranchDiffStats {
            additions: 143,
            deletions: 22,
        }),
    });

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::PullRequestNumber),
        Some("PR #20252".to_string())
    );
    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::BranchChanges),
        Some("+143 -22".to_string())
    );
}

#[tokio::test]
async fn raw_output_status_line_value_only_shows_when_enabled() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(None).await;

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::RawOutput),
        None
    );

    chat.set_raw_output_mode(true);

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::RawOutput),
        Some("raw output".to_string())
    );
}

#[tokio::test]
async fn status_line_branch_changes_render_no_changes() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(None).await;
    chat.status_line_git_summary = Some(StatusLineGitSummary {
        pull_request: None,
        branch_change_stats: Some(crate::branch_summary::GitBranchDiffStats {
            additions: 0,
            deletions: 0,
        }),
    });

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::BranchChanges),
        Some("No changes".to_string())
    );
}

#[tokio::test]
async fn stale_status_line_git_summary_update_is_ignored() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(None).await;
    chat.status_line_git_summary_cwd = Some(PathBuf::from("/expected"));
    chat.status_line_git_summary_pending = true;

    chat.set_status_line_git_summary(
        PathBuf::from("/other"),
        StatusLineGitSummary {
            pull_request: Some(crate::branch_summary::StatusLinePullRequest {
                number: 20_252,
                url: "https://github.com/openai/codex/pull/20252".to_string(),
            }),
            branch_change_stats: Some(crate::branch_summary::GitBranchDiffStats {
                additions: 143,
                deletions: 22,
            }),
        },
    );

    assert!(chat.status_line_git_summary.is_none());
    assert!(!chat.status_line_git_summary_pending);
}

#[tokio::test]
async fn raw_output_mode_can_change_without_inserting_notice() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    chat.set_raw_output_mode(true);

    assert!(chat.raw_output_mode());
    assert!(drain_insert_history(&mut rx).is_empty());

    chat.set_raw_output_mode_and_notify(false);

    assert!(!chat.raw_output_mode());
    let history = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        history.contains("Raw output mode off: rich transcript rendering restored."),
        "expected raw output notice, got {history:?}"
    );
}

#[tokio::test]
async fn flush_answer_stream_keeps_default_reflow_for_plain_text_tail() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let cwd = chat.config.cwd.to_path_buf();

    let mut controller = crate::streaming::controller::StreamController::new(
        Some(80),
        cwd.as_path(),
        HistoryRenderMode::Rich,
    );
    assert!(controller.push("plain response line\n"));
    chat.stream_controller = Some(controller);

    while rx.try_recv().is_ok() {}

    chat.flush_answer_stream_with_separator();

    let mut saw_consolidate = false;
    let mut saw_insert_history = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(_) => saw_insert_history = true,
            AppEvent::ConsolidateAgentMessage {
                scrollback_reflow,
                deferred_history_cell,
                ..
            } => {
                saw_consolidate = true;
                assert_eq!(
                    scrollback_reflow,
                    crate::app_event::ConsolidationScrollbackReflow::IfResizeReflowRan
                );
                assert!(deferred_history_cell.is_none());
            }
            _ => {}
        }
    }

    assert!(
        saw_consolidate,
        "expected stream finalization to consolidate"
    );
    assert!(
        saw_insert_history,
        "plain text should still insert history before consolidation"
    );
}

#[tokio::test]
async fn flush_answer_stream_requests_scrollback_reflow_for_live_table_tail() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let cwd = chat.config.cwd.to_path_buf();

    let mut controller = crate::streaming::controller::StreamController::new(
        Some(80),
        cwd.as_path(),
        HistoryRenderMode::Rich,
    );
    controller.push("| Name | Notes |\n");
    controller.push("| --- | --- |\n");
    controller.push("| alpha | tail held until final table render |\n");
    assert!(
        controller.has_live_tail(),
        "expected table holdback to leave a live tail for this regression",
    );
    chat.stream_controller = Some(controller);

    while rx.try_recv().is_ok() {}

    chat.flush_answer_stream_with_separator();

    let mut saw_consolidate = false;
    let mut saw_insert_history = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(_) => saw_insert_history = true,
            AppEvent::ConsolidateAgentMessage {
                scrollback_reflow,
                deferred_history_cell,
                ..
            } => {
                saw_consolidate = true;
                assert_eq!(
                    scrollback_reflow,
                    crate::app_event::ConsolidationScrollbackReflow::Required
                );
                assert!(
                    deferred_history_cell.is_some(),
                    "live table tail should be staged for consolidation",
                );
            }
            _ => {}
        }
    }

    assert!(
        saw_consolidate,
        "expected stream finalization to consolidate"
    );
    assert!(
        !saw_insert_history,
        "live table tail should not be inserted before canonical reflow"
    );
}

#[tokio::test]
async fn completed_plan_table_tail_skips_provisional_history_insert() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let cwd = chat.config.cwd.to_path_buf();

    let mut controller = crate::streaming::controller::PlanStreamController::new(
        Some(80),
        cwd.as_path(),
        HistoryRenderMode::Rich,
    );
    controller.push("| Step | Owner |\n");
    controller.push("| --- | --- |\n");
    controller.push("| Verify | Codex |\n");
    assert!(
        controller.has_live_tail(),
        "expected plan table holdback to leave a live tail",
    );
    chat.plan_stream_controller = Some(controller);
    chat.transcript.plan_delta_buffer =
        "| Step | Owner |\n| --- | --- |\n| Verify | Codex |\n".to_string();

    while rx.try_recv().is_ok() {}

    chat.on_plan_item_completed(String::new());

    let mut saw_source_backed_plan = false;
    let mut saw_stream_plan = false;
    let mut rendered_plan = String::new();
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            if cell.as_any().is::<history_cell::ProposedPlanCell>() {
                saw_source_backed_plan = true;
                rendered_plan = lines_to_single_string(&cell.display_lines(80));
            }
            saw_stream_plan |= cell.as_any().is::<history_cell::ProposedPlanStreamCell>();
        }
    }

    assert!(saw_source_backed_plan, "expected source-backed plan insert");
    assert!(
        rendered_plan.contains('━'),
        "expected completed plan table to render with separators, got: {rendered_plan:?}"
    );
    assert!(
        !saw_stream_plan,
        "live plan table tail should not be inserted provisionally"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "windows", ignore = "disabled on windows")]
async fn configured_pet_load_is_deferred_until_after_construction() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut cfg = test_config().await;
    cfg.terminal.pet = Some(crate::pets::DEFAULT_PET_ID.to_string());
    crate::pets::write_test_pack(&cfg.codex_home);
    let resolved_model = get_model_offline_for_tests(cfg.model.as_deref());
    let session_telemetry = test_session_telemetry(&cfg, resolved_model.as_str());
    let init = ChatWidgetInit {
        config: cfg.clone(),
        config_runtime: crate::test_support::TestProviderConfigRuntime::new(),
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: tx,
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: false,
        has_chatgpt_account: false,
        has_codex_backend_auth: false,
        model_catalog: test_model_catalog(&cfg),
        is_first_run: true,
        status_account_display: None,
        runtime_model_provider_base_url: None,
        initial_plan_type: None,
        model: Some(resolved_model),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        session_telemetry,
    };

    let chat = ChatWidget::new_with_app_event(init);

    assert!(!chat.ambient_pet_image_enabled());
    let event = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_matches!(
        event,
        AppEvent::ConfiguredPetLoaded { pet_id, result } => {
            assert_eq!(pet_id, crate::pets::DEFAULT_PET_ID);
            assert!(result.unwrap().is_some());
        }
    );
}

#[tokio::test]
async fn prefetch_rate_limits_is_gated_on_chatgpt_auth_provider() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    assert!(!chat.should_prefetch_rate_limits());

    set_chatgpt_auth(&mut chat);
    assert!(chat.should_prefetch_rate_limits());

    chat.config.model_provider.requires_openai_auth = false;
    assert!(!chat.should_prefetch_rate_limits());

    chat.prefetch_rate_limits();
    assert!(!chat.should_prefetch_rate_limits());
}

#[tokio::test]
async fn rate_limit_warnings_emit_thresholds() {
    let mut state = RateLimitWarningState::default();
    let mut warnings: Vec<String> = Vec::new();

    warnings.extend(state.take_warnings(Some(10.0), Some(10079), Some(55.0), Some(299)));
    warnings.extend(state.take_warnings(Some(55.0), Some(10081), Some(10.0), Some(299)));
    warnings.extend(state.take_warnings(Some(10.0), Some(10081), Some(80.0), Some(299)));
    warnings.extend(state.take_warnings(Some(80.0), Some(10081), Some(10.0), Some(299)));
    warnings.extend(state.take_warnings(Some(10.0), Some(10081), Some(95.0), Some(299)));
    warnings.extend(state.take_warnings(Some(95.0), Some(10079), Some(10.0), Some(299)));

    assert_eq!(
        warnings,
        vec![
            String::from(
                "Heads up, you have less than 25% of your 5h limit left. Run /status for a breakdown."
            ),
            String::from(
                "Heads up, you have less than 25% of your weekly limit left. Run /status for a breakdown.",
            ),
            String::from(
                "Heads up, you have less than 5% of your 5h limit left. Run /status for a breakdown."
            ),
            String::from(
                "Heads up, you have less than 5% of your weekly limit left. Run /status for a breakdown.",
            ),
        ],
        "expected one warning per limit for the highest crossed threshold"
    );
}

#[tokio::test]
async fn test_rate_limit_warnings_monthly() {
    let mut state = RateLimitWarningState::default();
    let mut warnings: Vec<String> = Vec::new();

    warnings.extend(state.take_warnings(Some(75.0), Some(43199), None, None));
    assert_eq!(
        warnings,
        vec![String::from(
            "Heads up, you have less than 25% of your monthly limit left. Run /status for a breakdown.",
        ),],
        "expected one warning per limit for the highest crossed threshold"
    );
}

#[test]
fn rate_limit_duration_labels_only_render_supported_windows() {
    assert_eq!(get_limits_duration(2 * 60), None);
    assert_eq!(get_limits_duration(24 * 60).as_deref(), Some("daily"));
    assert_eq!(
        get_limits_duration(365 * 24 * 60).as_deref(),
        Some("annual")
    );
}

#[tokio::test]
async fn test_rate_limit_warnings_use_generic_fallback_labels() {
    let mut state = RateLimitWarningState::default();

    assert_eq!(
        state.take_warnings(Some(75.0), None, Some(75.0), None,),
        vec![
            String::from(
                "Heads up, you have less than 25% of your secondary usage limit left. Run /status for a breakdown.",
            ),
            String::from(
                "Heads up, you have less than 25% of your usage limit left. Run /status for a breakdown.",
            ),
        ],
    );
}

#[tokio::test]
async fn test_rate_limit_warnings_use_secondary_fallback_for_unsupported_window() {
    let mut state = RateLimitWarningState::default();

    assert_eq!(
        state.take_warnings(Some(75.0), Some(2 * 60), None, None,),
        vec![String::from(
            "Heads up, you have less than 25% of your secondary usage limit left. Run /status for a breakdown.",
        )],
    );
}

#[tokio::test]
async fn status_line_uses_secondary_fallback_for_unsupported_window() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: Some(RateLimitWindowItem {
            used_percent: 50.0,
            window_minutes: Some(2 * 60),
            resets_at: None,
        }),
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::WeeklyLimit),
        Some("secondary usage 50% left".to_string())
    );
}

#[tokio::test]
async fn status_line_legacy_limit_items_prefer_matching_windows() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindowItem {
            used_percent: 94.0,
            window_minutes: Some(7 * 24 * 60),
            resets_at: None,
        }),
        secondary: Some(RateLimitWindowItem {
            used_percent: 40.0,
            window_minutes: Some(5 * 60),
            resets_at: None,
        }),
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::FiveHourLimit),
        Some("5h 60% left".to_string())
    );
    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::WeeklyLimit),
        Some("weekly 6% left".to_string())
    );
}

#[tokio::test]
async fn status_line_shows_secondary_non_weekly_when_primary_is_weekly() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindowItem {
            used_percent: 94.0,
            window_minutes: Some(7 * 24 * 60),
            resets_at: None,
        }),
        secondary: Some(RateLimitWindowItem {
            used_percent: 35.0,
            window_minutes: Some(30 * 24 * 60),
            resets_at: None,
        }),
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::FiveHourLimit),
        Some("monthly 65% left".to_string())
    );
    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::WeeklyLimit),
        Some("weekly 6% left".to_string())
    );
}

#[tokio::test]
async fn status_line_five_hour_item_omits_weekly_only_limit() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindowItem {
            used_percent: 9.0,
            window_minutes: Some(7 * 24 * 60),
            resets_at: None,
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::FiveHourLimit),
        None
    );
    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::WeeklyLimit),
        Some("weekly 91% left".to_string())
    );
}

#[tokio::test]
async fn status_line_single_monthly_primary_omits_weekly_limit_item() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindowItem {
            used_percent: 35.0,
            window_minutes: Some(30 * 24 * 60),
            resets_at: None,
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::FiveHourLimit),
        Some("monthly 65% left".to_string())
    );
    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::WeeklyLimit),
        None
    );
}

#[tokio::test]
async fn status_line_secondary_only_non_weekly_limit_omits_primary_limit_item() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: Some(RateLimitWindowItem {
            used_percent: 35.0,
            window_minutes: Some(30 * 24 * 60),
            resets_at: None,
        }),
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));

    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::FiveHourLimit),
        None
    );
    assert_eq!(
        chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::WeeklyLimit),
        Some("monthly 65% left".to_string())
    );
}

#[tokio::test]
async fn rate_limit_snapshot_keeps_prior_credits_when_missing_from_headers() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: Some(CreditsItem {
            has_credits: true,
            unlimited: false,
            balance: Some("17.5".to_string()),
        }),
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));
    let initial_balance = chat
        .rate_limit_snapshots_by_limit_id
        .get("codex")
        .and_then(|snapshot| snapshot.credits.as_ref())
        .and_then(|credits| credits.balance.as_deref());
    assert_eq!(initial_balance, Some("17.5"));

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindowItem {
            used_percent: 80.0,
            window_minutes: Some(60),
            resets_at: Some(123),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));

    let display = chat
        .rate_limit_snapshots_by_limit_id
        .get("codex")
        .expect("rate limits should be cached");
    let credits = display
        .credits
        .as_ref()
        .expect("credits should persist when headers omit them");

    assert_eq!(credits.balance.as_deref(), Some("17.5"));
    assert!(!credits.unlimited);
    assert_eq!(
        display.primary.as_ref().map(|window| window.used_percent),
        Some(80.0)
    );
}

#[tokio::test]
async fn rate_limit_snapshot_updates_and_retains_plan_type() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindowItem {
            used_percent: 10.0,
            window_minutes: Some(60),
            resets_at: None,
        }),
        secondary: Some(RateLimitWindowItem {
            used_percent: 5.0,
            window_minutes: Some(300),
            resets_at: None,
        }),
        credits: None,
        individual_limit: None,
        plan_type: Some("plus".to_string()),
        rate_limit_reached: None,
    }));
    assert_eq!(chat.plan_type, Some(PlanType::Plus));

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindowItem {
            used_percent: 25.0,
            window_minutes: Some(30),
            resets_at: Some(123),
        }),
        secondary: Some(RateLimitWindowItem {
            used_percent: 15.0,
            window_minutes: Some(300),
            resets_at: Some(234),
        }),
        credits: None,
        individual_limit: None,
        plan_type: Some("pro".to_string()),
        rate_limit_reached: None,
    }));
    assert_eq!(chat.plan_type, Some(PlanType::Pro));

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindowItem {
            used_percent: 30.0,
            window_minutes: Some(60),
            resets_at: Some(456),
        }),
        secondary: Some(RateLimitWindowItem {
            used_percent: 18.0,
            window_minutes: Some(300),
            resets_at: Some(567),
        }),
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));
    assert_eq!(chat.plan_type, Some(PlanType::Pro));
}

#[tokio::test]
async fn rate_limit_snapshots_keep_separate_entries_per_limit_id() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: Some("codex".to_string()),
        limit_name: Some("codex".to_string()),
        primary: Some(RateLimitWindowItem {
            used_percent: 20.0,
            window_minutes: Some(300),
            resets_at: Some(100),
        }),
        secondary: None,
        credits: Some(CreditsItem {
            has_credits: true,
            unlimited: false,
            balance: Some("5.00".to_string()),
        }),
        individual_limit: None,
        plan_type: Some("pro".to_string()),
        rate_limit_reached: None,
    }));

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: Some("codex_other".to_string()),
        limit_name: Some("codex_other".to_string()),
        primary: Some(RateLimitWindowItem {
            used_percent: 90.0,
            window_minutes: Some(60),
            resets_at: Some(200),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: Some("pro".to_string()),
        rate_limit_reached: None,
    }));

    let codex = chat
        .rate_limit_snapshots_by_limit_id
        .get("codex")
        .expect("codex snapshot should exist");
    let other = chat
        .rate_limit_snapshots_by_limit_id
        .get("codex_other")
        .expect("codex_other snapshot should exist");

    assert_eq!(codex.primary.as_ref().map(|w| w.used_percent), Some(20.0));
    assert_eq!(
        codex
            .credits
            .as_ref()
            .and_then(|credits| credits.balance.as_deref()),
        Some("5.00")
    );
    assert_eq!(other.primary.as_ref().map(|w| w.used_percent), Some(90.0));
    assert!(other.credits.is_none());
}

#[tokio::test]
async fn rate_limit_switch_prompt_skips_when_on_lower_cost_model() {
    let (mut chat, _, _) = make_chatwidget_manual(Some(NUDGE_MODEL_SLUG)).await;
    chat.has_chatgpt_account = true;

    chat.on_rate_limit_snapshot(Some(snapshot(95.0)));

    assert!(matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Idle
    ));
}

#[tokio::test]
async fn rate_limit_switch_prompt_skips_non_codex_limit() {
    let (mut chat, _, _) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.has_chatgpt_account = true;

    chat.on_rate_limit_snapshot(Some(RateLimitSnapshotItem {
        limit_id: Some("codex_other".to_string()),
        limit_name: Some("codex_other".to_string()),
        primary: Some(RateLimitWindowItem {
            used_percent: 95.0,
            window_minutes: Some(60),
            resets_at: None,
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached: None,
    }));

    assert!(matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Idle
    ));
}

#[tokio::test]
async fn rate_limit_switch_prompt_shows_once_per_session() {
    let (mut chat, _, _) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.has_chatgpt_account = true;

    chat.on_rate_limit_snapshot(Some(snapshot(90.0)));
    assert!(
        chat.rate_limit_warnings.primary_index >= 1,
        "warnings not emitted"
    );
    chat.maybe_show_pending_rate_limit_prompt();
    assert!(matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Shown
    ));

    chat.on_rate_limit_snapshot(Some(snapshot(95.0)));
    assert!(matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Shown
    ));
}

#[tokio::test]
async fn rate_limit_switch_prompt_respects_hidden_notice() {
    let (mut chat, _, _) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.has_chatgpt_account = true;
    chat.config.notices.hide_rate_limit_model_nudge = Some(true);

    chat.on_rate_limit_snapshot(Some(snapshot(95.0)));

    assert!(matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Idle
    ));
}

#[tokio::test]
async fn rate_limit_switch_prompt_defers_until_task_complete() {
    let (mut chat, _, _) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.has_chatgpt_account = true;

    chat.bottom_pane.set_task_running(true);
    chat.on_rate_limit_snapshot(Some(snapshot(90.0)));
    assert!(matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Pending
    ));

    chat.bottom_pane.set_task_running(false);
    chat.maybe_show_pending_rate_limit_prompt();
    assert!(matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Shown
    ));
}

#[tokio::test]
async fn rate_limit_switch_prompt_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.has_chatgpt_account = true;

    chat.on_rate_limit_snapshot(Some(snapshot(92.0)));
    chat.maybe_show_pending_rate_limit_prompt();

    let popup = render_bottom_popup(&chat, 80);
    assert_chatwidget_snapshot!("rate_limit_switch_prompt_popup", popup);
}

#[tokio::test]
async fn workspace_member_credits_depleted_prompts_and_sends_credits() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let mut limits = snapshot(100.0);
    limits.rate_limit_reached = Some(RateLimitReachedKind::WorkspaceMemberCreditsDepleted);
    chat.on_rate_limit_snapshot(Some(limits));

    chat.on_rate_limit_error(
        RateLimitErrorKind::Generic,
        "Usage limit reached.".to_string(),
    );
    let popup = render_bottom_popup(&chat, 90);
    assert_chatwidget_snapshot!("workspace_member_credits_depleted_prompt", popup);

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let event = next_send_add_credits_nudge_email_event(&mut rx);
    assert_eq!(event, "credits");
}

#[tokio::test]
async fn workspace_member_usage_limit_prompts_and_sends_usage_limit() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let mut limits = snapshot(100.0);
    limits.rate_limit_reached = Some(RateLimitReachedKind::WorkspaceMemberUsageLimitReached);
    chat.on_rate_limit_snapshot(Some(limits));

    chat.on_rate_limit_error(
        RateLimitErrorKind::UsageLimit,
        "Usage limit reached.".to_string(),
    );
    let popup = render_bottom_popup(&chat, 100);
    assert_chatwidget_snapshot!("workspace_member_usage_limit_prompt", popup);

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let event = next_send_add_credits_nudge_email_event(&mut rx);
    assert_eq!(event, "usage_limit");
}

#[tokio::test]
async fn header_rate_limit_snapshot_preserves_member_limit_type_for_error_prompt() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let mut usage_limits = snapshot(100.0);
    usage_limits.rate_limit_reached = Some(RateLimitReachedKind::WorkspaceMemberUsageLimitReached);
    chat.on_rate_limit_snapshot(Some(usage_limits));

    let mut header_limits = snapshot(100.0);
    header_limits.rate_limit_reached = None;
    chat.on_rate_limit_snapshot(Some(header_limits));

    chat.on_rate_limit_error(
        RateLimitErrorKind::UsageLimit,
        "Usage limit reached.".to_string(),
    );
    let popup = render_bottom_popup(&chat, 100);
    assert!(
        popup.contains("Request a limit increase from your owner"),
        "popup: {popup}"
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let event = next_send_add_credits_nudge_email_event(&mut rx);
    assert_eq!(event, "usage_limit");
}

#[tokio::test]
async fn usage_limit_error_remaps_stale_member_credits_state_to_usage_limit_prompt() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let mut limits = snapshot(100.0);
    limits.rate_limit_reached = Some(RateLimitReachedKind::WorkspaceMemberCreditsDepleted);
    chat.on_rate_limit_snapshot(Some(limits));

    chat.on_rate_limit_error(
        RateLimitErrorKind::UsageLimit,
        "Usage limit reached.".to_string(),
    );
    let popup = render_bottom_popup(&chat, 100);
    assert!(
        popup.contains("Request a limit increase from your owner"),
        "popup: {popup}"
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let event = next_send_add_credits_nudge_email_event(&mut rx);
    assert_eq!(event, "usage_limit");
}

#[tokio::test]
async fn workspace_owner_limit_states_do_not_prompt_for_owner_nudge() {
    for (limit_type, error_kind) in [
        (
            RateLimitReachedKind::WorkspaceOwnerCreditsDepleted,
            RateLimitErrorKind::Generic,
        ),
        (
            RateLimitReachedKind::WorkspaceOwnerUsageLimitReached,
            RateLimitErrorKind::UsageLimit,
        ),
        (
            RateLimitReachedKind::RateLimitReached,
            RateLimitErrorKind::Generic,
        ),
    ] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
        let mut limits = snapshot(100.0);
        limits.rate_limit_reached = Some(limit_type);
        chat.on_rate_limit_snapshot(Some(limits));

        chat.on_rate_limit_error(error_kind, "Usage limit reached.".to_string());
        let popup = render_bottom_popup(&chat, 90);
        assert!(!popup.contains("workspace owner"));
        assert_no_owner_nudge_or_rate_limit_refresh(&mut rx);
    }
}

#[tokio::test]
async fn workspace_owner_limit_states_render_state_specific_messages() {
    let cases = [
        (
            RateLimitReachedKind::WorkspaceOwnerCreditsDepleted,
            RateLimitErrorKind::Generic,
            "You're out of credits. Your workspace is out of credits. Add credits to continue using Codex.",
        ),
        (
            RateLimitReachedKind::WorkspaceOwnerUsageLimitReached,
            RateLimitErrorKind::UsageLimit,
            "Usage limit reached. You've reached your usage limit. Increase your limits to continue using codex.",
        ),
    ];

    let mut rendered_cases = Vec::new();
    for (limit_type, error_kind, expected) in cases {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
        let mut limits = snapshot(100.0);
        limits.rate_limit_reached = Some(limit_type);
        chat.on_rate_limit_snapshot(Some(limits));

        chat.on_rate_limit_error(error_kind, "Usage limit reached.".to_string());
        let rendered = drain_insert_history(&mut rx)
            .into_iter()
            .map(|lines| lines_to_single_string(&lines))
            .collect::<String>();
        assert!(rendered.contains(expected), "rendered: {rendered}");
        rendered_cases.push(rendered);
    }

    assert_chatwidget_snapshot!(
        "workspace_owner_limit_state_messages",
        rendered_cases.join("\n---\n")
    );
}

#[tokio::test]
async fn missing_rate_limit_reached_type_does_not_prompt_or_refresh() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_rate_limit_snapshot(Some(snapshot(100.0)));

    chat.on_rate_limit_error(
        RateLimitErrorKind::UsageLimit,
        "Usage limit reached.".to_string(),
    );
    let popup = render_bottom_popup(&chat, 90);
    assert!(!popup.contains("workspace owner"));
    assert_no_owner_nudge_or_rate_limit_refresh(&mut rx);
}

#[tokio::test]
async fn workspace_owner_nudge_default_no_dismisses_without_sending() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let mut limits = snapshot(100.0);
    limits.rate_limit_reached = Some(RateLimitReachedKind::WorkspaceMemberCreditsDepleted);
    chat.on_rate_limit_snapshot(Some(limits));

    chat.on_rate_limit_error(
        RateLimitErrorKind::Generic,
        "Usage limit reached.".to_string(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_no_owner_nudge_or_rate_limit_refresh(&mut rx);
}

#[tokio::test]
async fn workspace_owner_nudge_reappears_after_dismissing_no() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let mut limits = snapshot(100.0);
    limits.rate_limit_reached = Some(RateLimitReachedKind::WorkspaceMemberUsageLimitReached);
    chat.on_rate_limit_snapshot(Some(limits));

    chat.on_rate_limit_error(
        RateLimitErrorKind::UsageLimit,
        "Usage limit reached.".to_string(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_no_owner_nudge_or_rate_limit_refresh(&mut rx);

    chat.on_rate_limit_error(
        RateLimitErrorKind::UsageLimit,
        "Usage limit reached.".to_string(),
    );
    let popup = render_bottom_popup(&chat, 100);
    assert!(
        popup.contains("Request a limit increase from your owner"),
        "popup: {popup}"
    );
}

#[tokio::test]
async fn workspace_owner_credits_nudge_completion_renders_feedback() {
    let cases = [
        (Ok("sent".to_string()), "Workspace owner notified."),
        (
            Ok("cooldown_active".to_string()),
            "Workspace owner was already notified recently.",
        ),
        (
            Err("request failed".to_string()),
            "Could not notify your workspace owner. Please try again.",
        ),
    ];

    let mut rendered_cases = Vec::new();
    for (result, expected) in cases {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
        chat.start_add_credits_nudge_email_request("credits".to_string());
        chat.finish_add_credits_nudge_email_request(result);
        let rendered = drain_insert_history(&mut rx)
            .into_iter()
            .map(|lines| lines_to_single_string(&lines))
            .collect::<String>();
        assert!(rendered.contains(expected), "rendered: {rendered}");
        rendered_cases.push(rendered);
    }

    assert_chatwidget_snapshot!(
        "workspace_owner_credits_nudge_completion_feedback",
        rendered_cases.join("\n---\n")
    );
}

#[tokio::test]
async fn workspace_owner_usage_limit_nudge_completion_renders_feedback() {
    let cases = [
        (Ok("sent".to_string()), "Limit increase requested."),
        (
            Ok("cooldown_active".to_string()),
            "A limit increase was already requested recently.",
        ),
        (
            Err("request failed".to_string()),
            "Could not request a limit increase. Please try again.",
        ),
    ];

    let mut rendered_cases = Vec::new();
    for (result, expected) in cases {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
        chat.start_add_credits_nudge_email_request("usage_limit".to_string());
        chat.finish_add_credits_nudge_email_request(result);
        let rendered = drain_insert_history(&mut rx)
            .into_iter()
            .map(|lines| lines_to_single_string(&lines))
            .collect::<String>();
        assert!(rendered.contains(expected), "rendered: {rendered}");
        rendered_cases.push(rendered);
    }

    assert_chatwidget_snapshot!(
        "workspace_owner_usage_limit_nudge_completion_feedback",
        rendered_cases.join("\n---\n")
    );
}

fn next_send_add_credits_nudge_email_event(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> String {
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::SendAddCreditsNudgeEmail { credit_type } = event {
            return credit_type;
        }
    }
    panic!("expected SendAddCreditsNudgeEmail app event");
}

fn assert_no_owner_nudge_or_rate_limit_refresh(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(
                event,
                AppEvent::SendAddCreditsNudgeEmail { .. } | AppEvent::RefreshRateLimits { .. }
            ),
            "unexpected event: {event:?}"
        );
    }
}

#[tokio::test]
async fn streaming_final_answer_keeps_task_running_state() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());

    chat.on_task_started();
    chat.on_agent_message_delta("Final answer line\n".to_string());
    chat.on_commit_tick();
    drain_insert_history(&mut rx);

    assert!(chat.bottom_pane.is_task_running());
    assert!(!chat.bottom_pane.status_indicator_visible());

    chat.bottom_pane
        .set_composer_text("queued submission".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    assert_eq!(chat.input_queue.queued_user_messages.len(), 1);
    assert_eq!(
        chat.input_queue.queued_user_messages.front().unwrap().text,
        "queued submission"
    );
    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    match op_rx.try_recv() {
        Ok(Op::Interrupt { .. }) => {}
        other => panic!("expected Op::Interrupt, got {other:?}"),
    }
    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());
}

fn assert_goal_paused_event(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    thread_id: String,
) {
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::SetThreadGoalStatus {
            thread_id: event_thread_id,
            status: GoalState::Paused,
        }) if event_thread_id == thread_id
    );
}

#[tokio::test]
async fn idle_commit_ticks_do_not_restore_status_without_commentary_completion() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_task_started();
    assert_eq!(chat.bottom_pane.status_indicator_visible(), true);

    chat.on_agent_message_delta("Final answer line\n".to_string());
    chat.on_commit_tick();
    drain_insert_history(&mut rx);

    assert_eq!(chat.bottom_pane.status_indicator_visible(), false);
    assert_eq!(chat.bottom_pane.is_task_running(), true);

    chat.on_commit_tick();
    assert_eq!(chat.bottom_pane.status_indicator_visible(), false);
}

#[tokio::test]
async fn final_answer_completion_restores_status_indicator_for_pending_steer() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());

    chat.on_task_started();
    assert_eq!(chat.bottom_pane.status_indicator_visible(), true);

    chat.on_agent_message_delta("Long output line 1\n".to_string());
    chat.on_commit_tick();
    drain_insert_history(&mut rx);
    chat.on_agent_message_delta("Long output line 2\n".to_string());
    chat.on_commit_tick();
    drain_insert_history(&mut rx);

    assert_eq!(chat.bottom_pane.status_indicator_visible(), false);
    assert_eq!(chat.bottom_pane.is_task_running(), true);

    chat.bottom_pane.set_composer_text(
        "Please summarize the rest more briefly.".to_string(),
        Vec::new(),
        Vec::new(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(chat.input_queue.pending_steers.len(), 1);
    let items = match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => items,
        other => panic!("expected Op::UserTurn, got {other:?}"),
    };
    assert_eq!(
        items,
        vec![UserInput::Text {
            text: "Please summarize the rest more briefly.".to_string(),
            text_elements: Vec::new(),
        }]
    );

    complete_assistant_message(
        &mut chat,
        "msg-final",
        "Long output line 1\nLong output line 2\n",
        Some(MessagePhase::FinalAnswer),
    );

    assert_eq!(chat.bottom_pane.status_indicator_visible(), true);
    assert_eq!(chat.bottom_pane.is_task_running(), true);

    complete_user_message(
        &mut chat,
        "user-steer",
        "Please summarize the rest more briefly.",
    );

    assert!(chat.input_queue.pending_steers.is_empty());
    assert_eq!(chat.bottom_pane.status_indicator_visible(), true);
    assert_eq!(chat.bottom_pane.is_task_running(), true);
}

#[tokio::test]
async fn nonfinal_completion_restores_status_indicator_before_exec_begin() {
    for phase in [MessagePhase::Commentary, MessagePhase::PartialAnswer] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

        chat.on_task_started();
        assert_eq!(chat.bottom_pane.status_indicator_visible(), true);

        chat.on_agent_message_delta("Preamble line\n".to_string());
        chat.on_commit_tick();
        drain_insert_history(&mut rx);

        assert_eq!(chat.bottom_pane.status_indicator_visible(), false);

        complete_assistant_message(&mut chat, "msg-commentary", "Preamble line\n", Some(phase));

        assert_eq!(chat.bottom_pane.status_indicator_visible(), true);
        assert_eq!(chat.bottom_pane.is_task_running(), true);

        begin_exec(&mut chat, "call-1", "echo hi");
        assert_eq!(chat.bottom_pane.status_indicator_visible(), true);
    }
}

#[tokio::test]
async fn fast_status_indicator_requires_chatgpt_auth() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());
    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));

    assert!(!chat.should_show_fast_status(chat.current_model(), chat.current_service_tier(),));

    set_chatgpt_auth(&mut chat);
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());

    assert!(chat.should_show_fast_status(chat.current_model(), chat.current_service_tier(),));
}

#[tokio::test]
async fn fast_status_indicator_is_hidden_for_models_without_fast_support() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.3-codex")).await;
    set_fast_mode_test_catalog(&mut chat);
    assert!(!get_available_model(&chat, "gpt-5.3-codex").supports_fast_mode());
    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));
    set_chatgpt_auth(&mut chat);
    set_fast_mode_test_catalog(&mut chat);
    assert!(!get_available_model(&chat, "gpt-5.3-codex").supports_fast_mode());

    assert!(!chat.should_show_fast_status(chat.current_model(), chat.current_service_tier(),));
}

#[tokio::test]
async fn fast_status_indicator_is_hidden_when_fast_mode_is_off() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());
    set_chatgpt_auth(&mut chat);
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());

    assert!(!chat.should_show_fast_status(chat.current_model(), chat.current_service_tier(),));
}

#[tokio::test]
async fn ui_snapshots_small_heights_idle() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let (chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    for h in [1u16, 2, 3] {
        let name = format!("chat_small_idle_h{h}");
        let mut terminal = Terminal::new(TestBackend::new(40, h)).expect("create terminal");
        terminal
            .draw(|f| chat.render(f.area(), f.buffer_mut()))
            .expect("draw chat idle");
        assert_chatwidget_snapshot!(name, normalized_backend_snapshot(terminal.backend()));
    }
}

#[tokio::test]
async fn ui_snapshots_small_heights_task_running() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    handle_turn_started(&mut chat, "turn-1");
    handle_agent_reasoning_delta(&mut chat, "**Thinking**");
    for h in [1u16, 2, 3] {
        let name = format!("chat_small_running_h{h}");
        let mut terminal = Terminal::new(TestBackend::new(40, h)).expect("create terminal");
        terminal
            .draw(|f| chat.render(f.area(), f.buffer_mut()))
            .expect("draw chat running");
        assert_chatwidget_snapshot!(name, normalized_backend_snapshot(terminal.backend()));
    }
}

#[tokio::test]
#[serial]
async fn ambient_pet_stays_hidden_until_a_pet_is_selected() {
    use ratatui::layout::Rect;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_pet_image_support_for_tests(crate::pets::PetImageSupport::Supported(
        crate::pets::ImageProtocol::Kitty,
    ));
    assert!(chat.ambient_pet.is_none());

    crate::pets::write_test_pack(&chat.config.codex_home);
    chat.set_tui_pet(Some("codex".to_string()));

    let area = Rect::new(0, 0, 60, 20);
    let draw = chat
        .ambient_pet_draw(area, area.bottom())
        .expect("ambient pet draw request");
    assert_eq!(draw.x, 51);
    assert_eq!(draw.y, 14);
    assert_eq!(draw.columns, 9);
    assert_eq!(draw.rows, 5);
    assert_eq!(
        draw.y.saturating_add(draw.rows),
        area.bottom().saturating_sub(1)
    );

    handle_turn_started(&mut chat, "turn-1");
    handle_agent_reasoning_delta(&mut chat, "**Thinking**");
    let draw_with_status = chat
        .ambient_pet_draw(area, area.bottom())
        .expect("ambient pet draw request with status");
    assert_eq!(draw_with_status.y, draw.y);
    assert_eq!(
        draw_with_status.y.saturating_add(draw_with_status.rows),
        area.bottom().saturating_sub(1)
    );
}

#[tokio::test]
#[serial]
async fn ambient_pet_screen_bottom_anchor_uses_terminal_bottom() {
    use octopus_world_terminal_config::TuiPetAnchor;
    use ratatui::layout::Rect;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    enable_test_ambient_pet(&mut chat);

    let terminal_area = Rect::new(0, 0, 80, 24);
    let composer_bottom_y = 20;
    let default_draw = chat
        .ambient_pet_draw(terminal_area, composer_bottom_y)
        .expect("composer-anchored pet draw request");
    assert_eq!(default_draw.y, 14);

    chat.config.terminal.pet_anchor = TuiPetAnchor::ScreenBottom;
    let screen_bottom_draw = chat
        .ambient_pet_draw(terminal_area, composer_bottom_y)
        .expect("screen-bottom anchored pet draw request");
    assert_eq!(screen_bottom_draw.y, 18);
}

#[tokio::test]
#[serial]
async fn ambient_pet_can_be_disabled() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.set_tui_pet(Some(crate::pets::DISABLED_PET_ID.to_string()));

    assert!(chat.ambient_pet.is_none());
}

#[tokio::test]
#[serial]
async fn ambient_pet_reserves_history_wrap_width() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    enable_test_ambient_pet(&mut chat);

    assert_eq!(chat.history_wrap_width(80), 69);

    chat.set_tui_pet(Some(crate::pets::DISABLED_PET_ID.to_string()));

    assert_eq!(chat.history_wrap_width(80), 80);
}

#[tokio::test]
#[serial]
async fn ambient_pet_reduces_stream_width_and_composer_text_width() {
    use ratatui::Terminal;

    let (mut with_pet, _with_pet_rx, _with_pet_op_rx) = make_chatwidget_manual(None).await;
    enable_test_ambient_pet(&mut with_pet);
    with_pet.last_rendered_width.set(Some(80));
    let stream_width_with_pet = with_pet.current_stream_width(2);

    let (mut disabled, _disabled_rx, _disabled_op_rx) = make_chatwidget_manual(None).await;
    disabled.set_tui_pet(Some(crate::pets::DISABLED_PET_ID.to_string()));
    disabled.last_rendered_width.set(Some(80));
    let stream_width_without_pet = disabled.current_stream_width(2);

    assert_eq!(
        stream_width_with_pet,
        crate::width::usable_content_width(69, 2)
    );
    assert_eq!(
        stream_width_without_pet,
        crate::width::usable_content_width(80, 2)
    );
    assert!(stream_width_with_pet < stream_width_without_pet);

    let draft =
        "Minim commodo esse elit Lorem exercitation elit ipsum proident labore. Esse culpa aliqua"
            .to_string();
    with_pet
        .bottom_pane
        .set_composer_text(draft.clone(), Vec::new(), Vec::new());
    disabled
        .bottom_pane
        .set_composer_text(draft, Vec::new(), Vec::new());

    let mut with_pet_terminal = Terminal::new(TestBackend::new(80, 6)).expect("create terminal");
    with_pet_terminal
        .draw(|f| with_pet.render(f.area(), f.buffer_mut()))
        .expect("draw pet-enabled chat");
    let mut disabled_terminal = Terminal::new(TestBackend::new(80, 6)).expect("create terminal");
    disabled_terminal
        .draw(|f| disabled.render(f.area(), f.buffer_mut()))
        .expect("draw disabled-pet chat");

    let pet_row = buffer_row_containing(with_pet_terminal.backend().buffer(), "Minim")
        .expect("pet-enabled composer row should render draft");
    let disabled_row = buffer_row_containing(disabled_terminal.backend().buffer(), "Minim")
        .expect("disabled-pet composer row should render draft");

    assert!(row_tail_is_blank(&pet_row, 69));
    assert!(!row_tail_is_blank(&disabled_row, 69));
}

fn buffer_row_containing(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).expect("cell should exist").symbol())
                .collect::<String>()
        })
        .find(|row| row.contains(text))
}

fn row_tail_is_blank(row: &str, start_col: usize) -> bool {
    row.chars().skip(start_col).all(char::is_whitespace)
}

#[tokio::test]
#[serial]
async fn ambient_pet_draw_uses_terminal_screen_area_not_short_inline_viewport() {
    use ratatui::layout::Rect;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    enable_test_ambient_pet(&mut chat);

    assert!(
        chat.ambient_pet_draw(Rect::new(0, 21, 80, 3,), 24)
            .is_none(),
        "a normal short inline viewport cannot fit the ambient pet"
    );

    let draw = chat
        .ambient_pet_draw(Rect::new(0, 0, 80, 24), 24)
        .expect("full terminal screen has room for the ambient pet");
    assert_eq!(draw.x, 71);
    assert_eq!(draw.y, 18);
}

#[tokio::test]
#[serial]
async fn ambient_pet_hides_notification_text_overlay() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    enable_test_ambient_pet(&mut chat);
    for (kind, label) in [
        (crate::pets::PetNotificationKind::Running, "Running"),
        (crate::pets::PetNotificationKind::Waiting, "Needs input"),
        (crate::pets::PetNotificationKind::Review, "Ready"),
        (crate::pets::PetNotificationKind::Failed, "Blocked"),
    ] {
        chat.set_ambient_pet_notification(kind, None);
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("create terminal");
        terminal
            .draw(|f| chat.render(f.area(), f.buffer_mut()))
            .expect("draw ambient pet notification");
        assert!(
            !normalized_backend_snapshot(terminal.backend()).contains(label),
            "did not expect {label} notification text to render"
        );
    }
}

#[tokio::test]
async fn status_widget_active_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    handle_turn_started(&mut chat, "turn-1");
    handle_agent_reasoning_delta(&mut chat, "**Analyzing**");
    let height = chat.desired_height(80);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height))
        .expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw status widget");
    assert_chatwidget_snapshot!(
        "status_widget_active",
        normalized_backend_snapshot(terminal.backend())
    );
}

#[tokio::test]
async fn stream_error_updates_status_indicator() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.bottom_pane.set_task_running(true);
    let msg = "Reconnecting... 2/5";
    let details = "Idle timeout waiting for SSE";
    handle_stream_error(&mut chat, msg, Some(details.to_string()));

    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected no history cell for StreamError event"
    );
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), msg);
    assert_eq!(status.details(), Some(details));
}

#[tokio::test]
async fn stream_error_restores_hidden_status_indicator() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    chat.on_agent_message_delta("Preamble line\n".to_string());
    chat.on_commit_tick();
    drain_insert_history(&mut rx);
    assert!(!chat.bottom_pane.status_indicator_visible());

    let msg = "Reconnecting... 2/5";
    let details = "Idle timeout waiting for SSE";
    handle_stream_error(&mut chat, msg, Some(details.to_string()));

    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), msg);
    assert_eq!(status.details(), Some(details));
}

#[tokio::test]
async fn warning_event_adds_warning_history_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    handle_warning(&mut chat, "test warning message");

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected one warning history cell");
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("test warning message"),
        "warning cell missing content: {rendered}"
    );
}

#[tokio::test]
async fn repeated_model_metadata_warning_is_hidden_for_same_slug() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let warning = "Model metadata for `unknown-model` not found. Defaulting to fallback metadata; this can degrade performance and cause issues.";

    handle_warning(&mut chat, warning);
    handle_warning(&mut chat, warning);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected one warning history cell");
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("unknown-model"),
        "warning cell missing model slug: {rendered}"
    );
}

#[tokio::test]
async fn repeated_generic_warning_is_not_hidden() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    handle_warning(&mut chat, "test warning message");
    handle_warning(&mut chat, "test warning message");

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 2, "expected both warning history cells");
}

#[tokio::test]
async fn status_line_invalid_items_warn_once() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.config.terminal.status_line = Some(vec![
        "model_name".to_string(),
        "bogus_item".to_string(),
        "lines_changed".to_string(),
        "bogus_item".to_string(),
    ]);
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());

    chat.refresh_status_line();
    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected one warning history cell");
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("bogus_item"),
        "warning cell missing invalid item content: {rendered}"
    );

    chat.refresh_status_line();
    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected invalid status line warning to emit only once"
    );
}

#[tokio::test]
async fn status_line_context_used_renders_labeled_percent() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());
    chat.config.terminal.status_line = Some(vec!["context-used".to_string()]);

    chat.refresh_status_line();

    assert_eq!(status_line_text(&chat), Some("Context 0% used".to_string()));
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "context-used should remain a valid status line item"
    );
}

#[tokio::test]
async fn status_line_context_remaining_renders_labeled_percent() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());
    chat.config.terminal.status_line = Some(vec!["context-remaining".to_string()]);

    chat.refresh_status_line();

    assert_eq!(
        status_line_text(&chat),
        Some("Context 100% left".to_string())
    );
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "context-remaining should remain a valid status line item"
    );
}

#[tokio::test]
async fn status_line_legacy_context_usage_renders_context_used_percent() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());
    chat.config.terminal.status_line = Some(vec!["context-usage".to_string()]);

    chat.refresh_status_line();

    assert_eq!(status_line_text(&chat), Some("Context 0% used".to_string()));
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "legacy context-usage should remain a valid status line item"
    );
}

#[tokio::test]
async fn status_line_branch_state_resets_when_git_branch_disabled() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.status_line_branch = Some("main".to_string());
    chat.status_line_branch_pending = true;
    chat.status_line_branch_lookup_complete = true;
    chat.config.terminal.status_line = Some(vec!["model_name".to_string()]);

    chat.refresh_status_line();

    assert_eq!(chat.status_line_branch, None);
    assert!(!chat.status_line_branch_pending);
    assert!(!chat.status_line_branch_lookup_complete);
}

#[tokio::test]
async fn status_line_branch_refreshes_after_turn_complete() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    install_noop_workspace_command_runner(&mut chat);
    chat.config.terminal.status_line = Some(vec!["git-branch".to_string()]);
    chat.status_line_branch_lookup_complete = true;
    chat.status_line_branch_pending = false;

    handle_turn_completed(&mut chat, "turn-1", None);

    assert!(chat.status_line_branch_pending);
}

#[tokio::test]
async fn status_line_branch_refreshes_after_interrupt() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    install_noop_workspace_command_runner(&mut chat);
    chat.config.terminal.status_line = Some(vec!["git-branch".to_string()]);
    chat.status_line_branch_lookup_complete = true;
    chat.status_line_branch_pending = false;

    handle_turn_interrupted(&mut chat, "turn-1");

    assert!(chat.status_line_branch_pending);
}

fn install_noop_workspace_command_runner(chat: &mut ChatWidget) {
    chat.workspace_command_runner = Some(std::sync::Arc::new(NoopWorkspaceCommandRunner));
}

struct NoopWorkspaceCommandRunner;

impl crate::workspace_command::WorkspaceCommandExecutor for NoopWorkspaceCommandRunner {
    fn run(
        &self,
        _command: crate::workspace_command::WorkspaceCommand,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        crate::workspace_command::WorkspaceCommandOutput,
                        crate::workspace_command::WorkspaceCommandError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            Ok(crate::workspace_command::WorkspaceCommandOutput {
                exit_code: 1,
                stdout: String::new(),
                stderr: String::new(),
            })
        })
    }
}

#[tokio::test]
async fn status_line_fast_mode_renders_on_and_off() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.config.terminal.status_line = Some(vec!["fast-mode".to_string()]);

    chat.set_service_tier(Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()));
    chat.refresh_status_line();
    assert_eq!(status_line_text(&chat), Some("Fast off".to_string()));

    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));
    chat.refresh_status_line();
    assert_eq!(status_line_text(&chat), Some("Fast on".to_string()));
}

#[tokio::test]
async fn status_line_fast_mode_footer_snapshot() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.show_welcome_banner = false;
    chat.config.terminal.status_line = Some(vec!["fast-mode".to_string()]);
    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));
    chat.refresh_status_line();

    let width = 80;
    let height = chat.desired_height(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw fast-mode footer");
    assert_chatwidget_snapshot!(
        "status_line_fast_mode_footer",
        normalized_backend_snapshot(terminal.backend())
    );
}

#[tokio::test]
async fn status_line_model_with_reasoning_includes_fast_for_fast_capable_models() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());
    chat.config.cwd = test_project_path().abs();
    chat.config.terminal.status_line = Some(vec![
        "model-with-reasoning".to_string(),
        "context-used".to_string(),
        "current-dir".to_string(),
    ]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));
    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));
    set_chatgpt_auth(&mut chat);
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());
    chat.refresh_status_line();
    let test_cwd = test_path_display("/tmp/project");

    assert_eq!(
        status_line_text(&chat),
        Some(format!("gpt-5.4 xhigh fast · Context 0% used · {test_cwd}"))
    );

    chat.set_model("gpt-5.3-codex");
    chat.refresh_status_line();

    assert_eq!(
        status_line_text(&chat),
        Some(format!(
            "gpt-5.3-codex xhigh · Context 0% used · {test_cwd}"
        ))
    );
}

#[tokio::test]
async fn terminal_title_model_updates_on_model_change_without_manual_refresh() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.config.terminal.terminal_title = Some(vec!["model".to_string()]);
    chat.refresh_terminal_title();

    assert_eq!(chat.last_terminal_title, Some("gpt-5.4".to_string()));

    chat.set_model("gpt-5.3-codex");

    assert_eq!(chat.last_terminal_title, Some("gpt-5.3-codex".to_string()));
}

#[tokio::test]
async fn status_line_and_terminal_title_reasoning_render_only_effort() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.config.terminal.status_line = Some(vec!["reasoning".to_string()]);
    chat.config.terminal.terminal_title = Some(vec!["reasoning".to_string()]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));
    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));

    chat.refresh_status_line();
    chat.refresh_terminal_title();

    assert_eq!(status_line_text(&chat), Some("xhigh".to_string()));
    assert_eq!(chat.last_terminal_title, Some("xhigh".to_string()));
}

#[tokio::test]
async fn status_line_reasoning_updates_on_mode_switch_without_manual_refresh() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.3-codex")).await;
    chat.set_feature_enabled("collaboration_modes", true);
    chat.config.terminal.status_line = Some(vec!["reasoning".to_string()]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    assert_eq!(status_line_text(&chat), Some("high".to_string()));

    let plan_mask = collaboration_modes::plan_mask(chat.model_catalog.as_ref())
        .expect("expected plan collaboration mode");
    chat.set_collaboration_mask(plan_mask);

    assert_eq!(status_line_text(&chat), Some("medium".to_string()));
}

#[tokio::test]
async fn status_line_model_with_reasoning_updates_on_mode_switch_without_manual_refresh() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.3-codex")).await;
    chat.set_feature_enabled("collaboration_modes", true);
    chat.config.terminal.status_line = Some(vec!["model-with-reasoning".to_string()]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    assert_eq!(
        status_line_text(&chat),
        Some("gpt-5.3-codex high".to_string())
    );

    let plan_mask = collaboration_modes::plan_mask(chat.model_catalog.as_ref())
        .expect("expected plan collaboration mode");
    chat.set_collaboration_mask(plan_mask);

    assert_eq!(
        status_line_text(&chat),
        Some("gpt-5.3-codex medium".to_string())
    );

    let default_mask = collaboration_modes::default_mask(chat.model_catalog.as_ref())
        .expect("expected default collaboration mode");
    chat.set_collaboration_mask(default_mask);

    assert_eq!(
        status_line_text(&chat),
        Some("gpt-5.3-codex high".to_string())
    );
}

#[tokio::test]
async fn status_line_model_with_reasoning_plan_mode_footer_snapshot() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.3-codex")).await;
    chat.show_welcome_banner = false;
    chat.set_feature_enabled("collaboration_modes", true);
    chat.config.terminal.status_line = Some(vec!["model-with-reasoning".to_string()]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let plan_mask = collaboration_modes::plan_mask(chat.model_catalog.as_ref())
        .expect("expected plan collaboration mode");
    chat.set_collaboration_mask(plan_mask);

    let width = 80;
    let height = chat.desired_height(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw plan-mode footer");
    assert_chatwidget_snapshot!(
        "status_line_model_with_reasoning_plan_mode_footer",
        normalized_backend_snapshot(terminal.backend())
    );
}

#[tokio::test]
async fn renamed_thread_footer_title_snapshot() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.3-codex")).await;
    chat.show_welcome_banner = false;
    chat.config.terminal.status_line = Some(vec![
        "model-with-reasoning".to_string(),
        "thread-title".to_string(),
    ]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));
    chat.refresh_status_line();

    let thread_id = uuid::Uuid::now_v7().to_string();
    chat.thread_id = Some(thread_id.clone());
    chat.on_thread_name_updated(thread_id.clone(), Some("Roadmap cleanup".to_string()));

    let width = 80;
    let height = chat.desired_height(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw renamed-thread footer");
    assert_chatwidget_snapshot!(
        "renamed_thread_footer_title",
        normalized_backend_snapshot(terminal.backend())
    );
}

#[tokio::test]
async fn status_line_model_with_reasoning_fast_footer_snapshot() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());
    chat.show_welcome_banner = false;
    chat.config.cwd = test_project_path().abs();
    chat.config.terminal.status_line = Some(vec![
        "model-with-reasoning".to_string(),
        "context-used".to_string(),
        "current-dir".to_string(),
    ]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));
    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));
    set_chatgpt_auth(&mut chat);
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());
    chat.refresh_status_line();

    let width = 80;
    let height = chat.desired_height(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw model-with-reasoning footer");
    assert_chatwidget_snapshot!(
        "status_line_model_with_reasoning_fast_footer",
        normalized_backend_snapshot(terminal.backend())
    );
}

#[tokio::test]
async fn status_line_model_with_reasoning_context_remaining_footer_snapshot() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());
    chat.show_welcome_banner = false;
    chat.config.cwd = test_project_path().abs();
    chat.config.terminal.status_line = Some(vec![
        "model-with-reasoning".to_string(),
        "context-remaining".to_string(),
        "current-dir".to_string(),
    ]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));
    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));
    set_chatgpt_auth(&mut chat);
    set_fast_mode_test_catalog(&mut chat);
    assert!(get_available_model(&chat, "gpt-5.4").supports_fast_mode());
    chat.refresh_status_line();

    let width = 80;
    let height = chat.desired_height(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw model-with-reasoning footer");
    assert_chatwidget_snapshot!(
        "status_line_model_with_reasoning_context_remaining_footer",
        normalized_backend_snapshot(terminal.backend())
    );
}

#[test]
fn goal_status_indicator_line_formats_goal_text() {
    let cases = [
        (
            GoalStatusIndicator::Active {
                usage: Some("4K / 5K".to_string()),
            },
            "Pursuing goal (4K / 5K)",
        ),
        (
            GoalStatusIndicator::BudgetLimited {
                usage: Some("4K / 5K tokens".to_string()),
            },
            "Goal unmet (4K / 5K tokens)",
        ),
        (GoalStatusIndicator::Paused, "Goal paused (/goal resume)"),
        (GoalStatusIndicator::Blocked, "Goal blocked (/goal resume)"),
        (
            GoalStatusIndicator::UsageLimited,
            "Goal hit usage limits (/goal resume)",
        ),
        (
            GoalStatusIndicator::BudgetLimited { usage: None },
            "Goal abandoned",
        ),
        (
            GoalStatusIndicator::Complete {
                usage: Some("10h 12m".to_string()),
            },
            "Goal achieved (10h 12m)",
        ),
        (
            GoalStatusIndicator::Complete { usage: None },
            "Goal achieved",
        ),
    ];

    for (indicator, expected) in cases {
        let line =
            goal_status_indicator_line(Some(&indicator)).expect("goal indicator should render");
        let actual = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(expected, actual);
    }
}

#[tokio::test]
async fn runtime_metrics_websocket_timing_logs_and_final_separator_sums_totals() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled("runtime_metrics", true);

    chat.on_task_started();
    chat.apply_runtime_metrics_delta(RuntimeMetricsSummary {
        responses_api_engine_iapi_ttft_ms: 120,
        responses_api_engine_service_tbt_ms: 50,
        ..RuntimeMetricsSummary::default()
    });

    let first_log = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .find(|line| line.contains("WebSocket timing:"))
        .expect("expected websocket timing log");
    assert!(first_log.contains("TTFT: 120ms (iapi)"));
    assert!(first_log.contains("TBT: 50ms (service)"));

    chat.apply_runtime_metrics_delta(RuntimeMetricsSummary {
        responses_api_engine_iapi_ttft_ms: 80,
        ..RuntimeMetricsSummary::default()
    });

    let second_log = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .find(|line| line.contains("WebSocket timing:"))
        .expect("expected websocket timing log");
    assert!(second_log.contains("TTFT: 80ms (iapi)"));

    chat.on_task_complete(None, None, false);
    let mut final_separator = None;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            final_separator = Some(lines_to_single_string(&cell.display_lines(300)));
        }
    }
    let final_separator = final_separator.expect("expected final separator with runtime metrics");
    assert!(final_separator.contains("TTFT: 80ms (iapi)"));
    assert!(final_separator.contains("TBT: 50ms (service)"));
}

#[tokio::test]
async fn multiple_agent_messages_in_single_turn_emit_multiple_headers() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    handle_turn_started(&mut chat, "turn-1");

    complete_assistant_message(&mut chat, "msg-first", "First message", None);

    complete_assistant_message(&mut chat, "msg-second", "Second message", None);

    handle_turn_completed(&mut chat, "turn-1", None);

    let cells = drain_insert_history(&mut rx);
    let combined: String = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect();
    assert!(
        combined.contains("First message"),
        "missing first message: {combined}"
    );
    assert!(
        combined.contains("Second message"),
        "missing second message: {combined}"
    );
    let first_idx = combined.find("First message").unwrap();
    let second_idx = combined.find("Second message").unwrap();
    assert!(first_idx < second_idx, "messages out of order: {combined}");
}

#[tokio::test]
async fn final_reasoning_then_message_without_deltas_are_rendered() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    handle_agent_reasoning_final(&mut chat);
    complete_assistant_message(&mut chat, "msg-result", "Here is the result.", None);

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!(
        "final_reasoning_then_message_without_deltas_are_rendered",
        combined
    );
}

#[tokio::test]
async fn deltas_then_same_final_message_are_rendered_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    handle_agent_reasoning_delta(&mut chat, "I will ");
    handle_agent_reasoning_delta(&mut chat, "first analyze the ");
    handle_agent_reasoning_delta(&mut chat, "request.");
    handle_agent_reasoning_final(&mut chat);

    handle_agent_message_delta(&mut chat, "Here is the ");
    handle_agent_message_delta(&mut chat, "result.");

    complete_assistant_message(&mut chat, "msg-result", "Here is the result.", None);

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!(
        "deltas_then_same_final_message_are_rendered_snapshot",
        combined
    );
}

#[tokio::test]
async fn pre_tool_use_hook_events_render_snapshot() {
    assert_hook_events_snapshot(
        "PreToolUse",
        "pre-tool-use:0:/tmp/hooks.json",
        "warming the shell",
        "pre_tool_use_hook_events_render_snapshot",
    )
    .await;
}

#[tokio::test]
async fn post_tool_use_hook_events_render_snapshot() {
    assert_hook_events_snapshot(
        "PostToolUse",
        "post-tool-use:0:/tmp/hooks.json",
        "warming the shell",
        "post_tool_use_hook_events_render_snapshot",
    )
    .await;
}

#[tokio::test]
async fn session_start_hook_events_render_snapshot() {
    assert_hook_events_snapshot(
        "SessionStart",
        "session-start:0:/tmp/hooks.json",
        "warming the shell",
        "session_start_hook_events_render_snapshot",
    )
    .await;
}

fn hook_live_and_history_snapshot(chat: &ChatWidget, phase: &str, history: &str) -> String {
    let history = if history.is_empty() {
        "<empty>"
    } else {
        history
    };
    format!(
        "{phase}\nlive hooks:\n{}history:\n{history}",
        active_hook_blob(chat),
    )
}

#[tokio::test]
async fn chatwidget_markdown_code_blocks_vt100_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    handle_turn_started(&mut chat, "turn-1");
    let width: u16 = 80;
    let height: u16 = 50;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    term.set_viewport_area(Rect::new(0, height - 1, width, 1));

    let source: &str = r#"

    -- Indented code block (4 spaces)
    SELECT *
    FROM "users"
    WHERE "email" LIKE '%@example.com';

````markdown
```sh
printf 'fenced within fenced\n'
```
````

```jsonc
{
  // comment allowed in jsonc
  "path": "C:\\Program Files\\App",
  "regex": "^foo.*(bar)?$"
}
```
"#;

    let mut it = source.chars();
    loop {
        let mut delta = String::new();
        match it.next() {
            Some(c) => delta.push(c),
            None => break,
        }
        if let Some(c2) = it.next() {
            delta.push(c2);
        }

        handle_agent_message_delta(&mut chat, delta);
        loop {
            chat.on_commit_tick();
            let mut inserted_any = false;
            while let Ok(app_ev) = rx.try_recv() {
                if let AppEvent::InsertHistoryCell(cell) = app_ev {
                    let lines = cell.display_lines(width);
                    crate::insert_history::insert_history_lines(&mut term, lines)
                        .expect("Failed to insert history lines in test");
                    inserted_any = true;
                }
            }
            if !inserted_any {
                break;
            }
        }
    }

    handle_turn_completed(&mut chat, "turn-1", None);
    for lines in drain_insert_history(&mut rx) {
        crate::insert_history::insert_history_lines(&mut term, lines)
            .expect("Failed to insert history lines in test");
    }

    assert_chatwidget_snapshot!(
        "chatwidget_markdown_code_blocks_vt100_snapshot",
        normalize_snapshot_paths(term.backend().vt100().screen().contents())
    );
}

#[tokio::test]
async fn chatwidget_tall() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(uuid::Uuid::now_v7().to_string());
    handle_turn_started(&mut chat, "turn-1");
    for i in 0..30 {
        chat.queue_user_message(format!("Hello, world! {i}").into());
    }
    let width: u16 = 80;
    let height: u16 = 24;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let desired_height = chat.desired_height(width).min(height);
    term.set_viewport_area(Rect::new(0, height - desired_height, width, desired_height));
    term.draw(|f| {
        chat.render(f.area(), f.buffer_mut());
    })
    .unwrap();
    assert_chatwidget_snapshot!(
        "chatwidget_tall",
        normalize_snapshot_paths(term.backend().vt100().screen().contents())
    );
}
